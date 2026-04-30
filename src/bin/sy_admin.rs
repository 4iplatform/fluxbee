use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Cursor};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::future;
use tar::{Archive, Builder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};
use tokio::time;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;
use zip::ZipArchive;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    FunctionCallingConfig, FunctionCallingRunner, FunctionLoopItem, FunctionRunInput, FunctionTool,
    FunctionToolDefinition, FunctionToolRegistry, ModelSettings, OpenAiResponsesClient,
};
use fluxbee_sdk::nats::{NatsClient, NatsRequestEnvelope, NatsResponseEnvelope};
use fluxbee_sdk::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED,
    MSG_NODE_STATUS_GET, SCOPE_GLOBAL, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, build_node_secret_record, classify_admin_action,
    classify_system_message, connect, derive_action_outcome, load_node_secret_record_with_root,
    redacted_node_secret_record, save_node_secret_record_with_root, try_handle_default_node_status,
    ClientConfig, NodeConfig, NodeReceiver, NodeSecretDescriptor, NodeSecretError,
    NodeSecretRecord, NodeSecretWriteOptions, NodeSender, NODE_SECRET_REDACTION_TOKEN,
};
use json_router::runtime_manifest::{
    load_runtime_manifest_from_paths, runtime_manifest_write_v2_gate_enabled_from_env,
    write_runtime_manifest_file_atomic, RuntimeManifest, RuntimeManifestEntry,
};
use json_router::runtime_package::{
    install_validated_package_with_options, validate_package, DIST_RUNTIME_MANIFEST_PATH,
    DIST_RUNTIME_ROOT_DIR,
};
use json_router::shm::{
    now_epoch_ms, LsaRegionReader, RemoteHiveEntry, FLAG_DELETED, FLAG_STALE, HEARTBEAT_STALE_MS,
};
use sha2::{Digest, Sha256};

type AdminError = Box<dyn std::error::Error + Send + Sync>;
const PRIMARY_HIVE_ID: &str = "motherbee";
const DEFAULT_BLOB_ROOT: &str = "/var/lib/fluxbee/blob";
const MSG_ADMIN_COMMAND: &str = "ADMIN_COMMAND";
const MSG_ADMIN_COMMAND_RESPONSE: &str = "ADMIN_COMMAND_RESPONSE";
const DEFAULT_ADMIN_EXECUTOR_MODEL: &str = "gpt-5.4-mini";
const ADMIN_EXECUTOR_CONFIG_SCHEMA_VERSION: u32 = 1;
const ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI: &str = "openai_api_key";
const ADMIN_EXECUTOR_DEFAULT_CATALOG_MODE: &str = "full";
const ADMIN_EXECUTOR_PLAN_KIND: &str = "executor_plan";
const ADMIN_EXECUTOR_PLAN_VERSION: &str = "0.1";
const ADMIN_EXECUTOR_SCHEMA_OVERRIDE_ACTIONS: &[&str] = &["get_admin_action_help"];
const ADMIN_EXECUTOR_PILOT_ACTIONS: &[&str] = &[
    "list_admin_actions",
    "get_admin_action_help",
    "get_runtime",
    "list_nodes",
    "publish_runtime_package",
    "sync_hint",
    "update",
    "add_route",
    "delete_route",
    "add_vpn",
    "delete_vpn",
    "start_node",
    "restart_node",
    "run_node",
    "set_node_config",
    "get_node_status",
];

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
    role: Option<String>,
    admin: Option<AdminSection>,
    wan: Option<WanSection>,
    blob: Option<BlobSection>,
    storage: Option<StorageSection>,
}

#[derive(Debug, Deserialize)]
struct AdminSection {
    listen: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WanSection {
    authorized_hives: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct BlobSection {
    path: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct AiProvidersSection {
    openai: Option<OpenAiSection>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct OpenAiSection {
    api_key: Option<String>,
    default_model: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct AdminExecutorCatalogConfig {
    mode: Option<String>,
    actions: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
struct AdminExecutorNodeConfigFile {
    #[serde(default = "default_admin_executor_schema_version")]
    schema_version: u32,
    #[serde(default)]
    config_version: u64,
    ai_providers: Option<AiProvidersSection>,
    catalog: Option<AdminExecutorCatalogConfig>,
}

#[derive(Clone)]
struct AdminExecutorAiRuntime {
    model: String,
    model_settings: ModelSettings,
    client: OpenAiResponsesClient,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminExecutorPlan {
    plan_version: String,
    kind: String,
    metadata: serde_json::Map<String, serde_json::Value>,
    execution: AdminExecutorPlanExecution,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminExecutorPlanExecution {
    strict: bool,
    stop_on_error: bool,
    allow_help_lookup: bool,
    steps: Vec<AdminExecutorPlanStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminExecutorPlanStep {
    id: String,
    action: String,
    args: serde_json::Value,
    #[serde(default)]
    executor_fill: Option<AdminExecutorFill>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AdminExecutorFill {
    #[serde(default)]
    allowed: Vec<String>,
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminExecutorRunRequest {
    execution_id: String,
    plan: AdminExecutorPlan,
    #[serde(default)]
    executor_options: AdminExecutorRunOptions,
    #[serde(default)]
    labels: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AdminExecutorRunOptions {
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminExecutorStepEvent {
    execution_id: String,
    step_id: String,
    step_index: usize,
    step_action: String,
    status: String,
    timestamp: u64,
    summary: String,
    tool_name: Option<String>,
    tool_args_preview: Option<serde_json::Value>,
    result_preview: Option<serde_json::Value>,
    error_code: Option<String>,
    error_source: Option<String>,
    error_message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminExecutorRunSummary {
    execution_id: String,
    status: String,
    total_steps: usize,
    completed_steps: usize,
    failed_step_id: Option<String>,
    failed_step_action: Option<String>,
    failure: Option<AdminExecutorFailure>,
    log_path: Option<String>,
    final_message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminExecutorFailure {
    code: String,
    detail: String,
    source: String,
    step_id: Option<String>,
    step_action: Option<String>,
}

#[derive(Clone)]
struct AdminContext {
    config_dir: PathBuf,
    state_dir: PathBuf,
    socket_dir: PathBuf,
    blob_root: PathBuf,
    node_name: String,
    hive_id: String,
    authorized_hives: Vec<String>,
    nats_endpoint: String,
    nats_client: Arc<NatsClient>,
    command_lock: Arc<Mutex<()>>,
    executor_runtime: Arc<Mutex<Option<AdminExecutorAiRuntime>>>,
    executor_configured: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PublishRuntimePackageSource {
    InlinePackage { files: BTreeMap<String, String> },
    BundleUpload { blob_path: String },
}

#[derive(Debug, Clone, Deserialize)]
struct PublishRuntimePackageRequest {
    source: PublishRuntimePackageSource,
    #[serde(default)]
    set_current: Option<bool>,
    #[serde(default)]
    sync_to: Vec<String>,
    #[serde(default)]
    update_to: Vec<String>,
}

struct AdminRouterClient {
    sender: RwLock<NodeSender>,
    pending_admin: Mutex<HashMap<String, oneshot::Sender<Message>>>,
    system_tx: broadcast::Sender<Message>,
    query_tx: broadcast::Sender<Message>,
    internal_admin_tx: broadcast::Sender<Message>,
    system_command_tx: broadcast::Sender<Message>,
}

impl AdminRouterClient {
    fn new(sender: NodeSender) -> Self {
        let (system_tx, _) = broadcast::channel(256);
        let (query_tx, _) = broadcast::channel(256);
        let (internal_admin_tx, _) = broadcast::channel(256);
        let (system_command_tx, _) = broadcast::channel(256);
        Self {
            sender: RwLock::new(sender),
            pending_admin: Mutex::new(HashMap::new()),
            system_tx,
            query_tx,
            internal_admin_tx,
            system_command_tx,
        }
    }

    async fn connect_with_retry(
        config: NodeConfig,
        delay: Duration,
    ) -> Result<Arc<Self>, fluxbee_sdk::NodeError> {
        let (sender, receiver) = Self::connect_once_with_retry(&config, delay).await?;
        let client = Arc::new(Self::new(sender));
        client.start(receiver);
        Ok(client)
    }

    async fn connect_once_with_retry(
        config: &NodeConfig,
        delay: Duration,
    ) -> Result<(NodeSender, NodeReceiver), fluxbee_sdk::NodeError> {
        loop {
            match connect(config).await {
                Ok(result) => return Ok(result),
                Err(err) => {
                    tracing::warn!("connect failed: {err}");
                    time::sleep(delay).await;
                }
            }
        }
    }

    fn start(self: &Arc<Self>, receiver: NodeReceiver) {
        let client = Arc::clone(self);
        tokio::spawn(async move {
            client.recv_loop(receiver).await;
        });
    }

    async fn recv_loop(self: Arc<Self>, receiver: NodeReceiver) {
        let mut receiver = receiver;
        loop {
            match receiver.recv().await {
                Ok(msg) => self.dispatch(msg).await,
                Err(err) => {
                    tracing::warn!(error = %err, "router connection interrupted; reconnect is handled internally");
                    self.drain_pending_waiters().await;
                    // The SDK's connection_manager_loop reconnects transparently and resumes
                    // sending to the same NodeReceiver channel. Creating a second connection
                    // here with the same persistent UUID would register a competing node that
                    // steals messages but has no dispatch loop, causing ADMIN_COMMAND timeouts.
                }
            }
        }
    }

    async fn sender_snapshot(&self) -> NodeSender {
        self.sender.read().await.clone()
    }

    async fn dispatch(&self, msg: Message) {
        if msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some(MSG_NODE_STATUS_GET)
        {
            let sender = self.sender.read().await.clone();
            let _ = try_handle_default_node_status(&sender, &msg).await;
            return;
        }
        if msg.meta.msg_type == SYSTEM_KIND
            && matches!(msg.meta.msg.as_deref(), Some("CONFIG_GET" | "CONFIG_SET"))
        {
            let _ = self.system_command_tx.send(msg);
            return;
        }
        if msg.meta.msg_type == "admin" && msg.meta.msg.as_deref() == Some(MSG_ADMIN_COMMAND) {
            let _ = self.internal_admin_tx.send(msg);
            return;
        }
        if matches!(
            msg.meta.msg_type.as_str(),
            "admin" | SYSTEM_KIND | "command" | "command_response" | "query" | "query_response"
        ) {
            let mut pending = self.pending_admin.lock().await;
            if let Some(tx) = pending.remove(&msg.routing.trace_id) {
                let _ = tx.send(msg);
                return;
            }
            tracing::debug!(
                trace_id = %msg.routing.trace_id,
                msg_type = %msg.meta.msg_type,
                msg = ?msg.meta.msg,
                action = ?msg.meta.action,
                src = %msg.routing.src,
                dst = ?msg.routing.dst,
                "sy.admin saw admin/system message without pending waiter"
            );
        }
        if msg.meta.msg_type == SYSTEM_KIND && msg.meta.msg.as_deref() == Some("CONFIG_RESPONSE") {
            let _ = self.system_tx.send(msg);
            return;
        }
        if msg.meta.msg_type == "query_response" {
            let _ = self.query_tx.send(msg);
        }
    }

    async fn enqueue_admin_waiter(&self, trace_id: String, tx: oneshot::Sender<Message>) {
        let mut pending = self.pending_admin.lock().await;
        pending.insert(trace_id, tx);
    }

    async fn drop_admin_waiter(&self, trace_id: &str) {
        let mut pending = self.pending_admin.lock().await;
        pending.remove(trace_id);
    }

    async fn drain_pending_waiters(&self) {
        let mut pending = self.pending_admin.lock().await;
        pending.clear();
    }

    fn subscribe_system(&self) -> broadcast::Receiver<Message> {
        self.system_tx.subscribe()
    }

    fn subscribe_query(&self) -> broadcast::Receiver<Message> {
        self.query_tx.subscribe()
    }

    fn subscribe_internal_admin(&self) -> broadcast::Receiver<Message> {
        self.internal_admin_tx.subscribe()
    }

    fn subscribe_system_commands(&self) -> broadcast::Receiver<Message> {
        self.system_command_tx.subscribe()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct RouteConfig {
    prefix: String,
    #[serde(default)]
    match_kind: Option<String>,
    action: String,
    #[serde(default)]
    next_hop_hive: Option<String>,
    #[serde(default)]
    metric: Option<u32>,
    #[serde(default)]
    priority: Option<u16>,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq)]
struct VpnConfig {
    pattern: String,
    #[serde(default)]
    match_kind: Option<String>,
    vpn_id: u32,
    #[serde(default)]
    priority: Option<u16>,
}

fn default_debug_msg_type() -> String {
    "user".to_string()
}

fn default_debug_payload() -> serde_json::Value {
    serde_json::json!({})
}

#[derive(Debug, Deserialize)]
struct DebugNodeMessageRequest {
    node_name: String,
    #[serde(default = "default_debug_msg_type")]
    msg_type: String,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    src_ilk: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default, rename = "meta_target")]
    meta_target: Option<String>,
    #[serde(default, rename = "meta_action")]
    meta_action: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    context: Option<serde_json::Value>,
    #[serde(default = "default_debug_payload")]
    payload: serde_json::Value,
    #[serde(default)]
    ttl: Option<u8>,
}

#[tokio::main]
async fn main() -> Result<(), AdminError> {
    if cfg!(not(target_os = "linux")) {
        eprintln!("sy_admin supports only Linux targets.");
        std::process::exit(1);
    }
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();

    let hive = load_hive(&config_dir)?;
    let hive_id = hive.hive_id.clone();
    let blob_root = configured_blob_root(&hive);
    let authorized_hives = hive
        .wan
        .as_ref()
        .and_then(|wan| wan.authorized_hives.clone())
        .unwrap_or_default();
    if hive.hive_id == PRIMARY_HIVE_ID && !is_mother_role(hive.role.as_deref()) {
        return Err(format!(
            "invalid hive.yaml: hive_id='{}' is reserved for role=motherbee",
            PRIMARY_HIVE_ID
        )
        .into());
    }
    if !is_mother_role(hive.role.as_deref()) {
        tracing::warn!("SY.admin solo corre en motherbee; role != motherbee");
        return Ok(());
    }
    if hive.hive_id != PRIMARY_HIVE_ID {
        return Err(format!(
            "invalid hive.yaml: role=motherbee requires hive_id='{}' (got '{}')",
            PRIMARY_HIVE_ID, hive.hive_id
        )
        .into());
    }
    // Default to loopback for safer deployments; expose externally only via explicit config/env.
    let admin_listen = std::env::var("JSR_ADMIN_LISTEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| hive.admin.as_ref().and_then(|admin| admin.listen.clone()))
        .unwrap_or_else(|| "127.0.0.1:8080".to_string());
    let node_config = NodeConfig {
        name: "SY.admin".to_string(),
        router_socket: socket_dir.clone(),
        uuid_persistence_dir: state_dir.join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: config_dir.clone(),
        version: "1.0".to_string(),
    };
    let client_config = ClientConfig::new(node_config.clone());
    let router_client = AdminRouterClient::connect_with_retry(node_config, Duration::from_secs(1))
        .await
        .map_err(|err| {
            tracing::error!("router client connect failed: {err}");
            err
        })?;

    let (broadcast_tx, broadcast_rx) = mpsc::unbounded_channel::<BroadcastRequest>();
    let http_tx = broadcast_tx.clone();
    let nats_client = Arc::new(NatsClient::from_client_config(&client_config)?);
    let command_lock = Arc::new(Mutex::new(()));
    let node_name = admin_node_name(&hive.hive_id);
    let initial_executor_runtime = build_admin_executor_ai_runtime(&hive.hive_id, &node_name)?;
    let executor_configured = Arc::new(AtomicBool::new(initial_executor_runtime.is_some()));
    let executor_runtime = Arc::new(Mutex::new(initial_executor_runtime));
    let http_ctx = AdminContext {
        config_dir: config_dir.clone(),
        state_dir: state_dir.clone(),
        socket_dir: socket_dir.clone(),
        blob_root,
        node_name,
        hive_id,
        authorized_hives,
        nats_endpoint: nats_client.endpoint().to_string(),
        nats_client,
        command_lock,
        executor_runtime,
        executor_configured,
    };
    let internal_ctx = http_ctx.clone();
    let system_ctx = http_ctx.clone();
    let http_client = router_client.clone();
    tokio::spawn(async move {
        if let Err(err) = run_http_server(&admin_listen, &http_tx, http_ctx, http_client).await {
            tracing::error!("http server error: {err}");
        }
    });

    let loop_client = router_client.clone();
    tokio::spawn(async move {
        run_broadcast_loop(broadcast_rx, loop_client).await;
    });

    let internal_client = router_client.clone();
    let internal_rx = router_client.subscribe_internal_admin();
    tokio::spawn(async move {
        run_internal_admin_loop(internal_ctx, internal_client, internal_rx).await;
    });

    let system_client = router_client.clone();
    let system_rx = router_client.subscribe_system_commands();
    tokio::spawn(async move {
        run_system_command_loop(system_ctx, system_client, system_rx).await;
    });

    future::pending::<()>().await;
    Ok(())
}

enum BroadcastRequest {
    Routes {
        routes: Vec<RouteConfig>,
        version: u64,
        ack: oneshot::Sender<Result<(), String>>,
    },
    Vpns {
        vpns: Vec<VpnConfig>,
        version: u64,
        ack: oneshot::Sender<Result<(), String>>,
    },
}

#[derive(Debug, Deserialize)]
struct OpaRequest {
    #[serde(default)]
    rego: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default, alias = "target")]
    hive: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WfRulesRequest {
    #[serde(flatten)]
    payload: serde_json::Map<String, serde_json::Value>,
    #[serde(default, alias = "target")]
    hive: Option<String>,
}

async fn run_broadcast_loop(
    mut rx: mpsc::UnboundedReceiver<BroadcastRequest>,
    client: Arc<AdminRouterClient>,
) {
    while let Some(req) = rx.recv().await {
        let result = match req {
            BroadcastRequest::Routes {
                routes,
                version,
                ack,
            } => {
                match broadcast_config_changed(
                    &client,
                    "routes",
                    None,
                    None,
                    version,
                    serde_json::json!({ "routes": routes }),
                    None,
                )
                .await
                {
                    Ok(()) => {
                        let _ = ack.send(Ok(()));
                        Ok(())
                    }
                    Err(err) => {
                        let err_msg = err.to_string();
                        let _ = ack.send(Err(err_msg.clone()));
                        tracing::warn!(error = %err, "broadcast failed");
                        Err(err_msg)
                    }
                }
            }
            BroadcastRequest::Vpns { vpns, version, ack } => {
                match broadcast_config_changed(
                    &client,
                    "vpn",
                    None,
                    None,
                    version,
                    serde_json::json!({ "vpns": vpns }),
                    None,
                )
                .await
                {
                    Ok(()) => {
                        let _ = ack.send(Ok(()));
                        Ok(())
                    }
                    Err(err) => {
                        let err_msg = err.to_string();
                        let _ = ack.send(Err(err_msg.clone()));
                        tracing::warn!(error = %err, "broadcast failed");
                        Err(err_msg)
                    }
                }
            }
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "broadcast failed; message dropped");
        }
    }
}

async fn send_broadcast_request<F>(
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    build: F,
    label: &str,
) -> Result<(), AdminError>
where
    F: FnOnce(oneshot::Sender<Result<(), String>>) -> BroadcastRequest,
{
    let (ack_tx, ack_rx) = oneshot::channel::<Result<(), String>>();
    tx.send(build(ack_tx))
        .map_err(|_| format!("broadcast queue closed for {label}"))?;

    let timeout_secs = env_timeout_secs("JSR_ADMIN_BROADCAST_TIMEOUT_SECS").unwrap_or(5);
    match time::timeout(Duration::from_secs(timeout_secs), ack_rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(err))) => Err(format!("broadcast failed for {label}: {err}").into()),
        Ok(Err(_)) => Err(format!("broadcast ack channel closed for {label}").into()),
        Err(_) => Err(format!("broadcast ack timeout for {label} ({timeout_secs}s)").into()),
    }
}

fn load_hive(config_dir: &Path) -> Result<HiveFile, AdminError> {
    let data = fs::read_to_string(config_dir.join("hive.yaml"))?;
    Ok(serde_yaml::from_str(&data)?)
}

fn configured_blob_root(hive: &HiveFile) -> PathBuf {
    hive.blob
        .as_ref()
        .and_then(|blob| blob.path.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BLOB_ROOT))
}

fn default_admin_executor_schema_version() -> u32 {
    ADMIN_EXECUTOR_CONFIG_SCHEMA_VERSION
}

fn admin_node_name(hive_id: &str) -> String {
    format!("SY.admin@{hive_id}")
}

fn admin_nodes_root() -> PathBuf {
    json_router::paths::storage_root_dir().join("nodes")
}

fn admin_executor_node_dir(hive_id: &str) -> PathBuf {
    admin_nodes_root().join("SY").join(admin_node_name(hive_id))
}

fn admin_executor_config_path(hive_id: &str) -> PathBuf {
    admin_executor_node_dir(hive_id).join("config.json")
}

fn admin_executor_log_dir(state_dir: &Path, hive_id: &str) -> PathBuf {
    state_dir
        .join("admin-executor")
        .join(hive_id)
        .join("executions")
}

fn load_admin_executor_node_config(
    hive_id: &str,
) -> Result<Option<AdminExecutorNodeConfigFile>, AdminError> {
    let path = admin_executor_config_path(hive_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)?;
    let parsed = serde_json::from_str(&raw)?;
    Ok(Some(parsed))
}

fn save_admin_executor_node_config(
    hive_id: &str,
    config: &AdminExecutorNodeConfigFile,
) -> Result<PathBuf, AdminError> {
    let path = admin_executor_config_path(hive_id);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_vec_pretty(config)?)?;
    Ok(path)
}

fn load_admin_secret_record(node_name: &str) -> Result<Option<NodeSecretRecord>, AdminError> {
    match load_node_secret_record_with_root(node_name, admin_nodes_root()) {
        Ok(record) => Ok(Some(record)),
        Err(NodeSecretError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(Box::new(err)),
    }
}

fn merged_admin_executor_openai_section(
    config: Option<&AdminExecutorNodeConfigFile>,
    secrets: Option<&NodeSecretRecord>,
) -> Option<OpenAiSection> {
    let config_openai = config
        .and_then(|cfg| cfg.ai_providers.as_ref())
        .and_then(|providers| providers.openai.clone());
    let secret_api_key = secrets
        .and_then(|record| record.secrets.get(ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);

    match config_openai {
        Some(cfg) => Some(OpenAiSection {
            api_key: secret_api_key,
            default_model: cfg.default_model,
            max_tokens: cfg.max_tokens,
            temperature: cfg.temperature,
            top_p: cfg.top_p,
        }),
        None => secret_api_key.map(|api_key| OpenAiSection {
            api_key: Some(api_key),
            default_model: None,
            max_tokens: None,
            temperature: None,
            top_p: None,
        }),
    }
}

fn build_admin_executor_ai_runtime(
    hive_id: &str,
    node_name: &str,
) -> Result<Option<AdminExecutorAiRuntime>, AdminError> {
    let config = load_admin_executor_node_config(hive_id)?;
    let secrets = load_admin_secret_record(node_name)?;
    let openai = merged_admin_executor_openai_section(config.as_ref(), secrets.as_ref());
    let Some(openai) = openai else {
        return Ok(None);
    };
    let Some(api_key) = openai
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let model = openai
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_ADMIN_EXECUTOR_MODEL)
        .to_string();
    Ok(Some(AdminExecutorAiRuntime {
        model,
        model_settings: ModelSettings {
            temperature: openai.temperature,
            top_p: openai.top_p,
            max_output_tokens: openai.max_tokens,
        },
        client: OpenAiResponsesClient::new(api_key.to_string()),
    }))
}

async fn refresh_admin_executor_ai_runtime(ctx: &AdminContext) -> Result<bool, AdminError> {
    let runtime = build_admin_executor_ai_runtime(&ctx.hive_id, &ctx.node_name)?;
    ctx.executor_configured
        .store(runtime.is_some(), Ordering::Relaxed);
    *ctx.executor_runtime.lock().await = runtime;
    Ok(ctx.executor_configured.load(Ordering::Relaxed))
}

fn admin_executor_catalog_mode(config: Option<&AdminExecutorNodeConfigFile>) -> String {
    config
        .and_then(|cfg| cfg.catalog.as_ref())
        .and_then(|catalog| catalog.mode.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(ADMIN_EXECUTOR_DEFAULT_CATALOG_MODE)
        .to_string()
}

fn executor_visible_action_specs(
    config: Option<&AdminExecutorNodeConfigFile>,
) -> Vec<&'static InternalActionSpec> {
    let mode = admin_executor_catalog_mode(config);
    let configured_actions: HashSet<&str> = config
        .and_then(|cfg| cfg.catalog.as_ref())
        .and_then(|catalog| catalog.actions.as_ref())
        .map(|actions| {
            actions
                .iter()
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let allowlist: Option<HashSet<&str>> = match mode.as_str() {
        "subset" if configured_actions.is_empty() => {
            Some(ADMIN_EXECUTOR_PILOT_ACTIONS.iter().copied().collect())
        }
        "subset" => Some(configured_actions),
        _ => None,
    };

    INTERNAL_ACTION_REGISTRY
        .iter()
        .filter(|spec| {
            allowlist
                .as_ref()
                .map(|set| set.contains(spec.action))
                .unwrap_or(true)
        })
        .collect()
}

fn admin_contract_field_schema(field: &serde_json::Value) -> serde_json::Value {
    let field_type = field
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("string");
    let description = field
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if let Some(enum_values) = field_type
        .strip_prefix("enum(")
        .and_then(|value| value.strip_suffix(')'))
    {
        let values: Vec<serde_json::Value> = enum_values
            .split('|')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| serde_json::Value::String(value.to_string()))
            .collect();
        return serde_json::json!({
            "type": "string",
            "enum": values,
            "description": description,
        });
    }
    if let Some(item_type) = field_type.strip_suffix("[]") {
        let item_schema_type = match item_type {
            "bool" => "boolean",
            "u16" | "u32" | "u64" | "i64" => "integer",
            "f32" | "f64" => "number",
            "object" => "object",
            _ => "string",
        };
        let mut schema = serde_json::json!({
            "type": "array",
            "items": {
                "type": item_schema_type,
            },
            "description": description,
        });
        if item_schema_type == "object" {
            schema["items"]["additionalProperties"] = serde_json::Value::Bool(true);
        }
        return schema;
    }
    let schema_type = match field_type {
        "bool" => "boolean",
        "u16" | "u32" | "u64" | "i64" => "integer",
        "f32" | "f64" => "number",
        "object" => "object",
        _ => "string",
    };
    let mut schema = serde_json::json!({
        "type": schema_type,
        "description": description,
    });
    if schema_type == "object" {
        schema["additionalProperties"] = serde_json::Value::Bool(true);
    }
    schema
}

fn build_admin_executor_override_definition(
    spec: &InternalActionSpec,
) -> Option<FunctionToolDefinition> {
    match spec.action {
        "get_admin_action_help" => Some(FunctionToolDefinition {
            name: spec.action.to_string(),
            description: admin_action_summary(spec.action).to_string(),
            parameters_json_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "action": {
                        "type": "string",
                        "description": "Admin action name, for example add_hive."
                    }
                },
                "required": ["action"]
            }),
        }),
        _ => None,
    }
}

fn build_admin_executor_function_definition(spec: &InternalActionSpec) -> FunctionToolDefinition {
    if let Some(definition) = build_admin_executor_override_definition(spec) {
        return definition;
    }
    let mut properties = serde_json::Map::new();
    let mut required = Vec::<String>::new();

    for field in admin_action_path_params(spec.action) {
        if let Some(name) = field.get("name").and_then(serde_json::Value::as_str) {
            properties.insert(name.to_string(), admin_contract_field_schema(&field));
            required.push(name.to_string());
        }
    }
    for field in admin_action_body_required_fields(spec.action) {
        if let Some(name) = field.get("name").and_then(serde_json::Value::as_str) {
            properties.insert(name.to_string(), admin_contract_field_schema(&field));
            if !required.iter().any(|existing| existing == name) {
                required.push(name.to_string());
            }
        }
    }
    for field in admin_action_body_optional_fields(spec.action) {
        if let Some(name) = field.get("name").and_then(serde_json::Value::as_str) {
            let mut field_schema = admin_contract_field_schema(&field);
            // Optional fields are nullable: the model passes null when the plan does not
            // declare a value, signaling "use infrastructure default."
            if let Some(type_str) = field_schema
                .get("type")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            {
                field_schema["type"] = serde_json::json!([type_str, "null"]);
            }
            properties.insert(name.to_string(), field_schema);
        }
    }

    FunctionToolDefinition {
        name: spec.action.to_string(),
        description: admin_action_summary(spec.action).to_string(),
        parameters_json_schema: serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": properties,
            "required": required,
        }),
    }
}

fn build_admin_executor_function_catalog(
    config: Option<&AdminExecutorNodeConfigFile>,
) -> Vec<FunctionToolDefinition> {
    executor_visible_action_specs(config)
        .into_iter()
        .map(build_admin_executor_function_definition)
        .collect()
}

fn redact_executor_log_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (key, entry) in map {
                let lower = key.to_ascii_lowercase();
                if matches!(
                    lower.as_str(),
                    "api_key" | "token" | "secret" | "password" | "authorization"
                ) {
                    redacted.insert(
                        key.clone(),
                        serde_json::Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()),
                    );
                } else {
                    redacted.insert(key.clone(), redact_executor_log_value(entry));
                }
            }
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(redact_executor_log_value).collect())
        }
        _ => value.clone(),
    }
}

fn persist_admin_executor_run_log(
    ctx: &AdminContext,
    execution_id: &str,
    request: &AdminExecutorRunRequest,
    events: &[AdminExecutorStepEvent],
    summary: &AdminExecutorRunSummary,
) -> Result<String, AdminError> {
    let dir = admin_executor_log_dir(&ctx.state_dir, &ctx.hive_id);
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{execution_id}.json"));
    let body = serde_json::json!({
        "execution_id": execution_id,
        "request": {
            "execution_id": request.execution_id,
            "plan": redact_executor_log_value(&serde_json::to_value(&request.plan).unwrap_or_else(|_| serde_json::json!({}))),
            "executor_options": redact_executor_log_value(&serde_json::to_value(&request.executor_options).unwrap_or_else(|_| serde_json::json!({}))),
            "labels": redact_executor_log_value(&serde_json::to_value(&request.labels).unwrap_or_else(|_| serde_json::json!({}))),
        },
        "events": redact_executor_log_value(&serde_json::to_value(events).unwrap_or_else(|_| serde_json::json!([]))),
        "summary": redact_executor_log_value(&serde_json::to_value(summary).unwrap_or_else(|_| serde_json::json!({}))),
        "written_at": now_epoch_ms(),
    });
    fs::write(&path, serde_json::to_vec_pretty(&body)?)?;
    Ok(path.display().to_string())
}

fn parse_executor_plan(params: serde_json::Value) -> Result<AdminExecutorPlan, String> {
    let plan = serde_json::from_value::<AdminExecutorPlan>(params)
        .map_err(|err| format!("invalid executor_plan: {err}"))?;
    validate_executor_plan(&plan)?;
    Ok(plan)
}

fn validate_executor_plan(plan: &AdminExecutorPlan) -> Result<(), String> {
    if plan.plan_version.trim() != ADMIN_EXECUTOR_PLAN_VERSION {
        return Err(format!(
            "unsupported plan_version '{}'; expected {}",
            plan.plan_version, ADMIN_EXECUTOR_PLAN_VERSION
        ));
    }
    if plan.kind.trim() != ADMIN_EXECUTOR_PLAN_KIND {
        return Err(format!(
            "invalid kind '{}'; expected {}",
            plan.kind, ADMIN_EXECUTOR_PLAN_KIND
        ));
    }
    if plan.execution.steps.is_empty() {
        return Err("execution.steps must contain at least one step".to_string());
    }
    let mut seen_ids = HashSet::new();
    for (index, step) in plan.execution.steps.iter().enumerate() {
        let step_label = format!("execution.steps[{index}]");
        let step_id = step.id.trim();
        if step_id.is_empty() {
            return Err(format!("{step_label}.id must be non-empty"));
        }
        if !seen_ids.insert(step_id.to_string()) {
            return Err(format!("duplicate step id '{}'", step.id));
        }
        let action = step.action.trim();
        if action.is_empty() {
            return Err(format!("{step_label}.action must be non-empty"));
        }
        let spec = resolve_internal_action_spec(action)
            .map_err(|detail| format!("{step_label}.action {detail}: '{action}'"))?;
        let function_schema = build_admin_executor_function_definition(spec).parameters_json_schema;
        validate_value_against_schema(&step.args, &function_schema, &format!("{step_label}.args"))?;
        validate_executor_fill(step, &function_schema, &step_label)?;
    }
    Ok(())
}

fn validate_executor_fill(
    step: &AdminExecutorPlanStep,
    action_schema: &serde_json::Value,
    step_label: &str,
) -> Result<(), String> {
    let Some(fill) = step.executor_fill.as_ref() else {
        return Ok(());
    };
    let properties = action_schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| format!("{step_label}.args schema is missing properties"))?;
    let mut seen = HashSet::new();
    for field in &fill.allowed {
        let name = field.trim();
        if name.is_empty() {
            return Err(format!(
                "{step_label}.executor_fill.allowed contains empty field"
            ));
        }
        if !seen.insert(name.to_string()) {
            return Err(format!(
                "{step_label}.executor_fill.allowed contains duplicate field '{}'",
                field
            ));
        }
        if !properties.contains_key(name) {
            return Err(format!(
                "{step_label}.executor_fill.allowed references unknown field '{}'",
                field
            ));
        }
    }
    Ok(())
}

fn validate_value_against_schema(
    value: &serde_json::Value,
    schema: &serde_json::Value,
    path: &str,
) -> Result<(), String> {
    let schema_obj = schema
        .as_object()
        .ok_or_else(|| format!("{path}: schema must be an object"))?;
    let type_entry = schema_obj
        .get("type")
        .ok_or_else(|| format!("{path}: schema.type is required"))?;
    // type can be a string ("string") or an array (["string", "null"])
    let (schema_type, nullable) = if let Some(s) = type_entry.as_str() {
        (s, false)
    } else if let Some(arr) = type_entry.as_array() {
        let nullable = arr.iter().any(|v| v.as_str() == Some("null"));
        let primary = arr
            .iter()
            .find_map(|v| v.as_str().filter(|s| *s != "null"))
            .ok_or_else(|| format!("{path}: schema.type array has no non-null type"))?;
        (primary, nullable)
    } else {
        return Err(format!("{path}: schema.type must be a string or array"));
    };
    if nullable && value.is_null() {
        return Ok(());
    }
    match schema_type {
        "object" => validate_object_against_schema(value, schema_obj, path),
        "array" => validate_array_against_schema(value, schema_obj, path),
        "string" | "boolean" | "integer" | "number" | "null" => {
            validate_scalar_against_schema(value, schema_obj, path)
        }
        other => Err(format!("{path}: unsupported schema type '{other}'")),
    }
}

fn validate_object_against_schema(
    value: &serde_json::Value,
    schema_obj: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("{path}: expected object"))?;
    let properties = schema_obj
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required = schema_obj
        .get("required")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    for field in required {
        let field_name = field
            .as_str()
            .ok_or_else(|| format!("{path}: required entries must be strings"))?;
        if !obj.contains_key(field_name) {
            return Err(format!("{path}: missing required field '{field_name}'"));
        }
    }
    let additional_properties = schema_obj
        .get("additionalProperties")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    if !additional_properties {
        for key in obj.keys() {
            if !properties.contains_key(key) {
                return Err(format!("{path}: unexpected field '{key}'"));
            }
        }
    }
    for (key, field_value) in obj {
        if let Some(field_schema) = properties.get(key) {
            validate_value_against_schema(field_value, field_schema, &format!("{path}.{key}"))?;
        }
    }
    Ok(())
}

fn validate_array_against_schema(
    value: &serde_json::Value,
    schema_obj: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{path}: expected array"))?;
    let Some(item_schema) = schema_obj.get("items") else {
        return Ok(());
    };
    for (index, item) in items.iter().enumerate() {
        validate_value_against_schema(item, item_schema, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

fn validate_scalar_against_schema(
    value: &serde_json::Value,
    schema_obj: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> Result<(), String> {
    let type_entry = schema_obj
        .get("type")
        .ok_or_else(|| format!("{path}: schema.type is required"))?;
    let field_type = if let Some(s) = type_entry.as_str() {
        s
    } else if let Some(arr) = type_entry.as_array() {
        arr.iter()
            .find_map(|v| v.as_str().filter(|s| *s != "null"))
            .ok_or_else(|| format!("{path}: schema.type array has no non-null type"))?
    } else {
        return Err(format!("{path}: schema.type must be a string or array"));
    };
    let type_matches = match field_type {
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "null" => value.is_null(),
        other => return Err(format!("{path}: unsupported scalar type '{other}'")),
    };
    if !type_matches {
        return Err(format!("{path}: expected {field_type}"));
    }
    if let Some(enum_values) = schema_obj.get("enum").and_then(serde_json::Value::as_array) {
        if !enum_values.iter().any(|candidate| candidate == value) {
            return Err(format!("{path}: value is outside enum"));
        }
    }
    Ok(())
}

fn merge_executor_step_arguments(
    step: &AdminExecutorPlanStep,
    call_args: &serde_json::Value,
    function_schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let plan_args = step
        .args
        .as_object()
        .ok_or_else(|| format!("step '{}' args must be an object", step.id))?;
    let call_args = call_args
        .as_object()
        .ok_or_else(|| format!("step '{}' tool arguments must be an object", step.id))?;
    let mut merged = serde_json::Map::new();
    let allowed_fill: HashSet<&str> = step
        .executor_fill
        .as_ref()
        .map(|fill| fill.allowed.iter().map(String::as_str).collect())
        .unwrap_or_default();
    let schema_required: HashSet<&str> = function_schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    for (key, value) in plan_args {
        if let Some(call_value) = call_args.get(key) {
            if call_value != value && !call_value.is_null() {
                return Err(format!(
                    "step '{}' attempted to change declared arg '{}'",
                    step.id, key
                ));
            }
        }
        merged.insert(key.clone(), value.clone());
    }
    for (key, value) in call_args {
        if plan_args.contains_key(key) {
            continue;
        }
        // null on an optional field means "use infrastructure default" — omit from dispatch
        if value.is_null() && !schema_required.contains(key.as_str()) {
            continue;
        }
        if !allowed_fill.contains(key.as_str()) {
            return Err(format!(
                "step '{}' supplied undeclared arg '{}' not allowed by executor_fill",
                step.id, key
            ));
        }
        merged.insert(key.clone(), value.clone());
    }
    Ok(serde_json::Value::Object(merged))
}

fn executor_dispatch_target_and_params(
    spec: &InternalActionSpec,
    arguments: &serde_json::Value,
) -> Result<(Option<String>, serde_json::Value), String> {
    let mut params = arguments
        .as_object()
        .cloned()
        .ok_or_else(|| format!("action '{}' expects object arguments", spec.action))?;
    let target = if spec.requires_target {
        params
            .remove("hive")
            .and_then(|value| value.as_str().map(str::to_string))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    Ok((target, serde_json::Value::Object(params)))
}

fn config_changed_version_channel(subsystem: &str) -> &'static str {
    match subsystem {
        // routes + vpn share version stream because SY.config.routes keeps one config version.
        "routes" | "vpn" | "vpns" => "routes-vpns",
        "storage" => "storage",
        _ => "general",
    }
}

fn config_changed_version_path(state_dir: &Path, subsystem: &str) -> PathBuf {
    state_dir
        .join("config_versions")
        .join(format!("{}.txt", config_changed_version_channel(subsystem)))
}

fn allocate_config_changed_versions(
    state_dir: &Path,
    subsystem: &str,
    count: usize,
    requested_start: Option<u64>,
) -> Result<Vec<u64>, AdminError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let path = config_changed_version_path(state_dir, subsystem);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = fs::read_to_string(&path)
        .ok()
        .and_then(|data| data.trim().parse::<u64>().ok())
        .unwrap_or(0);

    let start = match requested_start {
        Some(requested) => {
            if requested <= current {
                return Err(
                    format!("version must be greater than current (current={current})").into(),
                );
            }
            requested
        }
        None => current.saturating_add(1),
    };
    let end = start.saturating_add(count as u64 - 1);
    fs::write(&path, end.to_string())?;
    Ok((start..=end).collect())
}

fn next_config_changed_version(
    state_dir: &Path,
    subsystem: &str,
    requested: Option<u64>,
) -> Result<u64, AdminError> {
    let mut versions = allocate_config_changed_versions(state_dir, subsystem, 1, requested)?;
    Ok(versions.remove(0))
}

async fn broadcast_config_changed(
    client: &AdminRouterClient,
    subsystem: &str,
    action: Option<String>,
    auto_apply: Option<bool>,
    version: u64,
    config: serde_json::Value,
    target_hive: Option<String>,
) -> Result<(), AdminError> {
    let sender = client.sender_snapshot().await;
    let dst = match target_hive {
        Some(hive) => Destination::Unicast(format!("SY.opa.rules@{}", hive)),
        None => Destination::Broadcast,
    };
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_CONFIG_CHANGED.to_string()),
            src_ilk: None,
            scope: Some(SCOPE_GLOBAL.to_string()),
            target: None,
            action: None,
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload: serde_json::to_value(ConfigChangedPayload {
            subsystem: subsystem.to_string(),
            action: action.clone(),
            auto_apply,
            version,
            config,
        })?,
    };
    sender.send(msg).await?;
    tracing::info!(
        subsystem = subsystem,
        action = action.as_deref().unwrap_or(""),
        version = version,
        "config changed broadcast sent"
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
struct ConfigUpdate {
    #[serde(default)]
    routes: Option<Vec<RouteConfig>>,
    #[serde(default)]
    vpns: Option<Vec<VpnConfig>>,
    #[serde(default)]
    version: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StorageUpdate {
    path: String,
    #[serde(default)]
    version: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StorageSection {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug)]
struct AdminRequest {
    action: String,
    payload: serde_json::Value,
    target: String,
    unicast: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InternalAdminCommandPayload {
    action: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    params: serde_json::Value,
    #[serde(default)]
    request_id: Option<String>,
}

async fn run_internal_admin_loop(
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
    mut rx: broadcast::Receiver<Message>,
) {
    loop {
        match rx.recv().await {
            Ok(msg) => {
                if let Err(err) = handle_internal_admin_command(&ctx, &client, &msg).await {
                    tracing::warn!(error = %err, "internal admin command handling failed");
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped = skipped,
                    "internal admin loop lagged; dropping stale commands"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn run_system_command_loop(
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
    mut rx: broadcast::Receiver<Message>,
) {
    loop {
        match rx.recv().await {
            Ok(msg) => {
                if let Err(err) = handle_system_command(&ctx, &client, &msg).await {
                    tracing::warn!(error = %err, "system command handling failed");
                }
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                tracing::warn!(
                    skipped = skipped,
                    "system command loop lagged; dropping stale commands"
                );
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

struct AdminExecutorStepTool {
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
    step: AdminExecutorPlanStep,
    definition: FunctionToolDefinition,
}

#[async_trait]
impl FunctionTool for AdminExecutorStepTool {
    fn definition(&self) -> FunctionToolDefinition {
        self.definition.clone()
    }

    async fn call(
        &self,
        arguments: serde_json::Value,
    ) -> fluxbee_ai_sdk::Result<serde_json::Value> {
        let merged_args = merge_executor_step_arguments(
            &self.step,
            &arguments,
            &self.definition.parameters_json_schema,
        )
        .map_err(fluxbee_ai_sdk::AiSdkError::Protocol)?;
        validate_value_against_schema(
            &merged_args,
            &self.definition.parameters_json_schema,
            &format!("step '{}'", self.step.id),
        )
        .map_err(fluxbee_ai_sdk::AiSdkError::Protocol)?;
        let spec = resolve_internal_action_spec(&self.step.action)
            .map_err(|detail| fluxbee_ai_sdk::AiSdkError::Protocol(detail.to_string()))?;
        let (target, params) = executor_dispatch_target_and_params(spec, &merged_args)
            .map_err(fluxbee_ai_sdk::AiSdkError::Protocol)?;
        let internal = dispatch_internal_admin_command(
            &self.ctx,
            &self.client,
            &self.step.action,
            target.as_deref(),
            params,
        )
        .await
        .map_err(|err| fluxbee_ai_sdk::AiSdkError::Protocol(err.to_string()))?;
        Ok(internal.envelope)
    }
}

struct AdminExecutorHelpTool {
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
    step: AdminExecutorPlanStep,
    definition: FunctionToolDefinition,
}

#[async_trait]
impl FunctionTool for AdminExecutorHelpTool {
    fn definition(&self) -> FunctionToolDefinition {
        self.definition.clone()
    }

    async fn call(
        &self,
        arguments: serde_json::Value,
    ) -> fluxbee_ai_sdk::Result<serde_json::Value> {
        validate_value_against_schema(
            &arguments,
            &self.definition.parameters_json_schema,
            &format!("help for step '{}'", self.step.id),
        )
        .map_err(fluxbee_ai_sdk::AiSdkError::Protocol)?;
        let requested_action = arguments
            .get("action")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                fluxbee_ai_sdk::AiSdkError::Protocol(
                    "get_admin_action_help requires non-empty action".to_string(),
                )
            })?;
        if requested_action != self.step.action {
            return Err(fluxbee_ai_sdk::AiSdkError::Protocol(format!(
                "help lookup for step '{}' may only target '{}'",
                self.step.id, self.step.action
            )));
        }
        let internal = dispatch_internal_admin_command(
            &self.ctx,
            &self.client,
            "get_admin_action_help",
            None,
            serde_json::json!({
                "action": requested_action,
                "action_name": requested_action
            }),
        )
        .await
        .map_err(|err| fluxbee_ai_sdk::AiSdkError::Protocol(err.to_string()))?;
        Ok(internal.envelope)
    }
}

struct PlanStepReviewTool {
    issues: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl FunctionTool for PlanStepReviewTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "report_step_issue".to_string(),
            description: "Report issues found in the step args. Call this only if the step args are invalid or incomplete according to the action contract. Do not call it if the step is valid.".to_string(),
            parameters_json_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "issues": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "List of specific issues found. Each issue should name the field and explain the problem."
                    }
                },
                "required": ["issues"]
            }),
        }
    }

    async fn call(
        &self,
        arguments: serde_json::Value,
    ) -> fluxbee_ai_sdk::Result<serde_json::Value> {
        let issues: Vec<String> = arguments
            .get("issues")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        *self.issues.lock().await = issues;
        Ok(serde_json::json!({"acknowledged": true}))
    }
}

async fn semantic_review_executor_plan(
    runtime: &AdminExecutorAiRuntime,
    plan: &AdminExecutorPlan,
) -> Result<(), String> {
    let mut all_issues: Vec<String> = Vec::new();

    for step in &plan.execution.steps {
        let (_status, help_json) = build_admin_action_help_response(&step.action);
        let help_text = help_json;

        let step_issues = Arc::new(tokio::sync::Mutex::new(Vec::<String>::new()));
        let mut tools = FunctionToolRegistry::new();
        tools
            .register(Arc::new(PlanStepReviewTool {
                issues: step_issues.clone(),
            }))
            .map_err(|e| e.to_string())?;

        let prompt = format!(
            "You are a Fluxbee plan reviewer.\n\
Review the executor plan step below against the action help contract.\n\
If the step args are invalid or incomplete — missing required fields, invalid field values, conditional requirements not satisfied — call report_step_issue with a list of specific issues.\n\
If the step args are correct and complete, do not call report_step_issue.\n\
\n\
Important review rules:\n\
- Validate only against the executable request contract: path params, required_fields, optional_fields, documented enums, and documented conditional requirements.\n\
- Do not invent extra acknowledgement, confirmation, approval, or consent fields unless they appear explicitly in the request contract.\n\
- `confirmation_required=true` is operator/UI metadata, not a step.args field requirement.\n\
- Do not reject a step for lacking a confirm/ack field when the request contract does not define one.\n\
\n\
Action help contract:\n{}\n\
\n\
Step to review:\n{}",
            help_text,
            serde_json::to_string_pretty(step).unwrap_or_default()
        );

        let model = runtime.client.clone().function_model(
            runtime.model.clone(),
            Some(prompt),
            runtime.model_settings.clone(),
        );
        let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
        let _ = runner
            .run_with_input(
                &model,
                &tools,
                FunctionRunInput {
                    current_user_message: format!("Review step '{}' ({}).", step.id, step.action),
                    current_user_parts: None,
                    immediate_memory: None,
                },
            )
            .await;

        let issues = step_issues.lock().await.clone();
        for issue in issues {
            all_issues.push(format!("step '{}' ({}): {}", step.id, step.action, issue));
        }
    }

    if all_issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "semantic review found {} issue(s):\n{}",
            all_issues.len(),
            all_issues.join("\n")
        ))
    }
}

fn admin_executor_prompt(step: &AdminExecutorPlanStep, plan: &AdminExecutorPlan) -> String {
    format!(
        "You are the Fluxbee executor specialist running inside SY.admin.\n\
Execute exactly one declared step from the provided executor plan.\n\
\n\
Execution sequence (mandatory):\n\
1. Call get_admin_action_help for the current step.action to read the exact contract, field semantics, valid values, and defaults.\n\
2. Using the help response, verify that step.args provides all required fields with valid values. If a required field is missing or a value is invalid, stop and report failure.\n\
3. Call the exact function named by step.action with exactly the arguments in step.args, and nothing else — unless a field is explicitly listed in step.executor_fill.allowed.\n\
\n\
Rules:\n\
- Always call get_admin_action_help before executing. It is not optional.\n\
- Call the exact function named by step.action.\n\
- Do not rename actions.\n\
- Do not add steps.\n\
- Do not change declared values from step.args.\n\
- For every optional field in the function schema that is NOT present in step.args: pass null. Do not invent a value, do not pass a default, pass null.\n\
- For required fields missing from step.args: stop and report failure. Do not invent values.\n\
- Fields in step.executor_fill.allowed may be passed with a real value only if the help confirms the value is derivable from current infrastructure state.\n\
- Keep the execution deterministic.\n\
Current step JSON:\n{}\n\
Plan metadata:\n{}",
        serde_json::to_string_pretty(step).unwrap_or_else(|_| "{}".to_string()),
        serde_json::to_string_pretty(&plan.metadata).unwrap_or_else(|_| "{}".to_string())
    )
}

fn build_executor_step_events_from_result(
    execution_id: &str,
    step_index: usize,
    step: &AdminExecutorPlanStep,
    result: &fluxbee_ai_sdk::function_calling::FunctionLoopRunResult,
) -> (
    Vec<AdminExecutorStepEvent>,
    bool,
    Option<AdminExecutorFailure>,
) {
    let mut events = vec![
        AdminExecutorStepEvent {
            execution_id: execution_id.to_string(),
            step_id: step.id.clone(),
            step_index,
            step_action: step.action.clone(),
            status: "queued".to_string(),
            timestamp: now_epoch_ms(),
            summary: format!("Step {} queued", step.id),
            tool_name: None,
            tool_args_preview: None,
            result_preview: None,
            error_code: None,
            error_source: None,
            error_message: None,
        },
        AdminExecutorStepEvent {
            execution_id: execution_id.to_string(),
            step_id: step.id.clone(),
            step_index,
            step_action: step.action.clone(),
            status: "running".to_string(),
            timestamp: now_epoch_ms(),
            summary: format!("Running {}", step.action),
            tool_name: None,
            tool_args_preview: Some(step.args.clone()),
            result_preview: None,
            error_code: None,
            error_source: None,
            error_message: None,
        },
    ];
    let mut action_called = false;
    let mut failure = None::<AdminExecutorFailure>;
    for item in &result.items {
        if let FunctionLoopItem::ToolResult {
            result: tool_result,
        } = item
        {
            let tool_name = tool_result.name.clone();
            let status = if tool_name == "get_admin_action_help" {
                "help_lookup"
            } else if tool_result.is_error {
                "failed"
            } else {
                "done"
            };
            if tool_name == step.action {
                action_called = true;
            }
            let (error_code, error_source, error_message) = if tool_result.is_error {
                let detail = tool_result
                    .output
                    .get("error_detail")
                    .cloned()
                    .unwrap_or_else(|| tool_result.output.clone());
                let detail_text = detail.to_string();
                let code = tool_result
                    .output
                    .get("error_code")
                    .and_then(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| {
                        if tool_name == "get_admin_action_help" {
                            "HELP_LOOKUP_FAILED".to_string()
                        } else {
                            "ADMIN_ACTION_FAILED".to_string()
                        }
                    });
                let source = if tool_name == "get_admin_action_help" {
                    "help_ambiguity".to_string()
                } else {
                    "admin_action_failure".to_string()
                };
                (Some(code), Some(source), Some(detail_text))
            } else {
                (None, None, None)
            };
            if tool_result.is_error && failure.is_none() {
                failure = Some(AdminExecutorFailure {
                    code: error_code
                        .clone()
                        .unwrap_or_else(|| "ADMIN_ACTION_FAILED".to_string()),
                    detail: error_message
                        .clone()
                        .unwrap_or_else(|| "executor tool failed".to_string()),
                    source: error_source
                        .clone()
                        .unwrap_or_else(|| "admin_action_failure".to_string()),
                    step_id: Some(step.id.clone()),
                    step_action: Some(step.action.clone()),
                });
            }
            events.push(AdminExecutorStepEvent {
                execution_id: execution_id.to_string(),
                step_id: step.id.clone(),
                step_index,
                step_action: step.action.clone(),
                status: status.to_string(),
                timestamp: now_epoch_ms(),
                summary: format!("{} -> {}", tool_name, status),
                tool_name: Some(tool_name),
                tool_args_preview: Some(tool_result.arguments.clone()),
                result_preview: Some(tool_result.output.clone()),
                error_code,
                error_source,
                error_message,
            });
        }
    }
    (events, action_called, failure)
}

async fn execute_admin_executor_step_fallback(
    ctx: &AdminContext,
    client: &Arc<AdminRouterClient>,
    execution_id: &str,
    step_index: usize,
    step: &AdminExecutorPlanStep,
) -> (AdminExecutorStepEvent, Option<AdminExecutorFailure>) {
    let timestamp = now_epoch_ms();
    let failure_from_detail = |detail: String, source: &str| AdminExecutorFailure {
        code: "EXECUTOR_NO_ACTION_CALL".to_string(),
        detail,
        source: source.to_string(),
        step_id: Some(step.id.clone()),
        step_action: Some(step.action.clone()),
    };

    let spec = match resolve_internal_action_spec(&step.action) {
        Ok(spec) => spec,
        Err(detail) => {
            let failure = failure_from_detail(detail.to_string(), "validation");
            return (
                AdminExecutorStepEvent {
                    execution_id: execution_id.to_string(),
                    step_id: step.id.clone(),
                    step_index,
                    step_action: step.action.clone(),
                    status: "failed".to_string(),
                    timestamp,
                    summary: format!("{} -> failed", step.action),
                    tool_name: Some(step.action.clone()),
                    tool_args_preview: Some(step.args.clone()),
                    result_preview: None,
                    error_code: Some(failure.code.clone()),
                    error_source: Some(failure.source.clone()),
                    error_message: Some(failure.detail.clone()),
                },
                Some(failure),
            );
        }
    };
    let (target, params) = match executor_dispatch_target_and_params(spec, &step.args) {
        Ok(values) => values,
        Err(detail) => {
            let failure = failure_from_detail(detail, "validation");
            return (
                AdminExecutorStepEvent {
                    execution_id: execution_id.to_string(),
                    step_id: step.id.clone(),
                    step_index,
                    step_action: step.action.clone(),
                    status: "failed".to_string(),
                    timestamp,
                    summary: format!("{} -> failed", step.action),
                    tool_name: Some(step.action.clone()),
                    tool_args_preview: Some(step.args.clone()),
                    result_preview: None,
                    error_code: Some(failure.code.clone()),
                    error_source: Some(failure.source.clone()),
                    error_message: Some(failure.detail.clone()),
                },
                Some(failure),
            );
        }
    };
    let (_http_status, body) =
        match handle_admin_command(ctx, client, &step.action, params, target).await {
            Ok(values) => values,
            Err(err) => {
                let failure = failure_from_detail(err.to_string(), "admin_action_failure");
                return (
                    AdminExecutorStepEvent {
                        execution_id: execution_id.to_string(),
                        step_id: step.id.clone(),
                        step_index,
                        step_action: step.action.clone(),
                        status: "failed".to_string(),
                        timestamp,
                        summary: format!("{} -> failed", step.action),
                        tool_name: Some(step.action.clone()),
                        tool_args_preview: Some(step.args.clone()),
                        result_preview: None,
                        error_code: Some(failure.code.clone()),
                        error_source: Some(failure.source.clone()),
                        error_message: Some(failure.detail.clone()),
                    },
                    Some(failure),
                );
            }
        };
    let internal = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(envelope) => envelope,
        Err(err) => {
            let failure = failure_from_detail(
                format!("invalid fallback envelope json: {err}"),
                "validation",
            );
            return (
                AdminExecutorStepEvent {
                    execution_id: execution_id.to_string(),
                    step_id: step.id.clone(),
                    step_index,
                    step_action: step.action.clone(),
                    status: "failed".to_string(),
                    timestamp,
                    summary: format!("{} -> failed", step.action),
                    tool_name: Some(step.action.clone()),
                    tool_args_preview: Some(step.args.clone()),
                    result_preview: Some(serde_json::json!({ "raw_body": body })),
                    error_code: Some(failure.code.clone()),
                    error_source: Some(failure.source.clone()),
                    error_message: Some(failure.detail.clone()),
                },
                Some(failure),
            );
        }
    };

    let status = internal
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("error");
    if status.eq_ignore_ascii_case("error") {
        let failure = AdminExecutorFailure {
            code: internal
                .get("error_code")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("ADMIN_ACTION_FAILED")
                .to_string(),
            detail: internal
                .get("error_detail")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("executor fallback dispatch failed")
                .to_string(),
            source: "admin_action_failure".to_string(),
            step_id: Some(step.id.clone()),
            step_action: Some(step.action.clone()),
        };
        (
            AdminExecutorStepEvent {
                execution_id: execution_id.to_string(),
                step_id: step.id.clone(),
                step_index,
                step_action: step.action.clone(),
                status: "failed".to_string(),
                timestamp,
                summary: format!("{} -> failed (fallback dispatch)", step.action),
                tool_name: Some(step.action.clone()),
                tool_args_preview: Some(step.args.clone()),
                result_preview: Some(internal),
                error_code: Some(failure.code.clone()),
                error_source: Some(failure.source.clone()),
                error_message: Some(failure.detail.clone()),
            },
            Some(failure),
        )
    } else {
        (
            AdminExecutorStepEvent {
                execution_id: execution_id.to_string(),
                step_id: step.id.clone(),
                step_index,
                step_action: step.action.clone(),
                status: "done".to_string(),
                timestamp,
                summary: format!("{} -> done (fallback dispatch)", step.action),
                tool_name: Some(step.action.clone()),
                tool_args_preview: Some(step.args.clone()),
                result_preview: Some(internal),
                error_code: None,
                error_source: None,
                error_message: None,
            },
            None,
        )
    }
}

async fn execute_admin_executor_plan(
    ctx: &AdminContext,
    client: &Arc<AdminRouterClient>,
    request: &AdminExecutorRunRequest,
) -> Result<serde_json::Value, AdminError> {
    validate_executor_plan(&request.plan).map_err(|detail| -> AdminError { detail.into() })?;
    let runtime = ctx
        .executor_runtime
        .lock()
        .await
        .clone()
        .ok_or_else(|| -> AdminError {
            "SY.admin executor is not configured with a valid OpenAI key".into()
        })?;
    let mut all_events = Vec::<AdminExecutorStepEvent>::new();
    let mut completed_steps = 0usize;
    let mut failed_step_id = None::<String>;
    let mut failed_step_action = None::<String>;
    let mut last_failure = None::<AdminExecutorFailure>;

    for (index, step) in request.plan.execution.steps.iter().enumerate() {
        if request.executor_options.dry_run {
            all_events.push(AdminExecutorStepEvent {
                execution_id: request.execution_id.clone(),
                step_id: step.id.clone(),
                step_index: index,
                step_action: step.action.clone(),
                status: "done".to_string(),
                timestamp: now_epoch_ms(),
                summary: "Dry-run: validated only".to_string(),
                tool_name: Some(step.action.clone()),
                tool_args_preview: Some(step.args.clone()),
                result_preview: None,
                error_code: None,
                error_source: None,
                error_message: None,
            });
            completed_steps += 1;
            continue;
        }

        let step_spec = resolve_internal_action_spec(&step.action)
            .map_err(|detail| -> AdminError { format!("step '{}': {detail}", step.id).into() })?;
        let mut tools = FunctionToolRegistry::new();
        tools
            .register(Arc::new(AdminExecutorStepTool {
                ctx: ctx.clone(),
                client: client.clone(),
                step: step.clone(),
                definition: build_admin_executor_function_definition(step_spec),
            }))
            .map_err(|err| -> AdminError {
                format!("executor tool registration failed: {err}").into()
            })?;
        if request.plan.execution.allow_help_lookup {
            let help_spec = resolve_internal_action_spec("get_admin_action_help")
                .map_err(|detail| -> AdminError { detail.into() })?;
            tools
                .register(Arc::new(AdminExecutorHelpTool {
                    ctx: ctx.clone(),
                    client: client.clone(),
                    step: step.clone(),
                    definition: build_admin_executor_function_definition(help_spec),
                }))
                .map_err(|err| -> AdminError {
                    format!("executor help tool registration failed: {err}").into()
                })?;
        }

        let model = runtime.client.clone().function_model(
            runtime.model.clone(),
            Some(admin_executor_prompt(step, &request.plan)),
            runtime.model_settings.clone(),
        );
        let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
        let result = runner
            .run_with_input(
                &model,
                &tools,
                FunctionRunInput {
                    current_user_message: format!(
                        "Execute step '{}' ({}) using the provided plan and tool contract.",
                        step.id, step.action
                    ),
                    current_user_parts: None,
                    immediate_memory: None,
                },
            )
            .await
            .map_err(|err| -> AdminError { format!("executor model run failed: {err}").into() })?;

        let (mut step_events, action_called, mut failure) =
            build_executor_step_events_from_result(&request.execution_id, index, step, &result);
        if !action_called && failure.is_none() {
            let (fallback_event, fallback_failure) = execute_admin_executor_step_fallback(
                ctx,
                client,
                &request.execution_id,
                index,
                step,
            )
            .await;
            step_events.push(fallback_event);
            failure = fallback_failure;
        }
        all_events.append(&mut step_events);
        if failure.is_none() {
            completed_steps += 1;
            continue;
        }
        last_failure = failure.clone();
        failed_step_id = Some(step.id.clone());
        failed_step_action = Some(step.action.clone());
        if request.plan.execution.stop_on_error {
            all_events.push(AdminExecutorStepEvent {
                execution_id: request.execution_id.clone(),
                step_id: step.id.clone(),
                step_index: index,
                step_action: step.action.clone(),
                status: "stopped".to_string(),
                timestamp: now_epoch_ms(),
                summary: "Execution stopped after failure".to_string(),
                tool_name: Some(step.action.clone()),
                tool_args_preview: Some(step.args.clone()),
                result_preview: None,
                error_code: failure.as_ref().map(|value| value.code.clone()),
                error_source: failure.as_ref().map(|value| value.source.clone()),
                error_message: failure.as_ref().map(|value| value.detail.clone()),
            });
            break;
        }
    }

    let status = if failed_step_id.is_some() {
        "failed"
    } else {
        "done"
    };
    let mut summary = AdminExecutorRunSummary {
        execution_id: request.execution_id.clone(),
        status: status.to_string(),
        total_steps: request.plan.execution.steps.len(),
        completed_steps,
        failed_step_id: failed_step_id.clone(),
        failed_step_action: failed_step_action.clone(),
        failure: last_failure.clone(),
        log_path: None,
        final_message: if let Some(step_id) = failed_step_id.as_deref() {
            format!("Execution stopped at step '{step_id}'")
        } else if request.executor_options.dry_run {
            "Dry-run completed successfully".to_string()
        } else {
            "Execution completed successfully".to_string()
        },
    };
    if status == "failed" && summary.failure.is_none() {
        summary.failure = Some(AdminExecutorFailure {
            code: "EXECUTION_FAILED".to_string(),
            detail: summary.final_message.clone(),
            source: "admin_action_failure".to_string(),
            step_id: failed_step_id.clone(),
            step_action: failed_step_action.clone(),
        });
    }
    summary.log_path =
        persist_admin_executor_run_log(ctx, &request.execution_id, request, &all_events, &summary)
            .ok();

    Ok(serde_json::json!({
        "status": summary.status,
        "execution_id": request.execution_id,
        "events": all_events,
        "summary": summary,
    }))
}

async fn handle_system_command(
    ctx: &AdminContext,
    client: &Arc<AdminRouterClient>,
    msg: &Message,
) -> Result<(), AdminError> {
    if msg.meta.msg_type != SYSTEM_KIND {
        return Ok(());
    }
    let Some(command) = msg.meta.msg.as_deref() else {
        return Ok(());
    };
    let sender = client.sender_snapshot().await;
    match command {
        "CONFIG_GET" => {
            let payload = build_admin_executor_config_get_payload(ctx)?;
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
        }
        "CONFIG_SET" => {
            let payload = apply_admin_executor_config_set(ctx, msg).await?;
            let response = build_node_config_response_message(msg, sender.uuid(), payload);
            sender.send(response).await?;
        }
        _ => {}
    }
    Ok(())
}

async fn handle_internal_admin_command(
    ctx: &AdminContext,
    client: &Arc<AdminRouterClient>,
    msg: &Message,
) -> Result<(), AdminError> {
    if msg.meta.msg_type != "admin" || msg.meta.msg.as_deref() != Some(MSG_ADMIN_COMMAND) {
        return Ok(());
    }

    let payload = serde_json::from_value::<InternalAdminCommandPayload>(msg.payload.clone());
    let parsed = match payload {
        Ok(parsed) => parsed,
        Err(err) => {
            let detail = serde_json::json!({ "message": err.to_string() });
            return send_admin_command_response(
                client,
                msg,
                "",
                "error",
                serde_json::Value::Null,
                Some("INVALID_REQUEST".to_string()),
                Some(detail),
                None,
            )
            .await;
        }
    };

    let action = match parsed
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(action) => action.to_string(),
        None => {
            let detail = serde_json::json!({ "message": "missing payload.action" });
            return send_admin_command_response(
                client,
                msg,
                "",
                "error",
                serde_json::Value::Null,
                Some("INVALID_REQUEST".to_string()),
                Some(detail),
                parsed.request_id,
            )
            .await;
        }
    };

    let internal = {
        let _command_guard = ctx.command_lock.lock().await;
        dispatch_internal_admin_command(
            ctx,
            client,
            &action,
            parsed.target.as_deref(),
            parsed.params,
        )
        .await?
    };
    let status = internal
        .envelope
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or(if internal.http_status < 400 {
            "ok"
        } else {
            "error"
        });
    let action_out = internal
        .envelope
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or(&action)
        .to_string();
    let payload = internal_response_payload_value(&internal.envelope);
    let error_code = internal
        .envelope
        .get("error_code")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let error_detail = internal
        .envelope
        .get("error_detail")
        .cloned()
        .and_then(|v| if v.is_null() { None } else { Some(v) });

    send_admin_command_response(
        client,
        msg,
        &action_out,
        status,
        payload,
        error_code,
        error_detail,
        parsed.request_id,
    )
    .await
}

fn internal_response_payload_value(envelope: &serde_json::Value) -> serde_json::Value {
    if let Some(value) = envelope.get("payload") {
        return value.clone();
    }
    let Some(mut object) = envelope.as_object().cloned() else {
        return serde_json::Value::Null;
    };
    for key in [
        "status",
        "action",
        "error_code",
        "error_detail",
        "request_id",
    ] {
        object.remove(key);
    }
    if object.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::Object(object)
    }
}

async fn send_admin_command_response(
    client: &AdminRouterClient,
    request_msg: &Message,
    action: &str,
    status: &str,
    payload: serde_json::Value,
    error_code: Option<String>,
    error_detail: Option<serde_json::Value>,
    request_id: Option<String>,
) -> Result<(), AdminError> {
    let sender = client.sender_snapshot().await;
    let action_class = classify_admin_action(action);
    let (action_result, result_origin) =
        derive_action_outcome(action_class, Some(status), error_code.as_deref());
    let mut body = serde_json::json!({
        "status": status,
        "action": action,
        "payload": payload,
        "error_code": error_code,
        "error_detail": error_detail,
    });
    if let Some(req_id) = request_id {
        body["request_id"] = serde_json::Value::String(req_id);
    }

    let response = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(request_msg.routing.src.clone()),
            ttl: 16,
            trace_id: request_msg.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: Some(MSG_ADMIN_COMMAND_RESPONSE.to_string()),
            src_ilk: None,
            scope: None,
            target: None,
            action: Some(action.to_string()),
            action_class,
            action_result,
            result_origin: result_origin.map(str::to_string),
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload: body,
    };
    sender.send(response).await?;
    Ok(())
}

#[derive(Debug)]
struct InternalAdminDispatchResult {
    http_status: u16,
    envelope: serde_json::Value,
}

#[derive(Clone, Copy)]
enum InternalActionRoute {
    Query(&'static str),
    Command(&'static str),
    Update,
    SyncHint,
    Inventory,
    OpaHttp(OpaAction),
    OpaQuery(&'static str),
    WfRulesHttp(WfRulesAction),
    WfRulesQuery(&'static str),
    TimerRpc(&'static str),
}

#[derive(Clone, Copy)]
struct InternalActionSpec {
    action: &'static str,
    route: InternalActionRoute,
    requires_target: bool,
    allow_legacy_hive_id: bool,
}

const INTERNAL_ACTION_REGISTRY_VERSION: &str = "6";

const INTERNAL_ACTION_REGISTRY: &[InternalActionSpec] = &[
    InternalActionSpec {
        action: "hive_status",
        route: InternalActionRoute::Query("hive_status"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_hives",
        route: InternalActionRoute::Query("list_hives"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_admin_actions",
        route: InternalActionRoute::Query("list_admin_actions"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_admin_action_help",
        route: InternalActionRoute::Command("get_admin_action_help"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "publish_runtime_package",
        route: InternalActionRoute::Command("publish_runtime_package"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_versions",
        route: InternalActionRoute::Query("list_versions"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_versions",
        route: InternalActionRoute::Query("get_versions"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_runtimes",
        route: InternalActionRoute::Query("list_runtimes"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_runtime",
        route: InternalActionRoute::Query("get_runtime"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "remove_runtime_version",
        route: InternalActionRoute::Command("remove_runtime_version"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_routes",
        route: InternalActionRoute::Query("list_routes"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_vpns",
        route: InternalActionRoute::Query("list_vpns"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_nodes",
        route: InternalActionRoute::Query("list_nodes"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_storage",
        route: InternalActionRoute::Query("get_storage"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "add_hive",
        route: InternalActionRoute::Command("add_hive"),
        requires_target: false,
        allow_legacy_hive_id: true,
    },
    InternalActionSpec {
        action: "get_hive",
        route: InternalActionRoute::Command("get_hive"),
        requires_target: false,
        allow_legacy_hive_id: true,
    },
    InternalActionSpec {
        action: "remove_hive",
        route: InternalActionRoute::Command("remove_hive"),
        requires_target: false,
        allow_legacy_hive_id: true,
    },
    InternalActionSpec {
        action: "add_route",
        route: InternalActionRoute::Command("add_route"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "delete_route",
        route: InternalActionRoute::Command("delete_route"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "add_vpn",
        route: InternalActionRoute::Command("add_vpn"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "delete_vpn",
        route: InternalActionRoute::Command("delete_vpn"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "run_node",
        route: InternalActionRoute::Command("run_node"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "start_node",
        route: InternalActionRoute::Command("start_node"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "restart_node",
        route: InternalActionRoute::Command("restart_node"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "kill_node",
        route: InternalActionRoute::Command("kill_node"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "remove_node_instance",
        route: InternalActionRoute::Command("remove_node_instance"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_node_config",
        route: InternalActionRoute::Command("get_node_config"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "set_node_config",
        route: InternalActionRoute::Command("set_node_config"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_node_state",
        route: InternalActionRoute::Command("get_node_state"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_node_status",
        route: InternalActionRoute::Command("get_node_status"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "send_node_message",
        route: InternalActionRoute::Command("send_node_message"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "node_control_config_get",
        route: InternalActionRoute::Command("node_control_config_get"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "node_control_config_set",
        route: InternalActionRoute::Command("node_control_config_set"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_ilks",
        route: InternalActionRoute::Query("list_ilks"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_ilk",
        route: InternalActionRoute::Command("get_ilk"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "update_tenant",
        route: InternalActionRoute::Command("update_tenant"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "set_tenant_sponsor",
        route: InternalActionRoute::Command("set_tenant_sponsor"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_deployments",
        route: InternalActionRoute::Command("list_deployments"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_deployments",
        route: InternalActionRoute::Command("get_deployments"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "list_drift_alerts",
        route: InternalActionRoute::Command("list_drift_alerts"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "get_drift_alerts",
        route: InternalActionRoute::Command("get_drift_alerts"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "set_storage",
        route: InternalActionRoute::Command("set_storage"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "update",
        route: InternalActionRoute::Update,
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "sync_hint",
        route: InternalActionRoute::SyncHint,
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "inventory",
        route: InternalActionRoute::Inventory,
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_compile_apply",
        route: InternalActionRoute::OpaHttp(OpaAction::CompileApply),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_compile",
        route: InternalActionRoute::OpaHttp(OpaAction::Compile),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_apply",
        route: InternalActionRoute::OpaHttp(OpaAction::Apply),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_rollback",
        route: InternalActionRoute::OpaHttp(OpaAction::Rollback),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_check",
        route: InternalActionRoute::OpaHttp(OpaAction::Check),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_get_policy",
        route: InternalActionRoute::OpaQuery("get_policy"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "opa_get_status",
        route: InternalActionRoute::OpaQuery("get_status"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_compile_apply",
        route: InternalActionRoute::WfRulesHttp(WfRulesAction::CompileApply),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_compile",
        route: InternalActionRoute::WfRulesHttp(WfRulesAction::Compile),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_apply",
        route: InternalActionRoute::WfRulesHttp(WfRulesAction::Apply),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_rollback",
        route: InternalActionRoute::WfRulesHttp(WfRulesAction::Rollback),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_delete",
        route: InternalActionRoute::WfRulesHttp(WfRulesAction::Delete),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_get_workflow",
        route: InternalActionRoute::WfRulesQuery("get_workflow"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_get_status",
        route: InternalActionRoute::WfRulesQuery("get_status"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "wf_rules_list_workflows",
        route: InternalActionRoute::WfRulesQuery("list_workflows"),
        requires_target: false,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_help",
        route: InternalActionRoute::TimerRpc("TIMER_HELP"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_get",
        route: InternalActionRoute::TimerRpc("TIMER_GET"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_list",
        route: InternalActionRoute::TimerRpc("TIMER_LIST"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_now",
        route: InternalActionRoute::TimerRpc("TIMER_NOW"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_now_in",
        route: InternalActionRoute::TimerRpc("TIMER_NOW_IN"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_convert",
        route: InternalActionRoute::TimerRpc("TIMER_CONVERT"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_parse",
        route: InternalActionRoute::TimerRpc("TIMER_PARSE"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
    InternalActionSpec {
        action: "timer_format",
        route: InternalActionRoute::TimerRpc("TIMER_FORMAT"),
        requires_target: true,
        allow_legacy_hive_id: false,
    },
];

async fn dispatch_internal_admin_command(
    ctx: &AdminContext,
    client: &Arc<AdminRouterClient>,
    action: &str,
    target: Option<&str>,
    params: serde_json::Value,
) -> Result<InternalAdminDispatchResult, AdminError> {
    match action {
        "executor_validate_plan" => {
            let plan = match parse_executor_plan(params) {
                Ok(plan) => plan,
                Err(detail) => return Ok(internal_invalid_request(action, &detail)),
            };
            let runtime = ctx.executor_runtime.lock().await.clone();
            if let Some(runtime) = runtime {
                if let Err(detail) = semantic_review_executor_plan(&runtime, &plan).await {
                    return Ok(InternalAdminDispatchResult {
                        http_status: 422,
                        envelope: serde_json::json!({
                            "status": "error",
                            "action": action,
                            "error_code": "PLAN_REVIEW_FAILED",
                            "error_detail": detail,
                        }),
                    });
                }
            }
            return Ok(InternalAdminDispatchResult {
                http_status: 200,
                envelope: serde_json::json!({
                    "status": "ok",
                    "action": action,
                    "payload": {
                        "ok": true,
                        "kind": plan.kind,
                        "plan_version": plan.plan_version,
                        "step_count": plan.execution.steps.len(),
                    }
                }),
            });
        }
        "executor_execute_plan" => {
            if target.is_some() {
                return Ok(internal_invalid_request(
                    action,
                    "payload.target is not used for executor_execute_plan",
                ));
            }
            let request = match serde_json::from_value::<AdminExecutorRunRequest>(params) {
                Ok(request) => request,
                Err(err) => {
                    return Ok(internal_invalid_request(
                        action,
                        &format!("invalid executor request: {err}"),
                    ))
                }
            };
            let payload = match execute_admin_executor_plan(ctx, client, &request).await {
                Ok(payload) => payload,
                Err(err) => {
                    return Ok(InternalAdminDispatchResult {
                        http_status: 409,
                        envelope: serde_json::json!({
                            "status": "error",
                            "action": action,
                            "payload": serde_json::Value::Null,
                            "error_code": "EXECUTOR_UNAVAILABLE",
                            "error_detail": err.to_string(),
                        }),
                    })
                }
            };
            return Ok(InternalAdminDispatchResult {
                http_status: if payload.get("status").and_then(serde_json::Value::as_str)
                    == Some("done")
                {
                    200
                } else {
                    409
                },
                envelope: serde_json::json!({
                    "status": if payload.get("status").and_then(serde_json::Value::as_str) == Some("done") { "ok" } else { "error" },
                    "action": action,
                    "payload": payload,
                    "error_code": if payload.get("status").and_then(serde_json::Value::as_str) == Some("done") { serde_json::Value::Null } else { serde_json::json!("EXECUTION_FAILED") },
                    "error_detail": if payload.get("status").and_then(serde_json::Value::as_str) == Some("done") {
                        serde_json::Value::Null
                    } else {
                        payload
                            .get("summary")
                            .and_then(|summary| summary.get("final_message"))
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!("execution failed"))
                    }
                }),
            });
        }
        _ => {}
    }
    let spec = match resolve_internal_action_spec(action) {
        Ok(route) => route,
        Err(detail) => return Ok(internal_invalid_request(action, detail)),
    };

    if let Some(detail) = internal_admin_payload_contract_error(spec, &params) {
        return Ok(internal_invalid_request(action, &detail));
    }

    let (status, body) = match spec.route {
        InternalActionRoute::Query(canonical) => {
            let hive = resolve_internal_action_hive(canonical, target, &params);
            if spec.requires_target && hive.is_none() {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            }
            let has_payload = params
                .as_object()
                .map(|value| !value.is_empty())
                .unwrap_or(false);
            if has_payload {
                handle_admin_query_with_payload(ctx, client, canonical, params, hive).await?
            } else {
                handle_admin_query(ctx, client, canonical, hive).await?
            }
        }
        InternalActionRoute::Command(canonical) => {
            let hive = resolve_internal_action_hive(canonical, target, &params);
            if spec.requires_target && hive.is_none() {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            }
            handle_admin_command(ctx, client, canonical, params, hive).await?
        }
        InternalActionRoute::Update => {
            let hive = resolve_internal_action_hive("update", target, &params);
            let Some(hive_id) = hive else {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            };
            handle_hive_update_command(ctx, client, hive_id, params).await?
        }
        InternalActionRoute::SyncHint => {
            let hive = resolve_internal_action_hive("sync_hint", target, &params);
            let Some(hive_id) = hive else {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            };
            handle_hive_sync_hint_command(ctx, client, hive_id, params).await?
        }
        InternalActionRoute::Inventory => handle_inventory_http(ctx, client, params).await?,
        InternalActionRoute::OpaHttp(opa_action) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            let req = parse_internal_opa_request(params, hive)
                .map_err(|detail| -> AdminError { detail.into() })?;
            handle_opa_http(ctx, client, req, opa_action).await?
        }
        InternalActionRoute::OpaQuery(query_action) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            handle_opa_query(ctx, client, query_action, hive.clone()).await?
        }
        InternalActionRoute::WfRulesHttp(wf_action) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            let req = parse_internal_wf_rules_request(params, hive)
                .map_err(|detail| -> AdminError { detail.into() })?;
            handle_wf_rules_http(ctx, client, req, wf_action).await?
        }
        InternalActionRoute::WfRulesQuery(query_action) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            let req = parse_internal_wf_rules_request(params, hive)
                .map_err(|detail| -> AdminError { detail.into() })?;
            handle_wf_rules_query(ctx, client, query_action, req).await?
        }
        InternalActionRoute::TimerRpc(timer_msg) => {
            let hive = resolve_internal_action_hive(action, target, &params);
            let Some(hive_id) = hive else {
                return Ok(internal_invalid_request(
                    action,
                    "missing target (payload.target required for this action)",
                ));
            };
            handle_timer_rpc(ctx, client, action, timer_msg, params, hive_id).await?
        }
    };

    Ok(InternalAdminDispatchResult {
        http_status: status,
        envelope: parse_internal_response_envelope(action, status, &body),
    })
}

fn parse_internal_response_envelope(
    action: &str,
    http_status: u16,
    body: &str,
) -> serde_json::Value {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(value) if value.is_object() => value,
        Ok(_) | Err(_) => serde_json::json!({
            "status": "error",
            "action": action,
            "payload": serde_json::Value::Null,
            "error_code": "TRANSPORT_ERROR",
            "error_detail": format!("invalid response envelope (http_status={http_status})"),
        }),
    }
}

fn normalize_internal_target_hive(target: Option<&str>) -> Option<String> {
    let raw = target?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some((_, hive)) = raw.rsplit_once('@') {
        let hive = hive.trim();
        if !hive.is_empty() {
            return Some(hive.to_string());
        }
    }
    Some(raw.to_string())
}

fn internal_admin_payload_contract_error(
    action: &InternalActionSpec,
    params: &serde_json::Value,
) -> Option<String> {
    if !params.is_object() {
        return None;
    }
    if params.get("hive_id").is_some() && !action.allow_legacy_hive_id {
        return Some(
            "legacy field 'hive_id' is not supported for this action; use payload.target"
                .to_string(),
        );
    }
    None
}

fn resolve_internal_action_hive(
    action: &str,
    target: Option<&str>,
    params: &serde_json::Value,
) -> Option<String> {
    if let Some(node_hive) = node_name_hive_from_payload(action, params).map(str::to_string) {
        return Some(node_hive);
    }
    normalize_internal_target_hive(target)
}

fn resolve_internal_action_spec(action: &str) -> Result<&'static InternalActionSpec, &'static str> {
    debug_assert!(!INTERNAL_ACTION_REGISTRY_VERSION.is_empty());
    if matches!(
        action,
        "list_routers" | "get_routers" | "add_router" | "delete_router"
    ) {
        return Err(
            "legacy router action is not supported; use inventory/list_nodes/list_routes/list_vpns",
        );
    }
    INTERNAL_ACTION_REGISTRY
        .iter()
        .find(|spec| spec.action == action)
        .ok_or("unknown action")
}

fn parse_internal_opa_request(
    params: serde_json::Value,
    default_hive: Option<String>,
) -> Result<OpaRequest, String> {
    if !params.is_null() && !params.is_object() {
        return Err("invalid params: expected JSON object for OPA action".to_string());
    }
    let mut req = if params.is_null() {
        OpaRequest {
            rego: None,
            entrypoint: None,
            version: None,
            action: None,
            hive: None,
        }
    } else {
        serde_json::from_value::<OpaRequest>(params)
            .map_err(|err| format!("invalid OPA params: {err}"))?
    };
    if req.hive.is_none() {
        req.hive = default_hive;
    }
    Ok(req)
}

fn parse_internal_wf_rules_request(
    params: serde_json::Value,
    default_hive: Option<String>,
) -> Result<WfRulesRequest, String> {
    if !params.is_null() && !params.is_object() {
        return Err("invalid params: expected JSON object for wf-rules action".to_string());
    }
    let mut req = if params.is_null() {
        WfRulesRequest {
            payload: serde_json::Map::new(),
            hive: None,
        }
    } else {
        serde_json::from_value::<WfRulesRequest>(params)
            .map_err(|err| format!("invalid wf-rules params: {err}"))?
    };
    if req.hive.is_none() {
        req.hive = default_hive;
    }
    Ok(req)
}

fn internal_invalid_request(action: &str, detail: &str) -> InternalAdminDispatchResult {
    InternalAdminDispatchResult {
        http_status: 400,
        envelope: serde_json::json!({
            "status": "error",
            "action": action,
            "payload": serde_json::Value::Null,
            "error_code": "INVALID_REQUEST",
            "error_detail": detail,
        }),
    }
}

#[derive(Clone, Copy)]
enum OpaAction {
    Compile,
    CompileApply,
    Apply,
    Rollback,
    Check,
}

#[derive(Clone, Copy)]
enum WfRulesAction {
    Compile,
    CompileApply,
    Apply,
    Rollback,
    Delete,
}

impl WfRulesAction {
    fn operation(&self) -> Option<&'static str> {
        match self {
            WfRulesAction::Compile => Some("compile"),
            WfRulesAction::CompileApply => Some("compile_apply"),
            WfRulesAction::Apply => Some("apply"),
            WfRulesAction::Rollback => Some("rollback"),
            WfRulesAction::Delete => None,
        }
    }

    fn command(&self) -> Option<&'static str> {
        match self {
            WfRulesAction::Delete => Some("delete_workflow"),
            _ => None,
        }
    }

    fn action_name(&self) -> &'static str {
        match self {
            WfRulesAction::Compile => "compile",
            WfRulesAction::CompileApply => "compile_apply",
            WfRulesAction::Apply => "apply",
            WfRulesAction::Rollback => "rollback",
            WfRulesAction::Delete => "delete_workflow",
        }
    }
}

impl OpaAction {
    fn as_str(&self) -> &'static str {
        match self {
            OpaAction::Compile => "compile",
            OpaAction::CompileApply => "compile",
            OpaAction::Apply => "apply",
            OpaAction::Rollback => "rollback",
            OpaAction::Check => "compile",
        }
    }

    fn needs_rego(&self) -> bool {
        matches!(
            self,
            OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check
        )
    }

    fn apply_after(&self) -> bool {
        matches!(self, OpaAction::CompileApply)
    }

    fn check_only(&self) -> bool {
        matches!(self, OpaAction::Check)
    }

    fn auto_apply_flag(&self) -> Option<bool> {
        match self {
            OpaAction::Compile => Some(false),
            OpaAction::CompileApply => Some(false),
            OpaAction::Check => Some(false),
            _ => None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigResponsePayload {
    subsystem: String,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    version: Option<u64>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    hive: Option<String>,
    #[serde(default)]
    compile_time_ms: Option<u64>,
    #[serde(default)]
    wasm_size_bytes: Option<u64>,
    #[serde(default)]
    hash: Option<String>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    error_detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpaResponseEntry {
    hive: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compile_time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wasm_size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpaQueryEntry {
    hive: String,
    status: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize, Serialize)]
struct StorageInboxMetrics {
    pending: i64,
    pending_with_error: i64,
    oldest_pending_age_s: i64,
    processed_total: i64,
    max_attempts: i32,
}

const SUBJECT_STORAGE_METRICS_GET: &str = "storage.metrics.get";
const STORAGE_METRICS_NATS_TIMEOUT_SECS: u64 = 8;

async fn run_http_server(
    listen: &str,
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: AdminContext,
    client: Arc<AdminRouterClient>,
) -> Result<(), AdminError> {
    let listener = TcpListener::bind(listen).await?;
    tracing::info!(addr = %listen, "sy.admin http listening");
    loop {
        let (mut stream, _) = listener.accept().await?;
        let tx = tx.clone();
        let ctx = ctx.clone();
        let client = client.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_http(&mut stream, &tx, &ctx, &client).await {
                tracing::warn!("http handler error: {err}");
            }
        });
    }
}

async fn handle_http(
    stream: &mut tokio::net::TcpStream,
    tx: &mpsc::UnboundedSender<BroadcastRequest>,
    ctx: &AdminContext,
    client: &AdminRouterClient,
) -> Result<(), AdminError> {
    let (method, path, headers, body) = read_http_request(stream).await?;
    let (path, query) = split_path_query(&path);
    if method == "GET" && path == "/health" {
        respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        return Ok(());
    }

    // FR9-T5: single global command lock shared by HTTP and internal socket commands.
    // Contention is blocking (wait), never immediate reject.
    let _command_guard = ctx.command_lock.lock().await;

    if method == "GET" {
        if path == "/inventory" {
            let mut payload = serde_json::json!({ "scope": "global" });
            if let Some(kind) = query
                .get("type")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["filter_type"] = serde_json::Value::String(kind.to_ascii_uppercase());
            }
            if let Some(hive_id) = query
                .get("hive")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["scope"] = serde_json::Value::String("hive".to_string());
                payload["filter_hive"] = serde_json::Value::String(hive_id.to_string());
            }
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
        if path == "/inventory/summary" {
            let payload = serde_json::json!({ "scope": "summary" });
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
        if path == "/inventory/nodes" {
            let mut payload = serde_json::json!({ "scope": "global" });
            if let Some(kind) = query
                .get("type")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["filter_type"] = serde_json::Value::String(kind.to_ascii_uppercase());
            }
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
        if let Some(hive_id) = path
            .strip_prefix("/inventory/")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .filter(|value| *value != "summary" && *value != "nodes")
        {
            let mut payload = serde_json::json!({
                "scope": "hive",
                "filter_hive": decode_percent(hive_id),
            });
            if let Some(kind) = query
                .get("type")
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
            {
                payload["filter_type"] = serde_json::Value::String(kind.to_ascii_uppercase());
            }
            let (status, resp) = handle_inventory_http(ctx, client, payload).await?;
            respond_json(stream, status, &resp).await?;
            return Ok(());
        }
    }
    if let Some((status, resp)) =
        handle_hive_paths(method.as_str(), path, &query, &body, ctx, client).await?
    {
        respond_json(stream, status, &resp).await?;
        return Ok(());
    }
    if let Some((status, resp, content_type)) =
        handle_modules_paths(method.as_str(), path, &body).await?
    {
        match content_type {
            Some(ct) => respond_bytes(stream, status, &ct, &resp).await?,
            None => {
                let body = String::from_utf8_lossy(&resp);
                respond_json(stream, status, &body).await?
            }
        }
        return Ok(());
    }
    match (method.as_str(), path) {
        ("GET", "/admin/actions") => {
            let (status, resp) =
                handle_admin_query(ctx, client, "list_admin_actions", None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/admin/executor/functions") => {
            let (status, resp) = build_admin_executor_function_catalog_response(ctx)?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/admin/runtime-packages/publish") => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(&body)?
            };
            let (status, resp) =
                handle_admin_command(ctx, client, "publish_runtime_package", payload, None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", path) if path.starts_with("/admin/actions/") => {
            let Some(action_name) = path.strip_prefix("/admin/actions/") else {
                respond_json(stream, 404, r#"{"error":"not_found"}"#).await?;
                return Ok(());
            };
            let payload = serde_json::json!({ "action_name": decode_percent(action_name) });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_admin_action_help", payload, None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/hive/status") => {
            let (status, resp) = handle_admin_query(ctx, client, "hive_status", None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/versions") => {
            let hive = query.get("hive").cloned();
            if hive.is_some() {
                let (status, resp) = handle_admin_query(ctx, client, "get_versions", hive).await?;
                respond_json(stream, status, &resp).await?;
            } else {
                let (status, resp) = handle_admin_query(ctx, client, "list_versions", None).await?;
                respond_json(stream, status, &resp).await?;
            }
        }
        ("GET", "/deployments") => {
            let hive = query.get("hive").cloned();
            let payload = deployments_payload_from_query(&query);
            if hive.is_some() {
                let (status, resp) =
                    handle_admin_command(ctx, client, "get_deployments", payload, hive).await?;
                respond_json(stream, status, &resp).await?;
            } else {
                let (status, resp) =
                    handle_admin_command(ctx, client, "list_deployments", payload, None).await?;
                respond_json(stream, status, &resp).await?;
            }
        }
        ("GET", "/drift-alerts") => {
            let hive = query.get("hive").cloned();
            let payload = drift_alerts_payload_from_query(&query);
            if hive.is_some() {
                let (status, resp) =
                    handle_admin_command(ctx, client, "get_drift_alerts", payload, hive).await?;
                respond_json(stream, status, &resp).await?;
            } else {
                let (status, resp) =
                    handle_admin_command(ctx, client, "list_drift_alerts", payload, None).await?;
                respond_json(stream, status, &resp).await?;
            }
        }
        ("GET", "/routes") => {
            let hive = query.get("hive").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_routes", hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/routes") => {
            let route: RouteConfig = serde_json::from_slice(&body)?;
            let hive = query.get("hive").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "add_route", serde_json::to_value(route)?, hive)
                    .await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/routes") => {
            let prefix = query.get("prefix").cloned().unwrap_or_default();
            let hive = query.get("hive").cloned();
            let payload = serde_json::json!({ "prefix": prefix });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_route", payload, hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/vpns") => {
            let hive = query.get("hive").cloned();
            let (status, resp) = handle_admin_query(ctx, client, "list_vpns", hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/vpns") => {
            let vpn: VpnConfig = serde_json::from_slice(&body)?;
            let hive = query.get("hive").cloned();
            let (status, resp) =
                handle_admin_command(ctx, client, "add_vpn", serde_json::to_value(vpn)?, hive)
                    .await?;
            respond_json(stream, status, &resp).await?;
        }
        ("DELETE", "/vpns") => {
            let pattern = query.get("pattern").cloned().unwrap_or_default();
            let hive = query.get("hive").cloned();
            let payload = serde_json::json!({ "pattern": pattern });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_vpn", payload, hive).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("PUT", "/config/routes") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            let mut broadcasts = usize::from(update.routes.is_some());
            broadcasts += usize::from(update.vpns.is_some());
            let mut versions = match allocate_config_changed_versions(
                &ctx.state_dir,
                "routes",
                broadcasts,
                update.version,
            ) {
                Ok(versions) => versions,
                Err(err) => {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "VERSION_MISMATCH",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 409, &body).await?;
                    return Ok(());
                }
            };
            if let Some(routes) = update.routes {
                let version = versions.remove(0);
                if let Err(err) = send_broadcast_request(
                    tx,
                    |ack| BroadcastRequest::Routes {
                        routes,
                        version,
                        ack,
                    },
                    "routes",
                )
                .await
                {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "CONFIG_BROADCAST_FAILED",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 502, &body).await?;
                    return Ok(());
                }
            }
            if let Some(vpns) = update.vpns {
                let version = versions.remove(0);
                if let Err(err) = send_broadcast_request(
                    tx,
                    |ack| BroadcastRequest::Vpns { vpns, version, ack },
                    "vpns",
                )
                .await
                {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "CONFIG_BROADCAST_FAILED",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 502, &body).await?;
                    return Ok(());
                }
            }
            tracing::info!("config routes update received");
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("PUT", "/config/vpns") => {
            let update: ConfigUpdate = serde_json::from_slice(&body)?;
            if let Some(vpns) = update.vpns {
                let version =
                    match next_config_changed_version(&ctx.state_dir, "vpn", update.version) {
                        Ok(version) => version,
                        Err(err) => {
                            let body = serde_json::json!({
                                "status": "error",
                                "error_code": "VERSION_MISMATCH",
                                "error_detail": err.to_string(),
                            })
                            .to_string();
                            respond_json(stream, 409, &body).await?;
                            return Ok(());
                        }
                    };
                if let Err(err) = send_broadcast_request(
                    tx,
                    |ack| BroadcastRequest::Vpns { vpns, version, ack },
                    "vpns",
                )
                .await
                {
                    let body = serde_json::json!({
                        "status": "error",
                        "error_code": "CONFIG_BROADCAST_FAILED",
                        "error_detail": err.to_string(),
                    })
                    .to_string();
                    respond_json(stream, 502, &body).await?;
                    return Ok(());
                }
            }
            tracing::info!("config vpns update received");
            respond_json(stream, 200, r#"{"status":"ok"}"#).await?;
        }
        ("GET", "/config/storage") => {
            let (status, resp) = handle_admin_query(ctx, client, "get_storage", None).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/config/storage/metrics") => {
            let (status, resp) = handle_storage_metrics_http(ctx).await;
            respond_json(stream, status, &resp).await?;
        }
        ("PUT", "/config/storage") => {
            let update: StorageUpdate = serde_json::from_slice(&body)?;
            let version =
                match next_config_changed_version(&ctx.state_dir, "storage", update.version) {
                    Ok(version) => version,
                    Err(err) => {
                        let body = serde_json::json!({
                            "status": "error",
                            "error_code": "VERSION_MISMATCH",
                            "error_detail": err.to_string(),
                        })
                        .to_string();
                        respond_json(stream, 409, &body).await?;
                        return Ok(());
                    }
                };
            let storage_payload = serde_json::json!({ "path": update.path });
            let (status, resp) =
                handle_admin_command(ctx, client, "set_storage", storage_payload.clone(), None)
                    .await?;
            if status != 200 {
                respond_json(stream, status, &resp).await?;
                return Ok(());
            }

            if let Err(err) = broadcast_config_changed(
                client,
                "storage",
                None,
                None,
                version,
                storage_payload,
                None,
            )
            .await
            {
                tracing::warn!("config storage broadcast failed after set_storage: {err}");
            }
            tracing::info!("config storage update received");
            respond_json(stream, 200, &resp).await?;
        }
        ("POST", "/opa/policy") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::CompileApply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/compile") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Compile).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/apply") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Apply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/rollback") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Rollback).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/opa/policy/check") => {
            let req: OpaRequest = serde_json::from_slice(&body)?;
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Check).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/opa/policy") => {
            let target = normalize_opa_target(
                query
                    .get("hive")
                    .cloned()
                    .or_else(|| query.get("target").cloned()),
            );
            let (status, resp) = handle_opa_query(ctx, client, "get_policy", target).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/opa/status") => {
            let target = normalize_opa_target(
                query
                    .get("hive")
                    .cloned()
                    .or_else(|| query.get("target").cloned()),
            );
            let (status, resp) = handle_opa_query(ctx, client, "get_status", target).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/wf-rules") => {
            let req: WfRulesRequest = serde_json::from_slice(&body)?;
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::CompileApply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/wf-rules/compile") => {
            let req: WfRulesRequest = serde_json::from_slice(&body)?;
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Compile).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/wf-rules/apply") => {
            let req: WfRulesRequest = serde_json::from_slice(&body)?;
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Apply).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/wf-rules/rollback") => {
            let req: WfRulesRequest = serde_json::from_slice(&body)?;
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Rollback).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("POST", "/wf-rules/delete") => {
            let req: WfRulesRequest = serde_json::from_slice(&body)?;
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Delete).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/wf-rules") => {
            let req = wf_rules_request_from_query(query, Some(ctx.hive_id.clone()));
            let action = if req.payload.contains_key("workflow_name") {
                "get_workflow"
            } else {
                "list_workflows"
            };
            let (status, resp) = handle_wf_rules_query(ctx, client, action, req).await?;
            respond_json(stream, status, &resp).await?;
        }
        ("GET", "/wf-rules/status") => {
            let req = wf_rules_request_from_query(query, Some(ctx.hive_id.clone()));
            let (status, resp) = handle_wf_rules_query(ctx, client, "get_status", req).await?;
            respond_json(stream, status, &resp).await?;
        }
        _ => {
            let _ = headers;
            respond_json(stream, 404, r#"{"error":"not_found"}"#).await?;
        }
    }
    Ok(())
}

async fn handle_inventory_http(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    let target = format!("SY.orchestrator@{}", ctx.hive_id);
    let timeout_secs = env_timeout_secs("JSR_ADMIN_INVENTORY_TIMEOUT_SECS").unwrap_or(10);
    let response = send_system_request(
        client,
        &target,
        "INVENTORY_REQUEST",
        "INVENTORY_RESPONSE",
        payload,
        Duration::from_secs(timeout_secs),
    )
    .await;
    Ok(build_admin_http_response("inventory", response))
}

async fn handle_hive_paths(
    method: &str,
    path: &str,
    query: &HashMap<String, String>,
    body: &[u8],
    ctx: &AdminContext,
    client: &AdminRouterClient,
) -> Result<Option<(u16, String)>, AdminError> {
    let trimmed = path.trim_matches('/');
    let parts = trimmed.split('/').collect::<Vec<_>>();
    if parts.first().copied() != Some("hives") {
        return Ok(None);
    }
    if parts.len() == 1 {
        match method {
            "GET" => {
                let (status, resp) = handle_admin_query(ctx, client, "list_hives", None).await?;
                return Ok(Some((status, resp)));
            }
            "POST" => {
                let payload = if body.is_empty() {
                    serde_json::json!({})
                } else {
                    serde_json::from_slice(body)?
                };
                let (status, resp) =
                    handle_admin_command(ctx, client, "add_hive", payload, None).await?;
                return Ok(Some((status, resp)));
            }
            _ => return Ok(None),
        }
    }
    let hive = parts[1].to_string();
    let rest = &parts[2..];
    if rest.is_empty() {
        return match method {
            "GET" => {
                let payload = serde_json::json!({ "hive_id": hive });
                let (status, resp) =
                    handle_admin_command(ctx, client, "get_hive", payload, None).await?;
                Ok(Some((status, resp)))
            }
            "DELETE" => {
                let payload = serde_json::json!({ "hive_id": hive });
                let (status, resp) =
                    handle_admin_command(ctx, client, "remove_hive", payload, None).await?;
                Ok(Some((status, resp)))
            }
            _ => Ok(None),
        };
    }
    match (method, rest) {
        ("GET", ["status"]) => {
            let payload = serde_json::json!({
                "scope": "hive",
                "filter_hive": hive,
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "inventory", payload, None).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["routes"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_routes", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["routes"]) => {
            let route: RouteConfig = serde_json::from_slice(body)?;
            let (status, resp) = handle_admin_command(
                ctx,
                client,
                "add_route",
                serde_json::to_value(route)?,
                Some(hive),
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["routes", prefix]) => {
            let payload = serde_json::json!({ "prefix": decode_percent(prefix) });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_route", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["vpns"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_vpns", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["vpns"]) => {
            let vpn: VpnConfig = serde_json::from_slice(body)?;
            let (status, resp) = handle_admin_command(
                ctx,
                client,
                "add_vpn",
                serde_json::to_value(vpn)?,
                Some(hive),
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["vpns", pattern]) => {
            let payload = serde_json::json!({ "pattern": decode_percent(pattern) });
            let (status, resp) =
                handle_admin_command(ctx, client, "delete_vpn", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_nodes", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_admin_command(ctx, client, "run_node", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes", name, "start"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if payload.get("node_name").is_none() {
                payload["node_name"] = serde_json::Value::String(decode_percent(name));
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "start_node", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes", name, "restart"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if payload.get("node_name").is_none() {
                payload["node_name"] = serde_json::Value::String(decode_percent(name));
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "restart_node", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["nodes", name]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if payload.get("node_name").is_none() {
                payload["node_name"] = serde_json::Value::String(decode_percent(name));
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "kill_node", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["nodes", name, "instance"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if payload.get("node_name").is_none() {
                payload["node_name"] = serde_json::Value::String(decode_percent(name));
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "remove_node_instance", payload, Some(hive))
                    .await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes", name, "config"]) => {
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_node_config", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("PUT", ["nodes", name, "config"]) => {
            let config_payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if !config_payload.is_object() {
                return Ok(Some((
                    400,
                    serde_json::json!({
                        "status": "error",
                        "action": "set_node_config",
                        "payload": serde_json::Value::Null,
                        "error_code": "INVALID_REQUEST",
                        "error_detail": "request body must be a JSON object",
                    })
                    .to_string(),
                )));
            }
            let replace = query
                .get("replace")
                .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let notify = query
                .get("notify")
                .map(|value| !(value == "0" || value.eq_ignore_ascii_case("false")))
                .unwrap_or(true);
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
                "config": config_payload,
                "replace": replace,
                "notify": notify,
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "set_node_config", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes", name, "state"]) => {
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_node_state", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["nodes", name, "status"]) => {
            let payload = serde_json::json!({
                "node_name": decode_percent(name),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_node_status", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes", name, "messages"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if payload.get("node_name").is_none() {
                payload["node_name"] = serde_json::Value::String(decode_percent(name));
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "send_node_message", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes", name, "control", "config-get"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if !payload.is_object() {
                return Ok(Some((
                    400,
                    serde_json::json!({
                        "status": "error",
                        "action": "node_control_config_get",
                        "payload": serde_json::Value::Null,
                        "error_code": "INVALID_REQUEST",
                        "error_detail": "request body must be a JSON object",
                    })
                    .to_string(),
                )));
            }
            payload["node_name"] = serde_json::Value::String(decode_percent(name));
            let (status, resp) =
                handle_admin_command(ctx, client, "node_control_config_get", payload, Some(hive))
                    .await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["nodes", name, "control", "config-set"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if !payload.is_object() {
                return Ok(Some((
                    400,
                    serde_json::json!({
                        "status": "error",
                        "action": "node_control_config_set",
                        "payload": serde_json::Value::Null,
                        "error_code": "INVALID_REQUEST",
                        "error_detail": "request body must be a JSON object",
                    })
                    .to_string(),
                )));
            }
            payload["node_name"] = serde_json::Value::String(decode_percent(name));
            let (status, resp) =
                handle_admin_command(ctx, client, "node_control_config_set", payload, Some(hive))
                    .await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["identity", "ilks"]) => {
            let (status, resp) = handle_admin_query(ctx, client, "list_ilks", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["identity", "ilks", ilk_id]) => {
            let payload = serde_json::json!({
                "ilk_id": decode_percent(ilk_id),
            });
            let (status, resp) =
                handle_admin_command(ctx, client, "get_ilk", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("PUT", ["identity", "tenants", tenant_id]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if !payload.is_object() {
                return Ok(Some((
                    400,
                    serde_json::json!({
                        "status": "error",
                        "action": "update_tenant",
                        "payload": serde_json::Value::Null,
                        "error_code": "INVALID_REQUEST",
                        "error_detail": "request body must be a JSON object",
                    })
                    .to_string(),
                )));
            }
            payload["tenant_id"] = serde_json::Value::String(decode_percent(tenant_id));
            let (status, resp) =
                handle_admin_command(ctx, client, "update_tenant", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["identity", "tenants", tenant_id, "sponsor"]) => {
            let mut payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            if !payload.is_object() {
                return Ok(Some((
                    400,
                    serde_json::json!({
                        "status": "error",
                        "action": "set_tenant_sponsor",
                        "payload": serde_json::Value::Null,
                        "error_code": "INVALID_REQUEST",
                        "error_detail": "request body must be a JSON object",
                    })
                    .to_string(),
                )));
            }
            payload["tenant_id"] = serde_json::Value::String(decode_percent(tenant_id));
            let (status, resp) =
                handle_admin_command(ctx, client, "set_tenant_sponsor", payload, Some(hive))
                    .await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["versions"]) => {
            let (status, resp) =
                handle_admin_query(ctx, client, "get_versions", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["runtimes"]) => {
            let (status, resp) =
                handle_admin_query(ctx, client, "list_runtimes", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["runtimes", runtime]) => {
            let payload = serde_json::json!({
                "runtime": decode_percent(runtime),
            });
            let (status, resp) =
                handle_admin_query_with_payload(ctx, client, "get_runtime", payload, Some(hive))
                    .await?;
            Ok(Some((status, resp)))
        }
        ("DELETE", ["runtimes", runtime, "versions", version]) => {
            let mut payload = serde_json::json!({
                "runtime": decode_percent(runtime),
                "runtime_version": decode_percent(version),
            });
            if let Some(test_hold_ms) = query
                .get("test_hold_ms")
                .and_then(|value| value.parse::<u64>().ok())
                .filter(|value| *value > 0)
            {
                payload["test_hold_ms"] = serde_json::json!(test_hold_ms);
            }
            let (status, resp) =
                handle_admin_command(ctx, client, "remove_runtime_version", payload, Some(hive))
                    .await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["deployments"]) => {
            let payload = deployments_payload_from_query(query);
            let (status, resp) =
                handle_admin_command(ctx, client, "get_deployments", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["drift-alerts"]) => {
            let payload = drift_alerts_payload_from_query(query);
            let (status, resp) =
                handle_admin_command(ctx, client, "get_drift_alerts", payload, Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["update"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) = handle_hive_update_command(ctx, client, hive, payload).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["sync-hint"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) = handle_hive_sync_hint_command(ctx, client, hive, payload).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::CompileApply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "compile"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Compile).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "apply"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Apply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "rollback"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Rollback).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["opa", "policy", "check"]) => {
            let req: OpaRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) = handle_opa_http(ctx, client, req, OpaAction::Check).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["opa", "policy"]) => {
            let (status, resp) = handle_opa_query(ctx, client, "get_policy", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["opa", "status"]) => {
            let (status, resp) = handle_opa_query(ctx, client, "get_status", Some(hive)).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["wf-rules"]) => {
            let req: WfRulesRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::CompileApply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["wf-rules", "compile"]) => {
            let req: WfRulesRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Compile).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["wf-rules", "apply"]) => {
            let req: WfRulesRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Apply).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["wf-rules", "rollback"]) => {
            let req: WfRulesRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Rollback).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["wf-rules", "delete"]) => {
            let req: WfRulesRequest = serde_json::from_slice(body)?;
            let mut req = req;
            req.hive = Some(hive);
            let (status, resp) =
                handle_wf_rules_http(ctx, client, req, WfRulesAction::Delete).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["wf-rules"]) => {
            let req = wf_rules_request_from_query(query.clone(), Some(hive));
            let action = if req.payload.contains_key("workflow_name") {
                "get_workflow"
            } else {
                "list_workflows"
            };
            let (status, resp) = handle_wf_rules_query(ctx, client, action, req).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["wf-rules", "status"]) => {
            let req = wf_rules_request_from_query(query.clone(), Some(hive));
            let (status, resp) = handle_wf_rules_query(ctx, client, "get_status", req).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["timer", "help"]) => {
            let (status, resp) = handle_timer_rpc(
                ctx,
                client,
                "timer_help",
                "TIMER_HELP",
                serde_json::json!({}),
                hive,
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["timer", "timers"]) => {
            let mut payload = serde_json::json!({});
            if let Some(owner_l2_name) =
                query.get("owner_l2_name").filter(|value| !value.is_empty())
            {
                payload["owner_l2_name"] = serde_json::json!(owner_l2_name);
            }
            if let Some(status_filter) =
                query.get("status_filter").filter(|value| !value.is_empty())
            {
                payload["status_filter"] = serde_json::json!(status_filter);
            }
            if let Some(limit) = query
                .get("limit")
                .and_then(|value| value.parse::<u64>().ok())
            {
                payload["limit"] = serde_json::json!(limit);
            }
            let (status, resp) =
                handle_timer_rpc(ctx, client, "timer_list", "TIMER_LIST", payload, hive).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["timer", "timers", timer_uuid]) => {
            let payload = serde_json::json!({
                "timer_uuid": decode_percent(timer_uuid),
            });
            let (status, resp) =
                handle_timer_rpc(ctx, client, "timer_get", "TIMER_GET", payload, hive).await?;
            Ok(Some((status, resp)))
        }
        ("GET", ["timer", "now"]) => {
            let (status, resp) = handle_timer_rpc(
                ctx,
                client,
                "timer_now",
                "TIMER_NOW",
                serde_json::json!({}),
                hive,
            )
            .await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["timer", "now-in"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_timer_rpc(ctx, client, "timer_now_in", "TIMER_NOW_IN", payload, hive)
                    .await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["timer", "convert"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_timer_rpc(ctx, client, "timer_convert", "TIMER_CONVERT", payload, hive)
                    .await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["timer", "parse"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_timer_rpc(ctx, client, "timer_parse", "TIMER_PARSE", payload, hive).await?;
            Ok(Some((status, resp)))
        }
        ("POST", ["timer", "format"]) => {
            let payload = if body.is_empty() {
                serde_json::json!({})
            } else {
                serde_json::from_slice(body)?
            };
            let (status, resp) =
                handle_timer_rpc(ctx, client, "timer_format", "TIMER_FORMAT", payload, hive)
                    .await?;
            Ok(Some((status, resp)))
        }
        _ => Ok(None),
    }
}

async fn handle_modules_paths(
    method: &str,
    path: &str,
    body: &[u8],
) -> Result<Option<(u16, Vec<u8>, Option<String>)>, AdminError> {
    let trimmed = path.trim_matches('/');
    let parts = trimmed.split('/').collect::<Vec<_>>();
    if parts.first().copied() != Some("modules") {
        return Ok(None);
    }
    let storage_root = storage_root();
    let modules_root = storage_root.join("modules");
    match (method, parts.as_slice()) {
        ("GET", ["modules"]) => {
            let modules = list_dirs(&modules_root)?;
            let body = serde_json::json!({
                "status": "ok",
                "modules": modules,
            })
            .to_string()
            .into_bytes();
            Ok(Some((200, body, None)))
        }
        ("GET", ["modules", name]) => {
            let name = decode_percent(name);
            let versions = list_dirs(&modules_root.join(&name))?;
            let body = serde_json::json!({
                "status": "ok",
                "name": name,
                "versions": versions,
            })
            .to_string()
            .into_bytes();
            Ok(Some((200, body, None)))
        }
        ("GET", ["modules", name, version]) => {
            let name = decode_percent(name);
            let version = decode_percent(version);
            let module_path = modules_root.join(&name).join(&version);
            if !module_path.exists() {
                let body = serde_json::json!({
                    "status": "error",
                    "error_code": "NOT_FOUND",
                })
                .to_string()
                .into_bytes();
                return Ok(Some((404, body, None)));
            }
            if module_path.is_file() {
                let data = fs::read(&module_path)?;
                return Ok(Some((
                    200,
                    data,
                    Some("application/octet-stream".to_string()),
                )));
            }
            let mut builder = Builder::new(Vec::new());
            builder.append_dir_all(".", &module_path)?;
            let data = builder.into_inner()?;
            Ok(Some((200, data, Some("application/x-tar".to_string()))))
        }
        ("POST", ["modules", name, version]) => {
            let name = decode_percent(name);
            let version = decode_percent(version);
            let module_dir = modules_root.join(&name).join(&version);
            if module_dir.exists() {
                let body = serde_json::json!({
                    "status": "error",
                    "error_code": "MODULE_EXISTS",
                })
                .to_string()
                .into_bytes();
                return Ok(Some((409, body, None)));
            }
            fs::create_dir_all(&module_dir)?;
            let mut archive = Archive::new(Cursor::new(body));
            if let Err(err) = archive.unpack(&module_dir) {
                let _ = fs::remove_dir_all(&module_dir);
                let body = serde_json::json!({
                    "status": "error",
                    "error_code": "INVALID_ARCHIVE",
                    "error_detail": err.to_string(),
                })
                .to_string()
                .into_bytes();
                return Ok(Some((400, body, None)));
            }
            let body = serde_json::json!({
                "status": "ok",
                "name": name,
                "version": version,
            })
            .to_string()
            .into_bytes();
            Ok(Some((200, body, None)))
        }
        _ => Ok(None),
    }
}

fn decode_percent(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn storage_root() -> PathBuf {
    let default_root = json_router::paths::storage_root_dir();
    let hive_path = json_router::paths::config_dir().join("hive.yaml");
    let data = match fs::read_to_string(&hive_path) {
        Ok(data) => data,
        Err(_) => return default_root,
    };
    let parsed: HiveFile = match serde_yaml::from_str(&data) {
        Ok(parsed) => parsed,
        Err(_) => return default_root,
    };
    if let Some(path) = parsed.storage.and_then(|storage| storage.path) {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    default_root
}

fn list_dirs(path: &Path) -> Result<Vec<String>, AdminError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                entries.push(name.to_string());
            }
        }
    }
    entries.sort();
    Ok(entries)
}

async fn read_http_request(
    stream: &mut tokio::net::TcpStream,
) -> Result<(String, String, HashMap<String, String>, Vec<u8>), AdminError> {
    let mut buf = Vec::new();
    let mut header_end = None;
    loop {
        let mut chunk = [0u8; 1024];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = find_double_crlf(&buf) {
            header_end = Some(pos + 4);
            break;
        }
    }
    let header_end = header_end.ok_or("invalid http request")?;
    let header_str = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = header_str.split("\r\n");
    let request_line = lines.next().ok_or("missing request line")?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or("missing method")?.to_string();
    let path = parts.next().ok_or("missing path")?.to_string();
    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let content_length = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let mut chunk = vec![0u8; content_length - body.len()];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Ok((method, path, headers, body))
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn split_path_query(path: &str) -> (&str, HashMap<String, String>) {
    if let Some((base, query)) = path.split_once('?') {
        (base, parse_query(query))
    } else {
        (path, HashMap::new())
    }
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some(parts) => parts,
            None => (pair, ""),
        };
        params.insert(key.to_string(), value.to_string());
    }
    params
}

fn deployments_payload_from_query(query: &HashMap<String, String>) -> serde_json::Value {
    let mut payload = serde_json::json!({});
    if let Some(limit_raw) = query.get("limit") {
        if let Ok(limit) = limit_raw.trim().parse::<u64>() {
            payload["limit"] = serde_json::Value::Number(limit.into());
        }
    }
    if let Some(category) = query
        .get("category")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        payload["category"] = serde_json::Value::String(category.to_string());
    }
    payload
}

fn drift_alerts_payload_from_query(query: &HashMap<String, String>) -> serde_json::Value {
    let mut payload = deployments_payload_from_query(query);
    if let Some(severity) = query
        .get("severity")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        payload["severity"] = serde_json::Value::String(severity.to_string());
    }
    if let Some(kind) = query
        .get("kind")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        payload["kind"] = serde_json::Value::String(kind.to_string());
    }
    payload
}

fn is_mother_role(role: Option<&str>) -> bool {
    matches!(role.map(|r| r.trim().to_ascii_lowercase()), Some(ref r) if r == "motherbee")
}

fn http_status_line(status: u16) -> &'static str {
    match status {
        200 => "HTTP/1.1 200 OK",
        202 => "HTTP/1.1 202 Accepted",
        400 => "HTTP/1.1 400 Bad Request",
        404 => "HTTP/1.1 404 Not Found",
        409 => "HTTP/1.1 409 Conflict",
        422 => "HTTP/1.1 422 Unprocessable Entity",
        500 => "HTTP/1.1 500 Internal Server Error",
        501 => "HTTP/1.1 501 Not Implemented",
        502 => "HTTP/1.1 502 Bad Gateway",
        503 => "HTTP/1.1 503 Service Unavailable",
        504 => "HTTP/1.1 504 Gateway Timeout",
        _ => "HTTP/1.1 500 Internal Server Error",
    }
}

fn error_code_to_http_status(error_code: &str) -> u16 {
    let code = error_code.trim().to_ascii_uppercase();
    if code == "TIMEOUT" || code == "WAN_TIMEOUT" || code == "SSH_TIMEOUT" {
        return 504;
    }
    if code.starts_with("SSH_") {
        return 502;
    }
    match code.as_str() {
        "INVALID_REQUEST" | "INVALID_ADDRESS" | "INVALID_ARCHIVE" | "INVALID_HIVE_ID"
        | "INVALID_ZIP" => 400,
        "NOT_FOUND"
        | "NODE_NOT_FOUND"
        | "BLOB_NOT_FOUND"
        | "RUNTIME_NOT_AVAILABLE"
        | "RUNTIME_NOT_FOUND"
        | "RUNTIME_VERSION_NOT_FOUND" => 404,
        "HIVE_EXISTS"
        | "MODULE_EXISTS"
        | "VERSION_MISMATCH"
        | "VERSION_CONFLICT"
        | "WAN_NOT_AUTHORIZED"
        | "NODE_INSTANCE_RUNNING"
        | "NODE_NOT_CONFIGURED"
        | "NOTHING_STAGED"
        | "NO_BACKUP"
        | "INSTANCES_ACTIVE"
        | "INSTANCES_UNKNOWN"
        | "STALE_CONFIG_VERSION"
        | "RUNTIME_IN_USE"
        | "RUNTIME_HAS_DEPENDENTS"
        | "RUNTIME_CURRENT_CONFLICT"
        | "BUSY" => 409,
        "COMPILE_ERROR"
        | "INVALID_CONFIG"
        | "INVALID_PACKAGE_LAYOUT"
        | "UNSUPPORTED_APPLY_MODE"
        | "PACKAGE_PUBLISH_FAILED" => 422,
        "INVALID_WORKFLOW_NAME" | "INVALID_CONFIG_SET" | "UNSUPPORTED_OPERATION" => 400,
        "WORKFLOW_NOT_FOUND" => 404,
        "NOT_IMPLEMENTED" => 501,
        "SERVICE_FAILED"
        | "SPAWN_FAILED"
        | "KILL_FAILED"
        | "REMOVE_FAILED"
        | "COPY_FAILED"
        | "CONFIG_FAILED"
        | "CONFIG_BROADCAST_FAILED"
        | "RUNTIME_ERROR"
        | "RUNTIME_COMMAND_FAILED"
        | "RUNTIME_UNAVAILABLE"
        | "RUNTIME_REMOVE_FAILED"
        | "ORCHESTRATOR_ERROR"
        | "RESTART_FAILED"
        | "TRANSPORT_ERROR" => 502,
        "SHM_NOT_FOUND" | "RUNTIME_MANIFEST_MISSING" | "MISSING_WAN_LISTEN" => 503,
        _ => 500,
    }
}

fn infer_transport_error_code(error_detail: &str) -> &'static str {
    let lower = error_detail.to_ascii_lowercase();
    if lower.contains("timeout") || lower.contains("timed out") {
        "TIMEOUT"
    } else {
        "TRANSPORT_ERROR"
    }
}

fn value_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

fn payload_error_code(payload: &serde_json::Value) -> Option<String> {
    value_string_field(payload, "error_code").or_else(|| {
        payload
            .get("error")
            .and_then(|value| value.get("code"))
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
    })
}

fn payload_error_detail(payload: &serde_json::Value) -> Option<String> {
    value_string_field(payload, "error_detail")
        .or_else(|| value_string_field(payload, "message"))
        .or_else(|| {
            payload
                .get("error")
                .and_then(|value| value.get("detail"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
        .or_else(|| {
            payload
                .get("error")
                .and_then(|value| value.get("message"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        })
}

async fn handle_storage_metrics_http(ctx: &AdminContext) -> (u16, String) {
    let action = "get_storage_metrics";
    let trace_id = Uuid::new_v4().to_string();
    let reply_subject = ctx.nats_client.inbox_reply_subject(&trace_id);
    let request_envelope = NatsRequestEnvelope::<serde_json::Value>::new(
        SUBJECT_STORAGE_METRICS_GET,
        trace_id.clone(),
        reply_subject.clone(),
        None,
    );
    let request_body = match serde_json::to_vec(&request_envelope) {
        Ok(body) => body,
        Err(err) => {
            let error_detail = err.to_string();
            return (
                500,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "status": "error",
                        "error_code": "INVALID_REQUEST",
                        "message": error_detail,
                    },
                    "error_code": "INVALID_REQUEST",
                    "error_detail": error_detail,
                })
                .to_string(),
            );
        }
    };
    let sid = 0u32;
    let request_started = Instant::now();
    tracing::info!(
        trace_id = %trace_id,
        endpoint = %ctx.nats_endpoint,
        request_subject = SUBJECT_STORAGE_METRICS_GET,
        reply_subject = %reply_subject,
        sid = sid,
        timeout_secs = STORAGE_METRICS_NATS_TIMEOUT_SECS,
        request_bytes = request_body.len(),
        "storage metrics nats request send"
    );
    let response_body = match ctx
        .nats_client
        .request_with_session_inbox(
            SUBJECT_STORAGE_METRICS_GET,
            &request_body,
            &reply_subject,
            Duration::from_secs(STORAGE_METRICS_NATS_TIMEOUT_SECS),
        )
        .await
    {
        Ok(body) => {
            let elapsed_ms = request_started.elapsed().as_millis() as u64;
            tracing::info!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = elapsed_ms,
                response_bytes = body.len(),
                "storage metrics nats response received"
            );
            body
        }
        Err(err) => {
            let error_detail = err.to_string();
            let elapsed_ms = request_started.elapsed().as_millis() as u64;
            tracing::warn!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = elapsed_ms,
                error = %error_detail,
                "storage metrics nats request failed"
            );
            return (
                503,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "status": "error",
                        "error_code": "STORAGE_METRICS_UNAVAILABLE",
                        "message": error_detail,
                    },
                    "error_code": "STORAGE_METRICS_UNAVAILABLE",
                    "error_detail": error_detail,
                })
                .to_string(),
            );
        }
    };
    let response: NatsResponseEnvelope<StorageInboxMetrics> =
        match serde_json::from_slice(&response_body) {
            Ok(resp) => resp,
            Err(err) => {
                let error_detail = err.to_string();
                let elapsed_ms = request_started.elapsed().as_millis() as u64;
                tracing::warn!(
                    trace_id = %trace_id,
                    endpoint = %ctx.nats_endpoint,
                    request_subject = SUBJECT_STORAGE_METRICS_GET,
                    reply_subject = %reply_subject,
                    sid = sid,
                    elapsed_ms = elapsed_ms,
                    error = %error_detail,
                    "storage metrics nats response decode failed"
                );
                return (
                    502,
                    serde_json::json!({
                        "status": "error",
                        "action": action,
                        "payload": {
                            "status": "error",
                            "error_code": "TRANSPORT_ERROR",
                            "message": error_detail,
                        },
                        "error_code": "TRANSPORT_ERROR",
                        "error_detail": error_detail,
                    })
                    .to_string(),
                );
            }
        };
    tracing::info!(
        trace_id = %trace_id,
        endpoint = %ctx.nats_endpoint,
        request_subject = SUBJECT_STORAGE_METRICS_GET,
        reply_subject = %reply_subject,
        sid = sid,
        elapsed_ms = request_started.elapsed().as_millis() as u64,
        response_status = %response.status,
        has_metrics = response.payload.is_some(),
        response_error_code = ?response.error_code,
        response_error_detail = ?response.error_detail,
        "storage metrics nats response decoded"
    );
    if response.action != SUBJECT_STORAGE_METRICS_GET {
        let error_detail = format!(
            "action mismatch expected={} got={}",
            SUBJECT_STORAGE_METRICS_GET, response.action
        );
        let elapsed_ms = request_started.elapsed().as_millis() as u64;
        tracing::warn!(
            trace_id = %trace_id,
            endpoint = %ctx.nats_endpoint,
            request_subject = SUBJECT_STORAGE_METRICS_GET,
            reply_subject = %reply_subject,
            sid = sid,
            elapsed_ms = elapsed_ms,
            error = %error_detail,
            "storage metrics nats response action mismatch"
        );
        return (
            502,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": {
                    "status": "error",
                    "error_code": "TRANSPORT_ERROR",
                    "message": error_detail,
                },
                "error_code": "TRANSPORT_ERROR",
                "error_detail": error_detail,
            })
            .to_string(),
        );
    }
    if response.trace_id != trace_id {
        let error_detail = format!(
            "trace_id mismatch expected={} got={}",
            trace_id, response.trace_id
        );
        let elapsed_ms = request_started.elapsed().as_millis() as u64;
        tracing::warn!(
            trace_id = %trace_id,
            endpoint = %ctx.nats_endpoint,
            request_subject = SUBJECT_STORAGE_METRICS_GET,
            reply_subject = %reply_subject,
            sid = sid,
            elapsed_ms = elapsed_ms,
            error = %error_detail,
            "storage metrics nats response trace mismatch"
        );
        return (
            502,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": {
                    "status": "error",
                    "error_code": "TRANSPORT_ERROR",
                    "message": error_detail,
                },
                "error_code": "TRANSPORT_ERROR",
                "error_detail": error_detail,
            })
            .to_string(),
        );
    }

    match (response.status.as_str(), response.payload) {
        ("ok", Some(metrics)) => {
            tracing::info!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                pending = metrics.pending,
                pending_with_error = metrics.pending_with_error,
                oldest_pending_age_s = metrics.oldest_pending_age_s,
                max_attempts = metrics.max_attempts,
                processed_total = metrics.processed_total,
                "storage metrics nats request success"
            );
            (
                200,
                serde_json::json!({
                    "status": "ok",
                    "action": action,
                    "payload": {
                        "status": "ok",
                        "metrics": metrics,
                    },
                    "error_code": serde_json::Value::Null,
                    "error_detail": serde_json::Value::Null,
                })
                .to_string(),
            )
        }
        _ => {
            let error_code = response
                .error_code
                .unwrap_or_else(|| "STORAGE_METRICS_UNAVAILABLE".to_string());
            let error_detail = response
                .error_detail
                .unwrap_or_else(|| "storage metrics request failed".to_string());
            tracing::warn!(
                trace_id = %trace_id,
                endpoint = %ctx.nats_endpoint,
                request_subject = SUBJECT_STORAGE_METRICS_GET,
                reply_subject = %reply_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                error_code = %error_code,
                error_detail = %error_detail,
                "storage metrics nats request returned error payload"
            );
            (
                503,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "status": "error",
                        "error_code": error_code,
                        "message": error_detail,
                    },
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            )
        }
    }
}

fn is_ok_status(status: Option<&str>) -> bool {
    matches!(
        status,
        Some(value)
            if value.eq_ignore_ascii_case("ok")
                || value.eq_ignore_ascii_case("not_found")
                || value.eq_ignore_ascii_case("sync_pending")
    )
}

fn payload_is_ok(payload: &serde_json::Value) -> bool {
    if is_ok_status(payload.get("status").and_then(|v| v.as_str())) {
        return true;
    }
    payload
        .get("ok")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

async fn respond_json(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> Result<(), AdminError> {
    let status_line = http_status_line(status);
    let response = format!(
        "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.as_bytes().len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

async fn respond_bytes(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> Result<(), AdminError> {
    let status_line = http_status_line(status);
    let header = format!(
        "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    Ok(())
}

fn next_opa_version(requested: Option<u64>) -> Result<u64, AdminError> {
    let path = json_router::paths::opa_version_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let current = fs::read_to_string(&path)
        .ok()
        .and_then(|data| data.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let next = match requested {
        Some(version) => {
            if version <= current {
                return Err(
                    format!("version must be greater than current (current={current})").into(),
                );
            }
            version
        }
        None => current + 1,
    };
    fs::write(&path, next.to_string())?;
    Ok(next)
}

async fn handle_opa_http(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    mut req: OpaRequest,
    action: OpaAction,
) -> Result<(u16, String), AdminError> {
    let version = match action {
        OpaAction::Apply => {
            let Some(version) = req.version else {
                let resp = serde_json::json!({
                    "status": "error",
                    "error_code": "VERSION_MISMATCH",
                    "error_detail": "version_required"
                });
                return Ok((400, resp.to_string()));
            };
            version
        }
        OpaAction::Rollback => req.version.unwrap_or(0),
        _ => next_opa_version(req.version)?,
    };
    let entrypoint = req
        .entrypoint
        .clone()
        .unwrap_or_else(|| "router/target".to_string());
    let target = normalize_opa_target(req.hive.take());

    if action.needs_rego() && req.rego.as_deref().unwrap_or("").is_empty() {
        let resp = serde_json::json!({
            "status": "error",
            "error_code": "COMPILE_ERROR",
            "error_detail": "rego_missing"
        });
        return Ok((400, resp.to_string()));
    }

    let mut responses = Vec::new();
    if matches!(
        action,
        OpaAction::Compile | OpaAction::CompileApply | OpaAction::Check
    ) {
        responses = send_opa_action(
            ctx,
            client,
            "compile",
            version,
            req.rego.clone(),
            Some(entrypoint.clone()),
            action.auto_apply_flag(),
            target.clone(),
        )
        .await?;
        if responses.is_empty() || responses.iter().any(|r| r.status != "ok") {
            return Ok(build_opa_http_response(
                ctx, action, version, responses, target,
            ));
        }
    }

    if action.apply_after() {
        let apply_responses = send_opa_action(
            ctx,
            client,
            "apply",
            version,
            None,
            None,
            None,
            target.clone(),
        )
        .await?;
        if apply_responses.is_empty() || apply_responses.iter().any(|r| r.status != "ok") {
            let mut combined = responses;
            combined.extend(apply_responses);
            return Ok(build_opa_http_response(
                ctx, action, version, combined, target,
            ));
        }
        responses.extend(apply_responses);
    } else if matches!(action, OpaAction::Apply | OpaAction::Rollback) {
        let apply_action = action.as_str();
        responses = send_opa_action(
            ctx,
            client,
            apply_action,
            version,
            None,
            None,
            None,
            target.clone(),
        )
        .await?;
    }

    Ok(build_opa_http_response(
        ctx, action, version, responses, target,
    ))
}

async fn handle_opa_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target: Option<String>,
) -> Result<(u16, String), AdminError> {
    let responses = send_opa_query(ctx, client, action, target.clone()).await?;
    Ok(build_opa_query_response(ctx, action, responses, target))
}

async fn handle_wf_rules_http(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    req: WfRulesRequest,
    action: WfRulesAction,
) -> Result<(u16, String), AdminError> {
    let target_hive = normalize_wf_rules_target(req.hive.clone(), &ctx.hive_id);
    let target_node = format!("SY.wf-rules@{}", target_hive);
    let response = if let Some(operation) = action.operation() {
        let mut payload = req.payload;
        payload.insert("operation".to_string(), serde_json::json!(operation));
        send_system_request_with_meta(
            client,
            &target_node,
            "CONFIG_SET",
            "CONFIG_RESPONSE",
            serde_json::json!({
                "node_name": target_node,
                "schema_version": 1,
                "config_version": 0,
                "apply_mode": "replace",
                "config": serde_json::Value::Object(payload),
            }),
            16,
            None,
            None,
            Some(target_node.clone()),
            Some("CONFIG_SET".to_string()),
            None,
            admin_action_timeout(action.action_name()),
        )
        .await
    } else if let Some(command) = action.command() {
        send_l2_action_request(
            client,
            &target_node,
            "command",
            command,
            serde_json::Value::Object(req.payload),
            admin_action_timeout(action.action_name()),
        )
        .await
    } else {
        Err("unsupported wf-rules action".into())
    };
    Ok(build_admin_http_response(action.action_name(), response))
}

async fn handle_wf_rules_query(
    _ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    req: WfRulesRequest,
) -> Result<(u16, String), AdminError> {
    let target_hive = normalize_wf_rules_target(req.hive.clone(), "");
    let target_node = if target_hive.is_empty() {
        return Err("wf-rules target hive is required".into());
    } else {
        format!("SY.wf-rules@{}", target_hive)
    };
    let response = send_l2_action_request(
        client,
        &target_node,
        "query",
        action,
        serde_json::Value::Object(req.payload),
        admin_action_timeout(action),
    )
    .await;
    Ok(build_admin_http_response(action, response))
}

fn wf_rules_request_from_query(
    mut query: HashMap<String, String>,
    default_hive: Option<String>,
) -> WfRulesRequest {
    let explicit_hive = query.remove("hive");
    let explicit_target = query.remove("target");
    let hive = explicit_hive.or(explicit_target).or(default_hive);
    WfRulesRequest {
        payload: query
            .into_iter()
            .map(|(key, value)| (key, serde_json::json!(value)))
            .collect(),
        hive,
    }
}

fn normalize_wf_rules_target(target: Option<String>, fallback_hive: &str) -> String {
    target
        .and_then(|value| {
            let trimmed = value.trim().to_string();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("broadcast") {
                None
            } else {
                Some(trimmed)
            }
        })
        .unwrap_or_else(|| fallback_hive.to_string())
}

async fn send_l2_action_request(
    client: &AdminRouterClient,
    target: &str,
    msg_type: &str,
    action: &str,
    payload: serde_json::Value,
    timeout_window: Duration,
) -> Result<serde_json::Value, AdminError> {
    use tokio::time::timeout;

    let sender = client.sender_snapshot().await;
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: msg_type.to_string(),
            action: Some(action.to_string()),
            target: Some(target.to_string()),
            action_class: classify_admin_action(action),
            ..Meta::default()
        },
        payload,
    };
    let (tx, rx) = oneshot::channel::<Message>();
    client.enqueue_admin_waiter(trace_id.clone(), tx).await;
    sender.send(msg).await?;

    let msg = match timeout(timeout_window, rx).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(_)) => return Err("l2 action response channel closed".into()),
        Err(_) => {
            client.drop_admin_waiter(&trace_id).await;
            return Err(format!(
                "l2 action timeout msg_type={} action={} target={} timeout_secs={}",
                msg_type,
                action,
                target,
                timeout_window.as_secs()
            )
            .into());
        }
    };

    if matches!(
        msg.meta.msg.as_deref(),
        Some("UNREACHABLE" | "TTL_EXCEEDED")
    ) {
        let system_msg = msg.meta.msg.as_deref().unwrap_or("");
        return Err(format!("router returned {} for target={}", system_msg, target).into());
    }

    let response_msg_type = msg.meta.msg_type.as_str();
    let expected_ok = match msg_type {
        "command" => response_msg_type == "command" || response_msg_type == "command_response",
        "query" => response_msg_type == "query" || response_msg_type == "query_response",
        _ => false,
    };
    if !expected_ok {
        return Err(format!(
            "invalid response type for {}: expected {} got {}",
            action, msg_type, response_msg_type
        )
        .into());
    }
    let action_matches = msg.meta.action.as_deref() == Some(action)
        || msg.meta.msg.as_deref() == Some(&format!("{}_RESPONSE", action.to_uppercase()));
    if !action_matches {
        return Err(format!(
            "invalid response action for {}: got action={:?} msg={:?}",
            action, msg.meta.action, msg.meta.msg
        )
        .into());
    }
    Ok(msg.payload)
}

async fn handle_timer_rpc(
    _ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    timer_msg: &str,
    payload: serde_json::Value,
    hive_id: String,
) -> Result<(u16, String), AdminError> {
    let target = format!("SY.timer@{}", hive_id);
    let normalized_payload = if payload.is_null() {
        serde_json::json!({})
    } else {
        payload
    };
    let response = send_system_request(
        client,
        &target,
        timer_msg,
        "TIMER_RESPONSE",
        normalized_payload,
        admin_action_timeout(action),
    )
    .await;
    Ok(build_admin_http_response(action, response))
}

async fn handle_admin_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    if action == "list_admin_actions" {
        return Ok(build_admin_actions_catalog_response());
    }
    if action == "list_ilks" {
        return handle_identity_query(ctx, client, action, hive).await;
    }
    let payload = normalize_admin_payload(action, serde_json::json!({}), hive.as_deref());
    let request = build_admin_request(ctx, action, payload, hive);
    let response = send_admin_request(client, request, admin_action_timeout(action)).await;
    Ok(build_admin_http_response(action, response))
}

async fn handle_admin_query_with_payload(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    let payload = normalize_admin_payload(action, payload, hive.as_deref());
    let request = build_admin_request(ctx, action, payload, hive);
    let response = send_admin_request(client, request, admin_action_timeout(action)).await;
    Ok(build_admin_http_response(action, response))
}

fn build_admin_actions_catalog_response() -> (u16, String) {
    let actions: Vec<serde_json::Value> = INTERNAL_ACTION_REGISTRY
        .iter()
        .map(build_admin_action_doc)
        .collect();
    (
        200,
        serde_json::json!({
            "status": "ok",
            "action": "list_admin_actions",
            "payload": {
                "registry_version": INTERNAL_ACTION_REGISTRY_VERSION,
                "actions": actions,
            },
            "error_code": serde_json::Value::Null,
            "error_detail": serde_json::Value::Null,
        })
        .to_string(),
    )
}

fn build_admin_action_help_response(action_name: &str) -> (u16, String) {
    let action_name = action_name.trim();
    let Some(spec) = INTERNAL_ACTION_REGISTRY
        .iter()
        .find(|spec| spec.action == action_name)
    else {
        return (
            404,
            serde_json::json!({
                "status": "error",
                "action": "get_admin_action_help",
                "payload": serde_json::Value::Null,
                "error_code": "ACTION_NOT_FOUND",
                "error_detail": format!("unknown admin action '{}'", action_name),
            })
            .to_string(),
        );
    };

    (
        200,
        serde_json::json!({
            "status": "ok",
            "action": "get_admin_action_help",
            "payload": {
                "registry_version": INTERNAL_ACTION_REGISTRY_VERSION,
                "entry": build_admin_action_doc(spec),
            },
            "error_code": serde_json::Value::Null,
            "error_detail": serde_json::Value::Null,
        })
        .to_string(),
    )
}

fn build_admin_executor_function_catalog_response(
    ctx: &AdminContext,
) -> Result<(u16, String), AdminError> {
    let node_config = load_admin_executor_node_config(&ctx.hive_id)?;
    let catalog = build_admin_executor_function_catalog(node_config.as_ref());
    let mode = admin_executor_catalog_mode(node_config.as_ref());
    Ok((
        200,
        serde_json::json!({
            "status": "ok",
            "action": "admin_executor_function_catalog",
            "payload": {
                "node_name": ctx.node_name,
                "catalog_mode": mode,
                "registry_version": INTERNAL_ACTION_REGISTRY_VERSION,
                "function_count": catalog.len(),
                "override_actions": ADMIN_EXECUTOR_SCHEMA_OVERRIDE_ACTIONS,
                "functions": catalog,
            },
            "error_code": serde_json::Value::Null,
            "error_detail": serde_json::Value::Null,
        })
        .to_string(),
    ))
}

fn extract_admin_executor_openai_api_key(
    payload: &serde_json::Value,
) -> Result<Option<String>, AdminError> {
    let config_root = payload.get("config").unwrap_or(payload);
    if let Some(value) = config_root
        .get("ai_providers")
        .and_then(|providers| providers.get("openai"))
        .and_then(|openai| openai.get("api_key"))
        .and_then(serde_json::Value::as_str)
    {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err("admin executor config-set received an empty OpenAI api_key".into());
        }
        return Ok(Some(trimmed.to_string()));
    }
    Ok(None)
}

fn merge_admin_executor_local_config(
    existing: Option<AdminExecutorNodeConfigFile>,
    payload: &serde_json::Value,
) -> Result<(AdminExecutorNodeConfigFile, bool), AdminError> {
    let mut merged = existing.unwrap_or_default();
    if merged.schema_version == 0 {
        merged.schema_version = ADMIN_EXECUTOR_CONFIG_SCHEMA_VERSION;
    }
    let mut changed = false;
    let config_root = payload.get("config").unwrap_or(payload);

    if let Some(openai) = config_root
        .get("ai_providers")
        .and_then(|providers| providers.get("openai"))
        .and_then(serde_json::Value::as_object)
    {
        let provider = merged
            .ai_providers
            .get_or_insert_with(AiProvidersSection::default);
        let openai_cfg = provider.openai.get_or_insert_with(OpenAiSection::default);
        if let Some(value) = openai.get("default_model") {
            openai_cfg.default_model = value
                .as_str()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(ToString::to_string);
            changed = true;
        }
        if let Some(value) = openai.get("max_tokens") {
            openai_cfg.max_tokens = value.as_u64().map(|v| v as u32);
            changed = true;
        }
        if let Some(value) = openai.get("temperature") {
            openai_cfg.temperature = value.as_f64().map(|v| v as f32);
            changed = true;
        }
        if let Some(value) = openai.get("top_p") {
            openai_cfg.top_p = value.as_f64().map(|v| v as f32);
            changed = true;
        }
    }

    if let Some(catalog) = config_root
        .get("catalog")
        .and_then(serde_json::Value::as_object)
    {
        let catalog_cfg = merged
            .catalog
            .get_or_insert_with(AdminExecutorCatalogConfig::default);
        if let Some(value) = catalog.get("mode") {
            let mode = value
                .as_str()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .ok_or("config.catalog.mode must be a non-empty string")?;
            if mode != "full" && mode != "subset" {
                return Err("config.catalog.mode must be 'full' or 'subset'".into());
            }
            catalog_cfg.mode = Some(mode.to_string());
            changed = true;
        }
        if let Some(value) = catalog.get("actions") {
            let values = value
                .as_array()
                .ok_or("config.catalog.actions must be an array of admin action names")?;
            let mut actions = Vec::new();
            for entry in values {
                let action = entry
                    .as_str()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .ok_or("config.catalog.actions entries must be non-empty strings")?;
                resolve_internal_action_spec(action).map_err(|err| {
                    format!("config.catalog.actions contains invalid action '{action}': {err}")
                })?;
                actions.push(action.to_string());
            }
            catalog_cfg.actions = Some(actions);
            changed = true;
        }
    }

    Ok((merged, changed))
}

fn build_admin_executor_config_get_payload(
    ctx: &AdminContext,
) -> Result<serde_json::Value, AdminError> {
    let node_config = load_admin_executor_node_config(&ctx.hive_id)?;
    let secret_record = load_admin_secret_record(&ctx.node_name)?;
    let merged = merged_admin_executor_openai_section(node_config.as_ref(), secret_record.as_ref());
    let config_version = node_config
        .as_ref()
        .map(|cfg| cfg.config_version)
        .unwrap_or(0);
    let schema_version = node_config
        .as_ref()
        .map(|cfg| cfg.schema_version)
        .unwrap_or(ADMIN_EXECUTOR_CONFIG_SCHEMA_VERSION);
    let configured = merged
        .as_ref()
        .and_then(|openai| openai.api_key.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some();
    let api_key_source = if secret_record
        .as_ref()
        .and_then(|record| record.secrets.get(ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        "local_file"
    } else {
        "missing"
    };
    let mut descriptor = NodeSecretDescriptor::new(
        "config.ai_providers.openai.api_key",
        ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI,
    );
    descriptor.required = true;
    descriptor.configured = configured;
    descriptor.persistence = "local_file".to_string();

    Ok(serde_json::json!({
        "ok": true,
        "node_name": ctx.node_name,
        "state": if configured { "configured" } else { "missing_secret" },
        "schema_version": schema_version,
        "config_version": config_version,
        "config": {
            "ai_providers": {
                "openai": {
                    "api_key": if configured { serde_json::Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()) } else { serde_json::Value::Null },
                    "default_model": merged.as_ref().and_then(|openai| openai.default_model.clone()).map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
                    "max_tokens": merged.as_ref().and_then(|openai| openai.max_tokens.map(serde_json::Value::from)).unwrap_or(serde_json::Value::Null),
                    "temperature": merged.as_ref().and_then(|openai| openai.temperature.map(serde_json::Value::from)).unwrap_or(serde_json::Value::Null),
                    "top_p": merged.as_ref().and_then(|openai| openai.top_p.map(serde_json::Value::from)).unwrap_or(serde_json::Value::Null)
                }
            },
            "catalog": {
                "mode": admin_executor_catalog_mode(node_config.as_ref()),
                "actions": node_config
                    .as_ref()
                    .and_then(|cfg| cfg.catalog.as_ref())
                    .and_then(|catalog| catalog.actions.clone())
                    .map(serde_json::Value::from)
                    .unwrap_or(serde_json::Value::Null)
            }
        },
        "contract": {
            "node_family": "SY",
            "node_kind": "SY.admin",
            "supports": ["CONFIG_GET", "CONFIG_SET"],
            "required_fields": ["config.ai_providers.openai.api_key"],
            "optional_fields": [
                "config.ai_providers.openai.default_model",
                "config.ai_providers.openai.max_tokens",
                "config.ai_providers.openai.temperature",
                "config.ai_providers.openai.top_p",
                "config.catalog.mode",
                "config.catalog.actions"
            ],
            "secrets": [descriptor],
            "notes": [
                "This is the standard node CONFIG_GET/SET surface for the SY.admin executor runtime.",
                "OpenAI secrets are stored in local secrets.json and always returned redacted.",
                "Catalog mode defaults to full. Use subset only when intentionally constraining the executor-visible action set."
            ]
        },
        "secret_record": secret_record.map(|record| redacted_node_secret_record(&record)),
        "api_key_source": api_key_source,
        "function_count": build_admin_executor_function_catalog(node_config.as_ref()).len()
    }))
}

fn admin_executor_config_error_response(
    ctx: &AdminContext,
    code: &str,
    message: &str,
) -> Result<serde_json::Value, AdminError> {
    let base = build_admin_executor_config_get_payload(ctx)?;
    Ok(base
        .as_object()
        .cloned()
        .map(|mut value| {
            value.insert("ok".to_string(), serde_json::Value::Bool(false));
            value.insert(
                "error".to_string(),
                serde_json::json!({
                    "code": code,
                    "message": message
                }),
            );
            serde_json::Value::Object(value)
        })
        .unwrap_or_else(|| {
            serde_json::json!({
                "ok": false,
                "node_name": ctx.node_name,
                "state": "error",
                "error": {
                    "code": code,
                    "message": message
                }
            })
        }))
}

async fn apply_admin_executor_config_set(
    ctx: &AdminContext,
    msg: &Message,
) -> Result<serde_json::Value, AdminError> {
    let requested_node_name = msg
        .payload
        .get("node_name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("config-set requires node_name")?;
    if requested_node_name != ctx.node_name {
        return admin_executor_config_error_response(
            ctx,
            "invalid_config",
            "config-set node_name does not match this node",
        );
    }
    if let Some(apply_mode) = msg
        .payload
        .get("apply_mode")
        .and_then(serde_json::Value::as_str)
    {
        if !apply_mode.trim().eq_ignore_ascii_case("replace") {
            return admin_executor_config_error_response(
                ctx,
                "unsupported_apply_mode",
                "SY.admin executor currently supports only apply_mode=replace",
            );
        }
    }

    let api_key = match extract_admin_executor_openai_api_key(&msg.payload) {
        Ok(value) => value,
        Err(err) => {
            return admin_executor_config_error_response(ctx, "invalid_config", &err.to_string());
        }
    };
    let current_config = load_admin_executor_node_config(&ctx.hive_id)?;
    let current_version = current_config
        .as_ref()
        .map(|cfg| cfg.config_version)
        .unwrap_or(0);
    let (mut merged_config, config_changed) =
        match merge_admin_executor_local_config(current_config, &msg.payload) {
            Ok(value) => value,
            Err(err) => {
                return admin_executor_config_error_response(
                    ctx,
                    "invalid_config",
                    &err.to_string(),
                );
            }
        };
    if api_key.is_none() && !config_changed {
        return admin_executor_config_error_response(
            ctx,
            "invalid_config",
            "config-set requires config.ai_providers.openai.api_key or another executor config field",
        );
    }

    merged_config.schema_version = msg
        .payload
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as u32)
        .unwrap_or(ADMIN_EXECUTOR_CONFIG_SCHEMA_VERSION);
    merged_config.config_version = msg
        .payload
        .get("config_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_else(|| current_version.saturating_add(1));

    let _ = save_admin_executor_node_config(&ctx.hive_id, &merged_config)?;

    let stored_openai_secret = if let Some(api_key) = api_key {
        let mut secrets = load_admin_secret_record(&ctx.node_name)?
            .map(|record| record.secrets)
            .unwrap_or_default();
        secrets.insert(
            ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI.to_string(),
            serde_json::Value::String(api_key),
        );
        let record = build_node_secret_record(
            secrets,
            &NodeSecretWriteOptions {
                updated_by_ilk: msg
                    .payload
                    .get("requested_by")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string),
                updated_by_label: Some("SY.admin".to_string()),
                trace_id: Some(msg.routing.trace_id.clone()),
            },
        );
        save_node_secret_record_with_root(&ctx.node_name, admin_nodes_root(), &record)
            .map_err(|err| -> AdminError { Box::new(err) })?;
        true
    } else {
        false
    };

    let configured = refresh_admin_executor_ai_runtime(ctx).await?;
    Ok(serde_json::json!({
        "ok": true,
        "node_name": ctx.node_name,
        "state": if configured { "configured" } else { "missing_secret" },
        "schema_version": merged_config.schema_version,
        "config_version": merged_config.config_version,
        "apply_mode": "replace",
        "config": {
            "ai_providers": {
                "openai": {
                    "api_key": if configured { serde_json::Value::String(NODE_SECRET_REDACTION_TOKEN.to_string()) } else { serde_json::Value::Null },
                    "default_model": merged_config.ai_providers.as_ref().and_then(|p| p.openai.as_ref()).and_then(|o| o.default_model.clone()).map(serde_json::Value::String).unwrap_or(serde_json::Value::Null),
                    "max_tokens": merged_config.ai_providers.as_ref().and_then(|p| p.openai.as_ref()).and_then(|o| o.max_tokens.map(serde_json::Value::from)).unwrap_or(serde_json::Value::Null),
                    "temperature": merged_config.ai_providers.as_ref().and_then(|p| p.openai.as_ref()).and_then(|o| o.temperature.map(serde_json::Value::from)).unwrap_or(serde_json::Value::Null),
                    "top_p": merged_config.ai_providers.as_ref().and_then(|p| p.openai.as_ref()).and_then(|o| o.top_p.map(serde_json::Value::from)).unwrap_or(serde_json::Value::Null)
                }
            },
            "catalog": {
                "mode": admin_executor_catalog_mode(Some(&merged_config)),
                "actions": merged_config.catalog.as_ref().and_then(|catalog| catalog.actions.clone()).map(serde_json::Value::from).unwrap_or(serde_json::Value::Null)
            }
        },
        "stored_secrets": if stored_openai_secret {
            serde_json::json!([{
                "field": "config.ai_providers.openai.api_key",
                "storage_key": ADMIN_EXECUTOR_LOCAL_SECRET_KEY_OPENAI,
                "value_redacted": true
            }])
        } else {
            serde_json::json!([])
        },
        "message": "SY.admin executor config persisted through CONFIG_SET."
    }))
}

fn build_admin_action_doc(spec: &InternalActionSpec) -> serde_json::Value {
    let path_patterns = admin_action_path_patterns(spec.action);
    let (handler, canonical_action) = match spec.route {
        InternalActionRoute::Query(canonical) => ("query", Some(canonical)),
        InternalActionRoute::Command(canonical) => ("command", Some(canonical)),
        InternalActionRoute::Update => ("update", None),
        InternalActionRoute::SyncHint => ("sync_hint", None),
        InternalActionRoute::Inventory => ("inventory", None),
        InternalActionRoute::OpaHttp(_) => ("opa_http", None),
        InternalActionRoute::OpaQuery(canonical) => ("opa_query", Some(canonical)),
        InternalActionRoute::WfRulesHttp(_) => ("wf_rules_http", None),
        InternalActionRoute::WfRulesQuery(canonical) => ("wf_rules_query", Some(canonical)),
        InternalActionRoute::TimerRpc(canonical) => ("timer_rpc", Some(canonical)),
    };
    serde_json::json!({
        "action": spec.action,
        "handler": handler,
        "canonical_action": canonical_action,
        "requires_target": spec.requires_target,
        "allow_legacy_hive_id": spec.allow_legacy_hive_id,
        "read_only": admin_action_is_read_only(spec.action),
        "confirmation_required": admin_action_requires_confirmation(spec.action),
        "summary": admin_action_summary(spec.action),
        "path_patterns": path_patterns,
        "path_patterns_are_templates": true,
        "execution_preference": "example_scmd",
        "request_contract": admin_action_request_contract(spec.action),
    })
}

fn admin_action_is_read_only(action: &str) -> bool {
    !matches!(
        action,
        "add_hive"
            | "publish_runtime_package"
            | "remove_hive"
            | "add_route"
            | "delete_route"
            | "add_vpn"
            | "delete_vpn"
            | "run_node"
            | "start_node"
            | "restart_node"
            | "kill_node"
            | "remove_node_instance"
            | "remove_runtime_version"
            | "set_node_config"
            | "node_control_config_set"
            | "send_node_message"
            | "set_storage"
            | "update"
            | "sync_hint"
            | "opa_compile_apply"
            | "opa_compile"
            | "opa_apply"
            | "opa_rollback"
            | "wf_rules_compile_apply"
            | "wf_rules_compile"
            | "wf_rules_apply"
            | "wf_rules_rollback"
            | "wf_rules_delete"
    )
}

fn admin_action_requires_confirmation(action: &str) -> bool {
    matches!(
        action,
        "publish_runtime_package"
            | "remove_hive"
            | "delete_route"
            | "delete_vpn"
            | "kill_node"
            | "remove_node_instance"
            | "remove_runtime_version"
            | "set_node_config"
            | "node_control_config_set"
            | "set_storage"
            | "update_tenant"
            | "set_tenant_sponsor"
            | "update"
            | "sync_hint"
            | "opa_compile_apply"
            | "opa_compile"
            | "opa_apply"
            | "opa_rollback"
    )
}

fn admin_action_summary(action: &str) -> &'static str {
    match action {
        "list_admin_actions" => "List the dynamic admin action catalog and help metadata.",
        "get_admin_action_help" => "Return help metadata for one admin action.",
        "publish_runtime_package" => "Publish one runtime package into dist/manifest on motherbee.",
        "hive_status" => "Read the local hive status summary.",
        "list_hives" => "List all known hives.",
        "get_hive" => "Read one hive definition.",
        "list_nodes" => {
            "List nodes for one hive only. Use inventory for system-wide node visibility."
        }
        "get_node_status" => "Read the effective runtime status of one node.",
        "get_node_state" => "Read the persisted state payload of one node.",
        "get_node_config" => "Read the stored effective config.json payload for a managed node.",
        "node_control_config_get" => {
            "Send CONFIG_GET to a node that participates in live control-plane config and return its CONFIG_RESPONSE."
        }
        "node_control_config_set" => {
            "Send CONFIG_SET to a node that participates in live control-plane config and return its CONFIG_RESPONSE."
        }
        "list_ilks" => "List identity ilks in a hive.",
        "get_ilk" => "Read one identity ilk.",
        "update_tenant" => "Update mutable fields of one identity tenant in a hive.",
        "set_tenant_sponsor" => {
            "Set or clear the sponsor relationship for one identity tenant in a hive."
        }
        "inventory" => "Read global or per-hive inventory, including system-wide node visibility.",
        "list_versions" => "List core and runtime versions across hives.",
        "get_versions" => "Read core and runtime versions for one hive.",
        "list_runtimes" => "List runtimes for one hive.",
        "get_runtime" => "Read one runtime definition in a hive.",
        "remove_runtime_version" => {
            "Delete one published runtime version from motherbee dist/manifest."
        }
        "list_routes" => "List route rules for a hive.",
        "list_vpns" => "List VPN patterns for a hive.",
        "get_storage" => "Read storage configuration and status.",
        "list_deployments" => "List historical deployments globally.",
        "get_deployments" => "List historical deployments targeting one hive.",
        "list_drift_alerts" => "List historical drift alerts globally.",
        "get_drift_alerts" => "List historical drift alerts for one hive.",
        "opa_get_policy" => "Read current OPA policy text for a hive.",
        "opa_get_status" => "Read OPA status for a hive.",
        "wf_rules_get_workflow" => "Read the current workflow definition managed by SY.wf-rules for a hive.",
        "wf_rules_get_status" => "Read workflow status from SY.wf-rules for a hive.",
        "wf_rules_list_workflows" => "List workflows managed by SY.wf-rules for a hive.",
        "timer_help" => "Read the self-described SY.timer capability catalog for a hive.",
        "timer_get" => "Read one timer by uuid from SY.timer for a hive.",
        "timer_list" => "List timers from SY.timer for a hive, optionally filtered by owner.",
        "timer_now" => "Read the current UTC time from SY.timer for a hive.",
        "timer_now_in" => "Read the current time in one IANA timezone from SY.timer for a hive.",
        "timer_convert" => "Convert one UTC instant through SY.timer for a hive.",
        "timer_parse" => "Parse a date/time string through SY.timer for a hive.",
        "timer_format" => "Format a UTC instant through SY.timer for a hive.",
        "opa_check" => "Validate OPA policy text without applying it.",
        "run_node" => "Create and start a new managed node instance in a hive.",
        "start_node" => "Start an existing persisted managed node instance in a hive.",
        "restart_node" => "Restart an existing managed node instance in a hive.",
        "kill_node" => "Stop a node in a hive. Optional purge_instance also removes its persisted instance directory.",
        "remove_node_instance" => "Delete a node instance from disk/runtime state.",
        "send_node_message" => "Send a system message to a node.",
        "set_node_config" => "Persist node config changes.",
        "set_storage" => "Persist storage configuration changes.",
        "add_hive" => "Create a hive and bootstrap it.",
        "remove_hive" => "Remove a hive.",
        "add_route" => "Add a route rule to a hive. Route action must be FORWARD (forward to next_hop_hive) or DROP. match_kind defaults to PREFIX when omitted. priority defaults to 100 when omitted.",
        "delete_route" => "Delete a route rule from a hive.",
        "add_vpn" => "Add a VPN pattern to a hive.",
        "delete_vpn" => "Delete a VPN pattern from a hive.",
        "update" => "Run hive update workflow.",
        "sync_hint" => "Trigger a sync hint workflow.",
        "opa_compile_apply" => "Compile and apply OPA policy.",
        "opa_compile" => "Compile OPA policy.",
        "opa_apply" => "Apply OPA policy.",
        "opa_rollback" => "Rollback OPA policy.",
        "wf_rules_compile_apply" => "Compile and apply a workflow definition through SY.wf-rules.",
        "wf_rules_compile" => "Compile a workflow definition without applying it.",
        "wf_rules_apply" => "Apply the staged workflow definition.",
        "wf_rules_rollback" => "Rollback to the backup workflow definition.",
        "wf_rules_delete" => "Delete a managed workflow through SY.wf-rules.",
        _ => "Admin action.",
    }
}

fn admin_action_path_patterns(action: &str) -> Vec<&'static str> {
    match action {
        "list_admin_actions" => vec!["GET /admin/actions"],
        "get_admin_action_help" => vec!["GET /admin/actions/{action}"],
        "publish_runtime_package" => vec!["POST /admin/runtime-packages/publish"],
        "hive_status" => vec!["GET /hive/status"],
        "list_hives" => vec!["GET /hives"],
        "get_hive" => vec!["GET /hives/{hive}"],
        "list_nodes" => vec!["GET /hives/{hive}/nodes"],
        "get_node_status" => vec!["GET /hives/{hive}/nodes/{node_name}/status"],
        "get_node_state" => vec!["GET /hives/{hive}/nodes/{node_name}/state"],
        "get_node_config" => vec!["GET /hives/{hive}/nodes/{node_name}/config"],
        "node_control_config_get" => {
            vec!["POST /hives/{hive}/nodes/{node_name}/control/config-get"]
        }
        "node_control_config_set" => {
            vec!["POST /hives/{hive}/nodes/{node_name}/control/config-set"]
        }
        "list_ilks" => vec!["GET /hives/{hive}/identity/ilks"],
        "get_ilk" => vec!["GET /hives/{hive}/identity/ilks/{ilk_id}"],
        "update_tenant" => vec!["PUT /hives/{hive}/identity/tenants/{tenant_id}"],
        "set_tenant_sponsor" => {
            vec!["POST /hives/{hive}/identity/tenants/{tenant_id}/sponsor"]
        }
        "inventory" => vec![
            "GET /inventory",
            "GET /inventory/summary",
            "GET /inventory/{hive}",
            "GET /hives/{hive}/inventory/summary",
            "GET /hives/{hive}/inventory/hive",
        ],
        "list_versions" => vec!["GET /versions"],
        "get_versions" => vec!["GET /hives/{hive}/versions"],
        "list_runtimes" => vec!["GET /hives/{hive}/runtimes"],
        "get_runtime" => vec!["GET /hives/{hive}/runtimes/{runtime}"],
        "remove_runtime_version" => {
            vec!["DELETE /hives/{hive}/runtimes/{runtime}/versions/{version}"]
        }
        "list_routes" => vec!["GET /hives/{hive}/routes"],
        "list_vpns" => vec!["GET /hives/{hive}/vpns"],
        "get_storage" => vec!["GET /config/storage"],
        "list_deployments" => vec!["GET /deployments"],
        "get_deployments" => vec!["GET /hives/{hive}/deployments"],
        "list_drift_alerts" => vec!["GET /drift-alerts"],
        "get_drift_alerts" => vec!["GET /hives/{hive}/drift-alerts"],
        "opa_get_policy" => vec!["GET /hives/{hive}/opa/policy"],
        "opa_get_status" => vec!["GET /hives/{hive}/opa/status"],
        "wf_rules_get_workflow" => vec!["GET /hives/{hive}/wf-rules"],
        "wf_rules_get_status" => vec!["GET /hives/{hive}/wf-rules/status"],
        "wf_rules_list_workflows" => vec!["GET /hives/{hive}/wf-rules"],
        "timer_help" => vec!["GET /hives/{hive}/timer/help"],
        "timer_get" => vec!["GET /hives/{hive}/timer/timers/{timer_uuid}"],
        "timer_list" => vec!["GET /hives/{hive}/timer/timers"],
        "timer_now" => vec!["GET /hives/{hive}/timer/now"],
        "timer_now_in" => vec!["POST /hives/{hive}/timer/now-in"],
        "timer_convert" => vec!["POST /hives/{hive}/timer/convert"],
        "timer_parse" => vec!["POST /hives/{hive}/timer/parse"],
        "timer_format" => vec!["POST /hives/{hive}/timer/format"],
        "opa_check" => vec!["POST /hives/{hive}/opa/policy/check"],
        "run_node" => vec!["POST /hives/{hive}/nodes"],
        "start_node" => vec!["POST /hives/{hive}/nodes/{node_name}/start"],
        "restart_node" => vec!["POST /hives/{hive}/nodes/{node_name}/restart"],
        "kill_node" => vec!["DELETE /hives/{hive}/nodes/{node_name}"],
        "remove_node_instance" => vec!["DELETE /hives/{hive}/nodes/{node_name}/instance"],
        "send_node_message" => vec!["POST /hives/{hive}/nodes/{node_name}/messages"],
        "set_node_config" => vec!["PUT /hives/{hive}/nodes/{node_name}/config"],
        "set_storage" => vec!["PUT /config/storage"],
        "add_hive" => vec!["POST /hives"],
        "remove_hive" => vec!["DELETE /hives/{hive}"],
        "add_route" => vec!["POST /hives/{hive}/routes"],
        "delete_route" => vec!["DELETE /hives/{hive}/routes/{prefix}"],
        "add_vpn" => vec!["POST /hives/{hive}/vpns"],
        "delete_vpn" => vec!["DELETE /hives/{hive}/vpns/{pattern}"],
        "update" => vec!["POST /hives/{hive}/update"],
        "sync_hint" => vec!["POST /hives/{hive}/sync-hint"],
        "opa_compile_apply" => vec!["POST /hives/{hive}/opa/policy"],
        "opa_compile" => vec!["POST /hives/{hive}/opa/policy/compile"],
        "opa_apply" => vec!["POST /hives/{hive}/opa/policy/apply"],
        "opa_rollback" => vec!["POST /hives/{hive}/opa/policy/rollback"],
        "wf_rules_compile_apply" => vec!["POST /hives/{hive}/wf-rules"],
        "wf_rules_compile" => vec!["POST /hives/{hive}/wf-rules/compile"],
        "wf_rules_apply" => vec!["POST /hives/{hive}/wf-rules/apply"],
        "wf_rules_rollback" => vec!["POST /hives/{hive}/wf-rules/rollback"],
        "wf_rules_delete" => vec!["POST /hives/{hive}/wf-rules/delete"],
        _ => Vec::new(),
    }
}

fn admin_action_request_contract(action: &str) -> serde_json::Value {
    let path_patterns = admin_action_path_patterns(action);
    let required_fields = admin_action_body_required_fields(action);
    let optional_fields = admin_action_body_optional_fields(action);
    let example_payload = admin_action_example_payload(action);
    let has_body = admin_action_body_required(action)
        || !required_fields.is_empty()
        || !optional_fields.is_empty()
        || !example_payload.is_null();
    serde_json::json!({
        "transport": "http_via_sy_admin",
        "method": admin_action_primary_method(action),
        "path_patterns": path_patterns,
        "path_patterns_are_templates": true,
        "execution_preference": "example_scmd",
        "path_params": admin_action_path_params(action),
        "body": {
            "kind": if has_body { "json_object" } else { "none" },
            "required": admin_action_body_required(action),
            "required_fields": required_fields,
            "optional_fields": optional_fields,
        },
        "example_payload": example_payload,
        "example_scmd": admin_action_example_scmd(action),
        "notes": admin_action_request_notes(action),
    })
}

fn admin_action_primary_method(action: &str) -> &'static str {
    admin_action_path_patterns(action)
        .first()
        .and_then(|pattern| pattern.split_whitespace().next())
        .unwrap_or("GET")
}

fn admin_action_path_params(action: &str) -> Vec<serde_json::Value> {
    match action {
        "get_admin_action_help" => vec![admin_action_path_param(
            "action",
            "string",
            "Admin action name, for example add_hive.",
        )],
        "publish_runtime_package" => Vec::new(),
        "get_hive" | "remove_hive" => vec![admin_action_path_param(
            "hive",
            "string",
            "Hive id, for example worker-220.",
        )],
        "list_nodes" | "list_ilks" | "get_versions" | "list_runtimes" | "list_routes"
        | "list_vpns" | "get_deployments" | "get_drift_alerts" | "opa_get_policy"
        | "opa_get_status" | "wf_rules_get_workflow" | "wf_rules_get_status"
        | "wf_rules_list_workflows" | "timer_help" | "timer_list" | "timer_now"
        | "timer_now_in" | "timer_convert" | "timer_parse" | "timer_format" | "update"
        | "sync_hint" | "opa_compile_apply" | "opa_compile" | "opa_apply"
        | "opa_rollback" | "opa_check" | "wf_rules_compile_apply" | "wf_rules_compile"
        | "wf_rules_apply" | "wf_rules_rollback" | "wf_rules_delete" => vec![
            admin_action_path_param(
            "hive",
            "string",
            "Target hive id in the URL path.",
        )],
        "timer_get" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param(
                "timer_uuid",
                "string",
                "Timer uuid to read from the local SY.timer instance.",
            ),
        ],
        "inventory" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param(
                "scope",
                "enum(summary|hive)",
                "Inventory view encoded in the last path segment.",
            ),
        ],
        "get_node_status"
        | "get_node_state"
        | "get_node_config"
        | "kill_node"
        | "remove_node_instance"
        | "send_node_message"
        | "node_control_config_get"
        | "node_control_config_set"
        | "set_node_config" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param(
                "node_name",
                "string",
                "Fully-qualified node name, for example SY.admin@motherbee.",
            ),
        ],
        "get_runtime" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param("runtime", "string", "Runtime name, for example ai.chat."),
        ],
        "remove_runtime_version" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param("runtime", "string", "Runtime name, for example ai.chat."),
            admin_action_path_param("version", "string", "Runtime version to delete."),
        ],
        "get_ilk" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param(
                "ilk_id",
                "string",
                "ILK identifier in prefixed format, for example ilk:550e8400-e29b-41d4-a716-446655440000.",
            ),
        ],
        "update_tenant" | "set_tenant_sponsor" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param(
                "tenant_id",
                "string",
                "Tenant identifier in prefixed format, for example tnt:550e8400-e29b-41d4-a716-446655440000.",
            ),
        ],
        "run_node" => vec![admin_action_path_param(
            "hive",
            "string",
            "Target hive where the node should be created and started.",
        )],
        "start_node" | "restart_node" => vec![
            admin_action_path_param("hive", "string", "Target hive id in the URL path."),
            admin_action_path_param(
                "node_name",
                "string",
                "Fully-qualified node name for the persisted instance.",
            ),
        ],
        "add_route" => vec![admin_action_path_param(
            "hive",
            "string",
            "Target hive where the route will be added.",
        )],
        "delete_route" => vec![
            admin_action_path_param("hive", "string", "Target hive where the route exists."),
            admin_action_path_param(
                "prefix",
                "string",
                "Route prefix encoded in the URL path when deleting via HTTP.",
            ),
        ],
        "add_vpn" => vec![admin_action_path_param(
            "hive",
            "string",
            "Target hive where the VPN rule will be added.",
        )],
        "delete_vpn" => vec![
            admin_action_path_param("hive", "string", "Target hive where the VPN rule exists."),
            admin_action_path_param(
                "pattern",
                "string",
                "VPN pattern encoded in the URL path when deleting via HTTP.",
            ),
        ],
        _ => Vec::new(),
    }
}

fn admin_action_body_required(action: &str) -> bool {
    matches!(
        action,
        "add_hive"
            | "publish_runtime_package"
            | "run_node"
            | "start_node"
            | "restart_node"
            | "add_route"
            | "add_vpn"
            | "set_node_config"
            | "send_node_message"
            | "node_control_config_set"
            | "set_storage"
            | "update"
            | "update_tenant"
            | "set_tenant_sponsor"
            | "opa_compile_apply"
            | "opa_compile"
            | "opa_apply"
            | "wf_rules_compile_apply"
            | "wf_rules_compile"
            | "wf_rules_apply"
            | "wf_rules_rollback"
            | "wf_rules_delete"
            | "opa_check"
            | "timer_now_in"
            | "timer_convert"
            | "timer_parse"
            | "timer_format"
    )
}

fn admin_action_body_required_fields(action: &str) -> Vec<serde_json::Value> {
    match action {
        "add_hive" => vec![
            admin_action_body_field("hive_id", "string", "Unique hive id to create."),
            admin_action_body_field(
                "address",
                "string",
                "WAN or bootstrap address reachable from motherbee.",
            ),
        ],
        "publish_runtime_package" => vec![admin_action_body_field(
            "source",
            "object",
            "Runtime package source descriptor. Supports source.kind=inline_package and source.kind=bundle_upload.",
        )],
        "run_node" => vec![admin_action_body_field(
            "node_name",
            "string",
            "Fully-qualified node name to start.",
        )],
        "add_route" => vec![
            admin_action_body_field("prefix", "string", "Route prefix matched against message destinations, for example AI.chat or tenant.acme."),
            admin_action_body_field(
                "action",
                "string",
                "Routing action. Must be FORWARD (forward matching traffic to the hive named in next_hop_hive) or DROP (silently drop matching traffic). Case-sensitive.",
            ),
        ],
        "add_vpn" => vec![
            admin_action_body_field("pattern", "string", "VPN match pattern."),
            admin_action_body_field("vpn_id", "u32", "VPN identifier."),
        ],
        "set_node_config" => vec![admin_action_body_field(
            "config",
            "object",
            "Node config patch or replacement object.",
        )],
        "node_control_config_set" => vec![
            admin_action_body_field(
                "schema_version",
                "u32",
                "Schema version understood by the node.",
            ),
            admin_action_body_field(
                "config_version",
                "u64",
                "Monotonic config version chosen by the caller or node contract.",
            ),
            admin_action_body_field(
                "apply_mode",
                "string",
                "Apply mode, currently typically replace.",
            ),
            admin_action_body_field(
                "config",
                "object",
                "Node-defined config payload forwarded without platform interpretation.",
            ),
        ],
        "send_node_message" => vec![
            admin_action_body_field("msg_type", "string", "System message type."),
            admin_action_body_field(
                "payload",
                "object",
                "Message payload forwarded to the target node.",
            ),
        ],
        "set_storage" => vec![admin_action_body_field(
            "path",
            "string",
            "Absolute storage path to persist locally.",
        )],
        "update" => vec![admin_action_body_field(
            "manifest_hash",
            "string",
            "Core/runtime/vendor manifest hash to apply.",
        )],
        "opa_compile_apply" | "opa_compile" | "opa_check" => vec![admin_action_body_field(
            "rego",
            "string",
            "OPA rego source text.",
        )],
        "wf_rules_compile_apply" | "wf_rules_compile" => vec![
            admin_action_body_field("workflow_name", "string", "Workflow logical name."),
            admin_action_body_field("definition", "object", "Workflow definition JSON."),
        ],
        "wf_rules_apply"
        | "wf_rules_rollback"
        | "wf_rules_get_workflow"
        | "wf_rules_get_status"
        | "wf_rules_delete" => vec![admin_action_body_field(
            "workflow_name",
            "string",
            "Workflow logical name.",
        )],
        "opa_apply" => vec![admin_action_body_field(
            "version",
            "u64",
            "Compiled OPA version to apply.",
        )],
        "timer_get" => vec![admin_action_body_field(
            "timer_uuid",
            "string",
            "Timer uuid to read when using internal admin dispatch instead of the HTTP path form.",
        )],
        "timer_now_in" => vec![admin_action_body_field(
            "tz",
            "string",
            "IANA timezone name, for example America/Argentina/Buenos_Aires.",
        )],
        "timer_convert" => vec![
            admin_action_body_field("instant_utc_ms", "i64", "UTC instant in unix milliseconds."),
            admin_action_body_field("to_tz", "string", "IANA timezone name to convert into."),
        ],
        "timer_parse" => vec![
            admin_action_body_field("input", "string", "Input date/time text to parse."),
            admin_action_body_field("layout", "string", "Go time layout used to parse input."),
            admin_action_body_field("tz", "string", "IANA timezone applied to the parsed input."),
        ],
        "timer_format" => vec![
            admin_action_body_field("instant_utc_ms", "i64", "UTC instant in unix milliseconds."),
            admin_action_body_field("layout", "string", "Go time layout used for formatting."),
            admin_action_body_field("tz", "string", "IANA timezone used for rendering."),
        ],
        "set_tenant_sponsor" => vec![admin_action_body_field(
            "sponsor_tenant_id",
            "string|null",
            "Prefixed tenant id to assign as sponsor, or null to clear the sponsor relationship.",
        )],
        _ => Vec::new(),
    }
}

fn admin_action_body_optional_fields(action: &str) -> Vec<serde_json::Value> {
    match action {
        "publish_runtime_package" => vec![
            admin_action_body_field(
                "set_current",
                "bool",
                "Optional current-pointer intent. When true, promote this version to current. When false, publish it only into available versions.",
            ),
            admin_action_body_field(
                "sync_to",
                "string[]",
                "Optional hives that should receive sync_hint(channel=dist) after publish.",
            ),
            admin_action_body_field(
                "update_to",
                "string[]",
                "Optional hives that should receive update(category=runtime) for the newly published runtime/version.",
            ),
        ],
        "add_hive" => vec![
            admin_action_body_field(
                "harden_ssh",
                "bool",
                "Enable SSH hardening after bootstrap.",
            ),
            admin_action_body_field(
                "restrict_ssh",
                "bool",
                "Restrict bootstrap SSH access after provisioning.",
            ),
            admin_action_body_field(
                "require_dist_sync",
                "bool",
                "Require dist sync readiness before success.",
            ),
            admin_action_body_field(
                "dist_sync_probe_timeout_secs",
                "u64",
                "Timeout for dist sync readiness probe.",
            ),
        ],
        "update_tenant" => vec![
            admin_action_body_field(
                "name",
                "string",
                "Optional new display name for the tenant.",
            ),
            admin_action_body_field(
                "domain",
                "string",
                "Optional domain value. Pass an empty string to clear it.",
            ),
            admin_action_body_field(
                "status",
                "string",
                "Optional tenant status. Accepted values: pending, active, suspended.",
            ),
            admin_action_body_field(
                "settings",
                "object",
                "Optional full tenant settings object replacement.",
            ),
            admin_action_body_field(
                "sponsor_tenant_id",
                "string|null",
                "Optional sponsor tenant id update. Pass null to clear sponsorship.",
            ),
            admin_action_body_field(
                "hive",
                "string",
                "Optional hive override when using internal admin dispatch.",
            ),
        ],
        "set_tenant_sponsor" => vec![
            admin_action_body_field(
                "hive",
                "string",
                "Optional hive override when using internal admin dispatch.",
            ),
        ],
        "run_node" => vec![
            admin_action_body_field(
                "runtime",
                "string",
                "Runtime name. Optional when derivable from node_name.",
            ),
            admin_action_body_field(
                "runtime_version",
                "string",
                "Runtime version. Defaults to current.",
            ),
            admin_action_body_field(
                "tenant_id",
                "string",
                "Optional tenant id for identity-aware first spawn. Required by runtimes that need tenant-scoped identity during initial instance creation.",
            ),
            admin_action_body_field("unit", "string", "Optional unit suffix override."),
            admin_action_body_field(
                "config",
                "object",
                "Optional runtime/node config passed during spawn.",
            ),
        ],
        "kill_node" => vec![
            admin_action_body_field(
                "force",
                "bool",
                "Force stop when supported by the target runtime.",
            ),
            admin_action_body_field(
                "purge_instance",
                "bool",
                "Also remove the node persisted instance directory after stop. Useful before reinstall/recreate.",
            ),
        ],
        "add_route" => vec![
            admin_action_body_field(
                "match_kind",
                "string",
                "How to match the prefix. Accepted values: PREFIX (default, matches any destination starting with the prefix), EXACT (matches the destination exactly), GLOB (glob pattern match). Omit this field to use PREFIX. Do not pass an empty string — it is invalid.",
            ),
            admin_action_body_field(
                "next_hop_hive",
                "string",
                "Destination hive name to forward traffic to. Required when action is FORWARD. Must be the exact hive name as registered in the cluster. Omit when action is DROP.",
            ),
            admin_action_body_field(
                "metric",
                "u32",
                "Route metric used for tie-breaking between routes with the same prefix and priority. Default is 0 when omitted. Lower metric is preferred.",
            ),
            admin_action_body_field(
                "priority",
                "u16",
                "Route evaluation priority. Higher value means higher priority and earlier evaluation. Default is 100 when omitted. Do not pass 0 unless intentionally setting the lowest possible priority.",
            ),
        ],
        "add_vpn" => vec![
            admin_action_body_field("match_kind", "string", "Optional VPN match kind."),
            admin_action_body_field("priority", "u16", "Optional VPN priority."),
        ],
        "set_node_config" => vec![
            admin_action_body_field(
                "replace",
                "bool",
                "Replace full config instead of patch/merge.",
            ),
            admin_action_body_field(
                "notify",
                "bool",
                "Notify runtime after persisting the config.",
            ),
        ],
        "node_control_config_get" | "node_control_config_set" => vec![
            admin_action_body_field(
                "request_id",
                "string",
                "Optional caller-defined request id forwarded in payload.",
            ),
            admin_action_body_field(
                "contract_version",
                "u32",
                "Optional common control-plane contract version hint.",
            ),
            admin_action_body_field(
                "requested_by",
                "string",
                "Optional caller identity label forwarded in payload.",
            ),
            admin_action_body_field(
                "src_ilk",
                "string",
                "Optional source ILK forwarded in message metadata.",
            ),
            admin_action_body_field(
                "scope",
                "string",
                "Optional metadata scope forwarded with the message.",
            ),
            admin_action_body_field(
                "context",
                "object",
                "Optional metadata context forwarded with the message.",
            ),
            admin_action_body_field("ttl", "u16", "Optional TTL, default 16."),
        ],
        "send_node_message" => vec![
            admin_action_body_field("msg", "string", "Optional message name."),
            admin_action_body_field("ttl", "u16", "Optional TTL, default 16."),
            admin_action_body_field("scope", "string", "Optional scope metadata."),
            admin_action_body_field("priority", "u16", "Optional priority metadata."),
            admin_action_body_field("src_ilk", "string", "Optional source ILK."),
            admin_action_body_field(
                "context",
                "object",
                "Optional metadata context forwarded with the message.",
            ),
        ],
        "update" => vec![
            admin_action_body_field(
                "category",
                "enum(runtime|core|vendor)",
                "Update category. Defaults to runtime.",
            ),
            admin_action_body_field(
                "manifest_version",
                "u64",
                "Expected manifest version. Defaults to 0.",
            ),
            admin_action_body_field(
                "runtime",
                "string",
                "Optional runtime scope for targeted runtime readiness/materialization.",
            ),
            admin_action_body_field(
                "runtime_version",
                "string",
                "Optional runtime version scope. Requires runtime. Defaults to current.",
            ),
        ],
        "sync_hint" => vec![
            admin_action_body_field(
                "channel",
                "enum(blob|dist)",
                "Sync channel. Defaults to blob.",
            ),
            admin_action_body_field(
                "folder_id",
                "string",
                "Explicit folder id. Defaults from channel.",
            ),
            admin_action_body_field(
                "wait_for_idle",
                "bool",
                "Wait for idle completion before returning.",
            ),
            admin_action_body_field(
                "timeout_ms",
                "u64",
                "Timeout while waiting for idle completion.",
            ),
        ],
        "opa_compile_apply" | "opa_compile" | "opa_check" => vec![
            admin_action_body_field(
                "entrypoint",
                "string",
                "OPA entrypoint. Defaults to router/target.",
            ),
            admin_action_body_field(
                "version",
                "u64",
                "Optional explicit version. Otherwise a new version is assigned.",
            ),
        ],
        "opa_apply" | "opa_rollback" => vec![admin_action_body_field(
            "hive",
            "string",
            "Optional explicit hive override for OPA broadcast targeting.",
        )],
        "wf_rules_compile_apply" => vec![
            admin_action_body_field(
                "auto_spawn",
                "bool",
                "When true, spawn the WF node if it does not exist after apply.",
            ),
            admin_action_body_field(
                "tenant_id",
                "string",
                "Required for first deploy when auto_spawn=true and the WF node does not already exist.",
            ),
            admin_action_body_field(
                "version",
                "u64",
                "Optional explicit workflow version/idempotency check.",
            ),
            admin_action_body_field(
                "hive",
                "string",
                "Optional explicit hive override for SY.wf-rules target.",
            ),
        ],
        "wf_rules_compile" => vec![
            admin_action_body_field(
                "auto_spawn",
                "bool",
                "Accepted for symmetry with compile_apply but ignored because compile does not deploy.",
            ),
            admin_action_body_field(
                "version",
                "u64",
                "Optional explicit workflow version/idempotency check.",
            ),
            admin_action_body_field(
                "hive",
                "string",
                "Optional explicit hive override for SY.wf-rules target.",
            ),
        ],
        "wf_rules_apply" | "wf_rules_rollback" => vec![
            admin_action_body_field(
                "auto_spawn",
                "bool",
                "When true, allow first deploy spawn if the WF node does not exist.",
            ),
            admin_action_body_field(
                "tenant_id",
                "string",
                "Required for first deploy when auto_spawn=true and the WF node does not already exist.",
            ),
            admin_action_body_field(
                "version",
                "u64",
                "Optional staged version guard for apply.",
            ),
            admin_action_body_field(
                "hive",
                "string",
                "Optional explicit hive override for SY.wf-rules target.",
            ),
        ],
        "wf_rules_delete" => vec![
            admin_action_body_field(
                "force",
                "bool",
                "Force delete even if active instances exist.",
            ),
            admin_action_body_field(
                "hive",
                "string",
                "Optional explicit hive override for SY.wf-rules target.",
            ),
        ],
        "wf_rules_get_workflow" | "wf_rules_get_status" | "wf_rules_list_workflows" => vec![
            admin_action_body_field(
                "hive",
                "string",
                "Optional explicit hive override for SY.wf-rules target.",
            ),
        ],
        "remove_runtime_version" => vec![admin_action_body_field(
            "test_hold_ms",
            "u64",
            "Optional test-only hold used by E2E/runtime lifecycle checks.",
        )],
        "timer_list" => vec![
            admin_action_body_field(
                "owner_l2_name",
                "string",
                "Optional canonical L2 owner filter. When omitted, SY.timer returns timers for the full hive.",
            ),
            admin_action_body_field(
                "status_filter",
                "string",
                "Optional timer status filter: pending, fired, canceled, or all.",
            ),
            admin_action_body_field(
                "limit",
                "u32",
                "Optional result limit. Defaults inside SY.timer and is capped there.",
            ),
        ],
        _ => Vec::new(),
    }
}

fn admin_action_example_payload(action: &str) -> serde_json::Value {
    match action {
        "publish_runtime_package" => serde_json::json!({
            "source": {
                "kind": "bundle_upload",
                "blob_path": "packages/incoming/ai-support-demo-0.1.0.zip"
            },
            "set_current": true,
            "sync_to": ["worker-220"],
            "update_to": ["worker-220"]
        }),
        "add_hive" => serde_json::json!({
            "hive_id": "worker-220",
            "address": "192.168.8.220"
        }),
        "run_node" => serde_json::json!({
            "node_name": "AI.chat@motherbee",
            "runtime_version": "current"
        }),
        "start_node" => serde_json::json!({
            "node_name": "AI.chat@motherbee"
        }),
        "restart_node" => serde_json::json!({
            "node_name": "AI.chat@motherbee"
        }),
        "kill_node" => serde_json::json!({
            "force": false,
            "purge_instance": true
        }),
        "add_route" => serde_json::json!({
            "prefix": "AI.chat.",
            "action": "FORWARD",
            "next_hop_hive": "worker-220"
        }),
        "delete_route" => serde_json::json!({
            "prefix": "AI.chat."
        }),
        "add_vpn" => serde_json::json!({
            "pattern": "worker-*",
            "vpn_id": 220
        }),
        "delete_vpn" => serde_json::json!({
            "pattern": "worker-*"
        }),
        "set_node_config" => serde_json::json!({
            "config": {
                "openai": {
                    "default_model": "gpt-4.1-mini"
                }
            },
            "replace": false,
            "notify": true
        }),
        "node_control_config_get" => serde_json::json!({
            "requested_by": "archi"
        }),
        "node_control_config_set" => serde_json::json!({
            "schema_version": 1,
            "config_version": 7,
            "apply_mode": "replace",
            "config": {
                "behavior": {
                    "kind": "openai_chat",
                    "model": "gpt-4.1-mini"
                }
            }
        }),
        "timer_now_in" => serde_json::json!({
            "tz": "America/Argentina/Buenos_Aires"
        }),
        "timer_get" => serde_json::json!({
            "timer_uuid": "9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001"
        }),
        "timer_list" => serde_json::json!({
            "owner_l2_name": "WF.onboarding@motherbee",
            "status_filter": "pending",
            "limit": 100
        }),
        "timer_convert" => serde_json::json!({
            "instant_utc_ms": 1775771100000i64,
            "to_tz": "Europe/Madrid"
        }),
        "timer_parse" => serde_json::json!({
            "input": "2026-04-10 18:30",
            "layout": "2006-01-02 15:04",
            "tz": "America/Argentina/Buenos_Aires"
        }),
        "timer_format" => serde_json::json!({
            "instant_utc_ms": 1775771100000i64,
            "layout": "2006-01-02 15:04 MST",
            "tz": "America/Argentina/Buenos_Aires"
        }),
        "send_node_message" => serde_json::json!({
            "msg_type": "PING",
            "payload": {
                "ping": true
            }
        }),
        "set_storage" => serde_json::json!({
            "path": "/var/lib/fluxbee"
        }),
        "update" => serde_json::json!({
            "manifest_hash": "sha256:deadbeef",
            "category": "runtime",
            "manifest_version": 42,
            "runtime": "ai.common",
            "runtime_version": "0.1.2"
        }),
        "sync_hint" => serde_json::json!({
            "channel": "blob",
            "wait_for_idle": true,
            "timeout_ms": 30000
        }),
        "opa_compile_apply" | "opa_compile" | "opa_check" => serde_json::json!({
            "rego": "package router\n\ndefault target = null\n",
            "entrypoint": "router/target"
        }),
        "opa_apply" => serde_json::json!({
            "version": 12
        }),
        "opa_rollback" => serde_json::json!({
            "version": 11
        }),
        "wf_rules_compile_apply" => serde_json::json!({
            "workflow_name": "invoice",
            "definition": {
                "wf_schema_version": "1",
                "workflow_type": "invoice",
                "description": "Issues an invoice",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "customer_id": {"type": "string"}
                    }
                },
                "initial_state": "collecting_data",
                "terminal_states": ["completed"],
                "states": []
            },
            "auto_spawn": true,
            "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
        }),
        "wf_rules_compile" => serde_json::json!({
            "workflow_name": "invoice",
            "definition": {
                "wf_schema_version": "1",
                "workflow_type": "invoice",
                "description": "Issues an invoice",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "customer_id": {"type": "string"}
                    }
                },
                "initial_state": "collecting_data",
                "terminal_states": ["completed"],
                "states": []
            }
        }),
        "wf_rules_apply" => serde_json::json!({
            "workflow_name": "invoice",
            "auto_spawn": true,
            "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
        }),
        "wf_rules_rollback" => serde_json::json!({
            "workflow_name": "invoice",
            "auto_spawn": true,
            "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
        }),
        "wf_rules_delete" => serde_json::json!({
            "workflow_name": "invoice",
            "force": false
        }),
        "wf_rules_get_workflow" | "wf_rules_get_status" => serde_json::json!({
            "workflow_name": "invoice"
        }),
        "update_tenant" => serde_json::json!({
            "status": "active",
            "sponsor_tenant_id": "tnt:11111111-1111-1111-1111-111111111111"
        }),
        "set_tenant_sponsor" => serde_json::json!({
            "sponsor_tenant_id": serde_json::Value::Null
        }),
        "remove_runtime_version" => serde_json::json!({
            "test_hold_ms": 250
        }),
        _ => serde_json::Value::Null,
    }
}

fn admin_action_example_scmd(action: &str) -> Option<String> {
    let text = match action {
        "list_admin_actions" => "curl -X GET /admin/actions",
        "get_admin_action_help" => "curl -X GET /admin/actions/add_hive",
        "publish_runtime_package" => {
            r#"curl -X POST /admin/runtime-packages/publish -d '{"source":{"kind":"bundle_upload","blob_path":"packages/incoming/ai-support-demo-0.1.0.zip"},"set_current":true,"sync_to":["worker-220"],"update_to":["worker-220"]}'"#
        }
        "hive_status" => "curl -X GET /hive/status",
        "list_hives" => "curl -X GET /hives",
        "get_hive" => "curl -X GET /hives/worker-220",
        "add_hive" => {
            r#"curl -X POST /hives -d '{"hive_id":"worker-220","address":"192.168.8.220"}'"#
        }
        "remove_hive" => "curl -X DELETE /hives/worker-220",
        "list_nodes" => "curl -X GET /hives/motherbee/nodes",
        "get_node_status" => "curl -X GET /hives/motherbee/nodes/SY.admin@motherbee/status",
        "get_node_state" => "curl -X GET /hives/motherbee/nodes/SY.admin@motherbee/state",
        "get_node_config" => "curl -X GET /hives/motherbee/nodes/AI.chat@motherbee/config",
        "run_node" => {
            r#"curl -X POST /hives/motherbee/nodes -d '{"node_name":"AI.chat@motherbee","runtime_version":"current"}'"#
        }
        "start_node" => {
            r#"curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/start -d '{"node_name":"AI.chat@motherbee"}'"#
        }
        "restart_node" => {
            r#"curl -X POST /hives/motherbee/nodes/AI.chat@motherbee/restart -d '{"node_name":"AI.chat@motherbee"}'"#
        }
        "kill_node" => {
            r#"curl -X DELETE /hives/motherbee/nodes/AI.chat@motherbee -d '{"force":false,"purge_instance":true}'"#
        }
        "remove_node_instance" => {
            "curl -X DELETE /hives/motherbee/nodes/AI.chat@motherbee/instance"
        }
        "send_node_message" => {
            r#"curl -X POST /hives/motherbee/nodes/SY.admin@motherbee/messages -d '{"msg_type":"PING","payload":{"ping":true}}'"#
        }
        "node_control_config_get" => {
            r#"curl -X POST /hives/motherbee/nodes/WF.invoice@motherbee/control/config-get -d '{"requested_by":"archi"}'"#
        }
        "node_control_config_set" => {
            r#"curl -X POST /hives/motherbee/nodes/WF.invoice@motherbee/control/config-set -d '{"schema_version":1,"config_version":2,"apply_mode":"replace","config":{"sy_timer_l2_name":"SY.timer@motherbee"}}'"#
        }
        "list_ilks" => "curl -X GET /hives/motherbee/identity/ilks",
        "get_ilk" => {
            "curl -X GET /hives/motherbee/identity/ilks/ilk:550e8400-e29b-41d4-a716-446655440000"
        }
        "update_tenant" => {
            r#"curl -X PUT /hives/motherbee/identity/tenants/tnt:550e8400-e29b-41d4-a716-446655440000 -d '{"status":"active","sponsor_tenant_id":"tnt:11111111-1111-1111-1111-111111111111"}'"#
        }
        "set_tenant_sponsor" => {
            r#"curl -X POST /hives/motherbee/identity/tenants/tnt:550e8400-e29b-41d4-a716-446655440000/sponsor -d '{"sponsor_tenant_id":null}'"#
        }
        "inventory" => "curl -X GET /inventory",
        "list_versions" => "curl -X GET /versions",
        "get_versions" => "curl -X GET /hives/motherbee/versions",
        "list_runtimes" => "curl -X GET /hives/motherbee/runtimes",
        "get_runtime" => "curl -X GET /hives/motherbee/runtimes/ai.chat",
        "remove_runtime_version" => {
            "curl -X DELETE /hives/motherbee/runtimes/ai.chat/versions/1.2.3"
        }
        "list_routes" => "curl -X GET /hives/motherbee/routes",
        "add_route" => {
            r#"curl -X POST /hives/motherbee/routes -d '{"prefix":"AI.chat.","action":"FORWARD","next_hop_hive":"worker-220"}'"#
        }
        "delete_route" => "curl -X DELETE /hives/motherbee/routes/AI.chat.",
        "list_vpns" => "curl -X GET /hives/motherbee/vpns",
        "add_vpn" => {
            r#"curl -X POST /hives/motherbee/vpns -d '{"pattern":"worker-*","vpn_id":220}'"#
        }
        "delete_vpn" => "curl -X DELETE /hives/motherbee/vpns/worker-*",
        "get_storage" => "curl -X GET /config/storage",
        "set_storage" => r#"curl -X PUT /config/storage -d '{"path":"/var/lib/fluxbee"}'"#,
        "list_deployments" => "curl -X GET /deployments",
        "get_deployments" => "curl -X GET /hives/motherbee/deployments",
        "list_drift_alerts" => "curl -X GET /drift-alerts",
        "get_drift_alerts" => "curl -X GET /hives/motherbee/drift-alerts",
        "update" => {
            r#"curl -X POST /hives/motherbee/update -d '{"manifest_hash":"sha256:deadbeef","category":"runtime","manifest_version":42,"runtime":"ai.common","runtime_version":"0.1.2"}'"#
        }
        "sync_hint" => {
            r#"curl -X POST /hives/motherbee/sync-hint -d '{"channel":"blob","wait_for_idle":true,"timeout_ms":30000}'"#
        }
        "opa_get_policy" => "curl -X GET /hives/motherbee/opa/policy",
        "opa_get_status" => "curl -X GET /hives/motherbee/opa/status",
        "timer_help" => "curl -X GET /hives/motherbee/timer/help",
        "timer_get" => "curl -X GET /hives/motherbee/timer/timers/9d8c6f4b-2f4d-4ad4-b5f4-8d8d9d9d0001",
        "timer_list" => {
            "curl -X GET '/hives/motherbee/timer/timers?owner_l2_name=WF.onboarding@motherbee&status_filter=pending&limit=100'"
        }
        "timer_now" => "curl -X GET /hives/motherbee/timer/now",
        "timer_now_in" => {
            r#"curl -X POST /hives/motherbee/timer/now-in -d '{"tz":"America/Argentina/Buenos_Aires"}'"#
        }
        "timer_convert" => {
            r#"curl -X POST /hives/motherbee/timer/convert -d '{"instant_utc_ms":1775771100000,"to_tz":"Europe/Madrid"}'"#
        }
        "timer_parse" => {
            r#"curl -X POST /hives/motherbee/timer/parse -d '{"input":"2026-04-10 18:30","layout":"2006-01-02 15:04","tz":"America/Argentina/Buenos_Aires"}'"#
        }
        "timer_format" => {
            r#"curl -X POST /hives/motherbee/timer/format -d '{"instant_utc_ms":1775771100000,"layout":"2006-01-02 15:04 MST","tz":"America/Argentina/Buenos_Aires"}'"#
        }
        "opa_compile_apply" => {
            r#"curl -X POST /hives/motherbee/opa/policy -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'"#
        }
        "opa_compile" => {
            r#"curl -X POST /hives/motherbee/opa/policy/compile -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'"#
        }
        "opa_apply" => r#"curl -X POST /hives/motherbee/opa/policy/apply -d '{"version":12}'"#,
        "opa_rollback" => {
            r#"curl -X POST /hives/motherbee/opa/policy/rollback -d '{"version":11}'"#
        }
        "opa_check" => {
            r#"curl -X POST /hives/motherbee/opa/policy/check -d '{"rego":"package router\n\ndefault target = null\n","entrypoint":"router/target"}'"#
        }
        "wf_rules_get_workflow" => {
            "curl -X GET '/hives/motherbee/wf-rules?workflow_name=invoice'"
        }
        "wf_rules_get_status" => {
            "curl -X GET '/hives/motherbee/wf-rules/status?workflow_name=invoice'"
        }
        "wf_rules_list_workflows" => "curl -X GET /hives/motherbee/wf-rules",
        "wf_rules_compile_apply" => {
            r#"curl -X POST /hives/motherbee/wf-rules -d '{"workflow_name":"invoice","definition":{"wf_schema_version":"1","workflow_type":"invoice","description":"Issues an invoice","input_schema":{"type":"object","properties":{"customer_id":{"type":"string"}}},"initial_state":"collecting_data","terminal_states":["completed"],"states":[]},"auto_spawn":true,"tenant_id":"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"}'"#
        }
        "wf_rules_compile" => {
            r#"curl -X POST /hives/motherbee/wf-rules/compile -d '{"workflow_name":"invoice","definition":{"wf_schema_version":"1","workflow_type":"invoice","description":"Issues an invoice","input_schema":{"type":"object","properties":{"customer_id":{"type":"string"}}},"initial_state":"collecting_data","terminal_states":["completed"],"states":[]}}'"#
        }
        "wf_rules_apply" => {
            r#"curl -X POST /hives/motherbee/wf-rules/apply -d '{"workflow_name":"invoice","auto_spawn":true,"tenant_id":"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"}'"#
        }
        "wf_rules_rollback" => {
            r#"curl -X POST /hives/motherbee/wf-rules/rollback -d '{"workflow_name":"invoice","auto_spawn":true,"tenant_id":"tnt:43d576a3-d712-4d91-9245-5d5463dd693e"}'"#
        }
        "wf_rules_delete" => {
            r#"curl -X POST /hives/motherbee/wf-rules/delete -d '{"workflow_name":"invoice","force":false}'"#
        }
        _ => return None,
    };
    Some(text.to_string())
}

fn admin_action_request_notes(action: &str) -> Vec<&'static str> {
    match action {
        "publish_runtime_package" => vec![
            "Motherbee-only operation.",
            "Supported source kinds: inline_package and bundle_upload.",
            "bundle_upload resolves blob_path relative to the configured blob root in hive.yaml.",
            "Publishing mutates dist/manifest but does not spawn nodes.",
            "Optional sync_to runs sync_hint(channel=dist) after publish; optional update_to runs update(category=runtime) scoped to the new runtime/version.",
        ],
        "add_hive" | "remove_hive" => vec![
            "Motherbee-only operation.",
            "For HTTP usage the hive id is in the body for add_hive and in the path for remove_hive.",
        ],
        "list_nodes" => vec![
            "This endpoint is hive-scoped only.",
            "For all nodes across the system, use GET /inventory or GET /inventory/summary instead.",
            "For node software versions, use GET /versions or GET /hives/{hive}/versions and map node names to core/runtime entries.",
        ],
        "run_node" => vec![
            "Use this only when creating/spawning a new managed instance.",
            "runtime can be omitted when it is derivable from node_name.",
            "For internal ADMIN_COMMAND dispatch, the hive target is encoded via payload.target; in HTTP it comes from /hives/{hive}.",
        ],
        "start_node" => vec![
            "Use this when the persisted managed node instance already exists and needs to be started again.",
            "This reuses the stored config.json and runtime metadata instead of creating a new instance.",
            "The hive target comes from the /hives/{hive} path in HTTP.",
        ],
        "restart_node" => vec![
            "Use this when the managed node instance already exists and should be restarted.",
            "If the transient systemd unit is missing or inactive, orchestrator can fall back to the start path for the persisted instance.",
            "The hive target comes from the /hives/{hive} path in HTTP.",
        ],
        "kill_node" | "remove_node_instance" | "set_node_config" | "send_node_message" => vec![
            "The hive target comes from the /hives/{hive} path in HTTP.",
            "The node_name path segment should be a fully-qualified Fluxbee node name.",
        ],
        "get_node_config" => vec![
            "The hive target comes from the /hives/{hive} path in HTTP.",
            "The node_name path segment should be a fully-qualified Fluxbee node name.",
            "This reads the stored effective config.json snapshot, not the live node-owned CONFIG_GET control-plane contract.",
            "Use this when you want the persisted node config, for example to inspect prompt/instructions already materialized in config.json.",
            "Use POST /hives/{hive}/nodes/{node_name}/control/config-get only when you need the node-defined live control-plane contract/config response.",
        ],
        "node_control_config_get" => vec![
            "The hive target comes from the /hives/{hive} path in HTTP.",
            "The node_name path segment should be a fully-qualified Fluxbee node name.",
            "This is the canonical live control-plane discovery path for nodes that expose CONFIG_GET, including AI.*, IO.*, WF.*, SY.storage, SY.identity, SY.cognition, and SY.config.routes.",
            "Admin forwards CONFIG_GET over L2 unicast and returns the node's CONFIG_RESPONSE.",
            "Use this when you need the node-defined live contract, dynamic config view, or secret metadata; not when a plain stored config.json read is enough.",
            "The node defines the response contract and config schema; SY.admin only standardizes transport.",
            "For WF.* v1, this returns the live workflow-node contract and effective config view; use GET /hives/{hive}/nodes/{node_name}/config when you want the persisted managed config.json or package metadata instead.",
            "For SY.storage, this is the canonical way to inspect the redacted DB secret contract and its effective source/persistence metadata.",
            "For SY.identity, this is the canonical way to inspect the redacted primary DB secret contract and degraded/bootstrap state.",
            "For SY.cognition, this is the canonical way to inspect semantic_tagger config, degraded_semantics_policy, AI secret redaction, and runtime counters for semantic/narrative processing.",
            "For SY.config.routes, this returns the live effective routes/vpns contract owned by the node, not the legacy admin list/add/delete action surface.",
        ],
        "node_control_config_set" => vec![
            "The hive target comes from the /hives/{hive} path in HTTP.",
            "The node_name path segment should be a fully-qualified Fluxbee node name.",
            "This is the canonical live control-plane mutation path for nodes that expose CONFIG_SET, including AI.*, IO.*, WF.*, SY.storage, SY.identity, SY.cognition, and SY.config.routes.",
            "Admin forwards CONFIG_SET over L2 unicast and returns the node's CONFIG_RESPONSE.",
            "The payload.config object is node-defined and is not interpreted by SY.admin.",
            "For WF.* v1, CONFIG_SET is persist-only and returns restart_required; it does not hot-apply CONFIG_CHANGED.",
            "For WF.* v1, do not mutate _system through CONFIG_SET. Managed package/runtime metadata remains owned by orchestrator.",
            "For SY.storage v1, the canonical secret field is config.database.postgres_url and the apply is persist-only until sy-storage is restarted.",
            "For SY.identity v1, the canonical secret field is config.database.postgres_url and the apply is persist-only until sy-identity is restarted.",
            "For SY.cognition, the canonical AI secret field is config.secrets.openai.api_key and semantic controls live under config.semantic_tagger.*.",
            "For SY.config.routes v1, CONFIG_SET replaces selected sections of the node-owned effective routes/vpns config; use add/delete actions when you want explicit domain mutations instead of a control-plane config replace.",
        ],
        "list_routes" | "list_vpns" => vec![
            "These are SY.config.routes domain read actions exposed as admin queries.",
            "Use them when you want the route/VPN lists in the legacy admin surface.",
            "For the node-owned live config contract, use node_control_config_get against SY.config.routes instead.",
        ],
        "add_route" => vec![
            "These are SY.config.routes domain mutation actions exposed as admin commands.",
            "They mutate one route/VPN rule at a time and preserve the legacy operational surface.",
            "For HTTP delete calls, the identifier lives in the final path segment; internal admin commands may still carry it in payload.",
            "Use node_control_config_set against SY.config.routes only when you want the node-owned live config replace path instead of one explicit domain mutation.",
            "The 'action' field controls how matching traffic is handled. Valid values are FORWARD and DROP only. Case-sensitive.",
            "When action=FORWARD, next_hop_hive is required and must be the exact name of the destination hive.",
            "When action=DROP, next_hop_hive must be omitted.",
            "match_kind defaults to PREFIX when omitted. Valid values: PREFIX, EXACT, GLOB. An empty string is invalid.",
            "priority defaults to 100 when omitted. Higher value means higher priority. Do not pass 0 unless intentionally setting lowest priority.",
            "metric defaults to 0 when omitted. Used only for tie-breaking between routes with the same prefix and priority.",
        ],
        "delete_route" | "add_vpn" | "delete_vpn" => vec![
            "These are SY.config.routes domain mutation actions exposed as admin commands.",
            "They mutate one route/VPN rule at a time and preserve the legacy operational surface.",
            "For HTTP delete calls, the identifier lives in the final path segment; internal admin commands may still carry it in payload.",
            "Use node_control_config_set against SY.config.routes only when you want the node-owned live config replace path instead of one explicit domain mutation.",
        ],
        "timer_help" | "timer_get" | "timer_list" | "timer_now" | "timer_now_in"
        | "timer_convert" | "timer_parse" | "timer_format" => vec![
            "These are narrow SY.timer read-only actions exposed through SY.admin for operator visibility and introspection.",
            "Direct node-to-node SDK usage remains the primary interaction model for SY.timer scheduling and owner-bound mutations.",
            "TIMER_GET and TIMER_LIST are open read operations in v1, so SY.admin may expose them without impersonation or ownership proxying.",
            "Owner-bound mutation verbs such as schedule/cancel/reschedule remain outside SY.admin in v1.",
        ],
        "get_ilk" => vec![
            "The hive target comes from the /hives/{hive} path in HTTP.",
            "The ilk_id path segment must use the prefixed UUID format ilk:<uuid>.",
            "Identity may resolve an old alias ILK to its canonical ILK if an alias mapping exists.",
            "This lookup is by ILK identifier, not by channel address, node name, or tenant.",
        ],
        "update_tenant" => vec![
            "This mutates the tenant record itself, not ILKs that belong to that tenant.",
            "Use TNT_CREATE to create a tenant. Use update_tenant only after the tenant already exists.",
            "Passing sponsor_tenant_id updates the tenant hierarchy. Passing sponsor_tenant_id=null clears sponsorship and makes the tenant a root/default candidate.",
            "Sponsor updates are validated against self-reference and sponsorship cycles.",
        ],
        "set_tenant_sponsor" => vec![
            "Focused identity admin action for changing only the sponsor relationship of a tenant.",
            "Pass sponsor_tenant_id as a prefixed tenant id to assign a sponsor, or null to clear it.",
            "This does not create a tenant and does not modify ILK membership.",
        ],
        "inventory" => vec![
            "Use GET /inventory for the full global inventory view.",
            "Use GET /inventory/summary for global counts only.",
            "Use GET /inventory/{hive} or /hives/{hive}/inventory/hive for one hive.",
        ],
        "list_versions" | "get_versions" => vec![
            "These endpoints report core component versions and runtime availability/current selections.",
            "They describe versions available to a hive, not the state of one node instance.",
            "For SY nodes, map node names to core components: for example SY.identity@motherbee -> core.components['sy-identity'].version.",
            "For runtime-backed nodes, map the node runtime family to runtimes.runtimes[<runtime>].current: for example AI.chat@motherbee -> runtimes.runtimes['ai.chat'].current.",
            "For IO or WF nodes with instance suffixes, use the runtime family/prefix, for example IO.slack.T123@motherbee -> io.slack and WF.blob.consume.diag.x@y -> wf.blob.consume.diag when present.",
            "Use GET /versions for cross-hive comparisons and GET /hives/{hive}/versions for one hive.",
        ],
        "update" => vec![
            "Legacy fields version/hash are rejected; use manifest_version/manifest_hash.",
            "For category=runtime you may optionally scope the update with runtime + runtime_version.",
            "Scoped runtime update uses targeted readiness for that runtime/version and no longer blocks on unrelated missing runtimes in the same manifest.",
            "The response still includes global_runtime_health so operators can see overall runtime drift without turning it into a hard blocker for the targeted deploy.",
            "Use GET /hives/{hive}/runtimes/{runtime} to inspect readiness/materialization for one runtime before or after update.",
        ],
        "sync_hint" => vec![
            "Defaults are channel=blob, wait_for_idle=true, timeout_ms=30000.",
        ],
        "opa_compile_apply" | "opa_compile" | "opa_check" => vec![
            "rego is required for compile/check flows.",
            "entrypoint defaults to router/target when omitted.",
        ],
        "opa_apply" => vec![
            "version is required for apply.",
        ],
        "opa_rollback" => vec![
            "If version is omitted, rollback uses the implementation default (currently 0).",
        ],
        "wf_rules_compile_apply" => vec![
            "workflow_name and definition are required.",
            "workflow_name must match ^[a-z][a-z0-9-]*(\\.[a-z][a-z0-9-]*)*$ — lowercase letters, digits, hyphens, and dot-separated segments only. Underscores are not allowed.",
            "SY.admin forwards this to SY.wf-rules using the node CONFIG_SET contract.",
            "tenant_id is required on the first deploy when auto_spawn=true and no managed WF node exists yet.",
        ],
        "wf_rules_compile" => vec![
            "workflow_name and definition are required.",
            "workflow_name must match ^[a-z][a-z0-9-]*(\\.[a-z][a-z0-9-]*)*$ — lowercase letters, digits, hyphens, and dot-separated segments only. Underscores are not allowed.",
            "SY.admin forwards this to SY.wf-rules using the node CONFIG_SET contract.",
            "compile validates and stages only; it never deploys, so tenant_id is not relevant here.",
        ],
        "wf_rules_apply" | "wf_rules_rollback" => vec![
            "workflow_name is required.",
            "SY.admin forwards this to SY.wf-rules using the node CONFIG_SET contract.",
            "tenant_id is only needed for first deploy when auto_spawn=true; existing-node rollout preserves the managed tenant_id.",
        ],
        "wf_rules_delete" => vec![
            "workflow_name is required.",
            "Delete is a direct command to SY.wf-rules, not a node CONFIG_SET operation.",
        ],
        "wf_rules_get_workflow" | "wf_rules_get_status" | "wf_rules_list_workflows" => vec![
            "These are direct query operations against SY.wf-rules.",
            "GET /hives/{hive}/wf-rules without workflow_name lists workflows; with workflow_name it returns the current definition.",
        ],
        "set_storage" => vec![
            "This persists the local storage path in hive.yaml on the target hive.",
        ],
        _ => vec![],
    }
}

fn admin_action_path_param(name: &str, value_type: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "type": value_type,
        "description": description,
    })
}

fn admin_action_body_field(name: &str, value_type: &str, description: &str) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "type": value_type,
        "description": description,
    })
}

fn runtime_package_staging_root(state_dir: &Path) -> PathBuf {
    state_dir.join("runtime-package-staging")
}

fn runtime_package_safe_rel_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("inline package contains empty file path".to_string());
    }
    let rel = PathBuf::from(trimmed);
    if rel.is_absolute() {
        return Err(format!(
            "inline package file path must be relative (got '{}')",
            trimmed
        ));
    }
    if rel
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "inline package file path must not contain '..' (got '{}')",
            trimmed
        ));
    }
    Ok(rel)
}

fn runtime_package_safe_blob_rel_path(raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("bundle_upload requires a non-empty blob_path".to_string());
    }
    let rel = PathBuf::from(trimmed);
    if rel.is_absolute() {
        return Err(format!(
            "bundle_upload blob_path must be relative to blob root (got '{}')",
            trimmed
        ));
    }
    if rel
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!(
            "bundle_upload blob_path must not contain '..' (got '{}')",
            trimmed
        ));
    }
    Ok(rel)
}

fn normalize_publish_follow_up_hives(
    raw: &[String],
    field_name: &str,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for value in raw {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(format!("{field_name} must not contain empty hive ids"));
        }
        if seen.insert(trimmed.to_string()) {
            out.push(trimmed.to_string());
        }
    }
    Ok(out)
}

fn materialize_inline_runtime_package(
    files: &BTreeMap<String, String>,
    staging_root: &Path,
) -> Result<PathBuf, String> {
    if files.is_empty() {
        return Err("inline package requires at least one file".to_string());
    }
    let package_root = staging_root.join(format!("inline-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(&package_root).map_err(|err| {
        format!(
            "failed to create inline package staging dir '{}': {}",
            package_root.display(),
            err
        )
    })?;

    for (raw_path, contents) in files {
        let rel = runtime_package_safe_rel_path(raw_path)?;
        let full_path = package_root.join(&rel);
        let Some(parent) = full_path.parent() else {
            let _ = fs::remove_dir_all(&package_root);
            return Err(format!(
                "inline package file path has no parent '{}'",
                raw_path
            ));
        };
        fs::create_dir_all(parent).map_err(|err| {
            let _ = fs::remove_dir_all(&package_root);
            format!(
                "failed to create inline package parent '{}' for '{}': {}",
                parent.display(),
                raw_path,
                err
            )
        })?;
        fs::write(&full_path, contents).map_err(|err| {
            let _ = fs::remove_dir_all(&package_root);
            format!(
                "failed to write inline package file '{}' : {}",
                full_path.display(),
                err
            )
        })?;
    }

    if !package_root.join("package.json").is_file() {
        let _ = fs::remove_dir_all(&package_root);
        return Err("inline package must include package.json at package root".to_string());
    }

    Ok(package_root)
}

fn materialize_bundle_runtime_package(
    blob_root: &Path,
    blob_path: &str,
    staging_root: &Path,
) -> Result<(PathBuf, PathBuf), String> {
    let rel_blob_path = runtime_package_safe_blob_rel_path(blob_path)?;
    let bundle_path = blob_root.join(&rel_blob_path);
    if !bundle_path.is_file() {
        return Err(format!("bundle blob not found '{}'", bundle_path.display()));
    }

    let extraction_root = staging_root.join(format!("bundle-{}", Uuid::new_v4().simple()));
    fs::create_dir_all(&extraction_root).map_err(|err| {
        format!(
            "failed to create bundle staging dir '{}': {}",
            extraction_root.display(),
            err
        )
    })?;

    let archive_file = match fs::File::open(&bundle_path) {
        Ok(file) => file,
        Err(err) => {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(format!(
                "failed to open bundle zip '{}' : {}",
                bundle_path.display(),
                err
            ));
        }
    };
    let mut archive = match ZipArchive::new(archive_file) {
        Ok(archive) => archive,
        Err(err) => {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(format!(
                "invalid zip at '{}' : {}",
                bundle_path.display(),
                err
            ));
        }
    };

    let mut root_dir_name: Option<String> = None;
    for index in 0..archive.len() {
        let mut entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(err) => {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err(format!(
                    "failed to read zip entry #{index} from '{}' : {}",
                    bundle_path.display(),
                    err
                ));
            }
        };

        let enclosed = match entry.enclosed_name().map(|path| path.to_path_buf()) {
            Some(path) if !path.as_os_str().is_empty() => path,
            _ => {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err(format!(
                    "bundle_upload zip contains unsafe entry paths '{}'",
                    bundle_path.display()
                ));
            }
        };

        let mut components = enclosed.components();
        let first = match components.next() {
            Some(std::path::Component::Normal(component)) => {
                component.to_string_lossy().to_string()
            }
            _ => {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err("bundle_upload zip must contain a single root directory".to_string());
            }
        };
        if let Some(expected) = root_dir_name.as_deref() {
            if expected != first {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err("bundle_upload zip must contain exactly one root directory".to_string());
            }
        } else {
            root_dir_name = Some(first);
        }

        if !entry.is_dir() && components.as_path().as_os_str().is_empty() {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(
                "bundle_upload zip must contain files under a single root directory".to_string(),
            );
        }

        let output_path = extraction_root.join(&enclosed);
        if entry.is_dir() {
            if let Err(err) = fs::create_dir_all(&output_path) {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err(format!(
                    "failed to create extracted directory '{}' : {}",
                    output_path.display(),
                    err
                ));
            }
            continue;
        }

        let Some(parent) = output_path.parent() else {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(format!(
                "failed to resolve parent directory for extracted file '{}'",
                output_path.display()
            ));
        };
        if let Err(err) = fs::create_dir_all(parent) {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(format!(
                "failed to create extracted parent '{}' : {}",
                parent.display(),
                err
            ));
        }

        let mut output_file = match fs::File::create(&output_path) {
            Ok(file) => file,
            Err(err) => {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err(format!(
                    "failed to create extracted file '{}' : {}",
                    output_path.display(),
                    err
                ));
            }
        };
        if let Err(err) = io::copy(&mut entry, &mut output_file) {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(format!(
                "failed to extract zip entry '{}' : {}",
                output_path.display(),
                err
            ));
        }
        if let Some(mode) = entry.unix_mode() {
            if let Err(err) = fs::set_permissions(&output_path, fs::Permissions::from_mode(mode)) {
                let _ = fs::remove_dir_all(&extraction_root);
                return Err(format!(
                    "failed to set extracted file permissions '{}' : {}",
                    output_path.display(),
                    err
                ));
            }
        }
    }

    let Some(root_dir_name) = root_dir_name else {
        let _ = fs::remove_dir_all(&extraction_root);
        return Err("bundle_upload zip is empty".to_string());
    };
    let package_root = extraction_root.join(root_dir_name);
    if !package_root.join("package.json").is_file() {
        let _ = fs::remove_dir_all(&extraction_root);
        return Err(
            "bundle_upload zip must contain package.json at the root directory".to_string(),
        );
    }

    Ok((package_root, bundle_path))
}

fn publish_runtime_package_inline(
    package_files: &BTreeMap<String, String>,
    staging_root: &Path,
    dist_root: &Path,
    manifest_path: &Path,
    set_current: bool,
) -> Result<serde_json::Value, String> {
    let package_root = materialize_inline_runtime_package(package_files, staging_root)?;
    let validated = match validate_package(&package_root, None) {
        Ok(validated) => validated,
        Err(err) => {
            let _ = fs::remove_dir_all(&package_root);
            return Err(err);
        }
    };
    let install = match install_validated_package_with_options(
        &validated,
        dist_root,
        manifest_path,
        set_current,
    ) {
        Ok(install) => install,
        Err(err) => {
            let _ = fs::remove_dir_all(&package_root);
            return Err(err);
        }
    };
    let _ = fs::remove_dir_all(&package_root);
    Ok(serde_json::json!({
        "source_kind": "inline_package",
        "runtime_name": install.runtime_name,
        "runtime_version": install.runtime_version,
        "package_type": install.package_type,
        "installed_path": install.installed_path.display().to_string(),
        "manifest_path": install.manifest_path.display().to_string(),
        "manifest_version": install.manifest_version,
        "copied_files": install.copied_files,
        "copied_bytes": install.copied_bytes,
    }))
}

fn publish_runtime_package_bundle(
    blob_root: &Path,
    blob_path: &str,
    staging_root: &Path,
    dist_root: &Path,
    manifest_path: &Path,
    set_current: bool,
) -> Result<serde_json::Value, String> {
    let (package_root, bundle_file) =
        materialize_bundle_runtime_package(blob_root, blob_path, staging_root)?;
    let extraction_root = package_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| package_root.clone());
    let validated = match validate_package(&package_root, None) {
        Ok(validated) => validated,
        Err(err) => {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(err);
        }
    };
    let install = match install_validated_package_with_options(
        &validated,
        dist_root,
        manifest_path,
        set_current,
    ) {
        Ok(install) => install,
        Err(err) => {
            let _ = fs::remove_dir_all(&extraction_root);
            return Err(err);
        }
    };
    let _ = fs::remove_dir_all(&extraction_root);
    let _ = fs::remove_file(&bundle_file);
    Ok(serde_json::json!({
        "source_kind": "bundle_upload",
        "blob_path": blob_path,
        "runtime_name": install.runtime_name,
        "runtime_version": install.runtime_version,
        "package_type": install.package_type,
        "installed_path": install.installed_path.display().to_string(),
        "manifest_path": install.manifest_path.display().to_string(),
        "manifest_version": install.manifest_version,
        "copied_files": install.copied_files,
        "copied_bytes": install.copied_bytes,
    }))
}

fn local_runtime_manifest_meta_from_file(path: &Path) -> Result<(u64, String), String> {
    let raw = fs::read(path).map_err(|err| {
        format!(
            "failed to read runtime manifest '{}' for follow-up: {}",
            path.display(),
            err
        )
    })?;
    let manifest_json: serde_json::Value = serde_json::from_slice(&raw).map_err(|err| {
        format!(
            "failed to parse runtime manifest '{}' for follow-up: {}",
            path.display(),
            err
        )
    })?;
    let version = manifest_json
        .get("version")
        .and_then(|value| value.as_u64())
        .or_else(|| {
            manifest_json
                .get("version")
                .and_then(|value| value.as_str())
                .and_then(|value| value.parse::<u64>().ok())
        })
        .ok_or_else(|| {
            format!(
                "runtime manifest '{}' missing numeric version for follow-up",
                path.display()
            )
        })?;
    let mut hasher = Sha256::new();
    hasher.update(&raw);
    Ok((version, format!("{:x}", hasher.finalize())))
}

fn valid_runtime_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

fn sanitize_runtime_suffix(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

fn load_local_runtime_manifest() -> Result<Option<RuntimeManifest>, String> {
    load_runtime_manifest_from_paths(&[PathBuf::from(DIST_RUNTIME_MANIFEST_PATH)])
}

fn runtime_manifest_entry_map_local(
    manifest: &RuntimeManifest,
) -> Result<BTreeMap<String, RuntimeManifestEntry>, String> {
    let runtimes = manifest
        .runtimes
        .as_object()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let mut out = BTreeMap::new();
    for (runtime, value) in runtimes {
        if !valid_runtime_token(runtime) {
            return Err(format!(
                "runtime manifest invalid runtime name '{}'",
                runtime
            ));
        }
        let entry = serde_json::from_value::<RuntimeManifestEntry>(value.clone())
            .map_err(|err| format!("runtime manifest invalid entry for '{}': {}", runtime, err))?;
        out.insert(runtime.to_string(), entry);
    }
    Ok(out)
}

fn runtime_versions_from_manifest_entry_local(entry: &RuntimeManifestEntry) -> Vec<String> {
    let mut out: Vec<String> = entry
        .available
        .iter()
        .map(|value| value.trim())
        .filter(|value| valid_runtime_token(value))
        .map(|value| value.to_string())
        .collect();
    if let Some(current) = entry
        .current
        .as_deref()
        .map(str::trim)
        .filter(|value| valid_runtime_token(value))
    {
        if !out.iter().any(|value| value == current) {
            out.push(current.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

fn can_remove_current_runtime_version_local(
    entry: &RuntimeManifestEntry,
    runtime_version: &str,
) -> bool {
    let current = entry.current.as_deref().map(str::trim).unwrap_or("");
    if current.is_empty() || current != runtime_version {
        return false;
    }
    runtime_versions_from_manifest_entry_local(entry) == vec![runtime_version.to_string()]
}

fn runtime_manifest_case_variants_local(
    runtimes: &BTreeMap<String, RuntimeManifestEntry>,
    runtime: &str,
) -> Vec<String> {
    runtimes
        .keys()
        .filter(|candidate| candidate.eq_ignore_ascii_case(runtime.trim()))
        .cloned()
        .collect()
}

fn resolve_runtime_key_local(
    runtimes: &BTreeMap<String, RuntimeManifestEntry>,
    runtime: &str,
) -> Result<String, String> {
    let trimmed = runtime.trim();
    if trimmed.is_empty() {
        return Err("missing runtime".to_string());
    }
    let variants = runtime_manifest_case_variants_local(runtimes, trimmed);
    match variants.len() {
        0 => Err(format!("runtime '{}' not found", trimmed)),
        1 => Ok(variants[0].clone()),
        _ => Err(format!(
            "runtime '{}' is ambiguous by case in manifest; variants={:?}",
            trimmed, variants
        )),
    }
}

fn runtime_dependents_summary_local(
    manifest: &RuntimeManifest,
    runtime: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let runtimes = runtime_manifest_entry_map_local(manifest)?;
    let mut dependents = Vec::new();
    for (name, entry) in runtimes {
        if !entry
            .runtime_base
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case(runtime))
        {
            continue;
        }
        dependents.push(serde_json::json!({
            "runtime": name,
            "current": entry.current,
            "available": runtime_versions_from_manifest_entry_local(&entry),
            "type": entry.package_type,
            "runtime_base": entry.runtime_base,
        }));
    }
    dependents.sort_by(|a, b| {
        let a_runtime = a
            .get("runtime")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let b_runtime = b
            .get("runtime")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        a_runtime.cmp(b_runtime)
    });
    Ok(dependents)
}

fn next_runtime_manifest_version_ms(now_ms: u64, current_manifest_version: u64) -> u64 {
    if now_ms > current_manifest_version {
        now_ms
    } else {
        current_manifest_version + 1
    }
}

fn remove_runtime_version_from_manifest_local(
    manifest: &RuntimeManifest,
    runtime: &str,
    runtime_version: &str,
) -> Result<RuntimeManifest, String> {
    let mut updated = manifest.clone();
    let runtime_map = runtime_manifest_entry_map_local(&updated)?;
    let runtime_key = resolve_runtime_key_local(&runtime_map, runtime)?;
    let runtime_entries = updated
        .runtimes
        .as_object_mut()
        .ok_or_else(|| "runtime manifest invalid: runtimes must be object".to_string())?;
    let entry_value = runtime_entries
        .get(&runtime_key)
        .cloned()
        .ok_or_else(|| format!("runtime '{}' not found", runtime_key))?;
    let mut entry = serde_json::from_value::<RuntimeManifestEntry>(entry_value).map_err(|err| {
        format!(
            "runtime manifest invalid entry for '{}': {}",
            runtime_key, err
        )
    })?;

    let current = entry.current.as_deref().map(str::trim).unwrap_or("");
    if runtime_version == "current" {
        return Err("RUNTIME_CURRENT_CONFLICT".to_string());
    }
    if !current.is_empty()
        && current == runtime_version
        && !can_remove_current_runtime_version_local(&entry, runtime_version)
    {
        return Err("RUNTIME_CURRENT_CONFLICT".to_string());
    }

    let version_exists = runtime_versions_from_manifest_entry_local(&entry)
        .iter()
        .any(|value| value == runtime_version);
    if !version_exists {
        return Err(format!(
            "version '{}' not available for runtime '{}'",
            runtime_version, runtime
        ));
    }

    entry
        .available
        .retain(|value| value.trim() != runtime_version);
    if current == runtime_version {
        entry.current = None;
    }
    if entry.available.is_empty() && entry.current.is_none() {
        runtime_entries.remove(&runtime_key);
    } else {
        runtime_entries.insert(
            runtime_key.clone(),
            serde_json::to_value(entry).map_err(|err| {
                format!(
                    "runtime manifest remove failed: serialize runtime '{}' failed: {}",
                    runtime_key, err
                )
            })?,
        );
    }

    let now_ms_i64 = Utc::now().timestamp_millis();
    let now_ms = u64::try_from(now_ms_i64).map_err(|_| {
        format!(
            "runtime manifest remove failed: invalid clock value {}",
            now_ms_i64
        )
    })?;
    updated.version = next_runtime_manifest_version_ms(now_ms, updated.version);
    updated.updated_at = Some(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true));
    updated.hash = None;
    Ok(updated)
}

fn quarantine_runtime_version_dir_local(
    runtime: &str,
    runtime_version: &str,
) -> Result<Option<(PathBuf, PathBuf)>, String> {
    let runtime_dir = Path::new(DIST_RUNTIME_ROOT_DIR).join(runtime);
    let version_dir = runtime_dir.join(runtime_version);
    if !version_dir.exists() {
        return Ok(None);
    }
    let quarantine = runtime_dir.join(format!(
        ".removing-{}-{}",
        sanitize_runtime_suffix(runtime_version),
        Uuid::new_v4().simple()
    ));
    fs::rename(&version_dir, &quarantine).map_err(|err| {
        format!(
            "failed to quarantine runtime version dir '{}' -> '{}': {}",
            version_dir.display(),
            quarantine.display(),
            err
        )
    })?;
    Ok(Some((version_dir, quarantine)))
}

fn restore_quarantined_runtime_version_dir_local(
    original: &Path,
    quarantine: &Path,
) -> Result<(), String> {
    fs::rename(quarantine, original).map_err(|err| {
        format!(
            "failed to restore quarantined runtime version dir '{}' -> '{}': {}",
            quarantine.display(),
            original.display(),
            err
        )
    })
}

fn finalize_quarantined_runtime_version_dir_local(
    runtime: &str,
    quarantine: &Path,
) -> Result<bool, String> {
    fs::remove_dir_all(quarantine).map_err(|err| {
        format!(
            "failed to remove quarantined runtime version dir '{}': {}",
            quarantine.display(),
            err
        )
    })?;
    let runtime_dir = Path::new(DIST_RUNTIME_ROOT_DIR).join(runtime);
    let runtime_dir_empty = runtime_dir
        .read_dir()
        .map(|mut read| read.next().is_none())
        .unwrap_or(false);
    if runtime_dir_empty {
        fs::remove_dir(&runtime_dir).map_err(|err| {
            format!(
                "failed to remove empty runtime dir '{}': {}",
                runtime_dir.display(),
                err
            )
        })?;
        return Ok(true);
    }
    Ok(false)
}

async fn query_runtime_usage_global_visible(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    runtime: &str,
    runtime_version: &str,
) -> Result<serde_json::Value, String> {
    let request = build_admin_request(
        ctx,
        "get_runtime",
        serde_json::json!({
            "runtime": runtime,
            "runtime_version": runtime_version,
        }),
        Some(ctx.hive_id.clone()),
    );
    let payload = send_admin_request(client, request, admin_action_timeout("get_runtime"))
        .await
        .map_err(|err| err.to_string())?;
    if !payload_is_ok(&payload) {
        return Err(payload_error_detail(&payload)
            .unwrap_or_else(|| "failed to query runtime usage".to_string()));
    }
    Ok(payload
        .get("usage_global_visible")
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "scope": "global_visible",
                "status": "ok",
                "in_use": false,
                "running_count": 0,
                "running_nodes": [],
            })
        }))
}

fn parse_admin_envelope(body: &str, action: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(body).map_err(|err| {
        format!(
            "failed to parse {action} response envelope from local handler: {} body={}",
            err, body
        )
    })
}

async fn run_publish_runtime_package_follow_up(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    runtime_name: &str,
    runtime_version: &str,
    manifest_version: u64,
    manifest_hash: &str,
    sync_to: &[String],
    update_to: &[String],
) -> Result<(u16, String, serde_json::Value), AdminError> {
    let mut sync_results = Vec::new();
    let mut update_results = Vec::new();
    let mut overall_http_status = 200u16;
    let mut overall_status = "ok".to_string();

    for hive in sync_to {
        let (http_status, body) = handle_hive_sync_hint_command(
            ctx,
            client,
            hive.clone(),
            serde_json::json!({
                "channel": "dist",
                "wait_for_idle": true,
                "timeout_ms": 30_000u64,
            }),
        )
        .await?;
        let envelope = parse_admin_envelope(&body, "sync_hint")?;
        let step_status = envelope
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("error");
        if step_status == "error" && overall_status != "error" {
            overall_status = "error".to_string();
            overall_http_status = http_status;
        } else if step_status == "sync_pending" && overall_status == "ok" {
            overall_status = "sync_pending".to_string();
            overall_http_status = 202;
        }
        sync_results.push(serde_json::json!({
            "hive": hive,
            "http_status": http_status,
            "response": envelope,
        }));
    }

    for hive in update_to {
        let (http_status, body) = handle_hive_update_command(
            ctx,
            client,
            hive.clone(),
            serde_json::json!({
                "category": "runtime",
                "manifest_version": manifest_version,
                "manifest_hash": manifest_hash,
                "runtime": runtime_name,
                "runtime_version": runtime_version,
            }),
        )
        .await?;
        let envelope = parse_admin_envelope(&body, "update")?;
        let step_status = envelope
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("error");
        if step_status == "error" && overall_status != "error" {
            overall_status = "error".to_string();
            overall_http_status = http_status;
        } else if step_status == "sync_pending" && overall_status == "ok" {
            overall_status = "sync_pending".to_string();
            overall_http_status = 202;
        }
        update_results.push(serde_json::json!({
            "hive": hive,
            "http_status": http_status,
            "response": envelope,
        }));
    }

    Ok((
        overall_http_status,
        overall_status,
        serde_json::json!({
            "requested": {
                "sync_to": sync_to,
                "update_to": update_to,
            },
            "sync_hint": sync_results,
            "update": update_results,
        }),
    ))
}

async fn handle_publish_runtime_package(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    if ctx.hive_id != PRIMARY_HIVE_ID {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "publish_runtime_package",
                "payload": serde_json::Value::Null,
                "error_code": "NOT_PRIMARY",
                "error_detail": "publish_runtime_package is motherbee-only",
            })
            .to_string(),
        ));
    }

    let req: PublishRuntimePackageRequest = match serde_json::from_value(payload) {
        Ok(req) => req,
        Err(err) => {
            return Ok((
                400,
                serde_json::json!({
                    "status": "error",
                    "action": "publish_runtime_package",
                    "payload": serde_json::Value::Null,
                    "error_code": "INVALID_REQUEST",
                    "error_detail": format!("invalid publish runtime package request: {err}"),
                })
                .to_string(),
            ))
        }
    };

    let manifest_path = PathBuf::from(DIST_RUNTIME_MANIFEST_PATH);
    if !manifest_path.exists() {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "publish_runtime_package",
                "payload": serde_json::Value::Null,
                "error_code": "RUNTIME_NOT_AVAILABLE",
                "error_detail": format!(
                    "runtime manifest missing at '{}' (initialize dist first)",
                    manifest_path.display()
                ),
            })
            .to_string(),
        ));
    }

    let staging_root = runtime_package_staging_root(&ctx.state_dir);
    fs::create_dir_all(&staging_root)?;
    let sync_to = match normalize_publish_follow_up_hives(&req.sync_to, "sync_to") {
        Ok(hives) => hives,
        Err(err) => {
            return Ok((
                400,
                serde_json::json!({
                    "status": "error",
                    "action": "publish_runtime_package",
                    "payload": serde_json::Value::Null,
                    "error_code": "INVALID_REQUEST",
                    "error_detail": err,
                })
                .to_string(),
            ))
        }
    };
    let update_to = match normalize_publish_follow_up_hives(&req.update_to, "update_to") {
        Ok(hives) => hives,
        Err(err) => {
            return Ok((
                400,
                serde_json::json!({
                    "status": "error",
                    "action": "publish_runtime_package",
                    "payload": serde_json::Value::Null,
                    "error_code": "INVALID_REQUEST",
                    "error_detail": err,
                })
                .to_string(),
            ))
        }
    };

    let set_current = req.set_current.unwrap_or(true);
    let publish_result = match &req.source {
        PublishRuntimePackageSource::InlinePackage { files } => publish_runtime_package_inline(
            files,
            &staging_root,
            Path::new(DIST_RUNTIME_ROOT_DIR),
            &manifest_path,
            set_current,
        ),
        PublishRuntimePackageSource::BundleUpload { blob_path } => publish_runtime_package_bundle(
            &ctx.blob_root,
            blob_path,
            &staging_root,
            Path::new(DIST_RUNTIME_ROOT_DIR),
            &manifest_path,
            set_current,
        ),
    };

    match publish_result {
        Ok(mut result) => {
            let (manifest_version, manifest_hash) =
                match local_runtime_manifest_meta_from_file(&manifest_path) {
                    Ok(meta) => meta,
                    Err(err) => {
                        return Ok((
                            502,
                            serde_json::json!({
                                "status": "error",
                                "action": "publish_runtime_package",
                                "payload": serde_json::Value::Null,
                                "error_code": "SERVICE_FAILED",
                                "error_detail": err,
                            })
                            .to_string(),
                        ))
                    }
                };
            let runtime_name = result
                .get("runtime_name")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            let runtime_version = result
                .get("runtime_version")
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some(obj) = result.as_object_mut() {
                obj.insert(
                    "manifest_version".to_string(),
                    serde_json::json!(manifest_version),
                );
                obj.insert(
                    "manifest_hash".to_string(),
                    serde_json::json!(manifest_hash),
                );
            }

            let (http_status, status, follow_up) = run_publish_runtime_package_follow_up(
                ctx,
                client,
                &runtime_name,
                &runtime_version,
                manifest_version,
                &manifest_hash,
                &sync_to,
                &update_to,
            )
            .await?;
            if let Some(obj) = result.as_object_mut() {
                obj.insert("follow_up".to_string(), follow_up);
            }

            let error_detail = if status == "error" {
                serde_json::json!("publish succeeded but one or more follow-up operations failed")
            } else {
                serde_json::Value::Null
            };
            Ok((
                http_status,
                serde_json::json!({
                    "status": status,
                    "action": "publish_runtime_package",
                    "payload": result,
                    "error_code": if status == "error" { serde_json::json!("SERVICE_FAILED") } else { serde_json::Value::Null },
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
        Err(err) => {
            let error_code = if err.contains("bundle blob not found") {
                "BLOB_NOT_FOUND"
            } else if err.contains("invalid zip") {
                "INVALID_ZIP"
            } else if err.contains("single root directory")
                || err.contains("package.json at the root directory")
            {
                "INVALID_PACKAGE_LAYOUT"
            } else if err.contains("install conflict:") || err.contains("different contents") {
                "VERSION_CONFLICT"
            } else if err.contains("already exists") {
                "VERSION_MISMATCH"
            } else if err.contains("package.json invalid")
                || err.contains("package validation failed")
                || err.contains("inline package")
                || err.contains("bundle_upload")
            {
                "INVALID_REQUEST"
            } else {
                "SERVICE_FAILED"
            };
            Ok((
                error_code_to_http_status(error_code),
                serde_json::json!({
                    "status": "error",
                    "action": "publish_runtime_package",
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": err,
                })
                .to_string(),
            ))
        }
    }
}

async fn handle_remove_runtime_version(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    let target_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    if ctx.hive_id != PRIMARY_HIVE_ID || target_hive != PRIMARY_HIVE_ID {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "NOT_PRIMARY",
                "error_detail": "remove_runtime_version is motherbee-only",
            })
            .to_string(),
        ));
    }

    let runtime = payload
        .get("runtime")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if runtime.is_empty() || !valid_runtime_token(runtime) {
        return Ok((
            400,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": "missing or invalid runtime",
            })
            .to_string(),
        ));
    }
    let runtime_version = payload
        .get("runtime_version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    if runtime_version.is_empty() || !valid_runtime_token(runtime_version) {
        return Ok((
            400,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": "missing or invalid runtime_version",
            })
            .to_string(),
        ));
    }
    if let Some(hold_ms) = payload
        .get("test_hold_ms")
        .and_then(|value| value.as_u64())
        .filter(|value| *value > 0)
    {
        tracing::info!(
            runtime = runtime,
            runtime_version = runtime_version,
            test_hold_ms = hold_ms,
            "runtime delete diagnostic hold active"
        );
        time::sleep(Duration::from_millis(hold_ms)).await;
    }

    let manifest = match load_local_runtime_manifest() {
        Ok(Some(manifest)) => manifest,
        Ok(None) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "RUNTIME_MANIFEST_MISSING",
                    "error_detail": "runtime manifest not found",
                })
                .to_string(),
            ));
        }
        Err(err) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "MANIFEST_INVALID",
                    "error_detail": err,
                })
                .to_string(),
            ));
        }
    };

    let runtime_map = match runtime_manifest_entry_map_local(&manifest) {
        Ok(map) => map,
        Err(err) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "MANIFEST_INVALID",
                    "error_detail": err,
                })
                .to_string(),
            ));
        }
    };
    let runtime_key = match resolve_runtime_key_local(&runtime_map, runtime) {
        Ok(value) => value,
        Err(_) => {
            return Ok((
                404,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "RUNTIME_NOT_FOUND",
                    "error_detail": format!("runtime '{}' not found", runtime),
                })
                .to_string(),
            ));
        }
    };
    let Some(entry) = runtime_map.get(&runtime_key) else {
        return Ok((
            404,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "RUNTIME_NOT_FOUND",
                "error_detail": format!("runtime '{}' not found", runtime),
            })
            .to_string(),
        ));
    };

    let current = entry.current.as_deref().map(str::trim).unwrap_or("");
    if runtime_version == "current" {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "RUNTIME_CURRENT_CONFLICT",
                "error_detail": format!(
                    "cannot delete current version '{}' for runtime '{}'",
                    current, runtime
                ),
            })
            .to_string(),
        ));
    }
    if !current.is_empty()
        && current == runtime_version
        && !can_remove_current_runtime_version_local(entry, runtime_version)
    {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "RUNTIME_CURRENT_CONFLICT",
                "error_detail": format!(
                    "cannot delete current version '{}' for runtime '{}'",
                    current, runtime
                ),
            })
            .to_string(),
        ));
    }

    let version_exists = runtime_versions_from_manifest_entry_local(entry)
        .iter()
        .any(|value| value == runtime_version);
    if !version_exists {
        return Ok((
            404,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "RUNTIME_VERSION_NOT_FOUND",
                "error_detail": format!(
                    "version '{}' not available for runtime '{}'",
                    runtime_version, runtime
                ),
            })
            .to_string(),
        ));
    }

    let usage_global = match query_runtime_usage_global_visible(
        ctx,
        client,
        &runtime_key,
        runtime_version,
    )
    .await
    {
        Ok(value) => value,
        Err(err) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "RUNTIME_REMOVE_FAILED",
                    "error_detail": format!("failed to resolve visible runtime usage before delete: {}", err),
                })
                .to_string(),
            ));
        }
    };
    let running_nodes = usage_global
        .get("running_nodes")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if !running_nodes.is_empty() {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": {
                    "usage_global_visible": usage_global,
                },
                "error_code": "RUNTIME_IN_USE",
                "error_detail": format!(
                    "runtime '{}' version '{}' is still running in visible inventory",
                    runtime, runtime_version
                ),
            })
            .to_string(),
        ));
    }

    let dependents = match runtime_dependents_summary_local(&manifest, &runtime_key) {
        Ok(value) => value,
        Err(err) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "MANIFEST_INVALID",
                    "error_detail": err,
                })
                .to_string(),
            ));
        }
    };
    if !dependents.is_empty() {
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": {
                    "dependents": dependents,
                },
                "error_code": "RUNTIME_HAS_DEPENDENTS",
                "error_detail": format!(
                    "runtime '{}' has published dependents; remove dependent runtimes first",
                    runtime
                ),
            })
            .to_string(),
        ));
    }

    let updated_manifest = match remove_runtime_version_from_manifest_local(
        &manifest,
        &runtime_key,
        runtime_version,
    ) {
        Ok(value) => value,
        Err(err) if err == "RUNTIME_CURRENT_CONFLICT" => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "RUNTIME_CURRENT_CONFLICT",
                    "error_detail": format!(
                        "cannot delete current version '{}' for runtime '{}'",
                        runtime_version, runtime
                    ),
                })
                .to_string(),
            ));
        }
        Err(err) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "RUNTIME_REMOVE_FAILED",
                    "error_detail": err,
                })
                .to_string(),
            ));
        }
    };

    let quarantined = match quarantine_runtime_version_dir_local(&runtime_key, runtime_version) {
        Ok(value) => value,
        Err(err) => {
            return Ok((
                409,
                serde_json::json!({
                    "status": "error",
                    "action": "remove_runtime_version",
                    "payload": serde_json::Value::Null,
                    "error_code": "RUNTIME_REMOVE_FAILED",
                    "error_detail": err,
                })
                .to_string(),
            ));
        }
    };

    let manifest_path = Path::new(DIST_RUNTIME_MANIFEST_PATH);
    let write_v2_gate_enabled = runtime_manifest_write_v2_gate_enabled_from_env();
    if let Err(err) =
        write_runtime_manifest_file_atomic(manifest_path, &updated_manifest, write_v2_gate_enabled)
    {
        if let Some((original, quarantine)) = quarantined.as_ref() {
            let _ = restore_quarantined_runtime_version_dir_local(original, quarantine);
        }
        return Ok((
            409,
            serde_json::json!({
                "status": "error",
                "action": "remove_runtime_version",
                "payload": serde_json::Value::Null,
                "error_code": "RUNTIME_REMOVE_FAILED",
                "error_detail": format!("failed to update runtime manifest: {}", err),
            })
            .to_string(),
        ));
    }

    let removed_runtime_dir = if let Some((original, quarantine)) = quarantined.as_ref() {
        match finalize_quarantined_runtime_version_dir_local(runtime, quarantine) {
            Ok(removed_runtime_dir) => removed_runtime_dir,
            Err(err) => {
                let rollback_manifest = write_runtime_manifest_file_atomic(
                    manifest_path,
                    &manifest,
                    write_v2_gate_enabled,
                );
                let rollback_fs =
                    restore_quarantined_runtime_version_dir_local(original, quarantine);
                return Ok((
                    409,
                    serde_json::json!({
                        "status": "error",
                        "action": "remove_runtime_version",
                        "payload": {
                            "rollback_manifest": rollback_manifest.err(),
                            "rollback_fs": rollback_fs.err(),
                        },
                        "error_code": "RUNTIME_REMOVE_FAILED",
                        "error_detail": format!("failed to finalize runtime dir removal: {}", err),
                    })
                    .to_string(),
                ));
            }
        }
    } else {
        false
    };

    let (manifest_version, manifest_hash) =
        match local_runtime_manifest_meta_from_file(manifest_path) {
            Ok(meta) => meta,
            Err(err) => {
                return Ok((
                    502,
                    serde_json::json!({
                        "status": "error",
                        "action": "remove_runtime_version",
                        "payload": serde_json::Value::Null,
                        "error_code": "SERVICE_FAILED",
                        "error_detail": err,
                    })
                    .to_string(),
                ));
            }
        };

    Ok((
        200,
        serde_json::json!({
            "status": "ok",
            "action": "remove_runtime_version",
            "payload": {
                "owner_hive": ctx.hive_id,
                "runtime": runtime_key.clone(),
                "removed_version": runtime_version,
                "current": updated_manifest
                    .runtimes
                    .get(&runtime_key)
                    .and_then(|value| value.get("current"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                "manifest_version": manifest_version,
                "manifest_hash": manifest_hash,
                "removed_path": Path::new(DIST_RUNTIME_ROOT_DIR)
                    .join(&runtime_key)
                    .join(runtime_version)
                    .display()
                    .to_string(),
                "removed_runtime_dir": removed_runtime_dir,
                "convergence_status": "owner_updated",
                "usage_global_visible": usage_global,
            },
            "error_code": serde_json::Value::Null,
            "error_detail": serde_json::Value::Null,
        })
        .to_string(),
    ))
}

async fn handle_admin_command(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    if matches!(action, "get_admin_action_help") {
        let action_name = payload
            .get("action")
            .or_else(|| payload.get("action_name"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        return Ok(build_admin_action_help_response(action_name));
    }
    if matches!(action, "publish_runtime_package") {
        return handle_publish_runtime_package(ctx, client, payload).await;
    }
    if matches!(action, "remove_runtime_version") {
        return handle_remove_runtime_version(ctx, client, payload, hive).await;
    }
    if matches!(action, "send_node_message") {
        return handle_send_node_message(ctx, client, payload, hive).await;
    }
    if matches!(
        action,
        "node_control_config_get" | "node_control_config_set"
    ) {
        return handle_node_control_command(ctx, client, action, payload, hive).await;
    }
    if matches!(action, "get_ilk" | "update_tenant" | "set_tenant_sponsor") {
        return handle_identity_command(ctx, client, action, payload, hive).await;
    }

    if let Some(detail) = admin_payload_contract_error(action, &payload) {
        return Ok((
            400u16,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": detail,
            })
            .to_string(),
        ));
    }

    let payload = normalize_admin_payload(action, payload, hive.as_deref());
    let request = build_admin_request(ctx, action, payload, hive);
    let target_hive = extract_hive_from_target(&request.target);
    let mut response = send_admin_request(client, request, admin_action_timeout(action)).await;
    if let Ok(ref payload) = response {
        if let Some(status) = payload.get("status").and_then(|v| v.as_str()) {
            if status == "ok" {
                if matches!(
                    action,
                    "add_route" | "delete_route" | "add_vpn" | "delete_vpn"
                ) {
                    if let Err(err) =
                        broadcast_full_config(ctx, client, action, target_hive.as_deref()).await
                    {
                        let detail = format!("action applied but broadcast failed: {err}");
                        tracing::warn!(error = %detail, "broadcast after admin action failed");
                        response = Ok(serde_json::json!({
                            "status": "error",
                            "error_code": "CONFIG_BROADCAST_FAILED",
                            "message": detail,
                        }));
                    }
                }
            }
        }
    }
    Ok(build_admin_http_response(action, response))
}

async fn handle_send_node_message(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    let sender = client.sender_snapshot().await;
    let req: DebugNodeMessageRequest = serde_json::from_value(payload)?;
    let target_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    let node_name = req.node_name.trim().to_string();
    if node_name.trim().is_empty() {
        return Ok((
            400,
            serde_json::json!({
                "status": "error",
                "action": "send_node_message",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": "node_name is required",
            })
            .to_string(),
        ));
    }
    if req.msg_type.trim().is_empty() {
        return Ok((
            400,
            serde_json::json!({
                "status": "error",
                "action": "send_node_message",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": "msg_type is required",
            })
            .to_string(),
        ));
    }
    let ttl = req.ttl.unwrap_or(16).max(1);
    let trace_id = Uuid::new_v4().to_string();
    let src_ilk = req.src_ilk.clone();
    let msg_name = req.msg.clone();
    let msg_type = req.msg_type.trim().to_string();
    let message = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(node_name.clone()),
            ttl,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: msg_type.clone(),
            msg: msg_name.as_ref().map(|value| value.trim().to_string()),
            src_ilk: src_ilk.clone(),
            scope: req.scope,
            target: req.meta_target,
            action: req.meta_action,
            priority: req.priority,
            context: req.context,
            ..Meta::default()
        },
        payload: req.payload,
    };
    sender.send(message).await?;
    Ok((
        200,
        serde_json::json!({
            "status": "ok",
            "action": "send_node_message",
            "payload": {
                "status": "sent",
                "target": target_hive,
                "node_name": node_name,
                "trace_id": trace_id,
                "msg_type": msg_type,
                "msg": msg_name,
                "src_ilk": src_ilk,
                "ttl": ttl,
            },
            "error_code": serde_json::Value::Null,
            "error_detail": serde_json::Value::Null,
        })
        .to_string(),
    ))
}

async fn handle_node_control_command(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    let timeout_window = admin_action_timeout(action)
        .checked_sub(Duration::from_secs(5))
        .unwrap_or_else(|| Duration::from_secs(25));
    let target_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    let mut payload_map = payload
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    let Some(node_name) = payload_map
        .get("node_name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
    else {
        return Ok((
            400,
            serde_json::json!({
                "status": "error",
                "action": action,
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": "node_name is required",
            })
            .to_string(),
        ));
    };
    if let Some(node_hive) = node_name
        .rsplit_once('@')
        .map(|(_, hive_id)| hive_id.trim())
    {
        if !node_hive.is_empty() && node_hive != target_hive {
            return Ok((
                400,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": serde_json::Value::Null,
                    "error_code": "INVALID_REQUEST",
                    "error_detail": format!(
                        "node_name hive '{}' does not match path hive '{}'",
                        node_hive, target_hive
                    ),
                })
                .to_string(),
            ));
        }
    }

    let ttl = payload_map
        .remove("ttl")
        .and_then(|value| value.as_u64())
        .map(|value| value.clamp(1, u8::MAX as u64) as u8)
        .unwrap_or(16);
    let src_ilk = payload_map
        .remove("src_ilk")
        .and_then(|value| value.as_str().map(|text| text.trim().to_string()))
        .filter(|value| !value.is_empty());
    let scope = payload_map
        .remove("scope")
        .and_then(|value| value.as_str().map(|text| text.trim().to_string()))
        .filter(|value| !value.is_empty());
    let context = payload_map.remove("context");

    let request_msg = match action {
        "node_control_config_get" => "CONFIG_GET",
        "node_control_config_set" => "CONFIG_SET",
        _ => return Err(format!("unsupported node control action: {action}").into()),
    };
    let request_payload = serde_json::Value::Object(payload_map);
    let response = send_system_request_with_meta(
        client,
        &node_name,
        request_msg,
        "CONFIG_RESPONSE",
        request_payload,
        ttl,
        src_ilk,
        scope,
        Some("node_config_control".to_string()),
        Some(request_msg.to_string()),
        context,
        timeout_window,
    )
    .await;

    match response {
        Ok(node_response) => {
            let node_ok = node_response
                .get("ok")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
            let node_error = node_response
                .get("error")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            Ok((
                200,
                serde_json::json!({
                    "status": "ok",
                    "action": action,
                    "payload": {
                        "target": target_hive,
                        "node_name": node_name,
                        "request_msg": request_msg,
                        "response_msg": "CONFIG_RESPONSE",
                        "node_ok": node_ok,
                        "node_error": node_error,
                        "response": node_response,
                    },
                    "error_code": serde_json::Value::Null,
                    "error_detail": serde_json::Value::Null,
                })
                .to_string(),
            ))
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            Ok((
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": {
                        "target": target_hive,
                        "node_name": node_name,
                        "request_msg": request_msg,
                        "response_msg": "CONFIG_RESPONSE",
                    },
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
    }
}

async fn handle_identity_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    let target_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    let target = format!("SY.identity@{}", target_hive);
    let (request_msg, response_msg) = match action {
        "list_ilks" => ("ILK_LIST", "ILK_LIST_RESPONSE"),
        _ => return Err(format!("unsupported identity admin query: {action}").into()),
    };
    let response = send_system_request(
        client,
        &target,
        request_msg,
        response_msg,
        serde_json::json!({}),
        admin_action_timeout(action),
    )
    .await;
    Ok(build_admin_http_response(action, response))
}

async fn handle_identity_command(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> Result<(u16, String), AdminError> {
    let target_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    let target = format!("SY.identity@{}", target_hive);
    let (request_msg, response_msg) = match action {
        "get_ilk" => ("ILK_GET", "ILK_GET_RESPONSE"),
        "update_tenant" => ("TNT_UPDATE", "TNT_UPDATE_RESPONSE"),
        "set_tenant_sponsor" => ("TNT_SET_SPONSOR", "TNT_SET_SPONSOR_RESPONSE"),
        _ => return Err(format!("unsupported identity admin action: {action}").into()),
    };
    let response = send_system_request(
        client,
        &target,
        request_msg,
        response_msg,
        payload,
        admin_action_timeout(action),
    )
    .await;
    Ok(build_admin_http_response(action, response))
}

async fn handle_hive_update_command(
    _ctx: &AdminContext,
    client: &AdminRouterClient,
    hive_id: String,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    let invalid_request = |detail: &str| {
        (
            400u16,
            serde_json::json!({
                "status": "error",
                "action": "update",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": detail,
            })
            .to_string(),
        )
    };

    let category = match payload.get("category") {
        None => "runtime".to_string(),
        Some(value) => match value
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
        {
            Some(category) if matches!(category.as_str(), "runtime" | "core" | "vendor") => {
                category
            }
            _ => {
                return Ok(invalid_request(
                    "category must be one of runtime/core/vendor",
                ))
            }
        },
    };

    let manifest_version = match payload.get("manifest_version") {
        None => 0,
        Some(value) => match value.as_u64() {
            Some(v) => v,
            None => {
                return Ok(invalid_request(
                    "manifest_version must be an unsigned integer",
                ))
            }
        },
    };
    if payload.get("version").is_some() {
        return Ok(invalid_request(
            "legacy field 'version' is not supported; use manifest_version",
        ));
    }

    let manifest_hash = match payload
        .get("manifest_hash")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(value) => value,
        None => return Ok(invalid_request("missing manifest_hash")),
    };
    if payload.get("hash").is_some() {
        return Ok(invalid_request(
            "legacy field 'hash' is not supported; use manifest_hash",
        ));
    }

    let runtime = payload
        .get("runtime")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let runtime_version = payload
        .get("runtime_version")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty());

    if category != "runtime" && (runtime.is_some() || runtime_version.is_some()) {
        return Ok(invalid_request(
            "runtime/runtime_version scope is only supported for category=runtime",
        ));
    }
    if runtime_version.is_some() && runtime.is_none() {
        return Ok(invalid_request("runtime_version requires runtime"));
    }

    let target = format!("SY.orchestrator@{}", hive_id);
    let mut system_payload = serde_json::json!({
        "category": category,
        "manifest_version": manifest_version,
        "manifest_hash": manifest_hash,
    });
    if let Some(obj) = system_payload.as_object_mut() {
        if let Some(runtime) = runtime {
            obj.insert("runtime".to_string(), serde_json::json!(runtime));
        }
        if let Some(runtime_version) = runtime_version {
            obj.insert(
                "runtime_version".to_string(),
                serde_json::json!(runtime_version),
            );
        }
    }

    let response = send_system_request(
        client,
        &target,
        "SYSTEM_UPDATE",
        "SYSTEM_UPDATE_RESPONSE",
        system_payload,
        admin_action_timeout("update"),
    )
    .await;

    match response {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let top_status = match status {
                "ok" => "ok",
                "sync_pending" => "sync_pending",
                _ => "error",
            };
            let error_code_opt = if matches!(status, "ok" | "sync_pending") {
                None
            } else {
                Some(payload_error_code(&payload).unwrap_or_else(|| "SERVICE_FAILED".into()))
            };
            let http_status = match status {
                "ok" => 200,
                "sync_pending" => 202,
                _ => {
                    error_code_to_http_status(error_code_opt.as_deref().unwrap_or("SERVICE_FAILED"))
                }
            };
            let error_code = if let Some(code) = error_code_opt {
                serde_json::json!(code)
            } else {
                serde_json::Value::Null
            };
            let error_detail = if status == "ok" || status == "sync_pending" {
                serde_json::Value::Null
            } else {
                serde_json::json!(payload_error_detail(&payload))
            };
            Ok((
                http_status,
                serde_json::json!({
                    "status": top_status,
                    "action": "update",
                    "payload": payload,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            Ok((
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": "update",
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
    }
}

async fn handle_hive_sync_hint_command(
    _ctx: &AdminContext,
    client: &AdminRouterClient,
    hive_id: String,
    payload: serde_json::Value,
) -> Result<(u16, String), AdminError> {
    let invalid_request = |detail: &str| {
        (
            400u16,
            serde_json::json!({
                "status": "error",
                "action": "sync_hint",
                "payload": serde_json::Value::Null,
                "error_code": "INVALID_REQUEST",
                "error_detail": detail,
            })
            .to_string(),
        )
    };

    let channel = match payload.get("channel") {
        None => "blob".to_string(),
        Some(value) => match value
            .as_str()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_ascii_lowercase())
        {
            Some(v) if matches!(v.as_str(), "blob" | "dist") => v,
            _ => return Ok(invalid_request("channel must be one of blob/dist")),
        },
    };

    let folder_id = match payload.get("folder_id") {
        None => match channel.as_str() {
            "blob" => "fluxbee-blob".to_string(),
            "dist" => "fluxbee-dist".to_string(),
            _ => unreachable!(),
        },
        Some(value) => match value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
            Some(v) => v.to_string(),
            None => return Ok(invalid_request("folder_id must be a non-empty string")),
        },
    };

    let wait_for_idle = match payload.get("wait_for_idle") {
        None => true,
        Some(value) => match value.as_bool() {
            Some(v) => v,
            None => return Ok(invalid_request("wait_for_idle must be boolean")),
        },
    };

    let timeout_ms = match payload.get("timeout_ms") {
        None => 30_000u64,
        Some(value) => match value.as_u64() {
            Some(v) if v > 0 => v,
            _ => {
                return Ok(invalid_request(
                    "timeout_ms must be a positive unsigned integer",
                ))
            }
        },
    };

    let target = format!("SY.orchestrator@{}", hive_id);
    let system_payload = serde_json::json!({
        "channel": channel,
        "folder_id": folder_id,
        "wait_for_idle": wait_for_idle,
        "timeout_ms": timeout_ms,
    });
    let response = send_system_request(
        client,
        &target,
        "SYSTEM_SYNC_HINT",
        "SYSTEM_SYNC_HINT_RESPONSE",
        system_payload,
        admin_action_timeout("sync_hint"),
    )
    .await;

    match response {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let top_status = match status {
                "ok" => "ok",
                "sync_pending" => "sync_pending",
                _ => "error",
            };
            let error_code_opt = if matches!(status, "ok" | "sync_pending") {
                None
            } else {
                Some(payload_error_code(&payload).unwrap_or_else(|| "SERVICE_FAILED".into()))
            };
            let http_status = match status {
                "ok" => 200,
                "sync_pending" => 202,
                _ => {
                    error_code_to_http_status(error_code_opt.as_deref().unwrap_or("SERVICE_FAILED"))
                }
            };
            let error_code = if let Some(code) = error_code_opt {
                serde_json::json!(code)
            } else {
                serde_json::Value::Null
            };
            let error_detail = if matches!(status, "ok" | "sync_pending") {
                serde_json::Value::Null
            } else {
                serde_json::json!(payload_error_detail(&payload))
            };
            Ok((
                http_status,
                serde_json::json!({
                    "status": top_status,
                    "action": "sync_hint",
                    "payload": payload,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            Ok((
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": "sync_hint",
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            ))
        }
    }
}

fn build_admin_request(
    ctx: &AdminContext,
    action: &str,
    payload: serde_json::Value,
    hive: Option<String>,
) -> AdminRequest {
    let requested_hive = hive.unwrap_or_else(|| ctx.hive_id.clone());
    let base = match action {
        "list_routes" | "add_route" | "delete_route" | "list_vpns" | "add_vpn" | "delete_vpn" => {
            "SY.config.routes"
        }
        "list_nodes"
        | "run_node"
        | "start_node"
        | "restart_node"
        | "kill_node"
        | "remove_node_instance"
        | "hive_status"
        | "get_storage"
        | "set_storage"
        | "list_hives"
        | "get_hive"
        | "list_versions"
        | "get_versions"
        | "list_runtimes"
        | "get_runtime"
        | "remove_runtime_version"
        | "list_deployments"
        | "get_deployments"
        | "list_drift_alerts"
        | "get_drift_alerts"
        | "get_node_config"
        | "set_node_config"
        | "get_node_state"
        | "get_node_status"
        | "remove_hive"
        | "add_hive" => "SY.orchestrator",
        _ => "SY.config.routes",
    };
    let route_hive = if action_routes_via_local_orchestrator(action) {
        ctx.hive_id.clone()
    } else {
        requested_hive.clone()
    };
    let target = if route_hive.contains('@') {
        route_hive
    } else {
        format!("{}@{}", base, route_hive)
    };
    AdminRequest {
        action: action.to_string(),
        payload,
        target,
        unicast: None,
    }
}

async fn send_admin_request(
    client: &AdminRouterClient,
    request: AdminRequest,
    timeout_window: Duration,
) -> Result<serde_json::Value, AdminError> {
    let sender = client.sender_snapshot().await;
    let dst = request
        .unicast
        .clone()
        .unwrap_or_else(|| request.target.clone());
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(dst.clone()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "admin".to_string(),
            msg: None,
            src_ilk: None,
            scope: None,
            target: Some(request.target.clone()),
            action: Some(request.action.clone()),
            action_class: classify_admin_action(&request.action),
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload: request.payload,
    };
    let (tx, rx) = oneshot::channel::<Message>();
    client.enqueue_admin_waiter(trace_id.clone(), tx).await;
    sender.send(msg).await?;

    use tokio::time::{timeout, Instant};
    let deadline = Instant::now() + timeout_window;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let msg = match timeout(remaining, rx).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(_)) => return Err("admin response channel closed".into()),
        Err(_) => {
            client.drop_admin_waiter(&trace_id).await;
            return Err(format!(
                "admin request timeout action={} timeout_secs={}",
                request.action,
                timeout_window.as_secs()
            )
            .into());
        }
    };
    Ok(msg.payload)
}

async fn send_system_request(
    client: &AdminRouterClient,
    target: &str,
    request_msg: &str,
    expected_response_msg: &str,
    payload: serde_json::Value,
    timeout_window: Duration,
) -> Result<serde_json::Value, AdminError> {
    send_system_request_with_meta(
        client,
        target,
        request_msg,
        expected_response_msg,
        payload,
        16,
        None,
        None,
        None,
        None,
        None,
        timeout_window,
    )
    .await
}

async fn send_system_request_with_meta(
    client: &AdminRouterClient,
    target: &str,
    request_msg: &str,
    expected_response_msg: &str,
    payload: serde_json::Value,
    ttl: u8,
    src_ilk: Option<String>,
    scope: Option<String>,
    meta_target: Option<String>,
    meta_action: Option<String>,
    context: Option<serde_json::Value>,
    timeout_window: Duration,
) -> Result<serde_json::Value, AdminError> {
    use tokio::time::timeout;

    let sender = client.sender_snapshot().await;
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(target.to_string()),
            ttl,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(request_msg.to_string()),
            src_ilk,
            scope,
            target: meta_target.or_else(|| Some(target.to_string())),
            action: meta_action,
            action_class: classify_system_message(request_msg),
            priority: None,
            context,
            ..Meta::default()
        },
        payload,
    };

    let (tx, rx) = oneshot::channel::<Message>();
    client.enqueue_admin_waiter(trace_id.clone(), tx).await;
    sender.send(msg).await?;

    let msg = match timeout(timeout_window, rx).await {
        Ok(Ok(msg)) => msg,
        Ok(Err(_)) => return Err("system response channel closed".into()),
        Err(_) => {
            client.drop_admin_waiter(&trace_id).await;
            return Err(format!(
                "system request timeout msg={} target={} timeout_secs={}",
                request_msg,
                target,
                timeout_window.as_secs()
            )
            .into());
        }
    };

    if msg.meta.msg_type != SYSTEM_KIND {
        return Err(format!(
            "invalid response type: expected={} got={}",
            SYSTEM_KIND, msg.meta.msg_type
        )
        .into());
    }
    if matches!(
        msg.meta.msg.as_deref(),
        Some("UNREACHABLE" | "TTL_EXCEEDED")
    ) {
        let system_msg = msg.meta.msg.as_deref().unwrap_or("");
        let reason = msg
            .payload
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let original_dst = msg
            .payload
            .get("original_dst")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let last_hop = msg
            .payload
            .get("last_hop")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        return Err(format!(
            "router returned {} for target={} reason={} original_dst={} last_hop={}",
            system_msg, target, reason, original_dst, last_hop
        )
        .into());
    }
    if msg.meta.msg.as_deref() != Some(expected_response_msg) {
        return Err(format!(
            "invalid response msg: expected={} got={}",
            expected_response_msg,
            msg.meta.msg.as_deref().unwrap_or("")
        )
        .into());
    }
    Ok(msg.payload)
}

fn env_timeout_secs(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse::<u64>().ok()
}

fn admin_action_timeout(action: &str) -> Duration {
    match action {
        // add_hive runs remote bootstrap + WAN wait and is expected to be long.
        "add_hive" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_ADD_HIVE_TIMEOUT_SECS").unwrap_or(180))
        }
        "update" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_UPDATE_TIMEOUT_SECS").unwrap_or(60))
        }
        "sync_hint" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_SYNC_HINT_TIMEOUT_SECS").unwrap_or(45))
        }
        "list_admin_actions"
        | "get_admin_action_help"
        | "hive_status"
        | "timer_help"
        | "timer_now"
        | "timer_now_in"
        | "timer_convert"
        | "timer_parse"
        | "timer_format" => {
            Duration::from_secs(env_timeout_secs("JSR_ADMIN_TIMEOUT_SECS").unwrap_or(5))
        }
        _ => Duration::from_secs(env_timeout_secs("JSR_ADMIN_ORCH_TIMEOUT_SECS").unwrap_or(30)),
    }
}

fn build_admin_http_response(
    action: &str,
    response: Result<serde_json::Value, AdminError>,
) -> (u16, String) {
    match response {
        Ok(payload) => {
            let status = payload
                .get("status")
                .and_then(|value| value.as_str())
                .unwrap_or("ok");
            if payload_is_ok(&payload) {
                let http_status = if status.eq_ignore_ascii_case("sync_pending") {
                    202
                } else {
                    200
                };
                return (
                    http_status,
                    serde_json::json!({
                        "status": if status.eq_ignore_ascii_case("sync_pending") { "sync_pending" } else { "ok" },
                        "action": action,
                        "payload": payload,
                        "error_code": serde_json::Value::Null,
                        "error_detail": serde_json::Value::Null,
                    })
                    .to_string(),
                );
            }

            let error_code =
                payload_error_code(&payload).unwrap_or_else(|| "SERVICE_FAILED".into());
            let error_detail = payload_error_detail(&payload);
            let status_code = error_code_to_http_status(&error_code);
            (
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": payload,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            )
        }
        Err(err) => {
            let error_detail = err.to_string();
            let error_code = infer_transport_error_code(&error_detail).to_string();
            let status_code = error_code_to_http_status(&error_code);
            (
                status_code,
                serde_json::json!({
                    "status": "error",
                    "action": action,
                    "payload": serde_json::Value::Null,
                    "error_code": error_code,
                    "error_detail": error_detail,
                })
                .to_string(),
            )
        }
    }
}

fn extract_hive_from_target(target: &str) -> Option<String> {
    target.split_once('@').map(|(_, hive)| hive.to_string())
}

fn action_routes_via_local_orchestrator(action: &str) -> bool {
    matches!(
        action,
        "list_nodes"
            | "run_node"
            | "start_node"
            | "restart_node"
            | "kill_node"
            | "remove_node_instance"
            | "hive_status"
            | "get_storage"
            | "set_storage"
            | "list_hives"
            | "get_hive"
            | "list_versions"
            | "get_versions"
            | "list_runtimes"
            | "get_runtime"
            | "remove_runtime_version"
            | "list_deployments"
            | "get_deployments"
            | "list_drift_alerts"
            | "get_drift_alerts"
            | "get_node_config"
            | "set_node_config"
            | "get_node_state"
            | "get_node_status"
            | "remove_hive"
            | "add_hive"
    )
}

fn normalize_admin_payload(
    action: &str,
    mut payload: serde_json::Value,
    hive: Option<&str>,
) -> serde_json::Value {
    if !action_routes_via_local_orchestrator(action) {
        return payload;
    }

    if let Some(node_hive) = node_name_hive_from_payload(action, &payload) {
        if let Some(requested_target) = payload
            .get("target")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if requested_target != node_hive {
                tracing::warn!(
                    action = action,
                    node_hive = %node_hive,
                    requested_target = %requested_target,
                    "node_name@hive overrides payload.target"
                );
            }
        }
        payload["target"] = serde_json::Value::String(node_hive.to_string());
        return payload;
    }

    if let Some(hive_id) = hive
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        if payload.get("target").is_none() {
            payload["target"] = serde_json::Value::String(hive_id.to_string());
        }
    }

    payload
}

fn node_name_hive_from_payload<'a>(
    action: &str,
    payload: &'a serde_json::Value,
) -> Option<&'a str> {
    if !is_node_scoped_action(action) {
        return None;
    }
    let node_name = payload
        .get("node_name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let (_, hive) = node_name.rsplit_once('@')?;
    let hive = hive.trim();
    if hive.is_empty() {
        return None;
    }
    Some(hive)
}

fn is_node_scoped_action(action: &str) -> bool {
    matches!(
        action,
        "run_node"
            | "start_node"
            | "restart_node"
            | "kill_node"
            | "remove_node_instance"
            | "get_node_config"
            | "set_node_config"
            | "node_control_config_get"
            | "node_control_config_set"
            | "get_node_state"
            | "get_node_status"
            | "send_node_message"
    )
}

fn admin_payload_contract_error(action: &str, payload: &serde_json::Value) -> Option<String> {
    let legacy = match action {
        "run_node" => {
            if payload.get("name").is_some() {
                Some(("name", "node_name"))
            } else if payload.get("version").is_some() {
                Some(("version", "runtime_version"))
            } else {
                None
            }
        }
        "start_node" | "restart_node" => payload.get("name").map(|_| ("name", "node_name")),
        "kill_node" => payload.get("name").map(|_| ("name", "node_name")),
        "remove_node_instance" => payload.get("name").map(|_| ("name", "node_name")),
        _ => None,
    };
    legacy.map(|(field, canonical)| {
        format!(
            "legacy field '{}' is not supported; use '{}'",
            field, canonical
        )
    })
}

async fn broadcast_full_config(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target_hive: Option<&str>,
) -> Result<(), AdminError> {
    let list_action = if action.contains("route") {
        "list_routes"
    } else {
        "list_vpns"
    };
    let list_req = build_admin_request(
        ctx,
        list_action,
        serde_json::json!({}),
        target_hive.map(|s| s.to_string()),
    );
    let response = send_admin_request(client, list_req, admin_action_timeout(list_action)).await?;
    let payload = response
        .get("status")
        .and_then(|v| v.as_str())
        .filter(|v| *v == "ok")
        .and_then(|_| list_config_items_payload(&response, list_action))
        .ok_or("list response missing routes/vpns")?;
    let config = if list_action == "list_routes" {
        serde_json::json!({ "routes": payload })
    } else {
        serde_json::json!({ "vpns": payload })
    };

    let subsystem = if list_action == "list_routes" {
        "routes"
    } else {
        "vpn"
    };
    let version = next_config_changed_version(&ctx.state_dir, subsystem, None)?;
    broadcast_config_changed(client, subsystem, None, None, version, config, None).await?;
    Ok(())
}

fn list_config_items_payload<'a>(
    response: &'a serde_json::Value,
    list_action: &str,
) -> Option<&'a serde_json::Value> {
    let item_key = if list_action == "list_routes" {
        "routes"
    } else {
        "vpns"
    };
    response
        .get("payload")
        .and_then(|payload| payload.get(item_key))
        .or_else(|| response.get(item_key))
}

async fn send_opa_action(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    version: u64,
    rego: Option<String>,
    entrypoint: Option<String>,
    auto_apply: Option<bool>,
    target: Option<String>,
) -> Result<Vec<OpaResponseEntry>, AdminError> {
    let mut cfg = serde_json::json!({});
    if let Some(rego) = rego {
        cfg = serde_json::json!({
            "rego": rego,
            "entrypoint": entrypoint.unwrap_or_else(|| "router/target".to_string()),
        });
    }
    broadcast_config_changed(
        client,
        "opa",
        Some(action.to_string()),
        auto_apply,
        version,
        cfg,
        target.clone(),
    )
    .await?;

    let expected = expected_hive_sets(ctx, target.as_deref()).effective;
    let mut receiver = client.subscribe_system();
    let responses = collect_opa_responses(&mut receiver, action, version, &expected).await;
    Ok(responses)
}

async fn send_opa_query(
    ctx: &AdminContext,
    client: &AdminRouterClient,
    action: &str,
    target: Option<String>,
) -> Result<Vec<OpaQueryEntry>, AdminError> {
    let sender = client.sender_snapshot().await;
    let target_pattern = target
        .as_deref()
        .map(|hive| format!("SY.opa.rules@{}", hive));
    let dst = match target.as_deref() {
        Some(hive) => Destination::Unicast(format!("SY.opa.rules@{}", hive)),
        None => Destination::Broadcast,
    };
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst,
            ttl: 16,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            msg_type: "query".to_string(),
            action: Some(action.to_string()),
            msg: None,
            src_ilk: None,
            scope: None,
            target: target_pattern,
            priority: None,
            context: None,
            ..Meta::default()
        },
        payload: serde_json::json!({}),
    };
    sender.send(msg).await?;

    let expected = expected_hive_sets(ctx, target.as_deref()).effective;
    let mut receiver = client.subscribe_query();
    let responses = collect_opa_query_responses(&mut receiver, action, &expected).await;
    Ok(responses)
}

fn normalize_opa_target(target: Option<String>) -> Option<String> {
    target.and_then(|value| {
        let trimmed = value.trim().to_string();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("broadcast") {
            None
        } else {
            Some(trimmed)
        }
    })
}

fn remote_hive_name(entry: &RemoteHiveEntry) -> Option<String> {
    if entry.hive_id_len == 0 {
        return None;
    }
    let len = entry.hive_id_len as usize;
    Some(String::from_utf8_lossy(&entry.hive_id[..len]).into_owned())
}

fn remote_hive_is_stale(entry: &RemoteHiveEntry, now: u64) -> bool {
    (entry.flags & (FLAG_DELETED | FLAG_STALE)) != 0
        || now.saturating_sub(entry.last_updated) > HEARTBEAT_STALE_MS
}

fn topology_hives_from_lsa(ctx: &AdminContext) -> Vec<String> {
    let mut hives = HashSet::new();
    hives.insert(ctx.hive_id.clone());

    let shm_name = format!("/jsr-lsa-{}", ctx.hive_id);
    let reader = match LsaRegionReader::open_read_only(&shm_name) {
        Ok(reader) => reader,
        Err(err) => {
            tracing::warn!(error = %err, "failed to open lsa region for OPA expected hives");
            let mut fallback: Vec<String> = hives.into_iter().collect();
            fallback.sort();
            return fallback;
        }
    };
    let snapshot = match reader.read_snapshot() {
        Some(snapshot) => snapshot,
        None => {
            tracing::warn!("lsa snapshot unavailable for OPA expected hives");
            let mut fallback: Vec<String> = hives.into_iter().collect();
            fallback.sort();
            return fallback;
        }
    };
    let now = now_epoch_ms();
    for entry in &snapshot.hives {
        if remote_hive_is_stale(entry, now) {
            continue;
        }
        if let Some(hive) = remote_hive_name(entry).filter(|name| !name.trim().is_empty()) {
            hives.insert(hive);
        }
    }
    let mut out: Vec<String> = hives.into_iter().collect();
    out.sort();
    out
}

#[derive(Debug, Clone)]
struct OpaExpectedHives {
    effective: Vec<String>,
    topology: Vec<String>,
    policy: Vec<String>,
}

fn expected_hive_sets(ctx: &AdminContext, target: Option<&str>) -> OpaExpectedHives {
    if let Some(target) = target {
        let single = vec![target.to_string()];
        return OpaExpectedHives {
            effective: single.clone(),
            topology: single.clone(),
            policy: single,
        };
    }

    let topology = topology_hives_from_lsa(ctx);
    let mut policy_set = HashSet::new();
    policy_set.insert(ctx.hive_id.clone());
    for hive in &ctx.authorized_hives {
        if !hive.trim().is_empty() {
            policy_set.insert(hive.clone());
        }
    }
    let mut policy: Vec<String> = policy_set.into_iter().collect();
    policy.sort();

    // Topology (LSA/SHM) is canonical for fanout expectations.
    // Policy list is kept for explicit observability in response payload.
    let effective = topology.clone();
    OpaExpectedHives {
        effective,
        topology,
        policy,
    }
}

async fn collect_opa_responses(
    receiver: &mut broadcast::Receiver<Message>,
    action: &str,
    version: u64,
    expected: &[String],
) -> Vec<OpaResponseEntry> {
    use tokio::time::{timeout, Instant};

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut responses: HashMap<String, OpaResponseEntry> = HashMap::new();

    while responses.len() < expected.len() && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = match timeout(remaining, receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(err)) => {
                tracing::warn!("opa response recv error: {err}");
                break;
            }
            Err(_) => break,
        };
        if msg.meta.msg.as_deref() != Some("CONFIG_RESPONSE") {
            continue;
        }
        let payload: ConfigResponsePayload = match serde_json::from_value(msg.payload) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!("invalid config response payload: {err}");
                continue;
            }
        };
        if payload.subsystem != "opa" {
            continue;
        }
        if payload.action.as_deref() != Some(action) {
            continue;
        }
        if payload.version.unwrap_or(version) != version {
            continue;
        }
        let hive = payload
            .hive
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let entry = OpaResponseEntry {
            hive: hive.clone(),
            status: payload.status.unwrap_or_else(|| "error".to_string()),
            version: payload.version,
            compile_time_ms: payload.compile_time_ms,
            wasm_size_bytes: payload.wasm_size_bytes,
            hash: payload.hash,
            error_code: payload.error_code,
            error_detail: payload.error_detail,
        };
        responses.insert(hive, entry);
    }

    responses.into_values().collect()
}

async fn collect_opa_query_responses(
    receiver: &mut broadcast::Receiver<Message>,
    action: &str,
    expected: &[String],
) -> Vec<OpaQueryEntry> {
    use tokio::time::{timeout, Instant};

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut responses: HashMap<String, OpaQueryEntry> = HashMap::new();

    while responses.len() < expected.len() && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let msg = match timeout(remaining, receiver.recv()).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(err)) => {
                tracing::warn!("opa query recv error: {err}");
                break;
            }
            Err(_) => break,
        };
        if msg.meta.msg_type != "query_response" {
            continue;
        }
        if msg.meta.action.as_deref() != Some(action) {
            continue;
        }
        let payload = msg.payload;
        let hive = payload
            .get("hive")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let status = payload
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("ok")
            .to_string();
        responses.insert(
            hive.clone(),
            OpaQueryEntry {
                hive,
                status,
                payload,
            },
        );
    }

    responses.into_values().collect()
}

fn build_opa_http_response(
    ctx: &AdminContext,
    action: OpaAction,
    version: u64,
    responses: Vec<OpaResponseEntry>,
    target: Option<String>,
) -> (u16, String) {
    let expected = expected_hive_sets(ctx, target.as_deref());
    let pending: Vec<String> = expected
        .effective
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_topology: Vec<String> = expected
        .topology
        .iter()
        .into_iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_policy: Vec<String> = expected
        .policy
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let timed_out = !pending.is_empty();

    let (error_code, error_detail) = if timed_out {
        (
            Some("TIMEOUT".to_string()),
            Some(format!("pending_hives={}", pending.join(","))),
        )
    } else if responses.is_empty() {
        (
            Some("SERVICE_FAILED".to_string()),
            Some("no responses received".to_string()),
        )
    } else {
        match responses
            .iter()
            .find(|r| !r.status.eq_ignore_ascii_case("ok"))
        {
            Some(entry) => (
                entry
                    .error_code
                    .clone()
                    .or_else(|| Some("SERVICE_FAILED".to_string())),
                entry.error_detail.clone(),
            ),
            None => (None, None),
        }
    };

    let status = if let Some(code) = error_code.as_deref() {
        error_code_to_http_status(code)
    } else {
        200
    };

    let body = serde_json::json!({
        "status": if status == 200 { "ok" } else { "error" },
        "version": version,
        "action": action.as_str(),
        "check_only": action.check_only(),
        "responses": responses,
        "pending": pending,
        "expected_hives_topology": expected.topology,
        "expected_hives_policy": expected.policy,
        "pending_hives_topology": pending_topology,
        "pending_hives_policy": pending_policy,
        "error_code": error_code,
        "error_detail": error_detail,
    })
    .to_string();
    (status, body)
}

fn build_opa_query_response(
    ctx: &AdminContext,
    action: &str,
    responses: Vec<OpaQueryEntry>,
    target: Option<String>,
) -> (u16, String) {
    let expected = expected_hive_sets(ctx, target.as_deref());
    let pending: Vec<String> = expected
        .effective
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_topology: Vec<String> = expected
        .topology
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let pending_policy: Vec<String> = expected
        .policy
        .iter()
        .filter(|hive| !responses.iter().any(|r| r.hive == **hive))
        .cloned()
        .collect();
    let timed_out = !pending.is_empty();

    let (error_code, error_detail) = if timed_out {
        (
            Some("TIMEOUT".to_string()),
            Some(format!("pending_hives={}", pending.join(","))),
        )
    } else if responses.is_empty() {
        (
            Some("SERVICE_FAILED".to_string()),
            Some("no responses received".to_string()),
        )
    } else {
        match responses
            .iter()
            .find(|r| !r.status.eq_ignore_ascii_case("ok"))
        {
            Some(entry) => (
                payload_error_code(&entry.payload).or_else(|| Some("SERVICE_FAILED".to_string())),
                payload_error_detail(&entry.payload),
            ),
            None => (None, None),
        }
    };

    let status = if let Some(code) = error_code.as_deref() {
        error_code_to_http_status(code)
    } else {
        200
    };

    let body = serde_json::json!({
        "status": if status == 200 { "ok" } else { "error" },
        "action": action,
        "responses": responses,
        "pending": pending,
        "expected_hives_topology": expected.topology,
        "expected_hives_policy": expected.policy,
        "pending_hives_topology": pending_topology,
        "pending_hives_policy": pending_policy,
        "error_code": error_code,
        "error_detail": error_detail,
    })
    .to_string();
    (status, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::broadcast;
    use zip::write::FileOptions;

    fn test_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn write_test_bundle(
        bundle_path: &Path,
        entries: &[(&str, &str, Option<u32>)],
    ) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = bundle_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::File::create(bundle_path)?;
        let mut zip = zip::ZipWriter::new(file);
        for (path, contents, mode) in entries {
            let mut options =
                FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            if let Some(mode) = mode {
                options = options.unix_permissions(*mode);
            }
            zip.start_file(*path, options)?;
            zip.write_all(contents.as_bytes())?;
        }
        zip.finish()?;
        Ok(())
    }

    #[test]
    fn internal_response_payload_value_prefers_payload_field() {
        let envelope = json!({
            "status": "ok",
            "action": "get_hive",
            "payload": {
                "hive_id": "worker-220"
            },
            "error_code": null,
            "error_detail": null
        });

        assert_eq!(
            internal_response_payload_value(&envelope),
            json!({ "hive_id": "worker-220" })
        );
    }

    #[test]
    fn internal_response_payload_value_falls_back_to_top_level_body() {
        let envelope = json!({
            "status": "ok",
            "action": "get_policy",
            "responses": [
                {
                    "hive": "motherbee",
                    "status": "ok",
                    "payload": {
                        "version": 10
                    }
                }
            ],
            "pending": [],
            "expected_hives_policy": ["motherbee"],
            "expected_hives_topology": ["motherbee"],
            "pending_hives_policy": [],
            "pending_hives_topology": [],
            "error_code": null,
            "error_detail": null
        });

        assert_eq!(
            internal_response_payload_value(&envelope),
            json!({
                "responses": [
                    {
                        "hive": "motherbee",
                        "status": "ok",
                        "payload": {
                            "version": 10
                        }
                    }
                ],
                "pending": [],
                "expected_hives_policy": ["motherbee"],
                "expected_hives_topology": ["motherbee"],
                "pending_hives_policy": [],
                "pending_hives_topology": []
            })
        );
    }

    #[tokio::test]
    async fn collect_opa_responses_accepts_sy_opa_rules_config_response_shape() {
        let (tx, mut rx) = broadcast::channel(8);
        tx.send(Message {
            routing: Routing {
                src: "SY.opa.rules@motherbee".to_string(),
                src_l2_name: Some("SY.opa.rules@motherbee".to_string()),
                dst: Destination::Unicast("SY.admin@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-opa-config".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some("CONFIG_RESPONSE".to_string()),
                ..Meta::default()
            },
            payload: json!({
                "subsystem": "opa",
                "action": "compile",
                "version": 7,
                "status": "ok",
                "hive": "motherbee",
                "compile_time_ms": 12,
                "wasm_size_bytes": 2048,
                "hash": "sha256:test"
            }),
        })
        .unwrap();

        let responses =
            collect_opa_responses(&mut rx, "compile", 7, &["motherbee".to_string()]).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].hive, "motherbee");
        assert_eq!(responses[0].status, "ok");
        assert_eq!(responses[0].version, Some(7));
        assert_eq!(responses[0].hash.as_deref(), Some("sha256:test"));
    }

    #[tokio::test]
    async fn collect_opa_query_responses_accepts_sy_opa_rules_query_response_shape() {
        let (tx, mut rx) = broadcast::channel(8);
        tx.send(Message {
            routing: Routing {
                src: "SY.opa.rules@motherbee".to_string(),
                src_l2_name: Some("SY.opa.rules@motherbee".to_string()),
                dst: Destination::Unicast("SY.admin@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-opa-query".to_string(),
            },
            meta: Meta {
                msg_type: "query_response".to_string(),
                action: Some("get_status".to_string()),
                ..Meta::default()
            },
            payload: json!({
                "hive": "motherbee",
                "status": "ok",
                "current_version": 4,
                "current_hash": "sha256:current"
            }),
        })
        .unwrap();

        let responses =
            collect_opa_query_responses(&mut rx, "get_status", &["motherbee".to_string()]).await;
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0].hive, "motherbee");
        assert_eq!(responses[0].status, "ok");
        assert_eq!(responses[0].payload["current_version"], json!(4));
    }

    #[test]
    fn list_config_items_payload_prefers_nested_payload_shape() {
        let response = json!({
            "status": "ok",
            "action": "list_routes",
            "payload": {
                "config_version": 7,
                "routes": [{ "prefix": "alpha/**" }]
            },
            "config_version": 7,
            "routes": [{ "prefix": "legacy/**" }]
        });

        assert_eq!(
            list_config_items_payload(&response, "list_routes"),
            Some(&json!([{ "prefix": "alpha/**" }]))
        );
    }

    #[test]
    fn list_config_items_payload_falls_back_to_legacy_top_level_shape() {
        let response = json!({
            "status": "ok",
            "vpns": [{ "pattern": "ops/*" }]
        });

        assert_eq!(
            list_config_items_payload(&response, "list_vpns"),
            Some(&json!([{ "pattern": "ops/*" }]))
        );
    }

    #[test]
    fn timer_help_action_doc_is_registered_as_timer_rpc() {
        let spec = resolve_internal_action_spec("timer_help").expect("timer_help must exist");
        let doc = build_admin_action_doc(spec);

        assert_eq!(doc["action"], json!("timer_help"));
        assert_eq!(doc["handler"], json!("timer_rpc"));
        assert_eq!(doc["canonical_action"], json!("TIMER_HELP"));
        assert_eq!(doc["read_only"], json!(true));
        assert_eq!(doc["requires_target"], json!(true));
    }

    #[test]
    fn timer_parse_request_contract_exposes_timer_path_and_fields() {
        let contract = admin_action_request_contract("timer_parse");

        assert_eq!(contract["method"], json!("POST"));
        assert_eq!(
            contract["path_patterns"],
            json!(["POST /hives/{hive}/timer/parse"])
        );
        assert_eq!(
            contract["body"]["required_fields"],
            json!([
                { "name": "input", "type": "string", "description": "Input date/time text to parse." },
                { "name": "layout", "type": "string", "description": "Go time layout used to parse input." },
                { "name": "tz", "type": "string", "description": "IANA timezone applied to the parsed input." }
            ])
        );
    }

    #[test]
    fn timer_get_action_doc_is_registered_as_timer_rpc() {
        let spec = resolve_internal_action_spec("timer_get").expect("timer_get must exist");
        let doc = build_admin_action_doc(spec);

        assert_eq!(doc["action"], json!("timer_get"));
        assert_eq!(doc["handler"], json!("timer_rpc"));
        assert_eq!(doc["canonical_action"], json!("TIMER_GET"));
        assert_eq!(doc["read_only"], json!(true));
    }

    #[test]
    fn timer_list_request_contract_exposes_owner_filter() {
        let contract = admin_action_request_contract("timer_list");

        assert_eq!(contract["method"], json!("GET"));
        assert_eq!(
            contract["path_patterns"],
            json!(["GET /hives/{hive}/timer/timers"])
        );
        assert_eq!(
            contract["body"]["optional_fields"],
            json!([
                { "name": "owner_l2_name", "type": "string", "description": "Optional canonical L2 owner filter. When omitted, SY.timer returns timers for the full hive." },
                { "name": "status_filter", "type": "string", "description": "Optional timer status filter: pending, fired, canceled, or all." },
                { "name": "limit", "type": "u32", "description": "Optional result limit. Defaults inside SY.timer and is capped there." }
            ])
        );
    }

    #[test]
    fn build_admin_http_response_accepts_timer_ok_boolean_contract() {
        let payload = json!({
            "ok": true,
            "verb": "TIMER_LIST",
            "count": 0,
            "timers": []
        });

        let (status, body) = build_admin_http_response("timer_list", Ok(payload));
        let body_json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert_eq!(status, 200);
        assert_eq!(body_json["status"], json!("ok"));
        assert_eq!(body_json["error_code"], serde_json::Value::Null);
    }

    #[test]
    fn wf_rules_compile_action_doc_is_registered_as_wf_rules_http() {
        let spec =
            resolve_internal_action_spec("wf_rules_compile").expect("wf_rules_compile must exist");
        let doc = build_admin_action_doc(spec);

        assert_eq!(doc["action"], json!("wf_rules_compile"));
        assert_eq!(doc["handler"], json!("wf_rules_http"));
        assert_eq!(doc["canonical_action"], serde_json::Value::Null);
        assert_eq!(doc["read_only"], json!(false));
        assert_eq!(doc["requires_target"], json!(false));
        assert_eq!(doc["path_patterns_are_templates"], json!(true));
        assert_eq!(doc["execution_preference"], json!("example_scmd"));
    }

    #[test]
    fn wf_rules_get_status_action_doc_is_registered_as_wf_rules_query() {
        let spec = resolve_internal_action_spec("wf_rules_get_status")
            .expect("wf_rules_get_status must exist");
        let doc = build_admin_action_doc(spec);

        assert_eq!(doc["action"], json!("wf_rules_get_status"));
        assert_eq!(doc["handler"], json!("wf_rules_query"));
        assert_eq!(doc["canonical_action"], json!("get_status"));
        assert_eq!(doc["read_only"], json!(true));
    }

    #[test]
    fn wf_rules_compile_request_contract_exposes_expected_body_fields() {
        let contract = admin_action_request_contract("wf_rules_compile");

        assert_eq!(contract["method"], json!("POST"));
        assert_eq!(
            contract["path_patterns"],
            json!(["POST /hives/{hive}/wf-rules/compile"])
        );
        assert_eq!(
            contract["body"]["required_fields"],
            json!([
                { "name": "workflow_name", "type": "string", "description": "Workflow logical name." },
                { "name": "definition", "type": "object", "description": "Workflow definition JSON." }
            ])
        );
        let optional_fields = contract["body"]["optional_fields"]
            .as_array()
            .expect("optional_fields must be an array");
        assert_eq!(optional_fields.len(), 3);
        assert!(optional_fields.iter().any(|field| {
            field["name"] == json!("hive")
                && field["type"] == json!("string")
                && field["description"]
                    == json!("Optional explicit hive override for SY.wf-rules target.")
        }));
        assert!(optional_fields.iter().any(|field| {
            field["name"] == json!("auto_spawn")
                && field["type"] == json!("bool")
                && field["description"]
                    == json!("Accepted for symmetry with compile_apply but ignored because compile does not deploy.")
        }));
        assert!(optional_fields.iter().any(|field| {
            field["name"] == json!("version")
                && field["type"] == json!("u64")
                && field["description"]
                    == json!("Optional explicit workflow version/idempotency check.")
        }));
    }

    #[test]
    fn wf_rules_status_request_contract_exposes_query_fields() {
        let contract = admin_action_request_contract("wf_rules_get_status");

        assert_eq!(contract["method"], json!("GET"));
        assert_eq!(
            contract["path_patterns"],
            json!(["GET /hives/{hive}/wf-rules/status"])
        );
        assert_eq!(contract["path_patterns_are_templates"], json!(true));
        assert_eq!(contract["execution_preference"], json!("example_scmd"));
        assert_eq!(
            contract["body"]["required_fields"],
            json!([
                { "name": "workflow_name", "type": "string", "description": "Workflow logical name." }
            ])
        );
        let optional_fields = contract["body"]["optional_fields"]
            .as_array()
            .expect("optional_fields must be an array");
        assert_eq!(optional_fields.len(), 1);
        assert_eq!(optional_fields[0]["name"], json!("hive"));
        assert_eq!(optional_fields[0]["type"], json!("string"));
        assert_eq!(
            optional_fields[0]["description"],
            json!("Optional explicit hive override for SY.wf-rules target.")
        );
    }

    #[test]
    fn wf_rules_request_from_query_uses_default_hive_and_removes_target_keys() {
        let req = wf_rules_request_from_query(
            HashMap::from([
                ("workflow_name".to_string(), "invoice".to_string()),
                ("hive".to_string(), "worker-22".to_string()),
                ("target".to_string(), "ignored-target".to_string()),
            ]),
            Some("motherbee".to_string()),
        );

        assert_eq!(req.hive.as_deref(), Some("worker-22"));
        assert_eq!(req.payload["workflow_name"], json!("invoice"));
        assert!(!req.payload.contains_key("hive"));
        assert!(!req.payload.contains_key("target"));
    }

    #[test]
    fn wf_rules_request_from_query_falls_back_to_default_hive() {
        let req = wf_rules_request_from_query(
            HashMap::from([("workflow_name".to_string(), "invoice".to_string())]),
            Some("motherbee".to_string()),
        );

        assert_eq!(req.hive.as_deref(), Some("motherbee"));
        assert_eq!(req.payload["workflow_name"], json!("invoice"));
    }

    #[test]
    fn normalize_wf_rules_target_treats_blank_and_broadcast_as_missing() {
        assert_eq!(
            normalize_wf_rules_target(Some("broadcast".to_string()), "motherbee"),
            "motherbee"
        );
        assert_eq!(
            normalize_wf_rules_target(Some("  ".to_string()), "motherbee"),
            "motherbee"
        );
        assert_eq!(
            normalize_wf_rules_target(Some("worker-22".to_string()), "motherbee"),
            "worker-22"
        );
    }

    #[test]
    fn parse_internal_wf_rules_request_rejects_non_object_payload() {
        let err = parse_internal_wf_rules_request(json!("bad"), Some("motherbee".to_string()))
            .expect_err("non-object params must fail");

        assert!(err.contains("expected JSON object"));
    }

    #[test]
    fn parse_internal_wf_rules_request_injects_default_hive() {
        let req = parse_internal_wf_rules_request(
            json!({
                "workflow_name": "invoice",
                "definition": { "wf_schema_version": "1" }
            }),
            Some("motherbee".to_string()),
        )
        .expect("valid wf-rules request");

        assert_eq!(req.hive.as_deref(), Some("motherbee"));
        assert_eq!(req.payload["workflow_name"], json!("invoice"));
        assert_eq!(req.payload["definition"]["wf_schema_version"], json!("1"));
    }

    #[test]
    fn build_admin_http_response_accepts_sy_wf_rules_ok_boolean_contract() {
        let payload = json!({
            "ok": true,
            "node_name": "SY.wf-rules@motherbee",
            "wf_node": { "action": "restarted", "status": "ok" }
        });

        let (status, body) = build_admin_http_response("wf_rules_apply", Ok(payload));
        let body_json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert_eq!(status, 200);
        assert_eq!(body_json["status"], json!("ok"));
        assert_eq!(
            body_json["payload"]["wf_node"]["action"],
            json!("restarted")
        );
    }

    #[test]
    fn http_status_line_supports_accepted() {
        assert_eq!(http_status_line(202), "HTTP/1.1 202 Accepted");
    }

    #[test]
    fn build_admin_http_response_maps_sy_wf_rules_error_payload() {
        let payload = json!({
            "ok": false,
            "error": {
                "code": "INSTANCES_ACTIVE",
                "detail": "workflow has running instances"
            }
        });

        let (status, body) = build_admin_http_response("wf_rules_delete", Ok(payload));
        let body_json: serde_json::Value = serde_json::from_str(&body).expect("valid json");

        assert_eq!(status, 409);
        assert_eq!(body_json["status"], json!("error"));
        assert_eq!(body_json["error_code"], json!("INSTANCES_ACTIVE"));
        assert_eq!(
            body_json["error_detail"],
            json!("workflow has running instances")
        );
    }

    #[test]
    fn admin_executor_function_definition_uses_strict_runtime_contract() {
        let spec = resolve_internal_action_spec("get_runtime").expect("get_runtime action");
        let definition = build_admin_executor_function_definition(spec);

        assert_eq!(definition.name, "get_runtime");
        assert_eq!(
            definition.parameters_json_schema["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            definition.parameters_json_schema["required"],
            json!(["hive", "runtime"])
        );
        assert_eq!(
            definition.parameters_json_schema["properties"]["hive"]["type"],
            json!("string")
        );
        assert_eq!(
            definition.parameters_json_schema["properties"]["runtime"]["type"],
            json!("string")
        );
    }

    #[test]
    fn admin_executor_subset_mode_falls_back_to_pilot_allowlist() {
        let config = AdminExecutorNodeConfigFile {
            catalog: Some(AdminExecutorCatalogConfig {
                mode: Some("subset".to_string()),
                actions: None,
            }),
            ..AdminExecutorNodeConfigFile::default()
        };

        let actions: Vec<&str> = executor_visible_action_specs(Some(&config))
            .into_iter()
            .map(|spec| spec.action)
            .collect();

        assert!(actions.contains(&"publish_runtime_package"));
        assert!(actions.contains(&"sync_hint"));
        assert!(actions.contains(&"update"));
        assert!(actions.contains(&"get_runtime"));
        assert!(actions.contains(&"start_node"));
        assert!(actions.contains(&"restart_node"));
        assert!(actions.contains(&"run_node"));
        assert!(!actions.contains(&"wf_rules_compile_apply"));
    }

    #[test]
    fn merge_admin_executor_local_config_rejects_unknown_catalog_action() {
        let err = merge_admin_executor_local_config(
            None,
            &json!({
                "config": {
                    "catalog": {
                        "mode": "subset",
                        "actions": ["definitely_not_real"]
                    }
                }
            }),
        )
        .expect_err("invalid actions must fail");

        assert!(err.to_string().contains("invalid action"));
    }

    #[test]
    fn admin_executor_help_definition_uses_action_override() {
        let spec =
            resolve_internal_action_spec("get_admin_action_help").expect("help action present");
        let definition = build_admin_executor_function_definition(spec);

        assert_eq!(definition.name, "get_admin_action_help");
        assert_eq!(
            definition.parameters_json_schema["required"],
            json!(["action"])
        );
        assert!(definition.parameters_json_schema["properties"]["action"].is_object());
        assert!(definition.parameters_json_schema["properties"]["action_name"].is_null());
    }

    #[test]
    fn redact_executor_log_value_redacts_secret_like_fields() {
        let value = json!({
            "api_key": "sk-live",
            "nested": {
                "token": "tok-123",
                "safe": "ok"
            },
            "list": [
                { "password": "pw" },
                { "safe": "item" }
            ]
        });

        let redacted = redact_executor_log_value(&value);

        assert_eq!(redacted["api_key"], json!(NODE_SECRET_REDACTION_TOKEN));
        assert_eq!(
            redacted["nested"]["token"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(redacted["nested"]["safe"], json!("ok"));
        assert_eq!(
            redacted["list"][0]["password"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(redacted["list"][1]["safe"], json!("item"));
    }

    #[test]
    fn executor_plan_validation_accepts_runtime_plan() {
        let plan = parse_executor_plan(json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {
                "name": "pilot_route_setup",
                "target_hive": "motherbee"
            },
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "allow_help_lookup": true,
                "steps": [{
                    "id": "s1",
                    "action": "get_runtime",
                    "args": {
                        "hive": "motherbee",
                        "runtime": "ai.chat"
                    }
                }]
            }
        }))
        .expect("valid executor plan");

        assert_eq!(plan.kind, "executor_plan");
        assert_eq!(plan.execution.steps.len(), 1);
        assert_eq!(plan.execution.steps[0].action, "get_runtime");
    }

    #[test]
    fn executor_plan_validation_accepts_publish_runtime_lifecycle_plan() {
        let plan = parse_executor_plan(json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {
                "name": "publish-and-run-support-demo",
                "target_hive": "motherbee"
            },
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "allow_help_lookup": true,
                "steps": [
                    {
                        "id": "s1",
                        "action": "publish_runtime_package",
                        "args": {
                            "source": {
                                "kind": "bundle_upload",
                                "blob_path": "packages/incoming/ai-support-demo-0.1.0.zip"
                            },
                            "sync_to": ["worker-220"],
                            "update_to": ["worker-220"]
                        }
                    },
                    {
                        "id": "s2",
                        "action": "run_node",
                        "args": {
                            "hive": "worker-220",
                            "node_name": "AI.support.demo@worker-220",
                            "runtime": "ai.support.demo",
                            "runtime_version": "current",
                            "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
                        }
                    }
                ]
            }
        }))
        .expect("valid publish lifecycle executor plan");

        assert_eq!(plan.execution.steps.len(), 2);
        assert_eq!(plan.execution.steps[0].action, "publish_runtime_package");
        assert_eq!(plan.execution.steps[1].action, "run_node");
    }

    #[test]
    fn executor_plan_validation_accepts_run_node_tenant_id() {
        let plan = parse_executor_plan(json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {
                "name": "spawn-with-tenant",
                "target_hive": "motherbee"
            },
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "allow_help_lookup": true,
                "steps": [{
                    "id": "s1",
                    "action": "run_node",
                    "args": {
                        "hive": "motherbee",
                        "node_name": "AI.support.demo@motherbee",
                        "runtime": "ai.support.demo",
                        "runtime_version": "current",
                        "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
                    }
                }]
            }
        }))
        .expect("run_node tenant_id must be accepted by executor plan schema");

        assert_eq!(plan.execution.steps.len(), 1);
        assert_eq!(
            plan.execution.steps[0].args["tenant_id"],
            json!("tnt:43d576a3-d712-4d91-9245-5d5463dd693e")
        );
    }

    #[test]
    fn executor_plan_validation_rejects_unknown_action() {
        let err = parse_executor_plan(json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {},
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "allow_help_lookup": true,
                "steps": [{
                    "id": "s1",
                    "action": "definitely_not_real",
                    "args": {}
                }]
            }
        }))
        .expect_err("unknown action must fail");

        assert!(err.contains("unknown action"));
    }

    #[test]
    fn executor_plan_validation_rejects_schema_drift() {
        let err = parse_executor_plan(json!({
            "plan_version": "0.1",
            "kind": "executor_plan",
            "metadata": {},
            "execution": {
                "strict": true,
                "stop_on_error": true,
                "allow_help_lookup": true,
                "steps": [{
                    "id": "s1",
                    "action": "get_runtime",
                    "args": {
                        "target": "motherbee",
                        "runtime_name": "ai.chat"
                    }
                }]
            }
        }))
        .expect_err("drifted arg names must fail");

        assert!(err.contains("missing required field 'hive'"));
    }

    #[test]
    fn executor_step_argument_merge_rejects_changes_to_declared_values() {
        let step = AdminExecutorPlanStep {
            id: "s1".to_string(),
            action: "get_runtime".to_string(),
            args: json!({
                "hive": "motherbee",
                "runtime": "ai.chat"
            }),
            executor_fill: None,
        };

        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "hive": {"type": "string"},
                "runtime": {"type": "string"}
            },
            "required": ["hive", "runtime"]
        });

        let err = merge_executor_step_arguments(
            &step,
            &json!({
                "hive": "otherbee",
                "runtime": "ai.chat"
            }),
            &schema,
        )
        .expect_err("declared arg mutation must fail");

        assert!(err.contains("attempted to change declared arg 'hive'"));
    }

    #[test]
    fn executor_step_argument_merge_keeps_required_publish_source() {
        let step = AdminExecutorPlanStep {
            id: "s1".to_string(),
            action: "publish_runtime_package".to_string(),
            args: json!({
                "source": {
                    "kind": "bundle_upload",
                    "blob_path": "packages/incoming/demo.zip"
                },
                "sync_to": ["worker-220"],
                "update_to": ["worker-220"]
            }),
            executor_fill: None,
        };

        let schema = build_admin_executor_function_definition(
            resolve_internal_action_spec("publish_runtime_package").expect("action must exist"),
        )
        .parameters_json_schema;

        let merged = merge_executor_step_arguments(
            &step,
            &json!({
                "sync_to": ["worker-220"],
                "update_to": ["worker-220"],
                "set_current": null
            }),
            &schema,
        )
        .expect("merge must preserve declared required fields");

        validate_value_against_schema(&merged, &schema, "step 's1'")
            .expect("merged args must still satisfy publish schema");
        assert_eq!(
            merged["source"]["blob_path"],
            json!("packages/incoming/demo.zip")
        );
    }

    #[test]
    fn publish_runtime_package_action_is_registered() {
        let spec =
            resolve_internal_action_spec("publish_runtime_package").expect("action must exist");
        assert!(!spec.requires_target);
        assert_eq!(
            admin_action_path_patterns("publish_runtime_package"),
            vec!["POST /admin/runtime-packages/publish"]
        );
    }

    #[test]
    fn managed_lifecycle_actions_are_registered() {
        let start_spec = resolve_internal_action_spec("start_node").expect("start_node must exist");
        let restart_spec =
            resolve_internal_action_spec("restart_node").expect("restart_node must exist");

        assert!(start_spec.requires_target);
        assert!(restart_spec.requires_target);
        assert_eq!(
            admin_action_path_patterns("start_node"),
            vec!["POST /hives/{hive}/nodes/{node_name}/start"]
        );
        assert_eq!(
            admin_action_path_patterns("restart_node"),
            vec!["POST /hives/{hive}/nodes/{node_name}/restart"]
        );
    }

    #[test]
    fn normalize_publish_follow_up_hives_dedupes_and_trims() {
        let out = normalize_publish_follow_up_hives(
            &[
                " worker-220 ".to_string(),
                "worker-220".to_string(),
                "worker-221".to_string(),
            ],
            "sync_to",
        )
        .expect("normalize follow-up hives");
        assert_eq!(out, vec!["worker-220", "worker-221"]);
    }

    #[test]
    fn local_runtime_manifest_meta_from_file_returns_version_and_hash() {
        let dir = test_temp_dir("sy-admin-manifest-meta");
        let manifest_path = dir.join("manifest.json");
        fs::write(
            &manifest_path,
            "{\n  \"schema_version\": 1,\n  \"version\": 1711111111111,\n  \"runtimes\": {}\n}",
        )
        .expect("write manifest");

        let (version, hash) = local_runtime_manifest_meta_from_file(&manifest_path)
            .expect("load local manifest meta");
        assert_eq!(version, 1711111111111);
        assert_eq!(hash.len(), 64);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn remove_runtime_version_from_manifest_local_removes_non_current_version() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test.gov": {
                    "available": ["0.9.0", "1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let updated = remove_runtime_version_from_manifest_local(&manifest, "ai.test.gov", "0.9.0")
            .expect("remove");
        let entry = updated
            .runtimes
            .get("ai.test.gov")
            .expect("runtime entry must remain");
        assert_eq!(entry["current"], serde_json::json!("1.0.0"));
        assert_eq!(entry["available"], serde_json::json!(["1.0.0"]));
        assert!(updated.version > manifest.version);
    }

    #[test]
    fn remove_runtime_version_from_manifest_local_rejects_current_version() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test.gov": {
                    "available": ["1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let err = remove_runtime_version_from_manifest_local(&manifest, "ai.test.gov", "1.0.0")
            .expect_err("current delete must fail");
        assert_eq!(err, "RUNTIME_CURRENT_CONFLICT");
    }

    #[test]
    fn remove_runtime_version_from_manifest_local_allows_deleting_sole_current_runtime() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test.gov": {
                    "available": ["1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let updated = remove_runtime_version_from_manifest_local(&manifest, "ai.test.gov", "1.0.0")
            .expect("remove sole current");
        assert!(updated.runtimes.get("ai.test.gov").is_none());
        assert!(updated.version > manifest.version);
    }

    #[test]
    fn can_remove_current_runtime_version_local_only_when_sole_version() {
        let sole = RuntimeManifestEntry {
            available: vec!["1.0.0".to_string()],
            current: Some("1.0.0".to_string()),
            package_type: Some("full_runtime".to_string()),
            runtime_base: None,
            extra: BTreeMap::new(),
        };
        assert!(can_remove_current_runtime_version_local(&sole, "1.0.0"));

        let multiple = RuntimeManifestEntry {
            available: vec!["0.9.0".to_string(), "1.0.0".to_string()],
            current: Some("1.0.0".to_string()),
            package_type: Some("full_runtime".to_string()),
            runtime_base: None,
            extra: BTreeMap::new(),
        };
        assert!(!can_remove_current_runtime_version_local(
            &multiple, "1.0.0"
        ));
    }

    #[test]
    fn remove_runtime_version_from_manifest_local_accepts_legacy_runtime_casing() {
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-03-16T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.test.gov": {
                    "available": ["0.9.0", "1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };

        let updated = remove_runtime_version_from_manifest_local(&manifest, "AI.test.gov", "0.9.0")
            .expect("remove using legacy casing");
        let entry = updated
            .runtimes
            .get("ai.test.gov")
            .expect("runtime entry must remain");
        assert_eq!(entry["available"], serde_json::json!(["1.0.0"]));
    }

    #[test]
    fn materialize_inline_runtime_package_rejects_parent_dir_component() {
        let staging_root = test_temp_dir("sy-admin-inline-bad-path");
        let err = materialize_inline_runtime_package(
            &BTreeMap::from([("../escape".to_string(), "bad".to_string())]),
            &staging_root,
        )
        .expect_err("parent dir path must fail");
        assert!(err.contains("must not contain '..'"), "err={err}");
        let _ = fs::remove_dir_all(staging_root);
    }

    #[test]
    fn publish_runtime_package_inline_installs_runtime_and_updates_manifest() {
        let staging_root = test_temp_dir("sy-admin-inline-stage");
        let dist_root = test_temp_dir("sy-admin-inline-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({
                "ai.generic": {
                    "available": ["1.0.0"],
                    "current": "1.0.0",
                    "type": "full_runtime"
                }
            }),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let result = publish_runtime_package_inline(
            &BTreeMap::from([
                (
                    "package.json".to_string(),
                    "{\n  \"name\": \"ai.support.demo\",\n  \"version\": \"0.1.0\",\n  \"type\": \"config_only\",\n  \"runtime_base\": \"ai.generic\"\n}".to_string(),
                ),
                (
                    "config/default-config.json".to_string(),
                    "{\n  \"tenant_id\": \"tnt:demo\"\n}".to_string(),
                ),
                (
                    "assets/prompts/system.txt".to_string(),
                    "You are a support agent.".to_string(),
                ),
            ]),
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect("publish inline package");

        assert_eq!(result["source_kind"], json!("inline_package"));
        assert_eq!(result["runtime_name"], json!("ai.support.demo"));
        assert_eq!(result["runtime_version"], json!("0.1.0"));
        assert_eq!(result["package_type"], json!("config_only"));

        let installed_package = runtimes_root.join("ai.support.demo/0.1.0/package.json");
        assert!(installed_package.exists(), "installed package.json missing");

        let manifest_raw = fs::read_to_string(&manifest_path).expect("read manifest");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_raw).expect("parse manifest");
        assert_eq!(
            manifest_json["runtimes"]["ai.support.demo"]["current"],
            json!("0.1.0")
        );
        assert_eq!(
            manifest_json["runtimes"]["ai.support.demo"]["type"],
            json!("config_only")
        );
        assert_eq!(
            manifest_json["runtimes"]["ai.support.demo"]["runtime_base"],
            json!("ai.generic")
        );

        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn publish_runtime_package_inline_is_idempotent_for_same_contents() {
        let staging_root = test_temp_dir("sy-admin-inline-idempotent-stage");
        let dist_root = test_temp_dir("sy-admin-inline-idempotent-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let files = BTreeMap::from([
            (
                "package.json".to_string(),
                "{\n  \"name\": \"ai.support.demo\",\n  \"version\": \"0.1.0\",\n  \"type\": \"config_only\",\n  \"runtime_base\": \"ai.generic\"\n}".to_string(),
            ),
            (
                "config/default-config.json".to_string(),
                "{\n  \"tenant_id\": \"tnt:demo\"\n}".to_string(),
            ),
        ]);

        let first = publish_runtime_package_inline(
            &files,
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect("first inline publish");
        let second = publish_runtime_package_inline(
            &files,
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect("second inline publish should be idempotent");

        assert_eq!(first["runtime_name"], json!("ai.support.demo"));
        assert_eq!(second["runtime_name"], json!("ai.support.demo"));
        assert_eq!(second["runtime_version"], json!("0.1.0"));
        assert_eq!(second["copied_files"], json!(0));
        assert_eq!(second["copied_bytes"], json!(0));

        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn publish_runtime_package_inline_rejects_conflicting_existing_version() {
        let staging_root = test_temp_dir("sy-admin-inline-conflict-stage");
        let dist_root = test_temp_dir("sy-admin-inline-conflict-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        publish_runtime_package_inline(
            &BTreeMap::from([
                (
                    "package.json".to_string(),
                    "{\n  \"name\": \"ai.support.demo\",\n  \"version\": \"0.1.0\",\n  \"type\": \"config_only\",\n  \"runtime_base\": \"ai.generic\"\n}".to_string(),
                ),
                (
                    "config/default-config.json".to_string(),
                    "{\n  \"tenant_id\": \"tnt:demo\"\n}".to_string(),
                ),
            ]),
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect("first inline publish");

        let err = publish_runtime_package_inline(
            &BTreeMap::from([
                (
                    "package.json".to_string(),
                    "{\n  \"name\": \"ai.support.demo\",\n  \"version\": \"0.1.0\",\n  \"type\": \"config_only\",\n  \"runtime_base\": \"ai.generic\"\n}".to_string(),
                ),
                (
                    "config/default-config.json".to_string(),
                    "{\n  \"tenant_id\": \"tnt:changed\"\n}".to_string(),
                ),
            ]),
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect_err("changed contents must conflict");
        assert!(err.contains("different contents"), "err={err}");

        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn publish_runtime_package_bundle_rejects_missing_blob() {
        let blob_root = test_temp_dir("sy-admin-bundle-missing-blob");
        let staging_root = test_temp_dir("sy-admin-bundle-missing-stage");
        let dist_root = test_temp_dir("sy-admin-bundle-missing-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let err = publish_runtime_package_bundle(
            &blob_root,
            "packages/incoming/missing.zip",
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect_err("missing blob bundle must fail");
        assert!(err.contains("bundle blob not found"), "err={err}");

        let _ = fs::remove_dir_all(blob_root);
        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn publish_runtime_package_bundle_rejects_invalid_zip() {
        let blob_root = test_temp_dir("sy-admin-bundle-badzip-blob");
        let staging_root = test_temp_dir("sy-admin-bundle-badzip-stage");
        let dist_root = test_temp_dir("sy-admin-bundle-badzip-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let bundle_path = blob_root.join("packages/incoming/not-a-zip.zip");
        fs::create_dir_all(bundle_path.parent().expect("bundle parent")).expect("create blob dir");
        fs::write(&bundle_path, "this is not a zip").expect("write invalid zip");

        let err = publish_runtime_package_bundle(
            &blob_root,
            "packages/incoming/not-a-zip.zip",
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect_err("invalid zip must fail");
        assert!(err.contains("invalid zip"), "err={err}");
        assert!(bundle_path.exists(), "invalid zip should remain in blob");

        let _ = fs::remove_dir_all(blob_root);
        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn publish_runtime_package_bundle_rejects_invalid_layout() {
        let blob_root = test_temp_dir("sy-admin-bundle-layout-blob");
        let staging_root = test_temp_dir("sy-admin-bundle-layout-stage");
        let dist_root = test_temp_dir("sy-admin-bundle-layout-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let bundle_path = blob_root.join("packages/incoming/invalid-layout.zip");
        write_test_bundle(
            &bundle_path,
            &[
                ("root-a/package.json", "{\n  \"name\": \"ai.bad.demo\",\n  \"version\": \"0.1.0\",\n  \"type\": \"config_only\",\n  \"runtime_base\": \"ai.generic\"\n}", Some(0o644)),
                ("root-b/config/default-config.json", "{\n  \"tenant_id\": \"tnt:bad\"\n}", Some(0o644)),
            ],
        )
        .expect("write invalid layout bundle");

        let err = publish_runtime_package_bundle(
            &blob_root,
            "packages/incoming/invalid-layout.zip",
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect_err("invalid layout must fail");
        assert!(
            err.contains("single root directory") || err.contains("exactly one root directory"),
            "err={err}"
        );
        assert!(
            bundle_path.exists(),
            "invalid layout bundle should remain in blob"
        );

        let _ = fs::remove_dir_all(blob_root);
        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }

    #[test]
    fn publish_runtime_package_bundle_installs_runtime_and_removes_blob() {
        let blob_root = test_temp_dir("sy-admin-bundle-ok-blob");
        let staging_root = test_temp_dir("sy-admin-bundle-ok-stage");
        let dist_root = test_temp_dir("sy-admin-bundle-ok-dist");
        let runtimes_root = dist_root.join("runtimes");
        fs::create_dir_all(&runtimes_root).expect("create runtimes root");
        let manifest_path = runtimes_root.join("manifest.json");
        let manifest = RuntimeManifest {
            schema_version: 1,
            version: 1710000000000,
            updated_at: Some("2026-04-20T00:00:00Z".to_string()),
            runtimes: serde_json::json!({}),
            hash: None,
        };
        write_runtime_manifest_file_atomic(&manifest_path, &manifest, false)
            .expect("seed manifest");

        let bundle_path = blob_root.join("packages/incoming/ai-full-demo-0.2.0.zip");
        write_test_bundle(
            &bundle_path,
            &[
                ("ai.full.demo/package.json", "{\n  \"name\": \"ai.full.demo\",\n  \"version\": \"0.2.0\",\n  \"type\": \"full_runtime\",\n  \"entry_point\": \"bin/start.sh\"\n}", Some(0o644)),
                ("ai.full.demo/bin/start.sh", "#!/bin/sh\necho ready\n", Some(0o755)),
            ],
        )
        .expect("write valid full runtime bundle");

        let result = publish_runtime_package_bundle(
            &blob_root,
            "packages/incoming/ai-full-demo-0.2.0.zip",
            &staging_root,
            &runtimes_root,
            &manifest_path,
            true,
        )
        .expect("publish bundle package");

        assert_eq!(result["source_kind"], json!("bundle_upload"));
        assert_eq!(result["runtime_name"], json!("ai.full.demo"));
        assert_eq!(result["runtime_version"], json!("0.2.0"));
        assert_eq!(result["package_type"], json!("full_runtime"));

        let installed_start = runtimes_root.join("ai.full.demo/0.2.0/bin/start.sh");
        assert!(installed_start.exists(), "installed start.sh missing");
        let installed_mode = fs::metadata(&installed_start)
            .expect("read installed start metadata")
            .permissions()
            .mode();
        assert_eq!(
            installed_mode & 0o111,
            0o111,
            "start.sh must remain executable"
        );
        assert!(
            !bundle_path.exists(),
            "successful publish should remove blob bundle"
        );

        let manifest_raw = fs::read_to_string(&manifest_path).expect("read manifest");
        let manifest_json: serde_json::Value =
            serde_json::from_str(&manifest_raw).expect("parse manifest");
        assert_eq!(
            manifest_json["runtimes"]["ai.full.demo"]["current"],
            json!("0.2.0")
        );
        assert_eq!(
            manifest_json["runtimes"]["ai.full.demo"]["type"],
            json!("full_runtime")
        );

        let _ = fs::remove_dir_all(blob_root);
        let _ = fs::remove_dir_all(staging_root);
        let _ = fs::remove_dir_all(dist_root);
    }
}
