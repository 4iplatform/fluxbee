use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_ai_sdk::{
    build_model_user_content_parts, build_reply_message_runtime_src, build_text_response,
    create_function_calling_model, create_llm_client, extract_response_envelope, extract_text,
    resolve_model_input_from_payload_with_options, AiNode, AiProvider, FunctionCallingConfig,
    FunctionCallingRunner, FunctionRunInput, FunctionTool, FunctionToolDefinition,
    FunctionToolProvider, FunctionToolRegistry, HiveAiConfig, ImmediateConversationMemory,
    LanceDbThreadStateStore, Message, ModelInputOptions, ModelSettings, NodeRuntime,
    ResolvedModelInput, RuntimeConfig, ThreadStateStore, ThreadStateToolsProvider,
};
use fluxbee_sdk::protocol::{
    Destination, Meta, Routing, VaultSecretChangedPayload, VaultSecretInterest, MSG_TTL_EXCEEDED,
    MSG_UNREACHABLE, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    managed_node_name, IdentityError, NodeConfig, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, RpcError, VaultCallerOwned, VaultClient,
    MSG_ILK_REGISTER,
};
use gov_common::{
    frontdesk_contract::{
        frontdesk_result_payload, parse_frontdesk_handoff_payload, FrontdeskHandoffPayload,
        FrontdeskResultPayload,
    },
    gov_identity_config_from_env, identity_error_is_transient, identity_error_log_summary,
    identity_error_to_tool_payload, resolve_case_tenant, GovIdentityConfig, CASE_TENANT_MISMATCH,
    CASE_TENANT_MISSING, TENANT_ROOT_NOT_REGISTRABLE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::fs as tokio_fs;
use tokio::sync::{Mutex, OwnedMutexGuard, RwLock};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

const MSG_NODE_STATUS_GET: &str = "NODE_STATUS_GET";
const MSG_NODE_STATUS_GET_RESPONSE: &str = "NODE_STATUS_GET_RESPONSE";
const NODE_STATUS_DEFAULT_HANDLER_ENABLED: &str = "NODE_STATUS_DEFAULT_HANDLER_ENABLED";
const NODE_STATUS_DEFAULT_HEALTH_STATE: &str = "NODE_STATUS_DEFAULT_HEALTH_STATE";
const IMMEDIATE_INTERACTION_MAX_CHARS: usize = 1_200;
const FRONTDESK_DEFAULT_SYSTEM_PROMPT: &str = r#"
You are SY.frontdesk.gov.

Goal:
- Collect required identity fields: name, email.
- Use thread_state_* to keep progress.
- On positive confirmation, call ilk_register and close.

State schema (data):
{
  "status": "collecting|awaiting_confirmation|completed|completed_error",
  "collected": {
    "name": null|string,
    "email": null|string,
    "phone": null|string,
    "company_name": null|string
  },
  "register_attempted": false,
  "register_error": null
}

Mandatory flow:
1) On every user turn, call thread_state_get first.
2) Extract and merge any new values for name/email/phone/company_name.
3) If state changed, call thread_state_put immediately.
4) If any required field is missing, ask ONLY missing fields.
5) If all required fields are present and status is not completed/completed_error:
   - set status=awaiting_confirmation
   - call thread_state_put
   - show summary and ask confirmation.

Extraction rules:
- If message contains an email pattern, map it to email.
- If message mentions phone or celular, map it to phone when clear.
- If message mentions company/org/empresa, map it to company_name when clear.
- If only one field is missing, treat concise user answer as that field unless clearly contradictory.

Confirmation normalization:
- positive: si,si,ok,confirmo,correcto,asi es,de acuerdo,dale
- negative: no,incorrecto,corregir,hay error

STRICT NO-LOOP RULE:
- If status==awaiting_confirmation and user is positive:
  - If register_attempted==false:
    a) Call ilk_register with:
       - src_ilk (from context)
       - identity_candidate {name,email,phone,company_name}
    b) If tool returns status=ok:
       - set status=completed
       - call thread_state_put
       - send final success message (if merged=true: the person was already registered with that email, and this channel is now linked to that registration)
       - stop (do not ask confirmation again)
    c) If tool returns status=error:
       - set status=completed_error
       - set register_attempted=true
       - set register_error={error_code,message}
       - call thread_state_put
       - send final error summary
       - stop (do not ask confirmation again)
  - If register_attempted==true:
    - send final error summary from register_error
    - stop (do not ask confirmation again)

- If status==awaiting_confirmation and user is negative:
  - set status=collecting
  - call thread_state_put
  - ask which field to correct.

ANTI-REASK RULE:
- If thread_state already contains a non-empty value for a required field, do not ask for that field again unless the user is explicitly correcting it.
- If the user says they already provided the data, inspect thread_state and current turn extraction before asking for missing fields again.
- If all required fields are already present in thread_state, never ask for the data again; move to confirmation or registration as appropriate.

ANTI-RECONFIRM RULE:
- If status==awaiting_confirmation and the user reply is positive, do not restate the collected data and do not ask for confirmation again; call ilk_register immediately.
- If registration already succeeded or failed terminally in the current turn, stop after the final message.

Do not invent src_ilk.
Keep replies short in Spanish.
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenAiApiKeySource {
    /// Model D' — secret resolved via `fluxbee_sdk::resolve_resource(Openai)`
    /// (pool match against SY.vault). This is the only supported source.
    Vault,
    Missing,
}

impl OpenAiApiKeySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Vault => "vault",
            Self::Missing => "missing",
        }
    }
}

fn frontdesk_default_instructions() -> String {
    FRONTDESK_DEFAULT_SYSTEM_PROMPT.trim().to_string()
}

fn frontdesk_default_instructions_snapshot() -> Value {
    json!({
        "source": "inline",
        "value": frontdesk_default_instructions(),
        "trim": true
    })
}

#[derive(Debug, Deserialize)]
struct FrontdeskBootstrapHiveFile {
    hive_id: String,
    #[serde(default)]
    ai: Option<HiveAiConfig>,
}

#[derive(Debug)]
struct NodeSection {
    name: String,
    version: String,
    router_socket: String,
    uuid_persistence_dir: String,
    config_dir: String,
    dynamic_config_dir: String,
}

/// Runtime defaults materialized into the effective config (`materialize_runtime_defaults`).
#[derive(Debug)]
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
    immediate_memory: ImmediateMemorySection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct ImmediateMemorySection {
    enabled: bool,
    recent_interactions_max: usize,
    active_operations_max: usize,
    summary_max_chars: usize,
    summary_refresh_every_turns: usize,
    trim_noise_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct BehaviorCapabilities {
    #[serde(default)]
    multimodal: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RunnerModelSettings {
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveConfigDocument {
    #[serde(default)]
    node: Option<EffectiveNodeSection>,
    #[serde(default)]
    behavior: EffectiveBehaviorSection,
    #[serde(default)]
    runtime: Option<EffectiveRuntimeSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveNodeSection {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    router_socket: Option<String>,
    #[serde(default)]
    uuid_persistence_dir: Option<String>,
    #[serde(default)]
    config_dir: Option<String>,
    #[serde(default)]
    dynamic_config_dir: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct EffectiveBehaviorSection {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    params: Option<EffectiveBehaviorParams>,
    #[serde(default)]
    instructions: Option<Value>,
    #[serde(default)]
    model_settings: Option<RunnerModelSettings>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    capabilities: Option<BehaviorCapabilities>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveBehaviorParams {
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    temperature: Option<f32>,
    #[serde(default)]
    top_p: Option<f32>,
    #[serde(default)]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct EffectiveRuntimeSection {
    #[serde(default)]
    read_timeout_ms: Option<u64>,
    #[serde(default)]
    handler_timeout_ms: Option<u64>,
    #[serde(default)]
    write_timeout_ms: Option<u64>,
    #[serde(default)]
    queue_capacity: Option<usize>,
    #[serde(default)]
    worker_pool_size: Option<usize>,
    #[serde(default)]
    retry_max_attempts: Option<usize>,
    #[serde(default)]
    retry_initial_backoff_ms: Option<u64>,
    #[serde(default)]
    retry_max_backoff_ms: Option<u64>,
    #[serde(default)]
    metrics_log_interval_ms: Option<u64>,
    #[serde(default)]
    immediate_memory: Option<ImmediateMemorySection>,
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
            immediate_memory: ImmediateMemorySection::default(),
        }
    }
}

impl Default for ImmediateMemorySection {
    fn default() -> Self {
        Self {
            enabled: false,
            recent_interactions_max: 10,
            active_operations_max: 8,
            summary_max_chars: 1_600,
            summary_refresh_every_turns: 3,
            trim_noise_enabled: true,
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

fn default_multimodal_for_runtime() -> bool {
    false
}

#[derive(Debug, Clone)]
enum NodeBehavior {
    Echo,
    OpenAiChat(OpenAiChatRuntime),
}

#[derive(Debug, Clone)]
struct OpenAiChatRuntime {
    provider: AiProvider,
    model: String,
    instructions: Option<String>,
    model_settings: ModelSettings,
    base_url: Option<String>,
    immediate_memory: ImmediateMemorySection,
    multimodal: bool,
}

struct GenericAiNode {
    node_name: String,
    /// Deterministic self ILK resolved at boot via
    /// `fluxbee_sdk::identity::wait_for_self_system_ilk_id` from identity
    /// SHM (SY.frontdesk.gov is a system node listed in `hive.yaml`, not a
    /// dynamic spawn, so it does NOT use `FLUXBEE_NODE_ILK_ID` like AI.* /
    /// IO.* do). Used as `meta.src_ilk` for outgoing identity / vault calls.
    /// `None` if SHM lookup failed at boot (degraded).
    self_ilk_id: Option<String>,
    behavior: Arc<RwLock<Option<NodeBehavior>>>,
    thread_state_store: Option<Arc<dyn ThreadStateStore>>,
    immediate_memory_store: Option<Arc<ImmediateMemoryStore>>,
    gov_identity: GovIdentityConfig,
    gov_identity_bridge: Option<Arc<GovIdentityBridge>>,
    /// Where the tenant of a case comes from: the tenant SY.identity holds for its ILK.
    ilk_tenant: IlkTenantLookup,
    /// Vault accessor over the canonical `Arc<RouterDispatcher>`.
    /// `None` when `self_ilk_id` / hive suffix is missing (degraded boot).
    vault: Option<VaultClient>,
    control_plane: Arc<RwLock<ControlPlaneState>>,
}

/// The tenant SY.identity holds for an ILK, `None` when it cannot be read. Production reads the
/// identity SHM (`identity_shm_ilk_tenant`); tests stub it.
type IlkTenantLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Reads the identity SHM of this node's hive, which SY.identity writes before it answers the
/// ILK_PROVISION that created the temporary ILK.
fn identity_shm_ilk_tenant(node_name: &str) -> IlkTenantLookup {
    let hive_id = node_name
        .split('@')
        .nth(1)
        .map(str::trim)
        .filter(|hive_id| !hive_id.is_empty())
        .map(ToString::to_string);
    Arc::new(move |ilk_id: &str| {
        let hive_id = hive_id.as_deref()?;
        match fluxbee_sdk::identity::list_ilks_from_hive_id(hive_id) {
            Ok(snapshot) => snapshot
                .ilks
                .into_iter()
                .find(|ilk| ilk.ilk_id == ilk_id)
                .map(|ilk| ilk.tenant_id),
            Err(err) => {
                tracing::warn!(
                    ilk_id,
                    error = %err,
                    "identity SHM unreadable: the tenant of the case is unknown"
                );
                None
            }
        }
    })
}

/// The tenant of a case (`resolve_case_tenant`): the one its ILK has, checked against the tenant a
/// handoff informed.
fn case_tenant_of(
    ilk_tenant: &IlkTenantLookup,
    src_ilk: Option<&str>,
    informed_tenant: Option<&str>,
) -> Result<String, &'static str> {
    let tenant = src_ilk
        .map(str::trim)
        .filter(|ilk_id| !ilk_id.is_empty())
        .and_then(|ilk_id| ilk_tenant(ilk_id));
    resolve_case_tenant(tenant.as_deref(), informed_tenant)
}

/// The tool's answer when the case has no tenant to register into (unknown, contradicted, or the
/// root tenant): no ILK_REGISTER is sent and no tenant is created.
fn case_tenant_error_payload(error_code: &str) -> Value {
    let message = if error_code == CASE_TENANT_MISMATCH {
        "the tenant informed with the case is not the tenant of its ILK; nothing was registered"
    } else if error_code == TENANT_ROOT_NOT_REGISTRABLE {
        "the case belongs to the root tenant, where nobody registers; nothing was registered and the ILK stays temporary"
    } else {
        "the tenant of the case is unknown (its ILK's tenant could not be read); nothing was registered and no tenant was created"
    };
    json!({
        "status": "error",
        "error_code": error_code,
        "message": message,
        "retryable": false
    })
}

fn missing_src_ilk_payload() -> Value {
    json!({
        "status": "error",
        "error_code": "missing_src_ilk",
        "message": "src_ilk is required",
        "retryable": false
    })
}

struct GovIdentityBridge {
    dispatcher: Arc<RouterDispatcher>,
}

impl GovIdentityBridge {
    fn new(dispatcher: Arc<RouterDispatcher>) -> Self {
        Self { dispatcher }
    }

    async fn call_ok(
        &self,
        identity: &GovIdentityConfig,
        action: &str,
        payload: Value,
    ) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, IdentityError> {
        let first = self
            .send_action_once(&identity.target, action, payload.clone(), identity.timeout)
            .await;
        // One retry on the fallback target when the target is not the identity primary or is not
        // on the router. The fallback's reply goes through the same status check as the first one.
        let use_fallback = match &first {
            Ok(out) => {
                out.payload.get("status").and_then(Value::as_str) == Some("error")
                    && out.payload.get("error_code").and_then(Value::as_str) == Some("NOT_PRIMARY")
            }
            Err(IdentityError::Unreachable { reason, .. }) => reason == "NODE_NOT_FOUND",
            Err(_) => false,
        };
        let reply = match identity.fallback_target.as_deref() {
            Some(fallback)
                if use_fallback && !fallback.trim().is_empty() && fallback != identity.target =>
            {
                self.send_action_once(fallback, action, payload, identity.timeout)
                    .await
            }
            _ => first,
        };
        identity_reply_outcome(action, reply?)
    }

    async fn send_action_once(
        &self,
        target: &str,
        action: &str,
        payload: Value,
        timeout: Duration,
    ) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, IdentityError> {
        let trace_id = Uuid::new_v4().to_string();
        let req = Message {
            routing: Routing {
                src: String::new(),
                src_l2_name: None,
                dst: Destination::Unicast(target.to_string()),
                ttl: 16,
                trace_id: trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(action.to_string()),
                ..Meta::default()
            },
            payload,
        };
        let expected_msg = format!("{action}_RESPONSE");
        let matcher = fluxbee_sdk::PendingMatcher::new(
            vec![fluxbee_sdk::RouteMatch::exact(SYSTEM_KIND, &expected_msg)],
            vec![
                fluxbee_sdk::RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
                fluxbee_sdk::RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
            ],
            vec![fluxbee_sdk::RouteMatch::any_msg_type(SYSTEM_KIND)],
        );
        let labels = fluxbee_sdk::RpcRequestLabels::new(target, action, expected_msg.clone());
        let msg = self
            .dispatcher
            .send_with_matcher(req, matcher, labels, timeout)
            .await
            .map_err(map_rpc_error_to_identity)?;
        Self::parse_identity_reply(msg, &expected_msg, target, trace_id)
    }

    fn parse_identity_reply(
        msg: Message,
        expected_msg: &str,
        target: &str,
        trace_id: String,
    ) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, IdentityError> {
        if msg.meta.msg.as_deref() == Some(expected_msg) {
            return Ok(fluxbee_sdk::IdentitySystemResult {
                payload: msg.payload,
                effective_target: target.to_string(),
                trace_id,
            });
        }
        if msg.meta.msg.as_deref() == Some(MSG_UNREACHABLE) {
            let original_dst = msg
                .payload
                .get("original_dst")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reason = msg
                .payload
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            return Err(IdentityError::Unreachable {
                reason: reason.to_string(),
                original_dst: original_dst.to_string(),
            });
        }
        if msg.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED) {
            let original_dst = msg
                .payload
                .get("original_dst")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let last_hop = msg
                .payload
                .get("last_hop")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(IdentityError::TtlExceeded {
                original_dst: original_dst.to_string(),
                last_hop: last_hop.to_string(),
            });
        }
        Err(IdentityError::InvalidResponse(format!(
            "invalid identity response: expected {expected_msg} trace_id={trace_id}, got msg={:?}",
            msg.meta.msg
        )))
    }
}

/// SY.identity answered: `status:"ok"` is the success; any other status is a rejection carrying
/// SY.identity's own `error_code` verbatim, so the caller classifies the code instead of guessing
/// it from text.
fn identity_reply_outcome(
    action: &str,
    out: fluxbee_sdk::IdentitySystemResult,
) -> std::result::Result<fluxbee_sdk::IdentitySystemResult, IdentityError> {
    if out.payload.get("status").and_then(Value::as_str) == Some("ok") {
        return Ok(out);
    }
    Err(IdentityError::SystemRejected {
        action: action.to_string(),
        error_code: out
            .payload
            .get("error_code")
            .and_then(Value::as_str)
            .unwrap_or("UNKNOWN")
            .to_string(),
        message: out
            .payload
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("identity returned non-ok status")
            .to_string(),
    })
}

/// Same mapping as SY.orchestrator's identity calls.
fn map_rpc_error_to_identity(err: RpcError) -> IdentityError {
    match err {
        RpcError::Node(err) => IdentityError::Node(err),
        RpcError::Unreachable {
            reason,
            original_dst,
        } => IdentityError::Unreachable {
            reason,
            original_dst,
        },
        RpcError::TtlExceeded {
            original_dst,
            last_hop,
        } => IdentityError::TtlExceeded {
            original_dst,
            last_hop,
        },
        RpcError::Timeout {
            trace_id,
            target,
            request_msg,
            timeout_ms,
            ..
        } => IdentityError::ActionTimeout {
            action: request_msg,
            trace_id,
            target,
            timeout_ms,
        },
        RpcError::InvalidRequest(message) => IdentityError::InvalidRequest(message),
        other => IdentityError::InvalidResponse(other.to_string()),
    }
}

fn identity_bridge_missing() -> IdentityError {
    IdentityError::InvalidRequest("identity bridge not initialized".to_string())
}

#[derive(Debug, Clone, Deserialize)]
struct IlkRegisterIdentityCandidate {
    name: String,
    email: String,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    company_name: Option<String>,
    /// Free-form extras (structured data Cloud supplies for a human). Stored verbatim into the
    /// ilk's free-form JSONB `identification.attributes` — future ad-hoc fields go here.
    #[serde(default)]
    attributes: Option<Value>,
}

/// What the LLM (or the handoff path) passes. No tenant: it is the case's, never the person's.
#[derive(Debug, Clone, Deserialize)]
struct IlkRegisterArgs {
    src_ilk: String,
    identity_candidate: IlkRegisterIdentityCandidate,
}

#[derive(Clone)]
struct IlkRegisterTool {
    scoped_src_ilk: Option<String>,
    /// The tenant a `frontdesk_handoff` informed; it only has to agree with the ILK's tenant.
    /// `None` on the conversational path.
    informed_tenant_id: Option<String>,
    ilk_tenant: IlkTenantLookup,
    identity: GovIdentityConfig,
    bridge: Option<Arc<GovIdentityBridge>>,
}

#[derive(Debug, Clone)]
struct BehaviorContext {
    thread_id: Option<String>,
    src_ilk: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FrontdeskThreadState {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    collected: FrontdeskCollectedState,
    #[serde(default)]
    tenant_id: Option<String>,
    #[serde(default)]
    registration_status: Option<String>,
    #[serde(default)]
    register_attempted: bool,
    #[serde(default)]
    register_error: Option<FrontdeskRegisterErrorState>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FrontdeskCollectedState {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    phone: Option<String>,
    #[serde(default)]
    company_name: Option<String>,
    #[serde(default)]
    attributes: Option<serde_json::Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct FrontdeskRegisterErrorState {
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedImmediateMemoryRecord {
    #[serde(default)]
    summary: Option<fluxbee_ai_sdk::ConversationSummary>,
    #[serde(default)]
    recent_interactions: Vec<fluxbee_ai_sdk::ImmediateInteraction>,
    updated_at: String,
}

#[derive(Debug, Clone)]
struct ImmediateMemoryStore {
    root_dir: PathBuf,
    key_gates: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl ImmediateMemoryStore {
    fn path_for_node(state_dir: &std::path::Path, node_name: &str) -> PathBuf {
        state_dir
            .join("ai-nodes")
            .join(sanitize_storage_key(node_name))
            .join("immediate-memory")
    }

    fn new(root_dir: impl Into<PathBuf>) -> Self {
        Self {
            root_dir: root_dir.into(),
            key_gates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn root_dir(&self) -> &std::path::Path {
        &self.root_dir
    }

    fn records_dir(&self) -> PathBuf {
        self.root_dir.join("threads")
    }

    fn key_file_path(&self, key: &str) -> PathBuf {
        self.records_dir()
            .join(format!("{}.json", sanitize_storage_key(key)))
    }

    async fn ensure_ready(&self) -> fluxbee_ai_sdk::Result<()> {
        tokio_fs::create_dir_all(self.records_dir())
            .await
            .map_err(|err| {
                fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                    "immediate memory init failed: {err}"
                ))
            })?;
        Ok(())
    }

    async fn lock_key(&self, key: &str) -> OwnedMutexGuard<()> {
        let safe = sanitize_storage_key(key);
        let gate = {
            let mut gates = self.key_gates.lock().await;
            gates
                .entry(safe)
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        gate.lock_owned().await
    }

    async fn get(
        &self,
        key: &str,
    ) -> fluxbee_ai_sdk::Result<Option<PersistedImmediateMemoryRecord>> {
        if key.trim().is_empty() {
            return Ok(None);
        }
        let _guard = self.lock_key(key).await;
        let path = self.key_file_path(key);
        let raw = match tokio_fs::read_to_string(&path).await {
            Ok(v) => v,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                    "immediate memory read failed: {err}"
                )))
            }
        };
        let parsed = serde_json::from_str::<PersistedImmediateMemoryRecord>(&raw)?;
        Ok(Some(parsed))
    }

    async fn put(
        &self,
        key: &str,
        record: &PersistedImmediateMemoryRecord,
    ) -> fluxbee_ai_sdk::Result<()> {
        if key.trim().is_empty() {
            return Ok(());
        }
        let _guard = self.lock_key(key).await;
        self.ensure_ready().await?;
        let path = self.key_file_path(key);
        let raw = serde_json::to_string_pretty(record)?;
        tokio_fs::write(path, raw).await.map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "immediate memory write failed: {err}"
            ))
        })?;
        Ok(())
    }
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
        if is_vault_secret_changed(&msg) {
            self.handle_vault_secret_changed(&msg).await;
            return Ok(None);
        }
        if is_control_plane(&msg) {
            return self.handle_control_plane(msg).await;
        }
        let behavior_ctx = BehaviorContext {
            thread_id: extract_thread_id(&msg),
            src_ilk: extract_src_ilk(&msg),
        };
        // The human-registration flow has TWO methods, distinguished by the METHOD (not by being a
        // human): the "auto" method arrives as a structured JSON `frontdesk_handoff` and runs
        // DETERMINISTICALLY (ILK_REGISTER, no LLM); the conversational method is a human chatting so
        // the frontdesk's LLM collects the data. The deterministic method must run even when the node
        // has no behavior configured — a Cloud register_human cannot depend on an LLM being set up —
        // so it is handled HERE, BEFORE the Configured/behavior gate that guards the LLM path.
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            if let Some(handoff) = parse_frontdesk_handoff_payload(&msg.payload) {
                tracing::info!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    operation = %handoff.operation,
                    "frontdesk: structured handoff → deterministic path (no LLM required)"
                );
                return Ok(Some(
                    self.handle_frontdesk_handoff(&msg, &behavior_ctx, handoff)
                        .await?,
                ));
            }
            // A case of the root tenant can never be registered (nobody registers there), so the
            // conversation does not start: the answer is final and needs neither the LLM nor
            // SY.identity.
            if let Some(result) = self.root_tenant_case_result(&behavior_ctx) {
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    src_ilk = ?behavior_ctx.src_ilk,
                    error_code = TENANT_ROOT_NOT_REGISTRABLE,
                    "frontdesk: a case of the root tenant (ilk NOT registered, stays temporary)"
                );
                return Ok(Some(build_frontdesk_result_reply(&msg, result)?));
            }
        }
        // Everything past here is the conversational/LLM method, which DOES require the node
        // Configured (a behavior/LLM) and a thread_id to track the data-collection dialogue.
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            let state = self.control_plane.read().await.current_state;
            if state != NodeLifecycleState::Configured {
                let payload = node_not_configured_payload(state);
                return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
            }
            if extract_thread_id(&msg).is_none() {
                let payload = invalid_payload_missing_thread_id();
                return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
            }
            // A user message that LOOKS like a handoff (type/subject/operation) but did NOT parse
            // fell through to the conversational path — surface it loudly, else a Cloud register_human
            // whose shape is off would be silently chatted at by the LLM instead of registered.
            let looks_like_handoff = msg
                .payload
                .as_object()
                .map(|o| {
                    o.contains_key("type")
                        || o.contains_key("subject")
                        || o.contains_key("operation")
                })
                .unwrap_or(false);
            if looks_like_handoff {
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    payload_keys = ?msg
                        .payload
                        .as_object()
                        .map(|o| o.keys().cloned().collect::<Vec<_>>()),
                    "frontdesk: payload looks like a handoff but did NOT parse → conversational path (check the handoff shape)"
                );
            }
            let src_ilk_source = src_ilk_source(&msg);
            if behavior_ctx.src_ilk.is_none() {
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    src_ilk_source = src_ilk_source,
                    "missing src_ilk in incoming user message"
                );
            } else {
                tracing::debug!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    src_ilk_source = src_ilk_source,
                    "resolved src_ilk in incoming user message"
                );
            }
        }

        let behavior = self.behavior.read().await.clone();
        let Some(behavior) = behavior else {
            let payload = node_runtime_not_ready_payload();
            return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
        };

        let (input, resolved_user_input): (String, Option<ResolvedModelInput>) = if msg
            .meta
            .msg_type
            .eq_ignore_ascii_case("user")
        {
            let options = ModelInputOptions {
                multimodal: matches!(&behavior, NodeBehavior::OpenAiChat(openai) if openai.multimodal),
                ..ModelInputOptions::default()
            };
            match resolve_model_input_from_payload_with_options(&msg.payload, &options).await {
                Ok(value) => (value.prompt_text.clone(), Some(value)),
                Err(err) => {
                    return Ok(Some(build_reply_message_runtime_src(
                        &msg,
                        err.to_error_payload(),
                    )))
                }
            }
        } else {
            (extract_text(&msg.payload).unwrap_or_default(), None)
        };
        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            // No text preview and no sender id: the messages a frontdesk receives are the person's
            // own identity data (name, email, phone).
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                src_ilk = ?behavior_ctx.src_ilk,
                sender_kind = ?incoming_sender_kind(&msg),
                thread_id = ?behavior_ctx.thread_id,
                input_len = input.len(),
                "incoming user message"
            );
        }
        let prior_frontdesk_state = if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            Some(self.load_frontdesk_thread_state(&behavior_ctx).await?)
        } else {
            None
        };
        let output = match &behavior {
            NodeBehavior::Echo => format!("Echo: {input}"),
            NodeBehavior::OpenAiChat(openai) => {
                let input_parts = if openai.multimodal {
                    if let Some(resolved) = resolved_user_input.as_ref() {
                        match build_model_user_content_parts(resolved).await {
                            Ok(parts) => Some(parts),
                            Err(err) => {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    error = %err,
                                    "failed to build structured user input parts; replying with canonical attachment error payload"
                                );
                                return Ok(Some(build_reply_message_runtime_src(
                                    &msg,
                                    err.to_error_payload(),
                                )));
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };
                match self
                    .run_ai_chat(openai, input, input_parts, &behavior_ctx)
                    .await
                {
                    Ok(output) => output,
                    Err(err) if err.to_string().contains("missing AI api key") => {
                        tracing::warn!(
                            node_name = %self.node_name,
                            trace_id = %msg.routing.trace_id,
                            error = %err,
                            "AI runtime missing api key; replying with runtime-not-ready payload"
                        );
                        let payload = missing_ai_api_key_payload(openai.provider);
                        return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
                    }
                    Err(err) => {
                        let attachment_summary =
                            attachment_summary_for_observability(resolved_user_input.as_ref());
                        if let fluxbee_ai_sdk::errors::AiSdkError::Protocol(msg_text) = &err {
                            if let Some((status, detail)) = parse_openai_status_error(msg_text) {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    model = %openai.model,
                                    provider_status = status,
                                    provider_param = ?extract_openai_error_param(msg_text),
                                    provider_detail = %trim_chars(&detail, 280),
                                    attachment_count = attachment_summary.count,
                                    attachment_total_bytes = attachment_summary.total_bytes,
                                    attachment_mimes = ?attachment_summary.mimes,
                                    error = %err,
                                    "AI runtime request failed with structured provider status; replying with provider error payload"
                                );
                            } else {
                                tracing::warn!(
                                    node_name = %self.node_name,
                                    trace_id = %msg.routing.trace_id,
                                    model = %openai.model,
                                    attachment_count = attachment_summary.count,
                                    attachment_total_bytes = attachment_summary.total_bytes,
                                    attachment_mimes = ?attachment_summary.mimes,
                                    error = %err,
                                    "AI runtime request failed; replying with provider error payload"
                                );
                            }
                        } else {
                            tracing::warn!(
                                node_name = %self.node_name,
                                trace_id = %msg.routing.trace_id,
                                model = %openai.model,
                                attachment_count = attachment_summary.count,
                                attachment_total_bytes = attachment_summary.total_bytes,
                                attachment_mimes = ?attachment_summary.mimes,
                                error = %err,
                                "AI runtime request failed; replying with provider error payload"
                            );
                        }
                        let payload = ai_runtime_error_payload(&err);
                        return Ok(Some(build_reply_message_runtime_src(&msg, payload)));
                    }
                }
            }
        };

        if msg.meta.msg_type.eq_ignore_ascii_case("user") {
            let payload = self
                .build_frontdesk_result_for_conversation(
                    &behavior_ctx,
                    output,
                    prior_frontdesk_state.flatten(),
                )
                .await?;
            return Ok(Some(build_frontdesk_result_reply(&msg, payload)?));
        }

        let payload = build_text_response(output)?;
        Ok(Some(build_reply_message_runtime_src(&msg, payload)))
    }
}

impl GenericAiNode {
    async fn run_ai_chat(
        &self,
        openai: &OpenAiChatRuntime,
        input: String,
        input_parts: Option<Vec<fluxbee_ai_sdk::ModelContentPart>>,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<String> {
        let api_key = self
            .resolve_ai_api_key(openai.provider)
            .await
            .ok_or_else(|| {
                fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                    "missing AI api key in SY.vault resource_type={}",
                    openai.provider
                ))
            })?;
        let client = create_llm_client(openai.provider, api_key.clone(), openai.base_url.clone());
        let tool_registry = self.build_tool_registry(ctx)?;
        if !tool_registry.definitions().is_empty() {
            let model = create_function_calling_model(
                openai.provider,
                api_key,
                openai.base_url.clone(),
                openai.model.clone(),
                openai.instructions.clone(),
                openai.model_settings.clone(),
            );
            let runner = FunctionCallingRunner::new(FunctionCallingConfig::default());
            let immediate_memory = self.load_immediate_memory_for_input(openai, ctx).await;
            let run_input = self.build_function_run_input(
                input.clone(),
                input_parts.clone(),
                ctx,
                openai,
                immediate_memory,
            );
            let result = runner
                .run_with_input(model.as_ref(), &tool_registry, run_input)
                .await?;
            if let Some(text) = result.final_assistant_text {
                self.persist_immediate_turn(openai, ctx, &input, &text)
                    .await;
                return Ok(text);
            }
        }

        let current_user_input = input.clone();
        let req = fluxbee_ai_sdk::llm::LlmRequest {
            model: openai.model.clone(),
            system: openai.instructions.clone(),
            input,
            input_parts,
            output_schema: None,
            max_output_tokens: None,
            model_settings: Some(openai.model_settings.clone()),
        };
        let response = fluxbee_ai_sdk::llm::LlmClient::generate(client.as_ref(), req).await?;
        self.persist_immediate_turn(openai, ctx, &current_user_input, &response.content)
            .await;
        Ok(response.content)
    }

    fn build_function_run_input(
        &self,
        input: String,
        input_parts: Option<Vec<fluxbee_ai_sdk::ModelContentPart>>,
        ctx: &BehaviorContext,
        openai: &OpenAiChatRuntime,
        immediate_memory: Option<ImmediateConversationMemory>,
    ) -> FunctionRunInput {
        if !openai.immediate_memory.enabled {
            return FunctionRunInput {
                current_user_message: input,
                current_user_parts: input_parts,
                immediate_memory: None,
            };
        }
        FunctionRunInput {
            current_user_message: input,
            current_user_parts: input_parts,
            immediate_memory: immediate_memory.or_else(|| {
                Some(ImmediateConversationMemory {
                    thread_id: ctx.thread_id.clone(),
                    scope_id: ctx.src_ilk.clone(),
                    summary: None,
                    recent_interactions: Vec::new(),
                    active_operations: Vec::new(),
                })
            }),
        }
    }

    async fn load_immediate_memory_for_input(
        &self,
        openai: &OpenAiChatRuntime,
        ctx: &BehaviorContext,
    ) -> Option<ImmediateConversationMemory> {
        if !openai.immediate_memory.enabled {
            return None;
        }
        let src_ilk = ctx.src_ilk.as_deref()?;
        let store = self.immediate_memory_store.as_ref()?;
        let record = match store.get(src_ilk).await {
            Ok(value) => value,
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    src_ilk = %src_ilk,
                    thread_id = ?ctx.thread_id,
                    error = %err,
                    "immediate memory get failed; continuing without persisted context"
                );
                None
            }
        };

        let (summary, recent_interactions) = if let Some(mut record) = record {
            record.summary = record
                .summary
                .map(|summary| trim_summary(summary, openai.immediate_memory.summary_max_chars));
            record.recent_interactions = prune_recent_interactions(
                record.recent_interactions,
                openai.immediate_memory.recent_interactions_max,
            );
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                memory_hit = true,
                recent_interactions = record.recent_interactions.len(),
                recent_interactions_max = openai.immediate_memory.recent_interactions_max,
                active_operations_max = openai.immediate_memory.active_operations_max,
                summary_max_chars = openai.immediate_memory.summary_max_chars,
                summary_refresh_status = "not_implemented_v1",
                "immediate memory loaded"
            );
            (record.summary, record.recent_interactions)
        } else {
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                memory_hit = false,
                recent_interactions_max = openai.immediate_memory.recent_interactions_max,
                active_operations_max = openai.immediate_memory.active_operations_max,
                summary_max_chars = openai.immediate_memory.summary_max_chars,
                summary_refresh_status = "not_implemented_v1",
                "immediate memory loaded"
            );
            (None, Vec::new())
        };

        Some(ImmediateConversationMemory {
            thread_id: ctx.thread_id.clone(),
            scope_id: ctx.src_ilk.clone(),
            summary,
            recent_interactions,
            active_operations: Vec::new(),
        })
    }

    async fn persist_immediate_turn(
        &self,
        openai: &OpenAiChatRuntime,
        ctx: &BehaviorContext,
        user_input: &str,
        assistant_output: &str,
    ) {
        if !openai.immediate_memory.enabled {
            return;
        }
        let Some(src_ilk) = ctx.src_ilk.as_deref() else {
            return;
        };
        let Some(store) = self.immediate_memory_store.as_ref() else {
            return;
        };

        let mut record = match store.get(src_ilk).await {
            Ok(Some(record)) => record,
            Ok(None) => PersistedImmediateMemoryRecord::default(),
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    src_ilk = %src_ilk,
                    thread_id = ?ctx.thread_id,
                    error = %err,
                    "immediate memory get-before-put failed; skipping persistence"
                );
                return;
            }
        };
        record.summary = record
            .summary
            .map(|summary| trim_summary(summary, openai.immediate_memory.summary_max_chars));
        record
            .recent_interactions
            .push(fluxbee_ai_sdk::ImmediateInteraction {
                role: fluxbee_ai_sdk::ImmediateRole::User,
                kind: fluxbee_ai_sdk::ImmediateInteractionKind::Text,
                content: trim_chars(user_input, IMMEDIATE_INTERACTION_MAX_CHARS),
            });
        record
            .recent_interactions
            .push(fluxbee_ai_sdk::ImmediateInteraction {
                role: fluxbee_ai_sdk::ImmediateRole::Assistant,
                kind: fluxbee_ai_sdk::ImmediateInteractionKind::Text,
                content: trim_chars(assistant_output, IMMEDIATE_INTERACTION_MAX_CHARS),
            });
        record.recent_interactions = prune_recent_interactions(
            record.recent_interactions,
            openai.immediate_memory.recent_interactions_max,
        );
        record.updated_at = chrono::Utc::now().to_rfc3339();

        if let Err(err) = store.put(src_ilk, &record).await {
            tracing::warn!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                error = %err,
                "immediate memory put failed; continuing without persistence"
            );
        } else {
            tracing::debug!(
                node_name = %self.node_name,
                src_ilk = %src_ilk,
                thread_id = ?ctx.thread_id,
                persisted_recent_interactions = record.recent_interactions.len(),
                recent_interactions_max = openai.immediate_memory.recent_interactions_max,
                summary_refresh_status = "not_implemented_v1",
                "immediate memory persisted"
            );
        }
    }

    fn build_tool_registry(
        &self,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<FunctionToolRegistry> {
        let mut registry = FunctionToolRegistry::new();
        self.register_common_tools(&mut registry, ctx)?;
        self.register_gov_tools(&mut registry, ctx)?;
        Ok(registry)
    }

    fn register_common_tools(
        &self,
        registry: &mut FunctionToolRegistry,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<()> {
        // Thread state tools remain the source for node-level "hard state".
        // In scoped AI runtimes the canonical key is src_ilk; thread_id is
        // conversational metadata only.
        // Immediate memory is managed separately by the runner as short-horizon context.
        if let (Some(store), Some(src_ilk)) = (&self.thread_state_store, &ctx.src_ilk) {
            let provider = ThreadStateToolsProvider::with_get_put_delete_scoped(
                store.clone(),
                src_ilk.clone(),
            );
            provider.register_tools(registry)?;
        }
        Ok(())
    }

    fn register_gov_tools(
        &self,
        registry: &mut FunctionToolRegistry,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<()> {
        // Conversational: the tenant is only the ILK's (the thread state is the LLM's to write).
        let tool = IlkRegisterTool {
            scoped_src_ilk: ctx.src_ilk.clone(),
            informed_tenant_id: None,
            ilk_tenant: self.ilk_tenant.clone(),
            identity: self.gov_identity.clone(),
            bridge: self.gov_identity_bridge.clone(),
        };
        registry.register(Arc::new(tool))?;
        Ok(())
    }

    async fn load_frontdesk_thread_state(
        &self,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<Option<FrontdeskThreadState>> {
        let (Some(store), Some(src_ilk)) = (&self.thread_state_store, ctx.src_ilk.as_deref())
        else {
            return Ok(None);
        };
        let record = store.get(src_ilk).await?;
        let Some(record) = record else {
            return Ok(None);
        };
        let state = serde_json::from_value::<FrontdeskThreadState>(record.data).map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "invalid_frontdesk_thread_state: {err}"
            ))
        })?;
        Ok(Some(state))
    }

    async fn store_frontdesk_thread_state(
        &self,
        ctx: &BehaviorContext,
        state: &FrontdeskThreadState,
    ) -> fluxbee_ai_sdk::Result<()> {
        let (Some(store), Some(src_ilk)) = (&self.thread_state_store, ctx.src_ilk.as_deref())
        else {
            return Ok(());
        };
        let payload = serde_json::to_value(state).map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "serialize_frontdesk_thread_state_failed: {err}"
            ))
        })?;
        store.put(src_ilk, payload, None).await
    }

    async fn delete_frontdesk_thread_state(
        &self,
        ctx: &BehaviorContext,
    ) -> fluxbee_ai_sdk::Result<()> {
        let (Some(store), Some(src_ilk)) = (&self.thread_state_store, ctx.src_ilk.as_deref())
        else {
            return Ok(());
        };
        store.delete(src_ilk).await
    }

    /// The final answer to a case of the root tenant, where nobody registers. `None` for any other
    /// case, including one whose tenant cannot be read (its registration answers that).
    fn root_tenant_case_result(&self, ctx: &BehaviorContext) -> Option<FrontdeskResultPayload> {
        match case_tenant_of(&self.ilk_tenant, ctx.src_ilk.as_deref(), None) {
            Err(TENANT_ROOT_NOT_REGISTRABLE) => {
                Some(build_frontdesk_result_from_register_response(
                    &case_tenant_error_payload(TENANT_ROOT_NOT_REGISTRABLE),
                    ctx.src_ilk.clone(),
                ))
            }
            _ => None,
        }
    }

    async fn handle_frontdesk_handoff(
        &self,
        msg: &Message,
        ctx: &BehaviorContext,
        handoff: FrontdeskHandoffPayload,
    ) -> fluxbee_ai_sdk::Result<Message> {
        let mut result = frontdesk_result_payload(
            "error",
            "INVALID_REQUEST",
            "No pude procesar el handoff de registro.",
        );
        result.error_code = Some("invalid_request".to_string());
        result.error_detail = Some("Unsupported handoff payload".to_string());
        result.ilk_id = ctx.src_ilk.clone();

        if handoff.operation != "complete_registration" {
            result.human_message = "La operación solicitada no está soportada.".to_string();
            result.error_detail = Some(format!(
                "Unsupported frontdesk handoff operation: {}",
                handoff.operation
            ));
            tracing::warn!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                operation = %handoff.operation,
                "frontdesk handoff: unsupported operation (not registered)"
            );
            return build_frontdesk_result_reply(msg, result);
        }

        let previous_state = self.load_frontdesk_thread_state(ctx).await?;
        let mut name = handoff
            .subject
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                previous_state
                    .as_ref()
                    .and_then(|state| state.collected.name.clone())
            });
        let mut email = handoff
            .subject
            .email
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                previous_state
                    .as_ref()
                    .and_then(|state| state.collected.email.clone())
            });
        let phone = handoff
            .subject
            .phone
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                previous_state
                    .as_ref()
                    .and_then(|state| state.collected.phone.clone())
            });
        let company_name = handoff
            .subject
            .company_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                previous_state
                    .as_ref()
                    .and_then(|state| state.collected.company_name.clone())
            });
        // The tenant of the case is the one SY.identity holds for its ILK; the handoff's tenant_id
        // only has to agree with it. Without one, or when it is the root tenant (where nobody
        // registers), nothing is registered and no tenant is created.
        let informed_tenant_id = handoff
            .tenant_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let tenant_id = match case_tenant_of(
            &self.ilk_tenant,
            ctx.src_ilk.as_deref(),
            informed_tenant_id.as_deref(),
        ) {
            Ok(tenant_id) => tenant_id,
            Err(error_code) => {
                // Without src_ilk there is no case at all: that is an invalid request.
                let error = if ctx.src_ilk.is_some() {
                    case_tenant_error_payload(error_code)
                } else {
                    missing_src_ilk_payload()
                };
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    src_ilk = ?ctx.src_ilk,
                    informed_tenant_id = ?informed_tenant_id,
                    error_code = error["error_code"].as_str().unwrap_or_default(),
                    "frontdesk handoff: no tenant to register the case into (ilk NOT registered, no tenant created)"
                );
                let result =
                    build_frontdesk_result_from_register_response(&error, ctx.src_ilk.clone());
                return build_frontdesk_result_reply(msg, result);
            }
        };

        // Merge free-form attributes across turns, symmetric with company_name: current handoff
        // wins, else recover from the partial state collected on an earlier (incomplete) turn.
        let attributes = handoff.subject.attributes.clone().or_else(|| {
            previous_state
                .as_ref()
                .and_then(|state| state.collected.attributes.clone())
        });

        let missing_fields = frontdesk_missing_fields(name.as_deref(), email.as_deref());
        if !missing_fields.is_empty() {
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                tenant_id = %tenant_id,
                missing = ?missing_fields,
                has_name = name.is_some(),
                has_email = email.is_some(),
                "frontdesk handoff incomplete → needs_input (ilk NOT registered)"
            );
            let state = FrontdeskThreadState {
                status: Some("collecting".to_string()),
                collected: FrontdeskCollectedState {
                    name: name.take(),
                    email: email.take(),
                    phone,
                    company_name,
                    attributes,
                },
                tenant_id: Some(tenant_id.clone()),
                registration_status: previous_state
                    .as_ref()
                    .and_then(|state| state.registration_status.clone())
                    .or(Some("temporary".to_string())),
                register_attempted: false,
                register_error: None,
            };
            self.store_frontdesk_thread_state(ctx, &state).await?;
            let mut payload = frontdesk_result_payload(
                "needs_input",
                "MISSING_REQUIRED_FIELDS",
                frontdesk_missing_fields_message(&missing_fields),
            );
            payload.missing_fields = missing_fields;
            payload.ilk_id = ctx.src_ilk.clone();
            payload.tenant_id = Some(tenant_id);
            payload.registration_status = Some("temporary".to_string());
            return build_frontdesk_result_reply(msg, payload);
        }

        let tool = IlkRegisterTool {
            scoped_src_ilk: ctx.src_ilk.clone(),
            informed_tenant_id,
            ilk_tenant: self.ilk_tenant.clone(),
            identity: self.gov_identity.clone(),
            bridge: self.gov_identity_bridge.clone(),
        };
        let registered_name = name
            .clone()
            .expect("validated handoff name should be present");
        let registered_email = email
            .clone()
            .expect("validated handoff email should be present");
        let register_arguments = json!({
            "src_ilk": ctx.src_ilk.clone().unwrap_or_default(),
            "identity_candidate": {
                "name": registered_name,
                "email": registered_email,
                "phone": phone,
                "company_name": company_name,
                "attributes": attributes
            }
        });
        let register_payload = tool.call(register_arguments).await?;
        let mut result =
            build_frontdesk_result_from_register_response(&register_payload, ctx.src_ilk.clone());
        if result.status == "ok" {
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                result_code = %result.result_code,
                src_ilk = ?ctx.src_ilk,
                ilk_id = ?result.ilk_id,
                tenant_id = ?result.tenant_id,
                registration_status = ?result.registration_status,
                "frontdesk handoff REGISTERED (ILK_REGISTER complete)"
            );
            self.delete_frontdesk_thread_state(ctx).await?;
        } else {
            result.tenant_id.get_or_insert_with(|| tenant_id.clone());
            // error_detail is not logged: it carries SY.identity's message, which can echo the
            // submitted identification.
            tracing::warn!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                result_code = %result.result_code,
                error_code = ?result.error_code,
                tenant_id = ?result.tenant_id,
                "frontdesk handoff register FAILED (ilk stays temporary)"
            );
            let state = FrontdeskThreadState {
                status: Some("completed_error".to_string()),
                collected: FrontdeskCollectedState {
                    name,
                    email,
                    phone,
                    company_name,
                    attributes,
                },
                // The case keeps its tenant, so a retried handoff still reports it.
                tenant_id: Some(tenant_id),
                registration_status: result.registration_status.clone(),
                register_attempted: true,
                register_error: Some(FrontdeskRegisterErrorState {
                    error_code: result.error_code.clone(),
                    message: result
                        .error_detail
                        .clone()
                        .or(Some(result.human_message.clone())),
                }),
            };
            self.store_frontdesk_thread_state(ctx, &state).await?;
        }
        build_frontdesk_result_reply(msg, result)
    }

    async fn build_frontdesk_result_for_conversation(
        &self,
        ctx: &BehaviorContext,
        human_message: String,
        prior_state: Option<FrontdeskThreadState>,
    ) -> fluxbee_ai_sdk::Result<FrontdeskResultPayload> {
        let after_state = self.load_frontdesk_thread_state(ctx).await?;
        if let Some(state) = after_state {
            if state.status.as_deref() == Some("completed") {
                let mut payload = frontdesk_result_payload("ok", "ALREADY_COMPLETE", human_message);
                payload.ilk_id = ctx.src_ilk.clone();
                payload.tenant_id = state.tenant_id.clone();
                payload.registration_status = state
                    .registration_status
                    .clone()
                    .or(Some("complete".to_string()));
                return Ok(payload);
            }
            if state.status.as_deref() == Some("completed_error") {
                let mut payload =
                    frontdesk_result_payload("error", "REGISTER_FAILED", human_message);
                payload.ilk_id = ctx.src_ilk.clone();
                payload.tenant_id = state.tenant_id.clone();
                payload.registration_status = state.registration_status.clone();
                payload.error_code = state
                    .register_error
                    .as_ref()
                    .and_then(|error| error.error_code.clone());
                payload.error_detail = state
                    .register_error
                    .as_ref()
                    .and_then(|error| error.message.clone());
                return Ok(payload);
            }

            let missing_fields = frontdesk_missing_fields(
                state.collected.name.as_deref(),
                state.collected.email.as_deref(),
            );
            let mut payload =
                frontdesk_result_payload("needs_input", "MISSING_REQUIRED_FIELDS", human_message);
            payload.missing_fields = missing_fields;
            payload.ilk_id = ctx.src_ilk.clone();
            payload.tenant_id = state.tenant_id.clone();
            payload.registration_status = state
                .registration_status
                .clone()
                .or(Some("temporary".to_string()));
            return Ok(payload);
        }

        // No thread_state was written this turn: the LLM chatted (greeting / asked for a field) but
        // NO registration happened. Report the TRUTH — a non-terminal in-conversation turn — NOT a
        // phantom REGISTERED. A genuine success persists status="completed" (prompt: put, not delete)
        // and is caught by the Some(state)=="completed" branch above, so None here unambiguously
        // means "no registration this turn". frontdesk_structured_response_payload maps any non-"ok"
        // status to success:false, so no consumer can read this as a completed registration.
        let mut payload = frontdesk_result_payload("needs_input", "IN_CONVERSATION", human_message);
        payload.ilk_id = ctx.src_ilk.clone();
        payload.tenant_id = prior_state.and_then(|state| state.tenant_id);
        payload.registration_status = Some("temporary".to_string());
        Ok(payload)
    }

    async fn resolve_ai_api_key(&self, provider: AiProvider) -> Option<String> {
        self.resolve_ai_api_key_with_source(provider).await.0
    }

    async fn configured_ai_provider(&self) -> AiProvider {
        self.behavior
            .read()
            .await
            .as_ref()
            .and_then(|behavior| match behavior {
                NodeBehavior::OpenAiChat(ai) => Some(ai.provider),
                NodeBehavior::Echo => None,
            })
            .unwrap_or(AiProvider::OpenAi)
    }

    /// Model D' — same contract as ai-generic: resolve via
    /// `resolve_resource(Openai)`. SY.frontdesk.gov is a system node so its
    /// tenant is `DEFAULT_ROOT_TENANT_ID` (it doesn't get
    /// FLUXBEE_NODE_TENANT_ID like AI.* / IO.* dynamic spawns).
    async fn resolve_ai_api_key_with_source(
        &self,
        provider: AiProvider,
    ) -> (Option<String>, OpenAiApiKeySource) {
        let Some(vault) = self.vault.as_ref() else {
            tracing::warn!(
                node_name = %self.node_name,
                "vault client unavailable (missing self_ilk_id / hive suffix); lookup skipped"
            );
            return (None, OpenAiApiKeySource::Missing);
        };
        let result = vault
            .resolve_resource(
                provider.resource_type(),
                fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
                Duration::from_secs(5),
            )
            .await;
        match result {
            Ok(Some(value)) => {
                let api_key = extract_openai_api_key_from_value(&value);
                if api_key.is_some() {
                    (api_key, OpenAiApiKeySource::Vault)
                } else {
                    tracing::warn!(
                        node_name = %self.node_name,
                        provider = %provider,
                        "vault AI secret did not carry a usable api_key"
                    );
                    (None, OpenAiApiKeySource::Missing)
                }
            }
            Ok(None) => (None, OpenAiApiKeySource::Missing),
            Err(err) => {
                tracing::warn!(error = %err, "frontdesk-gov vault resource lookup failed");
                (None, OpenAiApiKeySource::Missing)
            }
        }
    }

    /// Autonomous boot self-configure (SY.* pattern, cf. build_architect_ai_runtime): resolve the AI
    /// token from SY.vault (Model D', root tenant) and flip to Configured when it is present — the
    /// baked ai_chat behavior is already live, only the token decides the LLM gate. No token => stay
    /// Unconfigured (degraded: the deterministic JSON handoff still works; the LLM path enables when a
    /// VAULT_SECRET_CHANGED carries the token).
    /// Re-resolve the AI token from SY.vault (Model D', root tenant) and set the LLM gate live —
    /// Configured when the token resolves, Unconfigured when it does not; returns whether it resolved.
    /// A no-behavior FAILED_CONFIG node (broken hive.yaml) is left untouched — a token cannot help it.
    /// This is the frontdesk analog of `refresh_architect_ai_runtime`: the ONE resolve+set-state seam
    /// used at boot, on VAULT_SECRET_CHANGED, and on the CONFIG_SET refresh knob.
    async fn refresh_ai_gate(&self) -> bool {
        if self.behavior.read().await.is_none() {
            return false;
        }
        let provider = self.configured_ai_provider().await;
        let resolved = self.resolve_ai_api_key(provider).await.is_some();
        self.control_plane.write().await.current_state = if resolved {
            NodeLifecycleState::Configured
        } else {
            NodeLifecycleState::Unconfigured
        };
        resolved
    }

    async fn boot_self_configure(&self) {
        // No baked behavior (FAILED_CONFIG — e.g. hive.yaml broken) → a token cannot enable the LLM
        // path; leave the state (the deterministic handoff still works).
        if self.behavior.read().await.is_none() {
            return;
        }
        let provider = self.configured_ai_provider().await;
        if self.refresh_ai_gate().await {
            tracing::info!(
                node_name = %self.node_name,
                provider = %provider,
                "frontdesk-gov autonomous bootstrap: AI token resolved from vault → Configured (LLM conversational path enabled)"
            );
        } else {
            tracing::warn!(
                node_name = %self.node_name,
                provider = %provider,
                "frontdesk-gov autonomous bootstrap: no AI token in vault → degraded (Unconfigured); deterministic handoff works, LLM path enables on VAULT_SECRET_CHANGED"
            );
        }
    }

    async fn handle_vault_secret_changed(&self, msg: &Message) {
        let payload: VaultSecretChangedPayload = match serde_json::from_value(msg.payload.clone()) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(
                    node_name = %self.node_name,
                    trace_id = %msg.routing.trace_id,
                    error = %err,
                    "frontdesk-gov ignoring malformed VAULT_SECRET_CHANGED payload"
                );
                return;
            }
        };
        let provider = self.configured_ai_provider().await;
        let resource_type = provider.to_string();
        let interest = VaultSecretInterest {
            resource_type: &resource_type,
            my_tenant: fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
            my_ilk: self.self_ilk_id.as_deref(),
            system_caller: true,
        };
        if !payload.matches_interest(&interest) {
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                resource_type = %payload.resource_type,
                payload_tenant = %payload.tenant_id,
                payload_ilk = ?payload.ilk,
                my_tenant = fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
                my_ilk = ?self.self_ilk_id,
                "frontdesk-gov VAULT_SECRET_CHANGED does not match interest; ignoring"
            );
            return;
        }

        // No baked behavior (FAILED_CONFIG) → a token change/delete cannot enable the LLM path; leave
        // the state (the deterministic handoff still works regardless).
        if self.behavior.read().await.is_none() {
            tracing::warn!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                "frontdesk-gov AI vault secret changed but no baked behavior (FAILED_CONFIG); LLM path stays disabled"
            );
            return;
        }
        if matches!(payload.op, fluxbee_sdk::protocol::VaultSecretOp::Delete) {
            self.control_plane.write().await.current_state = NodeLifecycleState::Unconfigured;
            tracing::warn!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                key = %payload.key,
                version = payload.version,
                "frontdesk-gov AI vault secret deleted → degraded (Unconfigured); deterministic handoff still works, LLM path re-enables when the secret is restored"
            );
            return;
        }

        // Autonomous refresh (cf. refresh_architect_ai_runtime): ACT on the broadcast — re-resolve the
        // token and flip the LLM gate live via the shared seam.
        if self.refresh_ai_gate().await {
            tracing::info!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                op = %payload.op.as_str(),
                key = %payload.key,
                version = payload.version,
                "frontdesk-gov AI vault secret changed → resolved → Configured (LLM conversational path live)"
            );
        } else {
            tracing::warn!(
                node_name = %self.node_name,
                trace_id = %msg.routing.trace_id,
                op = %payload.op.as_str(),
                key = %payload.key,
                version = payload.version,
                "frontdesk-gov AI vault secret changed but no usable api_key → degraded (Unconfigured)"
            );
        }
    }

    async fn handle_control_plane(&self, msg: Message) -> fluxbee_ai_sdk::Result<Option<Message>> {
        let Some(command) = msg.meta.msg.as_deref() else {
            return Ok(None);
        };
        let (response_msg, response_payload) = if command.eq_ignore_ascii_case(MSG_NODE_STATUS_GET)
        {
            if !env_bool(NODE_STATUS_DEFAULT_HANDLER_ENABLED, true) {
                return Ok(None);
            }
            (
                MSG_NODE_STATUS_GET_RESPONSE,
                self.build_node_status_get_response().await,
            )
        } else if command.eq_ignore_ascii_case("CONFIG_SET") {
            ("CONFIG_RESPONSE", self.apply_config_set(&msg).await)
        } else if command.eq_ignore_ascii_case("CONFIG_GET") {
            ("CONFIG_RESPONSE", self.build_config_get_response().await)
        } else if command.eq_ignore_ascii_case("PING") {
            ("PONG", self.build_ping_response().await)
        } else if command.eq_ignore_ascii_case("STATUS") {
            ("STATUS_RESPONSE", self.build_status_response().await)
        } else {
            let state = self.control_plane.read().await.current_state;
            (
                "CONFIG_RESPONSE",
                self.error_response(
                    "unknown_system_msg",
                    format!("Unsupported control-plane command: {command}"),
                    1,
                    0,
                    state.as_str(),
                ),
            )
        };
        Ok(Some(build_control_plane_response(
            &msg,
            response_msg,
            response_payload,
        )))
    }

    async fn apply_config_set(&self, msg: &Message) -> Value {
        let subsystem = match msg.payload.get("subsystem").and_then(Value::as_str) {
            Some(value) if value == "ai_node" => value,
            Some(value) => {
                return self.invalid_config_response(
                    None,
                    None,
                    format!("Invalid payload.subsystem: expected 'ai_node', got '{value}'"),
                );
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
            return self.invalid_config_response(
                None,
                None,
                format!(
                    "Invalid payload.node_name: expected '{}', got '{}'",
                    self.node_name, requested_node_name
                ),
            );
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
        let apply_mode = match msg.payload.get("apply_mode").and_then(Value::as_str) {
            Some(value) => value,
            None => {
                return self.invalid_config_response(
                    Some(schema_version),
                    Some(config_version),
                    "Missing required field: payload.apply_mode".to_string(),
                );
            }
        };
        if apply_mode != "replace" {
            return self.error_response(
                "unsupported_apply_mode",
                format!("Unsupported payload.apply_mode='{apply_mode}' (only 'replace' is supported in current phase)"),
                schema_version,
                config_version,
                self.control_plane.read().await.current_state.as_str(),
            );
        }

        let _ = subsystem;
        // Autonomous node (Model D', like SY.architect's CONFIG_SET): the AI runtime is baked and
        // driven by the SY.vault token — NOTHING on the CONFIG_SET surface is settable. Reject any
        // config/secret/behavior field, then treat CONFIG_SET as a manual VAULT-REFRESH trigger:
        // re-resolve the token via the shared seam and report the resulting state. Nothing is persisted.
        if let Some(field) = frontdesk_rejected_config_field(&msg.payload) {
            return self.error_response(
                "config_not_accepted",
                format!(
                    "{field} is not accepted: SY.frontdesk.gov is autonomous — AI provider/model are hive-wide (hive.yaml) and the credential lives in SY.vault (resource_type=openai); CONFIG_SET only re-resolves the vault token."
                ),
                schema_version,
                config_version,
                self.control_plane.read().await.current_state.as_str(),
            );
        }
        let resolved = self.refresh_ai_gate().await;
        tracing::info!(
            node_name = %self.node_name,
            resolved,
            "frontdesk-gov CONFIG_SET: Model D' vault re-resolve (no config accepted, nothing persisted)"
        );
        self.build_config_get_response().await
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
            "state": state,
            "schema_version": schema_version,
            "config_version": config_version,
            "error": {
                "code": code,
                "message": message
            },
            "effective_config": Value::Null
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
        let (state_name, config_source, schema_version, config_version, effective_config) = {
            let state = self.control_plane.read().await;
            (
                state.current_state.as_str().to_string(),
                state.config_source,
                state.schema_version,
                state.config_version,
                state.effective_config.clone(),
            )
        };
        // `ok` reflects the LLM gate (token resolved), NOT config presence: effective_config is always
        // Some (baked), so the truthful diagnostic is whether the node is Configured (token live).
        let configured = state_name == NodeLifecycleState::Configured.as_str();
        let provider = self.configured_ai_provider().await;
        let api_key_source = self.resolve_ai_api_key_with_source(provider).await.1;
        // Baked engine (hive-wide provider/model from hive.yaml) — reported for diagnostics, mirroring
        // architect's config.ai block.
        let (engine_provider, engine_model) = match self.behavior.read().await.as_ref() {
            Some(NodeBehavior::OpenAiChat(rt)) => (rt.provider.to_string(), rt.model.clone()),
            _ => (provider.to_string(), String::new()),
        };
        let error = if configured {
            Value::Null
        } else {
            json!({"code":"missing_secret","message":"AI token not resolved from SY.vault; degraded (the deterministic handoff still works). Load resource_type=openai in SY.vault."})
        };
        json!({
            "subsystem": "ai_node",
            "node_name": self.node_name.as_str(),
            "ok": configured,
            "state": state_name,
            "config_source": config_source,
            "api_key_source": api_key_source.as_str(),
            "schema_version": schema_version,
            "config_version": config_version,
            "config": {
                "ai": { "default_provider": engine_provider, "model": engine_model }
            },
            "contract": {
                "node_family": "SY",
                "node_kind": "SY.frontdesk.gov",
                "supports": ["CONFIG_GET", "CONFIG_SET"],
                "required_fields": [],
                "optional_fields": [],
                "resources": [{
                    "resource_type": provider.to_string(),
                    "required": true,
                    "source": "SY.vault",
                    "tenant_id": fluxbee_sdk::DEFAULT_ROOT_TENANT_ID,
                    "configured": api_key_source == OpenAiApiKeySource::Vault
                }],
                "notes": [
                    "SY.frontdesk.gov is autonomous: its behavior is a baked ai_chat frontdesk (runtime-owned prompt) and is NOT settable via CONFIG_SET.",
                    "The AI provider/model are hive-wide and configured only in hive.yaml (a baked fallback applies otherwise).",
                    "The AI credential is resolved from SY.vault (resource_type=openai) at boot and on VAULT_SECRET_CHANGED; CONFIG_SET only RE-RESOLVES it — no config accepted, nothing persisted.",
                    "Secret plaintext is never persisted in node-local config or returned by CONFIG_GET."
                ]
            },
            "effective_config": effective_config.as_ref().map(redact_secrets),
            "error": error,
        })
    }

    async fn build_ping_response(&self) -> Value {
        let state = self.control_plane.read().await;
        json!({
            "ok": true,
            "node_name": self.node_name.as_str(),
            "state": state.current_state.as_str(),
        })
    }

    async fn build_status_response(&self) -> Value {
        let state = self.control_plane.read().await;
        let behavior_kind = self
            .behavior
            .read()
            .await
            .as_ref()
            .map(NodeBehavior::kind)
            .unwrap_or("none");
        json!({
            "state": state.current_state.as_str(),
            "node_name": self.node_name.as_str(),
            "behavior_kind": behavior_kind,
            "config_source": state.config_source,
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": Value::Null
        })
    }

    async fn build_node_status_get_response(&self) -> Value {
        let health_state = std::env::var(NODE_STATUS_DEFAULT_HEALTH_STATE)
            .ok()
            .as_deref()
            .map(normalize_health_state)
            .unwrap_or("HEALTHY");
        json!({
            "status": "ok",
            "health_state": health_state
        })
    }
}

fn normalize_health_state(raw: &str) -> &'static str {
    match raw.trim().to_ascii_uppercase().as_str() {
        "HEALTHY" => "HEALTHY",
        "DEGRADED" => "DEGRADED",
        "ERROR" => "ERROR",
        "UNKNOWN" => "UNKNOWN",
        _ => "HEALTHY",
    }
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|raw| match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        })
        .unwrap_or(default)
}

#[async_trait]
impl FunctionTool for IlkRegisterTool {
    fn definition(&self) -> FunctionToolDefinition {
        FunctionToolDefinition {
            name: "ilk_register".to_string(),
            description: "Register the person of this conversation (its temporary ILK) in SY.identity. The organization (tenant) comes with the case: never ask for one."
                .to_string(),
            parameters_json_schema: json!({
                "type": "object",
                "properties": {
                    "src_ilk": { "type": "string", "minLength": 1 },
                    "identity_candidate": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string", "minLength": 1 },
                            "email": { "type": "string", "minLength": 3 },
                            "phone": { "type": "string" },
                            "company_name": { "type": "string" },
                            "attributes": { "type": "object" }
                        },
                        "required": ["name", "email"],
                        "additionalProperties": false
                    }
                },
                "required": ["src_ilk", "identity_candidate"],
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, arguments: Value) -> fluxbee_ai_sdk::Result<Value> {
        let args: IlkRegisterArgs = serde_json::from_value(arguments).map_err(|err| {
            fluxbee_ai_sdk::errors::AiSdkError::Protocol(format!(
                "ilk_register: invalid arguments: {err}"
            ))
        })?;

        let src_ilk_owned = self.scoped_src_ilk.clone().unwrap_or(args.src_ilk);
        let src_ilk = src_ilk_owned.trim();
        if src_ilk.is_empty() {
            return Ok(missing_src_ilk_payload());
        }

        if args.identity_candidate.name.trim().is_empty()
            || args.identity_candidate.email.trim().is_empty()
        {
            return Ok(json!({
                "status": "error",
                "error_code": "invalid_identity_candidate",
                "message": "identity_candidate.name and identity_candidate.email are required",
                "retryable": false
            }));
        }

        // The tenant of the case, never one the person or the LLM gives, never a new one, and
        // never the root tenant.
        let tenant_id = match case_tenant_of(
            &self.ilk_tenant,
            Some(src_ilk),
            self.informed_tenant_id.as_deref(),
        ) {
            Ok(tenant_id) => tenant_id,
            Err(error_code) => {
                tracing::warn!(
                    op = "ilk_register",
                    src_ilk = %src_ilk,
                    informed_tenant_id = ?self.informed_tenant_id,
                    error_code,
                    "no tenant to register the case into: ILK_REGISTER not sent, no tenant created"
                );
                return Ok(case_tenant_error_payload(error_code));
            }
        };

        tracing::info!(
            op = "ilk_register",
            src_ilk = %src_ilk,
            tenant_id = %tenant_id,
            target = %self.identity.target,
            has_fallback = self.identity.fallback_target.is_some(),
            "dispatching identity registration request"
        );

        let payload = json!({
            "ilk_id": src_ilk,
            "ilk_type": "human",
            "tenant_id": tenant_id,
            "identification": {
                "display_name": args.identity_candidate.name,
                "email": args.identity_candidate.email,
                "phone": args.identity_candidate.phone,
                "company_name": args.identity_candidate.company_name,
                "attributes": args.identity_candidate.attributes,
            }
        });
        // Only non-personal fields: the identification VALUES (name, email, phone, company,
        // attributes) never reach the logs, only which fields are present.
        tracing::info!(
            op = "ilk_register",
            target = %self.identity.target,
            msg = %MSG_ILK_REGISTER,
            ilk_id = %src_ilk,
            tenant_id = %tenant_id,
            identification_fields = ?present_identification_fields(&payload["identification"]),
            "sending ILK_REGISTER to identity"
        );
        let result = if let Some(bridge) = &self.bridge {
            bridge
                .call_ok(&self.identity, MSG_ILK_REGISTER, payload)
                .await
        } else {
            Err(identity_bridge_missing())
        };

        match result {
            Ok(out) => {
                // The ilk the person ended on: this one, or after a merge by email the one that
                // already had that email.
                let ilk_id = out
                    .payload
                    .get("ilk_id")
                    .and_then(Value::as_str)
                    .unwrap_or(src_ilk)
                    .to_string();
                let merged = out
                    .payload
                    .get("merged")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                tracing::info!(
                    op = "ilk_register",
                    trace_id = %out.trace_id,
                    effective_target = %out.effective_target,
                    ilk_id = %ilk_id,
                    merged,
                    response_payload = %out.payload,
                    "received ILK_REGISTER response from identity"
                );
                Ok(json!({
                    "status": "ok",
                    "registered": true,
                    "merged": merged,
                    "ilk_id": ilk_id,
                    "tenant_id": tenant_id,
                    "effective_target": out.effective_target,
                    "trace_id": out.trace_id,
                    "identity_payload": out.payload
                }))
            }
            Err(err) => {
                tracing::warn!(
                    op = "ilk_register",
                    target = %self.identity.target,
                    error = %identity_error_log_summary(&err),
                    "ILK_REGISTER failed"
                );
                Ok(identity_error_to_tool_payload(&err))
            }
        }
    }
}

/// Names of the identification fields that carry a value — what a log may say about the person.
fn present_identification_fields(identification: &Value) -> Vec<String> {
    identification
        .as_object()
        .map(|fields| {
            fields
                .iter()
                .filter(|(_, value)| match value {
                    Value::Null => false,
                    Value::String(text) => !text.trim().is_empty(),
                    Value::Object(map) => !map.is_empty(),
                    _ => true,
                })
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default()
}

impl NodeBehavior {
    fn kind(&self) -> &'static str {
        match self {
            Self::Echo => "echo",
            Self::OpenAiChat(_) => "ai_chat",
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log_filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string());
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_filter))
        .init();

    // Model D': SY.frontdesk.gov self-ILK is deterministic from its L2 name
    // (no SHM wait). Identity uses the same formula to seed SHM, so the
    // value matches what other nodes look up.
    let frontdesk_self_ilk_id = match fluxbee_sdk::load_hive_id(std::path::Path::new(
        &default_config_dir(),
    )) {
        Ok(hive_id) => {
            let l2 = format!("SY.frontdesk.gov@{hive_id}");
            let ilk = fluxbee_sdk::deterministic_system_ilk_id(&l2);
            tracing::info!(self_ilk_id = %ilk, "SY.frontdesk.gov self ILK computed deterministically");
            Some(ilk)
        }
        Err(err) => {
            tracing::warn!(error = %err, "SY.frontdesk.gov could not read hive.yaml to derive self ILK; identity-bearing outgoing calls will fail");
            None
        }
    };

    let args = parse_runner_args()?;
    let bootstrap_node = bootstrap_node_from_args(&args)?;
    run_unconfigured_bootstrap(bootstrap_node, frontdesk_self_ilk_id).await?;
    Ok(())
}

fn build_frontdesk_gov_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(fluxbee_ai_sdk::AI_RUNTIME_CHANNEL)
        .post_pending_rule(
            RouteMatch::Any,
            RouteTarget::Command(fluxbee_ai_sdk::AI_RUNTIME_CHANNEL),
        )
        .build()
}

fn vault_client_for(
    dispatcher: Arc<RouterDispatcher>,
    node_name: &str,
    self_ilk_id: Option<&str>,
) -> Option<VaultClient> {
    let ilk = self_ilk_id.map(str::trim).filter(|v| !v.is_empty())?;
    // The name must still be a full L2 name, but the vault is the motherbee's, whatever hive
    // this node runs on.
    node_name.split('@').nth(1).filter(|v| !v.is_empty())?;
    Some(VaultClient::for_primary(
        dispatcher,
        VaultCallerOwned::new(ilk.to_string(), node_name.to_string()),
    ))
}

async fn run_unconfigured_bootstrap(
    node: NodeSection,
    self_ilk_id: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let node_name = node.name.clone();
    let dynamic_dir = PathBuf::from(node.dynamic_config_dir.clone());
    let thread_state_store = init_thread_state_store(&node_name, &dynamic_dir).await;
    let immediate_memory_store = init_immediate_memory_store(&node_name, &dynamic_dir).await;
    // SY.frontdesk.gov is an AUTONOMOUS system node — same pattern as SY.architect and SY.admin's
    // executor (build_architect_ai_runtime / refresh_architect_ai_runtime): its AI runtime is BAKED
    // (it is ALWAYS an ai_chat frontdesk — prompt via frontdesk_default_instructions, engine via
    // load_hive_ai_engine's hive.yaml-or-baked-fallback), and the ONLY external input is the AI token
    // in SY.vault. It reads NO spawn/persisted/CONFIG_SET config for the AI runtime. The token is
    // resolved after the dispatcher exists (boot_self_configure) and decides Configured (LLM
    // conversational path) vs Unconfigured (degraded — the deterministic JSON-handoff path still works
    // with no LLM). The baked behavior is Some in BOTH states; only the token toggles the gate.
    let baked_doc = materialize_effective_defaults(
        &node_name,
        EffectiveConfigDocument {
            node: Some(EffectiveNodeSection {
                config_dir: Some(node.config_dir.clone()),
                ..EffectiveNodeSection::default()
            }),
            behavior: EffectiveBehaviorSection {
                kind: "ai_chat".to_string(),
                ..EffectiveBehaviorSection::default()
            },
            ..EffectiveConfigDocument::default()
        },
    );
    let (behavior, boot_state) = match build_behavior_from_effective_config(&baked_doc) {
        // Behavior baked OK — boot Unconfigured; boot_self_configure flips to Configured when the vault
        // token is present, else stays Unconfigured (degraded: the deterministic handoff still works).
        Ok(behavior) => (Some(behavior), NodeLifecycleState::Unconfigured),
        // The baked behavior could not be built (e.g. hive.yaml missing/invalid) — a real
        // misconfiguration, so boot FAILED_CONFIG. The deterministic handoff still runs (it is gated
        // before the behavior); the LLM conversational path stays disabled.
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "frontdesk-gov: baked ai_chat behavior could not be built (hive.yaml/engine?); booting FAILED_CONFIG (deterministic handoff still works, LLM path disabled)"
            );
            (None, NodeLifecycleState::FailedConfig)
        }
    };
    let state = ControlPlaneState {
        current_state: boot_state,
        config_source: "baked",
        effective_config: Some(serde_json::to_value(&baked_doc).unwrap_or(Value::Null)),
        schema_version: 0,
        config_version: 0,
    };

    let runner_node_config = NodeConfig {
        name: node.name,
        router_socket: PathBuf::from(node.router_socket),
        uuid_persistence_dir: PathBuf::from(node.uuid_persistence_dir),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: PathBuf::from(node.config_dir),
        version: node.version,
    };
    tracing::info!(
        node_name = %node_name,
        "starting ai_node_runner bootstrap instance"
    );
    let gov_identity = gov_identity_config_from_env();
    let profile = build_frontdesk_gov_rpc_profile()
        .map_err(|err| format!("frontdesk-gov rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(runner_node_config, Duration::from_secs(1), profile)
            .await?;
    tracing::info!(node_name = %node_name, "frontdesk-gov connected to router");
    let vault = vault_client_for(dispatcher.clone(), &node_name, self_ilk_id.as_deref());
    let gov_identity_bridge = Some(Arc::new(GovIdentityBridge::new(dispatcher.clone())));
    let ai_node = GenericAiNode {
        node_name: node_name.clone(),
        self_ilk_id,
        behavior: Arc::new(RwLock::new(behavior)),
        thread_state_store,
        immediate_memory_store,
        gov_identity,
        gov_identity_bridge,
        ilk_tenant: identity_shm_ilk_tenant(&node_name),
        vault,
        control_plane: Arc::new(RwLock::new(state)),
    };
    // Now that the dispatcher/vault exist, resolve the AI token from SY.vault and flip to Configured
    // when present (autonomous SY.* pattern, cf. build_architect_ai_runtime). No token => stay
    // Unconfigured (degraded — the deterministic handoff still works).
    ai_node.boot_self_configure().await;
    let runtime = NodeRuntime::new(dispatcher, ai_node);
    runtime.run_with_config(RuntimeConfig::default()).await?;
    Ok(())
}

#[derive(Debug, Default)]
struct BootstrapArgs {
    node_name: Option<String>,
    version: Option<String>,
    router_socket: Option<String>,
    uuid_persistence_dir: Option<String>,
    config_dir: Option<String>,
    dynamic_config_dir: Option<String>,
}

fn parse_runner_args() -> Result<BootstrapArgs, Box<dyn std::error::Error + Send + Sync>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut parsed = BootstrapArgs::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--node-name" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --node-name".to_string().into());
                };
                parsed.node_name = Some(value.clone());
                i += 2;
            }
            "--version" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --version".to_string().into());
                };
                parsed.version = Some(value.clone());
                i += 2;
            }
            "--router-socket" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --router-socket".to_string().into());
                };
                parsed.router_socket = Some(value.clone());
                i += 2;
            }
            "--uuid-persistence-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --uuid-persistence-dir"
                        .to_string()
                        .into());
                };
                parsed.uuid_persistence_dir = Some(value.clone());
                i += 2;
            }
            "--config-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --config-dir".to_string().into());
                };
                parsed.config_dir = Some(value.clone());
                i += 2;
            }
            "--dynamic-config-dir" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("missing value after --dynamic-config-dir"
                        .to_string()
                        .into());
                };
                parsed.dynamic_config_dir = Some(value.clone());
                i += 2;
            }
            other => {
                return Err(format!("unknown argument: {other}").into());
            }
        }
    }

    Ok(parsed)
}

fn bootstrap_node_from_args(
    args: &BootstrapArgs,
) -> Result<NodeSection, Box<dyn std::error::Error + Send + Sync>> {
    let config_dir = args
        .config_dir
        .clone()
        .or_else(|| std::env::var("AI_CONFIG_DIR").ok())
        .unwrap_or_else(default_config_dir);
    let dynamic_config_dir = args
        .dynamic_config_dir
        .clone()
        .or_else(|| std::env::var("AI_DYNAMIC_CONFIG_DIR").ok())
        .unwrap_or_else(default_dynamic_config_dir);
    let mut node_name_source = "args_or_env";
    let name = args
        .node_name
        .clone()
        .or_else(|| {
            let resolved = managed_node_name("", &["AI_NODE_NAME", "NODE_NAME"]);
            if resolved.trim().is_empty() {
                None
            } else {
                Some(resolved)
            }
        })
        .or_else(|| {
            let inferred = infer_frontdesk_node_name_from_hive(PathBuf::from(&config_dir).as_path());
            if inferred.is_some() {
                node_name_source = "hive_yaml";
            }
            inferred
        })
        .ok_or_else(|| {
            "pass --node-name (or FLUXBEE_NODE_NAME/AI_NODE_NAME env var), or provide hive.yaml with hive_id for SY.frontdesk.gov bootstrap".to_string()
        })?;
    tracing::info!(
        node_name = %name,
        node_name_source,
        config_dir = %config_dir,
        dynamic_config_dir = %dynamic_config_dir,
        "resolved SY.frontdesk.gov bootstrap node"
    );
    Ok(NodeSection {
        name,
        version: args
            .version
            .clone()
            .or_else(|| std::env::var("AI_NODE_VERSION").ok())
            .unwrap_or_else(default_version),
        router_socket: args
            .router_socket
            .clone()
            .or_else(|| std::env::var("AI_ROUTER_SOCKET").ok())
            .unwrap_or_else(default_router_socket),
        uuid_persistence_dir: args
            .uuid_persistence_dir
            .clone()
            .or_else(|| std::env::var("AI_UUID_PERSISTENCE_DIR").ok())
            .unwrap_or_else(default_state_dir),
        config_dir: args.config_dir.clone().unwrap_or(config_dir),
        dynamic_config_dir: args
            .dynamic_config_dir
            .clone()
            .unwrap_or(dynamic_config_dir),
    })
}

fn infer_frontdesk_node_name_from_hive(config_dir: &std::path::Path) -> Option<String> {
    let raw = fs::read_to_string(config_dir.join("hive.yaml")).ok()?;
    let hive: FrontdeskBootstrapHiveFile = serde_yaml::from_str(&raw).ok()?;
    let hive_id = hive.hive_id.trim();
    if hive_id.is_empty() {
        return None;
    }
    tracing::info!(
        hive_id,
        config_dir = %config_dir.display(),
        "bootstrapping SY.frontdesk.gov node_name from hive.yaml"
    );
    Some(format!("SY.frontdesk.gov@{hive_id}"))
}

fn build_behavior_from_effective_config(
    config: &EffectiveConfigDocument,
) -> Result<NodeBehavior, Box<dyn std::error::Error + Send + Sync>> {
    let config_dir = config
        .node
        .as_ref()
        .and_then(|node| node.config_dir.as_deref())
        .unwrap_or("/etc/fluxbee");
    let engine = load_hive_ai_engine(config_dir)?;
    let behavior = &config.behavior;
    let kind = behavior.kind.as_str();
    if kind.is_empty() {
        return Err("missing behavior.kind in effective config"
            .to_string()
            .into());
    }

    match kind {
        "echo" => Ok(NodeBehavior::Echo),
        "ai_chat" => {
            let instructions = extract_instructions_from_effective_config(behavior)
                .or_else(|| Some(frontdesk_default_instructions()));
            let model_settings = extract_model_settings_from_effective_config(behavior);
            let base_url = behavior.base_url.clone();
            let immediate_memory = config
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.immediate_memory.clone())
                .unwrap_or_default();
            let multimodal = behavior
                .capabilities
                .as_ref()
                .and_then(|caps| caps.multimodal)
                .unwrap_or_else(default_multimodal_for_runtime);

            Ok(NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                provider: engine.provider,
                model: engine.model,
                instructions,
                model_settings,
                base_url,
                immediate_memory,
                multimodal,
            }))
        }
        other => Err(format!("unsupported behavior.kind '{other}'").into()),
    }
}

fn load_hive_ai_engine(
    config_dir: &str,
) -> Result<fluxbee_ai_sdk::EffectiveAiEngine, Box<dyn std::error::Error + Send + Sync>> {
    let raw = fs::read_to_string(PathBuf::from(config_dir).join("hive.yaml"))?;
    let hive: FrontdeskBootstrapHiveFile = serde_yaml::from_str(&raw)?;
    hive.ai
        .as_ref()
        .map(HiveAiConfig::effective)
        .transpose()
        .map_err(Into::into)
        .map(|engine| engine.unwrap_or_else(HiveAiConfig::fallback))
}

/// Model D' — extract the openai api_key from a vault `value`. Vault may
/// return either a bare string or an object with `api_key` field; both are
/// accepted (matches what SY consumers do).
fn extract_openai_api_key_from_value(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str().map(str::trim).filter(|v| !v.is_empty()) {
        return Some(s.to_string());
    }
    value
        .get("api_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// Model D' (cf. `reject_architect_secret_fields`): SY.frontdesk.gov accepts NO config on CONFIG_SET —
/// its behavior is baked (ai_chat + the frontdesk prompt), its provider/model are hive-wide (hive.yaml),
/// and its credential lives in SY.vault. Returns the first offending config field, if any.
fn frontdesk_rejected_config_field(payload: &Value) -> Option<&'static str> {
    let config = payload.get("config").unwrap_or(payload);
    if config.get("ai").is_some() || config.get("ai_providers").is_some() {
        return Some("config.ai / config.ai_providers");
    }
    if config.get("behavior").is_some() {
        return Some("config.behavior");
    }
    if config.get("api_key").is_some() || config.get("api_key_ref").is_some() {
        return Some("config.api_key");
    }
    None
}

fn materialize_effective_defaults(
    node_name: &str,
    mut config: EffectiveConfigDocument,
) -> EffectiveConfigDocument {
    if config.node.is_none() {
        config.node = Some(EffectiveNodeSection::default());
    }
    if let Some(node) = config.node.as_mut() {
        if node.name.is_none() {
            node.name = Some(node_name.to_string());
        }
    }
    if config.runtime.is_none() {
        config.runtime = Some(EffectiveRuntimeSection::default());
    }
    if let Some(runtime) = config.runtime.as_mut() {
        materialize_runtime_defaults(runtime);
    }
    if config.behavior.kind.eq_ignore_ascii_case("ai_chat") {
        if config.behavior.instructions.is_none() {
            config.behavior.instructions = Some(frontdesk_default_instructions_snapshot());
        }
        if config.behavior.capabilities.is_none() {
            config.behavior.capabilities = Some(BehaviorCapabilities {
                multimodal: Some(default_multimodal_for_runtime()),
            });
        } else if let Some(caps) = config.behavior.capabilities.as_mut() {
            if caps.multimodal.is_none() {
                caps.multimodal = Some(default_multimodal_for_runtime());
            }
        }
    }
    config
}

fn materialize_runtime_defaults(runtime: &mut EffectiveRuntimeSection) {
    let defaults = RuntimeSection::default();
    if runtime.read_timeout_ms.is_none() {
        runtime.read_timeout_ms = Some(defaults.read_timeout_ms);
    }
    if runtime.handler_timeout_ms.is_none() {
        runtime.handler_timeout_ms = Some(defaults.handler_timeout_ms);
    }
    if runtime.write_timeout_ms.is_none() {
        runtime.write_timeout_ms = Some(defaults.write_timeout_ms);
    }
    if runtime.queue_capacity.is_none() {
        runtime.queue_capacity = Some(defaults.queue_capacity);
    }
    if runtime.worker_pool_size.is_none() {
        runtime.worker_pool_size = Some(defaults.worker_pool_size);
    }
    if runtime.retry_max_attempts.is_none() {
        runtime.retry_max_attempts = Some(defaults.retry_max_attempts);
    }
    if runtime.retry_initial_backoff_ms.is_none() {
        runtime.retry_initial_backoff_ms = Some(defaults.retry_initial_backoff_ms);
    }
    if runtime.retry_max_backoff_ms.is_none() {
        runtime.retry_max_backoff_ms = Some(defaults.retry_max_backoff_ms);
    }
    if runtime.metrics_log_interval_ms.is_none() {
        runtime.metrics_log_interval_ms = Some(defaults.metrics_log_interval_ms);
    }
    if runtime.immediate_memory.is_none() {
        runtime.immediate_memory = Some(defaults.immediate_memory);
    }
}

fn is_control_plane(msg: &Message) -> bool {
    msg.meta.msg_type.eq_ignore_ascii_case("system")
        || msg.meta.msg_type.eq_ignore_ascii_case("admin")
}

fn is_vault_secret_changed(msg: &Message) -> bool {
    msg.meta.msg_type.eq_ignore_ascii_case(SYSTEM_KIND)
        && msg.meta.msg.as_deref() == Some(MSG_VAULT_SECRET_CHANGED)
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

fn extract_instructions_from_effective_config(
    behavior: &EffectiveBehaviorSection,
) -> Option<String> {
    behavior
        .instructions
        .as_ref()
        .and_then(|v| {
            if let Some(inline) = v.as_str() {
                return Some(inline.to_string());
            }
            v.get("value")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            behavior
                .params
                .as_ref()
                .and_then(|p| p.system_prompt.clone())
        })
}

fn extract_model_settings_from_effective_config(
    behavior: &EffectiveBehaviorSection,
) -> ModelSettings {
    let direct = behavior.model_settings.as_ref();
    let params = behavior.params.as_ref();
    ModelSettings {
        temperature: direct
            .and_then(|v| v.temperature)
            .or_else(|| params.and_then(|v| v.temperature)),
        top_p: direct
            .and_then(|v| v.top_p)
            .or_else(|| params.and_then(|v| v.top_p)),
        max_output_tokens: direct
            .and_then(|v| v.max_output_tokens)
            .or_else(|| params.and_then(|v| v.max_output_tokens)),
    }
}

fn node_runtime_not_ready_payload() -> Value {
    json!({
        "type": "error",
        "code": "node_runtime_not_ready",
        "message": "AI node runtime is not ready to process user messages yet.",
        "retryable": true
    })
}

fn missing_ai_api_key_payload(provider: AiProvider) -> Value {
    json!({
        "type": "error",
        "code": "missing_ai_api_key",
        "message": format!("Missing AI API key in SY.vault resource_type={provider}."),
        "retryable": true
    })
}

fn ai_runtime_error_payload(err: &fluxbee_ai_sdk::errors::AiSdkError) -> Value {
    match err {
        fluxbee_ai_sdk::errors::AiSdkError::Http(http_err)
            if http_err.is_timeout() || http_err.is_connect() =>
        {
            json!({
                "type": "error",
                "code": "provider_unreachable",
                "message": "The AI provider is temporarily unreachable. Please retry shortly.",
                "retryable": true
            })
        }
        fluxbee_ai_sdk::errors::AiSdkError::Timeout(_)
        | fluxbee_ai_sdk::errors::AiSdkError::RecoverableExhausted(_) => json!({
            "type": "error",
            "code": "provider_timeout",
            "message": "The AI provider did not respond in time. Please retry.",
            "retryable": true
        }),
        fluxbee_ai_sdk::errors::AiSdkError::Protocol(msg) => {
            if let Some((status, detail)) = parse_openai_status_error(msg) {
                if status == 400 || status == 404 || status == 422 {
                    if extract_openai_error_param(msg)
                        .as_deref()
                        .is_some_and(is_openai_attachment_param)
                    {
                        return json!({
                            "type": "error",
                            "code": "provider_attachment_invalid_request",
                            "message": "The AI provider rejected one or more attached files for the current model/provider.",
                            "retryable": false,
                            "provider_status": status,
                            "provider_detail": trim_chars(&detail, 280)
                        });
                    }
                }
                let (code, retryable, message) = match status {
                    400 | 404 | 422 => (
                        "provider_invalid_request",
                        false,
                        "The request is not valid for the AI provider.",
                    ),
                    401 | 403 => (
                        "provider_auth_error",
                        false,
                        "AI provider authentication failed. Check configured credentials.",
                    ),
                    408 => (
                        "provider_timeout",
                        true,
                        "The AI provider timed out while processing the request.",
                    ),
                    429 => (
                        "provider_rate_limited",
                        true,
                        "The AI provider is rate limiting requests. Retry shortly.",
                    ),
                    500..=599 => (
                        "provider_unavailable",
                        true,
                        "The AI provider is temporarily unavailable.",
                    ),
                    _ => (
                        "provider_error",
                        true,
                        "The AI provider returned an error while processing the request.",
                    ),
                };
                return json!({
                    "type": "error",
                    "code": code,
                    "message": message,
                    "retryable": retryable,
                    "provider_status": status,
                    "provider_detail": trim_chars(&detail, 280)
                });
            }
            json!({
                "type": "error",
                "code": "provider_error",
                "message": "The AI provider returned an unexpected error.",
                "retryable": false
            })
        }
        other => json!({
            "type": "error",
            "code": "ai_runtime_error",
            "message": format!("AI runtime failure: {}", trim_chars(&other.to_string(), 220)),
            "retryable": other.is_recoverable()
        }),
    }
}

fn build_frontdesk_result_reply(
    msg: &Message,
    payload: FrontdeskResultPayload,
) -> fluxbee_ai_sdk::Result<Message> {
    let value = if let Some(contract) = extract_response_envelope(&msg.meta)? {
        validate_frontdesk_response_envelope(&contract)?;
        build_text_response(serde_json::to_string(
            &frontdesk_structured_response_payload(&payload, &contract),
        )?)?
    } else {
        build_text_response(payload.human_message.clone())?
    };
    Ok(build_reply_message_runtime_src(msg, value))
}

fn validate_frontdesk_response_envelope(contract: &Value) -> fluxbee_ai_sdk::Result<()> {
    let contract_obj = contract.as_object().ok_or_else(|| {
        fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
            detail: "response envelope must be a JSON object".to_string(),
        }
    })?;
    let kind = contract_obj
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(
            || fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: "response envelope.kind is required".to_string(),
            },
        )?;
    if kind != "json_object_v1" {
        return Err(
            fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: format!("unsupported response envelope kind: {kind}"),
            },
        );
    }

    let properties = contract_obj
        .get("properties")
        .and_then(Value::as_object)
        .ok_or_else(
            || fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: "response envelope.properties is required".to_string(),
            },
        )?;
    let required = contract_obj
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(
            || fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: "response envelope.required is required".to_string(),
            },
        )?;

    for field in required {
        let field_name = field.as_str().ok_or_else(|| {
            fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: "response envelope.required entries must be strings".to_string(),
            }
        })?;
        if !properties.contains_key(field_name) {
            return Err(
                fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                    detail: format!(
                    "required field '{field_name}' is missing from response envelope.properties"
                ),
                },
            );
        }
    }

    for (field_name, field_schema) in properties {
        let field_obj = field_schema.as_object().ok_or_else(|| {
            fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: format!("response envelope.properties.{field_name} must be an object"),
            }
        })?;
        let field_type = field_obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(
                || fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                    detail: format!("response envelope.properties.{field_name}.type is required"),
                },
            )?;
        let supported = match field_name.as_str() {
            "success" => field_type == "boolean",
            "human_message" => field_type == "string",
            "error_code" => field_type == "string",
            "ilk_id" => field_type == "string",
            "merged" => field_type == "boolean",
            _ => false,
        };
        if !supported {
            return Err(fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract {
                detail: format!(
                    "frontdesk cannot satisfy response envelope field '{field_name}' with type '{field_type}'"
                ),
            });
        }
    }

    Ok(())
}

/// The structured verdict, with the optional fields the caller's contract asks for: `ilk_id` is the
/// ILK the person ended up on (after a merge, the one that already had the email, not the
/// temporary), and `merged` says that happened.
fn frontdesk_structured_response_payload(payload: &FrontdeskResultPayload, contract: &Value) -> Value {
    let requested = |field: &str| {
        contract
            .get("properties")
            .and_then(Value::as_object)
            .is_some_and(|properties| properties.contains_key(field))
    };
    let success = payload.status == "ok";
    let error_code = if success {
        None
    } else {
        Some(match payload.result_code.as_str() {
            "MISSING_REQUIRED_FIELDS" => "missing_required_fields",
            "IN_CONVERSATION" => "in_conversation",
            "INVALID_REQUEST" => "invalid_request",
            "IDENTITY_UNAVAILABLE" => "identity_unavailable",
            "TENANT_UNRESOLVED" => "tenant_unresolved",
            "TENANT_NOT_REGISTRABLE" => "tenant_not_registrable",
            "REGISTER_FAILED" => "register_failed",
            _ => "unknown",
        })
    };
    let mut obj = serde_json::Map::new();
    obj.insert("success".to_string(), Value::Bool(success));
    obj.insert(
        "human_message".to_string(),
        Value::String(payload.human_message.clone()),
    );
    if let Some(error_code) = error_code {
        obj.insert(
            "error_code".to_string(),
            Value::String(error_code.to_string()),
        );
    }
    if requested("ilk_id") {
        if let Some(ilk_id) = payload.ilk_id.as_deref().filter(|id| !id.is_empty()) {
            obj.insert("ilk_id".to_string(), Value::String(ilk_id.to_string()));
        }
    }
    if requested("merged") {
        obj.insert(
            "merged".to_string(),
            Value::Bool(payload.result_code == "MERGED"),
        );
    }
    Value::Object(obj)
}

fn frontdesk_missing_fields(name: Option<&str>, email: Option<&str>) -> Vec<String> {
    let mut missing = Vec::new();
    if name.map(|value| value.trim().is_empty()).unwrap_or(true) {
        missing.push("display_name".to_string());
    }
    if email.map(|value| value.trim().is_empty()).unwrap_or(true) {
        missing.push("email".to_string());
    }
    missing
}

fn frontdesk_missing_fields_message(missing_fields: &[String]) -> String {
    match missing_fields {
        [field] if field == "display_name" => {
            "Necesito tu nombre para continuar con el registro.".to_string()
        }
        [field] if field == "email" => {
            "Necesito tu email para continuar con el registro.".to_string()
        }
        _ => "Necesito tu nombre y tu email para continuar con el registro.".to_string(),
    }
}

fn build_frontdesk_result_from_register_response(
    register_payload: &Value,
    src_ilk: Option<String>,
) -> FrontdeskResultPayload {
    let status = register_payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    if status.eq_ignore_ascii_case("ok") {
        let identity = register_payload.get("identity_payload");
        let merged = identity
            .and_then(|value| value.get("merged"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut payload = if merged {
            frontdesk_result_payload(
                "ok",
                "MERGED",
                "Ya estabas registrado con ese email: este canal quedó asociado a tu registro.",
            )
        } else {
            frontdesk_result_payload("ok", "REGISTERED", "Registro completado correctamente.")
        };
        // The ilk the person ended on, as SY.identity answered: after a merge it is not src_ilk.
        payload.ilk_id = identity
            .and_then(|value| value.get("ilk_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .or(src_ilk);
        payload.tenant_id = identity
            .and_then(|value| value.get("tenant_id"))
            .and_then(Value::as_str)
            .map(ToString::to_string);
        payload.registration_status = Some("complete".to_string());
        return payload;
    }

    let error_code = register_payload
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or("IDENTITY_ERROR");
    // Only a transient failure is IDENTITY_UNAVAILABLE. A code SY.identity answered with is a final
    // verdict (TENANT_PENDING, ILK_DELETED, SYSTEM_ILK_PROTECTED, DUPLICATE_EMAIL, ...) and keeps
    // its own error_code.
    let (result_code, human_message) = if identity_error_is_transient(error_code) {
        (
            "IDENTITY_UNAVAILABLE",
            "No pude completar el registro en este momento.",
        )
    } else if error_code == CASE_TENANT_MISSING {
        // No tenant for the case: nothing to register into, and the frontdesk creates none.
        (
            "TENANT_UNRESOLVED",
            "No pude completar el registro: este contacto no tiene una organización asignada.",
        )
    } else if error_code == TENANT_ROOT_NOT_REGISTRABLE {
        // The case is of the root tenant, where nobody registers: the same final answer whether
        // the frontdesk saw it or SY.identity did.
        (
            "TENANT_NOT_REGISTRABLE",
            "No puedo completar el registro por este canal: no pertenece a ninguna organización.",
        )
    } else if error_code.starts_with("INVALID_")
        || matches!(
            error_code,
            "missing_src_ilk" | "invalid_identity_candidate" | CASE_TENANT_MISMATCH
        )
    {
        ("INVALID_REQUEST", "No pude completar el registro.")
    } else {
        ("REGISTER_FAILED", "No pude completar el registro.")
    };
    let mut payload = frontdesk_result_payload("error", result_code, human_message);
    payload.error_code = Some(error_code.to_string());
    payload.error_detail = register_payload
        .get("message")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    payload.ilk_id = src_ilk;
    payload.registration_status = Some("temporary".to_string());
    payload
}

/// Extract (status, detail) from a provider HTTP error. Matches BOTH the OpenAI and Anthropic
/// adapter formats (audit M3): "<provider> error status={code} type={t} [request_id=..] message={m}".
/// The old parser matched only the OpenAI marker + a now-removed " body=" tail, so Anthropic errors
/// (401/429/5xx/400) all collapsed to a generic unclassified payload under an Anthropic hive.
fn parse_openai_status_error(message: &str) -> Option<(u16, String)> {
    let idx = message.find("error status=")?;
    let after = &message[idx + "error status=".len()..];
    let status = after
        .split([' ', ','])
        .next()?
        .trim()
        .parse::<u16>()
        .ok()?;
    let detail = after
        .split_once("message=")
        .map(|(_, msg)| msg.trim().to_string())
        .or_else(|| {
            after
                .split_once(" body=")
                .map(|(_, body)| body.trim().to_string())
        })
        .unwrap_or_default();
    Some((status, detail))
}

#[derive(Debug, Default)]
struct AttachmentObservabilitySummary {
    count: usize,
    total_bytes: u64,
    mimes: Vec<String>,
}

fn attachment_summary_for_observability(
    resolved_user_input: Option<&ResolvedModelInput>,
) -> AttachmentObservabilitySummary {
    let Some(input) = resolved_user_input else {
        return AttachmentObservabilitySummary::default();
    };
    let count = input.attachments.len();
    let total_bytes = input
        .attachments
        .iter()
        .map(|attachment| attachment.blob_ref.size)
        .sum();
    let mimes = input
        .attachments
        .iter()
        .map(|attachment| attachment.blob_ref.mime.clone())
        .collect::<Vec<_>>();
    AttachmentObservabilitySummary {
        count,
        total_bytes,
        mimes,
    }
}

/// Extract the OpenAI error `param` from a provider error string "openai error status=N type=T
/// param=P message=M" (audit B6/M3: the raw JSON body no longer travels). Empty/absent -> None.
fn extract_openai_error_param(error_str: &str) -> Option<String> {
    let after = error_str.split_once("param=")?.1;
    let param = after
        .split_once(" message=")
        .map(|(param, _)| param)
        .unwrap_or(after)
        .trim();
    (!param.is_empty()).then(|| param.to_string())
}

fn is_openai_attachment_param(param: &str) -> bool {
    let param = param.trim();
    param.contains(".file_data")
        || param.contains(".file_id")
        || param.contains(".file_url")
        || param.contains(".image_url")
        || param.contains(".content")
}

fn infer_state_dir_from_dynamic(dynamic_config_dir: &std::path::Path) -> PathBuf {
    dynamic_config_dir
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("/var/lib/fluxbee/state"))
}

fn sanitize_storage_key(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "ai-node".to_string()
    } else {
        output
    }
}

async fn init_thread_state_store(
    node_name: &str,
    dynamic_config_dir: &std::path::Path,
) -> Option<Arc<dyn ThreadStateStore>> {
    let state_dir = infer_state_dir_from_dynamic(dynamic_config_dir);
    let store_root = LanceDbThreadStateStore::path_for_node(&state_dir, node_name);
    let store = LanceDbThreadStateStore::new(store_root);
    match store.ensure_ready().await {
        Ok(()) => {
            tracing::info!(
                node_name = %node_name,
                path = %store.root_dir().display(),
                "thread state store ready"
            );
            Some(Arc::new(store))
        }
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "thread state store unavailable; continuing in degraded mode"
            );
            None
        }
    }
}

async fn init_immediate_memory_store(
    node_name: &str,
    dynamic_config_dir: &std::path::Path,
) -> Option<Arc<ImmediateMemoryStore>> {
    let state_dir = infer_state_dir_from_dynamic(dynamic_config_dir);
    let store_root = ImmediateMemoryStore::path_for_node(&state_dir, node_name);
    let store = ImmediateMemoryStore::new(store_root);
    match store.ensure_ready().await {
        Ok(()) => {
            tracing::info!(
                node_name = %node_name,
                path = %store.root_dir().display(),
                "immediate memory store ready"
            );
            Some(Arc::new(store))
        }
        Err(err) => {
            tracing::warn!(
                node_name = %node_name,
                error = %err,
                "immediate memory store unavailable; continuing without immediate persistence"
            );
            None
        }
    }
}

fn prune_recent_interactions(
    interactions: Vec<fluxbee_ai_sdk::ImmediateInteraction>,
    max_items: usize,
) -> Vec<fluxbee_ai_sdk::ImmediateInteraction> {
    if max_items == 0 {
        return Vec::new();
    }
    let len = interactions.len();
    let keep_from = len.saturating_sub(max_items);
    interactions.into_iter().skip(keep_from).collect()
}

fn trim_summary(
    mut summary: fluxbee_ai_sdk::ConversationSummary,
    max_chars: usize,
) -> fluxbee_ai_sdk::ConversationSummary {
    summary.goal = summary.goal.map(|v| trim_chars(&v, max_chars));
    summary.current_focus = summary.current_focus.map(|v| trim_chars(&v, max_chars));
    summary.decisions = summary
        .decisions
        .into_iter()
        .map(|v| trim_chars(&v, max_chars))
        .collect();
    summary.confirmed_facts = summary
        .confirmed_facts
        .into_iter()
        .map(|v| trim_chars(&v, max_chars))
        .collect();
    summary.open_questions = summary
        .open_questions
        .into_iter()
        .map(|v| trim_chars(&v, max_chars))
        .collect();
    summary
}

fn trim_chars(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::new();
    for ch in value.chars().take(max_chars) {
        out.push(ch);
    }
    out
}

fn invalid_payload_missing_thread_id() -> Value {
    json!({
        "type": "error",
        "code": "invalid_payload",
        "message": "Missing required thread_id for user message.",
        "retryable": false
    })
}

fn extract_thread_id(msg: &Message) -> Option<String> {
    msg.meta
        .thread_id
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn extract_src_ilk(msg: &Message) -> Option<String> {
    msg.meta
        .src_ilk
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

fn src_ilk_source(msg: &Message) -> &'static str {
    if msg
        .meta
        .src_ilk
        .as_deref()
        .map(str::trim)
        .is_some_and(|v| !v.is_empty())
    {
        return "meta";
    }
    "missing"
}

/// The IO sender's kind only: its id is the person's handle (a phone number, a user id).
fn incoming_sender_kind(msg: &Message) -> Option<String> {
    msg.meta
        .context
        .as_ref()?
        .get("io")?
        .get("sender")?
        .get("kind")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| !kind.is_empty())
        .map(ToString::to_string)
}

#[allow(dead_code)]
fn require_src_ilk(ctx: &BehaviorContext) -> fluxbee_ai_sdk::Result<&str> {
    ctx.src_ilk
        .as_deref()
        .ok_or_else(|| fluxbee_ai_sdk::errors::AiSdkError::Protocol("missing_src_ilk".to_string()))
}

#[cfg(test)]
mod tests {
    /// The contract the IO producers send (io_common::frontdesk_gate::frontdesk_response_contract).
    fn producer_contract() -> Value {
        json!({
            "kind": "json_object_v1",
            "required": ["success", "human_message"],
            "properties": {
                "success": {"type": "boolean"},
                "human_message": {"type": "string"},
                "error_code": {"type": "string"},
                "ilk_id": {"type": "string"},
                "merged": {"type": "boolean"}
            }
        })
    }

    use super::*;
    use fluxbee_ai_sdk::{Destination, Meta, Routing};
    use gov_common::frontdesk_contract::FRONTDESK_RESULT_PAYLOAD_TYPE;
    use std::fs;
    use std::sync::Arc;
    use std::sync::{Mutex, OnceLock};
    use tokio::sync::RwLock;

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn sample_request() -> Message {
        Message {
            routing: Routing {
                src: "SY.orchestrator@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.frontdesk.gov@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-123".to_string(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some(MSG_NODE_STATUS_GET.to_string()),
                src_ilk: None,
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload: json!({}),
        }
    }

    #[test]
    fn control_plane_response_keeps_trace_id_and_replies_to_request_src() {
        let req = sample_request();
        let res = build_control_plane_response(
            &req,
            MSG_NODE_STATUS_GET_RESPONSE,
            json!({"status":"ok","health_state":"HEALTHY"}),
        );

        assert_eq!(res.routing.trace_id, req.routing.trace_id);
        assert!(matches!(
            res.routing.dst,
            Destination::Unicast(ref dst) if dst == &req.routing.src
        ));
    }

    #[test]
    fn control_plane_response_sets_expected_msg_name() {
        let req = sample_request();
        let res = build_control_plane_response(
            &req,
            MSG_NODE_STATUS_GET_RESPONSE,
            json!({"status":"ok","health_state":"HEALTHY"}),
        );
        assert_eq!(res.meta.msg.as_deref(), Some(MSG_NODE_STATUS_GET_RESPONSE));
    }

    fn test_node() -> GenericAiNode {
        let gov_identity = GovIdentityConfig::default();
        GenericAiNode {
            node_name: "SY.frontdesk.gov".to_string(),
            self_ilk_id: None,
            behavior: Arc::new(RwLock::new(None)),
            thread_state_store: None,
            immediate_memory_store: None,
            gov_identity,
            gov_identity_bridge: None,
            ilk_tenant: test_ilk_tenant(),
            vault: None,
            control_plane: Arc::new(RwLock::new(ControlPlaneState {
                current_state: NodeLifecycleState::Unconfigured,
                config_source: "none",
                effective_config: None,
                schema_version: 0,
                config_version: 0,
            })),
        }
    }

    #[tokio::test]
    async fn node_status_get_respects_handler_enabled_env_false() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED, "false");
        std::env::remove_var(NODE_STATUS_DEFAULT_HEALTH_STATE);
        let node = test_node();
        let req = sample_request();
        let response = node
            .handle_control_plane(req)
            .await
            .expect("control-plane should not fail");
        assert!(response.is_none());
        std::env::remove_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED);
    }

    #[tokio::test]
    async fn node_status_get_uses_env_health_state_and_falls_back_to_healthy() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::set_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED, "true");
        let node = test_node();
        let req = sample_request();

        std::env::set_var(NODE_STATUS_DEFAULT_HEALTH_STATE, "DEGRADED");
        let degraded = node
            .handle_control_plane(req.clone())
            .await
            .expect("control-plane should not fail")
            .expect("status response should exist");
        assert_eq!(
            degraded.payload.get("health_state").and_then(Value::as_str),
            Some("DEGRADED")
        );

        std::env::set_var(NODE_STATUS_DEFAULT_HEALTH_STATE, "not-a-valid-state");
        let fallback = node
            .handle_control_plane(req)
            .await
            .expect("control-plane should not fail")
            .expect("status response should exist");
        assert_eq!(
            fallback.payload.get("health_state").and_then(Value::as_str),
            Some("HEALTHY")
        );

        std::env::remove_var(NODE_STATUS_DEFAULT_HEALTH_STATE);
        std::env::remove_var(NODE_STATUS_DEFAULT_HANDLER_ENABLED);
    }

    #[tokio::test]
    async fn ai_chat_missing_api_key_returns_error_payload_instead_of_fatal() {
        let _guard = env_lock().lock().expect("env lock");
        std::env::remove_var("OPENAI_API_KEY_MISSING_FOR_TEST");
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::OpenAiChat(OpenAiChatRuntime {
                provider: AiProvider::OpenAi,
                model: "gpt-4.1-mini".to_string(),
                instructions: Some("Test instructions".to_string()),
                model_settings: ModelSettings::default(),
                base_url: None,
                immediate_memory: ImmediateMemorySection::default(),
                multimodal: false,
            }));
        }

        let msg = sample_user_request_with_context(
            json!({ "thread_id": "sim-thread-1" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        let response = node
            .on_message(msg)
            .await
            .expect("on_message should not fail fatally")
            .expect("response should be present");

        assert_eq!(
            response.payload.get("code").and_then(Value::as_str),
            Some("missing_ai_api_key")
        );
        assert_eq!(
            response.payload.get("retryable").and_then(Value::as_bool),
            Some(true)
        );
        std::env::remove_var("OPENAI_API_KEY_MISSING_FOR_TEST");
    }

    #[tokio::test]
    async fn frontdesk_handoff_incomplete_returns_text_payload_without_envelope() {
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }

        let mut msg = sample_user_request_with_context(
            json!({ "thread_id": "frontdesk-thread-1" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        msg.payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": {
                "display_name": "Juan Perez"
            }
        });

        let response = node
            .on_message(msg)
            .await
            .expect("handoff should not fail")
            .expect("response should exist");
        assert_eq!(
            response.payload.get("type").and_then(Value::as_str),
            Some("text")
        );
        assert_eq!(
            extract_text(&response.payload).as_deref(),
            Some("Necesito tu email para continuar con el registro.")
        );
    }

    #[tokio::test]
    async fn frontdesk_handoff_runs_deterministically_even_when_unconfigured() {
        // The deterministic (JSON handoff) method must NOT require the node Configured — a Cloud
        // register_human cannot depend on the frontdesk having an LLM behavior. test_node() is
        // UNCONFIGURED by default; the handoff must still reach handle_frontdesk_handoff (here it is
        // incomplete → needs_input) rather than being rejected with node_not_configured.
        let node = test_node();
        // NOTE: intentionally NOT setting Configured.
        let mut msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-unconfigured-1",
                "response_envelope": {
                    "kind": "json_object_v1",
                    "required": ["success", "human_message"],
                    "properties": {
                        "success": { "type": "boolean" },
                        "human_message": { "type": "string" },
                        "error_code": { "type": "string" }
                    }
                }
            }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        msg.payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": { "display_name": "Juan Perez" }
        });

        let response = node
            .on_message(msg)
            .await
            .expect("handoff should not fail")
            .expect("response should exist");
        // Must NOT be the node-not-configured rejection: the deterministic path ran despite no config.
        assert_ne!(
            response.payload.get("code").and_then(Value::as_str),
            Some("node_not_configured"),
            "unconfigured node must still handle the deterministic handoff, not reject it"
        );
        // It reached handle_frontdesk_handoff → structured needs_input (success:false, email missing).
        assert_eq!(
            response.payload.get("type").and_then(Value::as_str),
            Some("text")
        );
        let content = extract_text(&response.payload).expect("structured text");
        let structured: Value = serde_json::from_str(&content).expect("valid structured json");
        assert_eq!(
            structured.get("success").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[tokio::test]
    async fn frontdesk_handoff_with_response_envelope_returns_structured_text_payload() {
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }

        let mut msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-structured-1",
                "response_envelope": {
                    "kind": "json_object_v1",
                    "required": ["success", "human_message"],
                    "properties": {
                        "success": { "type": "boolean" },
                        "human_message": { "type": "string" },
                        "error_code": { "type": "string" }
                    }
                }
            }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        msg.payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": {
                "display_name": "Juan Perez"
            }
        });

        let response = node
            .on_message(msg)
            .await
            .expect("handoff should not fail")
            .expect("response should exist");
        assert_eq!(
            response.payload.get("type").and_then(Value::as_str),
            Some("text")
        );
        let content = extract_text(&response.payload).expect("structured text");
        let structured: Value = serde_json::from_str(&content).expect("valid structured json");
        assert_eq!(
            structured.get("success").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            structured.get("error_code").and_then(Value::as_str),
            Some("missing_required_fields")
        );
        assert_eq!(
            structured.get("human_message").and_then(Value::as_str),
            Some("Necesito tu email para continuar con el registro.")
        );
    }

    #[tokio::test]
    async fn gov_user_echo_returns_text_payload_without_envelope() {
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::Echo);
        }

        let msg = sample_user_request_with_context(
            json!({ "thread_id": "frontdesk-thread-2" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        let response = node
            .on_message(msg)
            .await
            .expect("on_message should not fail")
            .expect("response should exist");
        assert_eq!(
            response.payload.get("type").and_then(Value::as_str),
            Some("text")
        );
        assert_eq!(
            extract_text(&response.payload).as_deref(),
            Some("Echo: hola")
        );
    }

    #[tokio::test]
    async fn gov_user_echo_with_response_envelope_returns_structured_text_payload() {
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::Echo);
        }

        let msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-structured-2",
                "response_envelope": {
                    "kind": "json_object_v1",
                    "required": ["success", "human_message"],
                    "properties": {
                        "success": { "type": "boolean" },
                        "human_message": { "type": "string" },
                        "error_code": { "type": "string" }
                    }
                }
            }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        let response = node
            .on_message(msg)
            .await
            .expect("on_message should not fail")
            .expect("response should exist");
        assert_eq!(
            response.payload.get("type").and_then(Value::as_str),
            Some("text")
        );
        let content = extract_text(&response.payload).expect("structured text");
        let structured: Value = serde_json::from_str(&content).expect("valid structured json");
        // A plain conversational turn (Echo: no tool call, no thread_state written) must NOT be
        // reported as a completed registration — that was the false-REGISTERED bug. The response is
        // structured (envelope present) but success=false with error_code=in_conversation.
        assert_eq!(
            structured.get("success").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            structured.get("human_message").and_then(Value::as_str),
            Some("Echo: hola")
        );
        assert_eq!(
            structured.get("error_code").and_then(Value::as_str),
            Some("in_conversation")
        );
    }

    #[tokio::test]
    async fn frontdesk_rejects_invalid_response_envelope() {
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::Echo);
        }

        let msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-invalid-envelope",
                "response_envelope": {
                    "kind": "json_object_v1",
                    "required": ["success"]
                }
            }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        let err = node
            .on_message(msg)
            .await
            .expect_err("invalid envelope should fail");
        assert!(matches!(
            err,
            fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract { .. }
        ));
    }

    #[tokio::test]
    async fn frontdesk_rejects_unfulfillable_response_envelope() {
        let node = test_node();
        {
            let mut state = node.control_plane.write().await;
            state.current_state = NodeLifecycleState::Configured;
        }
        {
            let mut behavior = node.behavior.write().await;
            *behavior = Some(NodeBehavior::Echo);
        }

        let msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-unfulfillable-envelope",
                "response_envelope": {
                    "kind": "json_object_v1",
                    "required": ["success", "human_message", "missing_fields"],
                    "properties": {
                        "success": { "type": "boolean" },
                        "human_message": { "type": "string" },
                        "missing_fields": { "type": "string" }
                    }
                }
            }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        let err = node
            .on_message(msg)
            .await
            .expect_err("unfulfillable envelope should fail");
        assert!(matches!(
            err,
            fluxbee_ai_sdk::errors::AiSdkError::InvalidResponseContract { .. }
        ));
    }

    fn sample_user_request_with_context(
        context: Value,
        top_level_src_ilk: Option<&str>,
    ) -> Message {
        let meta_thread_id = context
            .get("thread_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        Message {
            routing: Routing {
                src: "IO.sim.local@motherbee".to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.frontdesk.gov@motherbee".to_string()),
                ttl: 16,
                trace_id: "trace-user-123".to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: None,
                src_ilk: top_level_src_ilk.map(ToString::to_string),
                thread_id: meta_thread_id,
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: Some(context),
                ..Meta::default()
            },
            payload: json!({"type":"text","content":"hola"}),
        }
    }

    #[test]
    fn frontdesk_registry_exposes_ilk_register_by_default() {
        let node = test_node();
        let registry = node
            .build_tool_registry(&BehaviorContext {
                thread_id: None,
                src_ilk: None,
            })
            .expect("registry");
        let names = registry
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "ilk_register"));
    }

    #[test]
    fn extract_src_ilk_reads_from_meta_top_level_first() {
        let msg = sample_user_request_with_context(
            json!({ "src_ilk": "ilk:legacy-context-value" }),
            Some("ilk:11111111-1111-4111-8111-111111111111"),
        );
        assert_eq!(
            extract_src_ilk(&msg).as_deref(),
            Some("ilk:11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(src_ilk_source(&msg), "meta");
    }

    #[test]
    fn extract_src_ilk_does_not_read_legacy_meta_context() {
        let msg = sample_user_request_with_context(
            json!({ "src_ilk": "ilk:11111111-1111-4111-8111-111111111111" }),
            None,
        );
        assert_eq!(extract_src_ilk(&msg), None);
        assert_eq!(src_ilk_source(&msg), "missing");
    }

    #[test]
    fn extract_src_ilk_reports_missing_when_absent() {
        let msg = sample_user_request_with_context(json!({}), None);
        assert_eq!(extract_src_ilk(&msg), None);
        assert_eq!(src_ilk_source(&msg), "missing");
    }

    #[test]
    fn extract_thread_id_reads_from_meta_top_level_first() {
        let mut msg =
            sample_user_request_with_context(json!({ "thread_id": "legacy-thread-1" }), None);
        msg.meta.thread_id = Some("thread:canonical-1".to_string());
        assert_eq!(
            extract_thread_id(&msg).as_deref(),
            Some("thread:canonical-1")
        );
    }

    #[test]
    fn extract_thread_id_does_not_read_legacy_meta_context() {
        let msg = sample_user_request_with_context(
            json!({
                "io": {
                    "conversation": {
                        "thread_id": "legacy-thread-1"
                    }
                }
            }),
            None,
        );
        assert_eq!(extract_thread_id(&msg), None);
    }

    #[test]
    fn require_src_ilk_returns_missing_src_ilk_error() {
        let ctx = BehaviorContext {
            thread_id: None,
            src_ilk: None,
        };
        let err = require_src_ilk(&ctx).expect_err("missing src_ilk should fail");
        assert!(err.to_string().contains("missing_src_ilk"));
    }

    #[test]
    fn frontdesk_config_set_rejects_all_config_fields() {
        // Autonomous node (Model D'): CONFIG_SET accepts NO config — the behavior is baked and the
        // credential lives in SY.vault. Any config/secret/behavior field is rejected; an empty config
        // (a bare vault re-resolve trigger) is accepted.
        assert_eq!(
            frontdesk_rejected_config_field(&json!({"config": {"behavior": {"kind": "ai_chat"}}})),
            Some("config.behavior")
        );
        assert_eq!(
            frontdesk_rejected_config_field(&json!({"config": {"ai": {"model": "gpt-5.5"}}})),
            Some("config.ai / config.ai_providers")
        );
        assert_eq!(
            frontdesk_rejected_config_field(&json!({"config": {"api_key": "sk-x"}})),
            Some("config.api_key")
        );
        assert_eq!(
            frontdesk_rejected_config_field(&json!({"config": {}})),
            None
        );
    }

    #[test]
    fn materialize_effective_config_defaults_injects_frontdesk_prompt_when_missing() {
        let config = materialize_effective_defaults(
            "SY.frontdesk.gov@motherbee",
            EffectiveConfigDocument {
                behavior: EffectiveBehaviorSection {
                    kind: "ai_chat".to_string(),
                    ..EffectiveBehaviorSection::default()
                },
                ..EffectiveConfigDocument::default()
            },
        );

        let instructions = config
            .behavior
            .instructions
            .as_ref()
            .and_then(Value::as_object)
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str);
        assert_eq!(
            instructions,
            Some(frontdesk_default_instructions().as_str())
        );
    }

    #[test]
    fn build_behavior_from_effective_config_uses_frontdesk_prompt_when_missing() {
        let root =
            std::env::temp_dir().join(format!("frontdesk-ai-config-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&root).expect("create temp config dir");
        fs::write(root.join("hive.yaml"), "hive_id: motherbee\n").expect("write hive.yaml");
        let behavior = build_behavior_from_effective_config(&EffectiveConfigDocument {
            node: Some(EffectiveNodeSection {
                config_dir: Some(root.to_string_lossy().to_string()),
                ..EffectiveNodeSection::default()
            }),
            behavior: EffectiveBehaviorSection {
                kind: "ai_chat".to_string(),
                ..EffectiveBehaviorSection::default()
            },
            ..EffectiveConfigDocument::default()
        })
        .expect("build behavior");
        let _ = fs::remove_dir_all(&root);

        match behavior {
            NodeBehavior::OpenAiChat(openai) => {
                assert_eq!(
                    openai.instructions.as_deref(),
                    Some(frontdesk_default_instructions().as_str())
                );
            }
            other => panic!("expected OpenAiChat behavior, got {other:?}"),
        }
    }

    #[test]
    fn infer_frontdesk_node_name_from_hive_uses_hive_id() {
        let root = std::env::temp_dir().join(format!(
            "frontdesk-hive-bootstrap-{}",
            Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&root).expect("create temp config dir");
        fs::write(root.join("hive.yaml"), "hive_id: motherbee\n").expect("write hive.yaml");

        let inferred = infer_frontdesk_node_name_from_hive(root.as_path());
        assert_eq!(inferred.as_deref(), Some("SY.frontdesk.gov@motherbee"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn frontdesk_structured_response_payload_omits_error_code_on_success() {
        let payload = FrontdeskResultPayload {
            payload_type: FRONTDESK_RESULT_PAYLOAD_TYPE.to_string(),
            schema_version: 1,
            status: "ok".to_string(),
            result_code: "REGISTERED".to_string(),
            human_message: "Registro completado correctamente.".to_string(),
            missing_fields: Vec::new(),
            error_code: None,
            error_detail: None,
            ilk_id: Some("ilk:test".to_string()),
            tenant_id: Some("tnt:test".to_string()),
            registration_status: Some("complete".to_string()),
        };

        let structured = frontdesk_structured_response_payload(&payload, &producer_contract());
        assert_eq!(
            structured.get("success").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            structured.get("human_message").and_then(Value::as_str),
            Some("Registro completado correctamente.")
        );
        assert!(structured.get("error_code").is_none());
    }

    #[test]
    fn frontdesk_structured_response_payload_maps_error_code_on_failure() {
        let payload = FrontdeskResultPayload {
            payload_type: FRONTDESK_RESULT_PAYLOAD_TYPE.to_string(),
            schema_version: 1,
            status: "error".to_string(),
            result_code: "REGISTER_FAILED".to_string(),
            human_message: "No pude completar el registro.".to_string(),
            missing_fields: Vec::new(),
            error_code: Some("register_failed".to_string()),
            error_detail: None,
            ilk_id: Some("ilk:test".to_string()),
            tenant_id: Some("tnt:test".to_string()),
            registration_status: Some("temporary".to_string()),
        };

        let structured = frontdesk_structured_response_payload(&payload, &producer_contract());
        assert_eq!(
            structured.get("success").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            structured.get("error_code").and_then(Value::as_str),
            Some("register_failed")
        );
    }

    const TEST_ILK: &str = "ilk:11111111-1111-4111-8111-111111111111";
    const TEST_TENANT: &str = "tnt:22222222-2222-4222-8222-222222222222";
    const OTHER_TENANT: &str = "tnt:33333333-3333-4333-8333-333333333333";
    const UNKNOWN_ILK: &str = "ilk:44444444-4444-4444-8444-444444444444";
    const REGISTERED_ILK: &str = "ilk:55555555-5555-4555-8555-555555555555";
    const ROOT_ILK: &str = "ilk:66666666-6666-4666-8666-666666666666";

    /// The identity SHM of the tests: TEST_ILK is a temporary ILK of TEST_TENANT, and ROOT_ILK one
    /// a root-tenant IO node provisioned.
    fn test_ilk_tenant() -> IlkTenantLookup {
        Arc::new(|ilk_id: &str| match ilk_id {
            TEST_ILK => Some(TEST_TENANT.to_string()),
            ROOT_ILK => Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID.to_string()),
            _ => None,
        })
    }

    fn register_failure(error_code: &str) -> FrontdeskResultPayload {
        let reply = fluxbee_sdk::IdentitySystemResult {
            payload: json!({
                "status": "error",
                "error_code": error_code,
                "message": "failed to register ilk"
            }),
            effective_target: "SY.identity@motherbee".to_string(),
            trace_id: "trace-identity-1".to_string(),
        };
        let err = identity_reply_outcome(MSG_ILK_REGISTER, reply)
            .expect_err("a non-ok identity reply is a rejection");
        build_frontdesk_result_from_register_response(
            &identity_error_to_tool_payload(&err),
            Some(TEST_ILK.to_string()),
        )
    }

    #[test]
    fn final_identity_rejections_are_register_failed_not_identity_unavailable() {
        for code in [
            "TENANT_PENDING",
            "TENANT_SUSPENDED",
            "TENANT_DELETED",
            "ILK_DELETED",
            "ILK_NOT_FOUND",
            "SYSTEM_ILK_PROTECTED",
            "DUPLICATE_EMAIL",
            "UNAUTHORIZED_REGISTRAR",
        ] {
            let result = register_failure(code);
            assert_eq!(result.status, "error");
            assert_eq!(result.result_code, "REGISTER_FAILED", "{code}");
            assert_eq!(result.error_code.as_deref(), Some(code));
            assert_eq!(result.registration_status.as_deref(), Some("temporary"));
            assert_eq!(
                frontdesk_structured_response_payload(&result, &producer_contract())["error_code"],
                "register_failed"
            );
        }
        let invalid = register_failure("INVALID_TENANT");
        assert_eq!(invalid.result_code, "INVALID_REQUEST");
        assert_eq!(invalid.error_code.as_deref(), Some("INVALID_TENANT"));
    }

    #[test]
    fn transient_identity_failures_stay_identity_unavailable() {
        for code in ["NOT_PRIMARY", "DB_NOT_READY", "DB_WRITE_FAILED"] {
            let result = register_failure(code);
            assert_eq!(result.result_code, "IDENTITY_UNAVAILABLE", "{code}");
            assert_eq!(result.error_code.as_deref(), Some(code));
        }
        let unreachable = identity_error_to_tool_payload(&IdentityError::Unreachable {
            reason: "NODE_NOT_FOUND".to_string(),
            original_dst: "SY.identity@motherbee".to_string(),
        });
        let result = build_frontdesk_result_from_register_response(&unreachable, None);
        assert_eq!(result.result_code, "IDENTITY_UNAVAILABLE");
        assert_eq!(
            frontdesk_structured_response_payload(&result, &producer_contract())["error_code"],
            "identity_unavailable"
        );
    }

    #[test]
    fn identity_reply_outcome_accepts_only_status_ok() {
        let reply = |payload: Value| fluxbee_sdk::IdentitySystemResult {
            payload,
            effective_target: "SY.identity@motherbee".to_string(),
            trace_id: "trace-identity-2".to_string(),
        };
        assert!(identity_reply_outcome(
            MSG_ILK_REGISTER,
            reply(json!({"status": "ok", "ilk_id": TEST_ILK}))
        )
        .is_ok());
        let err = identity_reply_outcome(MSG_ILK_REGISTER, reply(json!({"status": "error"})))
            .expect_err("status error is a rejection");
        assert!(matches!(
            err,
            IdentityError::SystemRejected { ref action, ref error_code, .. }
                if action == MSG_ILK_REGISTER && error_code == "UNKNOWN"
        ));
    }

    fn test_tool(
        scoped_src_ilk: Option<&str>,
        informed_tenant_id: Option<&str>,
    ) -> IlkRegisterTool {
        IlkRegisterTool {
            scoped_src_ilk: scoped_src_ilk.map(ToString::to_string),
            informed_tenant_id: informed_tenant_id.map(ToString::to_string),
            ilk_tenant: test_ilk_tenant(),
            identity: GovIdentityConfig::default(),
            bridge: None,
        }
    }

    #[test]
    fn ilk_register_schema_has_no_thread_id() {
        let schema = test_tool(None, None).definition().parameters_json_schema;
        assert!(schema["properties"].get("thread_id").is_none());
    }

    #[test]
    fn ilk_register_schema_takes_no_tenant() {
        let schema = test_tool(None, None).definition().parameters_json_schema;
        assert!(schema["properties"].get("tenant_id").is_none());
        let candidate = &schema["properties"]["identity_candidate"];
        assert!(candidate["properties"].get("tenant_hint").is_none());
        assert_eq!(candidate["additionalProperties"], false);
    }

    /// Collects what the fmt layer writes on this thread while its guard lives.
    #[derive(Clone, Default)]
    struct LogCapture(Arc<Mutex<Vec<u8>>>);

    thread_local! {
        static THREAD_CAPTURE: std::cell::RefCell<Option<LogCapture>> =
            const { std::cell::RefCell::new(None) };
    }

    /// The writer of the one global test subscriber: this thread's capture, if any. A global
    /// subscriber that takes every level keeps tracing's per-callsite interest cache from
    /// silencing a capture when parallel tests hit the same callsites (as `set_default` did).
    struct ThreadCaptureWriter;

    impl std::io::Write for ThreadCaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            THREAD_CAPTURE.with(|capture| {
                if let Some(capture) = capture.borrow().as_ref() {
                    capture
                        .0
                        .lock()
                        .expect("log capture")
                        .extend_from_slice(buf);
                }
            });
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Ends this thread's capture.
    struct LogCaptureGuard;

    impl Drop for LogCaptureGuard {
        fn drop(&mut self) {
            THREAD_CAPTURE.with(|capture| *capture.borrow_mut() = None);
        }
    }

    impl LogCapture {
        fn start() -> (Self, LogCaptureGuard) {
            static GLOBAL_SUBSCRIBER: OnceLock<()> = OnceLock::new();
            GLOBAL_SUBSCRIBER.get_or_init(|| {
                let subscriber = tracing_subscriber::fmt()
                    .with_writer(|| ThreadCaptureWriter)
                    .with_ansi(false)
                    .with_max_level(tracing::Level::TRACE)
                    .finish();
                tracing::subscriber::set_global_default(subscriber)
                    .expect("the test log subscriber is the only global one");
            });
            // A callsite registered while the subscriber was being installed keeps no stale
            // "never" interest.
            tracing::callsite::rebuild_interest_cache();
            let capture = Self::default();
            THREAD_CAPTURE.with(|slot| *slot.borrow_mut() = Some(capture.clone()));
            (capture, LogCaptureGuard)
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("log capture")).into_owned()
        }
    }

    const PERSONAL_VALUES: [&str; 6] = [
        "Juana Secreta",
        "juana.secreta@example.com",
        "+5491100009999",
        "Empresa Secreta SA",
        "crm-secret-42",
        "Hint Secreto SRL",
    ];

    fn assert_no_personal_data(logs: &str) {
        assert!(!logs.is_empty(), "expected captured logs");
        for value in PERSONAL_VALUES {
            assert!(
                !logs.contains(value),
                "personal data {value:?} leaked into the logs:\n{logs}"
            );
        }
    }

    #[tokio::test]
    async fn ilk_register_tool_logs_carry_no_personal_data() {
        let tool = test_tool(Some(TEST_ILK), None);
        let candidate = json!({
            "name": "Juana Secreta",
            "email": "juana.secreta@example.com",
            "phone": "+5491100009999",
            "company_name": "Empresa Secreta SA",
            "attributes": { "crm_customer_id": "crm-secret-42" },
            "tenant_hint": "Hint Secreto SRL"
        });
        let (logs, _guard) = LogCapture::start();
        // Straight to ILK_REGISTER with the case's tenant (it then fails: no identity bridge).
        let registered = tool
            .call(json!({ "src_ilk": TEST_ILK, "identity_candidate": candidate }))
            .await
            .expect("tool call");
        assert_eq!(registered["status"], "error");

        let text = logs.text();
        assert_no_personal_data(&text);
        assert!(text.contains("sending ILK_REGISTER to identity"));
        assert!(text.contains(TEST_ILK));
        assert!(text.contains(TEST_TENANT));
        assert!(
            text.contains("\"email\""),
            "field names are logged:\n{text}"
        );
    }

    #[tokio::test]
    async fn ilk_register_takes_the_tenant_of_the_case_never_the_llms() {
        // The LLM (or the person through it) names a tenant: it is not a parameter, and it is not
        // followed. The registration goes to the tenant of the case's ILK.
        let tool = test_tool(Some(TEST_ILK), None);
        let (logs, _guard) = LogCapture::start();
        let out = tool
            .call(json!({
                "src_ilk": TEST_ILK,
                "tenant_id": OTHER_TENANT,
                "identity_candidate": {
                    "name": "Ana", "email": "ana@example.com", "tenant_hint": OTHER_TENANT
                }
            }))
            .await
            .expect("tool call");
        assert_eq!(
            out["error_code"], "IDENTITY_ERROR",
            "it reached ILK_REGISTER"
        );

        let text = logs.text();
        assert!(text.contains("sending ILK_REGISTER to identity"));
        assert!(text.contains(TEST_TENANT));
        assert!(!text.contains(OTHER_TENANT), "{text}");
        assert!(!text.contains("TNT_CREATE"), "{text}");
    }

    #[tokio::test]
    async fn ilk_register_without_the_case_tenant_sends_nothing_and_creates_no_tenant() {
        // An ILK whose tenant cannot be read: whatever the LLM says, nothing goes to identity.
        let tool = test_tool(Some(UNKNOWN_ILK), None);
        let (logs, _guard) = LogCapture::start();
        let out = tool
            .call(json!({
                "src_ilk": UNKNOWN_ILK,
                "tenant_id": TEST_TENANT,
                "identity_candidate": {
                    "name": "Ana", "email": "ana@example.com", "company_name": "Acme SA"
                }
            }))
            .await
            .expect("tool call");
        assert_eq!(out["status"], "error");
        assert_eq!(out["error_code"], CASE_TENANT_MISSING);
        let text = logs.text();
        assert!(!text.contains("sending ILK_REGISTER"), "{text}");
        assert!(!text.contains("TNT_CREATE"), "{text}");
    }

    #[test]
    fn tenant_errors_are_explicit_results() {
        let missing = build_frontdesk_result_from_register_response(
            &case_tenant_error_payload(CASE_TENANT_MISSING),
            Some(UNKNOWN_ILK.to_string()),
        );
        assert_eq!(missing.status, "error");
        assert_eq!(missing.result_code, "TENANT_UNRESOLVED");
        assert_eq!(missing.error_code.as_deref(), Some(CASE_TENANT_MISSING));
        assert_eq!(
            frontdesk_structured_response_payload(&missing, &producer_contract())["error_code"],
            "tenant_unresolved"
        );
        let mismatch = build_frontdesk_result_from_register_response(
            &case_tenant_error_payload(CASE_TENANT_MISMATCH),
            Some(TEST_ILK.to_string()),
        );
        assert_eq!(mismatch.result_code, "INVALID_REQUEST");
        assert_eq!(mismatch.error_code.as_deref(), Some(CASE_TENANT_MISMATCH));
    }

    #[test]
    fn a_root_tenant_case_is_a_final_tenant_not_registrable() {
        // The same answer whether the frontdesk sees the root tenant or SY.identity answers it.
        for result in [
            build_frontdesk_result_from_register_response(
                &case_tenant_error_payload(TENANT_ROOT_NOT_REGISTRABLE),
                Some(ROOT_ILK.to_string()),
            ),
            register_failure(TENANT_ROOT_NOT_REGISTRABLE),
        ] {
            assert_eq!(result.status, "error");
            assert_eq!(result.result_code, "TENANT_NOT_REGISTRABLE");
            assert_eq!(
                result.error_code.as_deref(),
                Some(TENANT_ROOT_NOT_REGISTRABLE)
            );
            assert_eq!(result.registration_status.as_deref(), Some("temporary"));
            assert!(result.human_message.contains("ninguna organización"));
            let structured = frontdesk_structured_response_payload(&result, &producer_contract());
            assert_eq!(structured["success"], false);
            assert_eq!(structured["error_code"], "tenant_not_registrable");
        }
    }

    #[tokio::test]
    async fn ilk_register_never_registers_into_the_root_tenant() {
        let tool = test_tool(Some(ROOT_ILK), None);
        let (logs, _guard) = LogCapture::start();
        let out = tool
            .call(json!({
                "src_ilk": ROOT_ILK,
                "identity_candidate": { "name": "Ana", "email": "ana@example.com" }
            }))
            .await
            .expect("tool call");
        assert_eq!(out["status"], "error");
        assert_eq!(out["error_code"], TENANT_ROOT_NOT_REGISTRABLE);
        assert_eq!(out["retryable"], false);
        let text = logs.text();
        assert!(!text.contains("sending ILK_REGISTER"), "{text}");
    }

    #[tokio::test]
    async fn a_handoff_of_a_root_tenant_case_registers_nothing() {
        let (node, state) = test_node_with_state();
        for informed in [None, Some(fluxbee_sdk::DEFAULT_ROOT_TENANT_ID)] {
            let reply = structured_reply(&node, structured_handoff(ROOT_ILK, informed)).await;
            assert_eq!(reply["success"], false);
            assert_eq!(reply["error_code"], "tenant_not_registrable");
        }
        // Not even asked for missing data (the IO first-contact gate sends none).
        let mut first_contact = structured_handoff(ROOT_ILK, None);
        first_contact.payload["subject"] = json!({ "attributes": { "channel_type": "slack" } });
        let reply = structured_reply(&node, first_contact).await;
        assert_eq!(reply["error_code"], "tenant_not_registrable");
        assert!(state.0.lock().expect("thread state").is_empty());
    }

    #[tokio::test]
    async fn a_conversation_of_a_root_tenant_case_ends_before_the_llm() {
        // No LLM configured: a case of the root tenant still gets its final answer.
        let (node, state) = test_node_with_state();
        let mut msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-root",
                "response_envelope": producer_contract()
            }),
            Some(ROOT_ILK),
        );
        msg.payload = json!({ "type": "text", "content": "hola, quiero registrarme" });
        let reply = structured_reply(&node, msg).await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["error_code"], "tenant_not_registrable");
        assert!(state.0.lock().expect("thread state").is_empty());

        // Without an envelope the person reads the human message.
        let plain = sample_user_request_with_context(
            json!({ "thread_id": "frontdesk-thread-root" }),
            Some(ROOT_ILK),
        );
        let response = node
            .on_message(plain)
            .await
            .expect("on_message")
            .expect("response");
        let text = extract_text(&response.payload).expect("text");
        assert!(
            text.contains("no pertenece a ninguna organización"),
            "{text}"
        );

        // Any other case goes on to the conversation, which here needs the LLM.
        let other = sample_user_request_with_context(
            json!({ "thread_id": "frontdesk-thread-other" }),
            Some(TEST_ILK),
        );
        let response = node
            .on_message(other)
            .await
            .expect("on_message")
            .expect("response");
        assert_eq!(response.payload["code"], "node_not_configured");
    }

    #[test]
    fn a_merge_is_reported_with_the_ilk_the_person_ended_on() {
        let tool_output = |identity_payload: Value| {
            json!({
                "status": "ok",
                "registered": true,
                "identity_payload": identity_payload
            })
        };
        let merged = build_frontdesk_result_from_register_response(
            &tool_output(json!({
                "status": "ok", "ilk_id": REGISTERED_ILK, "tenant_id": TEST_TENANT,
                "registration_status": "complete", "merged": true, "merged_from_ilk_id": TEST_ILK
            })),
            Some(TEST_ILK.to_string()),
        );
        assert_eq!(merged.status, "ok");
        assert_eq!(merged.result_code, "MERGED");
        assert_eq!(merged.ilk_id.as_deref(), Some(REGISTERED_ILK));
        assert_eq!(merged.tenant_id.as_deref(), Some(TEST_TENANT));
        assert_eq!(merged.registration_status.as_deref(), Some("complete"));
        let structured = frontdesk_structured_response_payload(&merged, &producer_contract());
        assert_eq!(structured["success"], true);
        assert!(structured.get("error_code").is_none());
        // The caller learns where the person ended up, not the temporary it provisioned.
        assert_eq!(structured["ilk_id"], REGISTERED_ILK);
        assert_eq!(structured["merged"], true);
        // A caller that did not ask for them does not get them.
        let minimal = json!({
            "kind": "json_object_v1",
            "required": ["success", "human_message"],
            "properties": {"success": {"type": "boolean"}, "human_message": {"type": "string"}}
        });
        let structured_minimal = frontdesk_structured_response_payload(&merged, &minimal);
        assert!(structured_minimal.get("ilk_id").is_none());
        assert!(structured_minimal.get("merged").is_none());
        assert!(structured["human_message"]
            .as_str()
            .is_some_and(|text| text.contains("Ya estabas registrado")));

        let registered = build_frontdesk_result_from_register_response(
            &tool_output(json!({
                "status": "ok", "ilk_id": TEST_ILK, "tenant_id": TEST_TENANT,
                "registration_status": "complete", "merged": false
            })),
            Some(TEST_ILK.to_string()),
        );
        assert_eq!(registered.result_code, "REGISTERED");
        assert_eq!(registered.ilk_id.as_deref(), Some(TEST_ILK));
        assert_eq!(registered.tenant_id.as_deref(), Some(TEST_TENANT));
    }

    /// Thread state in memory, so a test can see what a handoff left for the case.
    #[derive(Default)]
    struct MemoryThreadState(Mutex<HashMap<String, Value>>);

    #[async_trait]
    impl ThreadStateStore for MemoryThreadState {
        async fn get(
            &self,
            key: &str,
        ) -> fluxbee_ai_sdk::Result<Option<fluxbee_ai_sdk::ThreadStateRecord>> {
            let state = self.0.lock().expect("thread state");
            Ok(state
                .get(key)
                .map(|data| fluxbee_ai_sdk::ThreadStateRecord {
                    thread_id: key.to_string(),
                    data: data.clone(),
                    updated_at: String::new(),
                    ttl_seconds: None,
                }))
        }

        async fn put(
            &self,
            key: &str,
            data: Value,
            _ttl_seconds: Option<u64>,
        ) -> fluxbee_ai_sdk::Result<()> {
            self.0
                .lock()
                .expect("thread state")
                .insert(key.to_string(), data);
            Ok(())
        }

        async fn delete(&self, key: &str) -> fluxbee_ai_sdk::Result<()> {
            self.0.lock().expect("thread state").remove(key);
            Ok(())
        }
    }

    fn test_node_with_state() -> (GenericAiNode, Arc<MemoryThreadState>) {
        let state = Arc::new(MemoryThreadState::default());
        let mut node = test_node();
        node.thread_state_store = Some(state.clone() as Arc<dyn ThreadStateStore>);
        (node, state)
    }

    fn structured_handoff(src_ilk: &str, tenant_id: Option<&str>) -> Message {
        let mut msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-tenant",
                "response_envelope": {
                    "kind": "json_object_v1",
                    "required": ["success", "human_message"],
                    "properties": {
                        "success": { "type": "boolean" },
                        "human_message": { "type": "string" },
                        "error_code": { "type": "string" }
                    }
                }
            }),
            Some(src_ilk),
        );
        msg.payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": { "display_name": "Ana", "email": "ana@example.com" },
            "tenant_id": tenant_id
        });
        msg
    }

    async fn structured_reply(node: &GenericAiNode, msg: Message) -> Value {
        let response = node
            .on_message(msg)
            .await
            .expect("handoff should not fail")
            .expect("response should exist");
        let content = extract_text(&response.payload).expect("structured text");
        serde_json::from_str(&content).expect("valid structured json")
    }

    #[tokio::test]
    async fn handoff_with_a_tenant_that_contradicts_the_ilk_registers_nothing() {
        let (node, state) = test_node_with_state();
        let reply = structured_reply(&node, structured_handoff(TEST_ILK, Some(OTHER_TENANT))).await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["error_code"], "invalid_request");
        assert!(state.0.lock().expect("thread state").is_empty());
    }

    #[tokio::test]
    async fn handoff_without_a_case_tenant_answers_tenant_unresolved() {
        // A handoff tenant alone does not make one: the ILK's tenant cannot be read.
        let (node, state) = test_node_with_state();
        let reply =
            structured_reply(&node, structured_handoff(UNKNOWN_ILK, Some(TEST_TENANT))).await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["error_code"], "tenant_unresolved");
        assert!(state.0.lock().expect("thread state").is_empty());
    }

    #[tokio::test]
    async fn handoff_without_src_ilk_is_an_invalid_request() {
        let (node, state) = test_node_with_state();
        let mut msg = structured_handoff(TEST_ILK, Some(TEST_TENANT));
        msg.meta.src_ilk = None;
        let reply = structured_reply(&node, msg).await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["error_code"], "invalid_request");
        assert!(state.0.lock().expect("thread state").is_empty());
    }

    #[tokio::test]
    async fn a_failed_handoff_keeps_the_tenant_of_its_case() {
        // No tenant_id in the handoff: the case's comes from its ILK. The registration fails (no
        // identity bridge here) and the case keeps its tenant for the retry.
        let (node, state) = test_node_with_state();
        let reply = structured_reply(&node, structured_handoff(TEST_ILK, None)).await;
        assert_eq!(reply["success"], false);
        assert_eq!(reply["error_code"], "identity_unavailable");
        let kept = state.0.lock().expect("thread state").get(TEST_ILK).cloned();
        let kept = kept.expect("the failed case keeps its state");
        assert_eq!(kept["status"], "completed_error");
        assert_eq!(kept["tenant_id"], TEST_TENANT);

        // The retry, again without tenant_id, still knows its tenant.
        let retry = structured_reply(&node, structured_handoff(TEST_ILK, None)).await;
        assert_eq!(retry["error_code"], "identity_unavailable");
        let kept = state.0.lock().expect("thread state").get(TEST_ILK).cloned();
        assert_eq!(kept.expect("state")["tenant_id"], TEST_TENANT);
    }

    #[tokio::test]
    async fn frontdesk_handoff_logs_carry_no_personal_data() {
        let node = test_node();
        let mut msg = sample_user_request_with_context(
            json!({ "thread_id": "frontdesk-thread-privacy-1" }),
            Some(TEST_ILK),
        );
        msg.payload = json!({
            "type": "frontdesk_handoff",
            "schema_version": 1,
            "operation": "complete_registration",
            "subject": {
                "display_name": "Juana Secreta",
                "email": "juana.secreta@example.com",
                "phone": "+5491100009999",
                "company_name": "Empresa Secreta SA",
                "attributes": { "crm_customer_id": "crm-secret-42" }
            },
            "tenant_id": TEST_TENANT
        });
        let (logs, _guard) = LogCapture::start();
        node.on_message(msg)
            .await
            .expect("handoff should not fail")
            .expect("response should exist");

        let text = logs.text();
        assert_no_personal_data(&text);
        assert!(text.contains("frontdesk handoff register FAILED"));
    }

    #[tokio::test]
    async fn incoming_user_message_log_has_no_text_or_sender_id() {
        let node = test_node();
        node.control_plane.write().await.current_state = NodeLifecycleState::Configured;
        *node.behavior.write().await = Some(NodeBehavior::Echo);
        let mut msg = sample_user_request_with_context(
            json!({
                "thread_id": "frontdesk-thread-privacy-2",
                "io": { "sender": { "kind": "whatsapp", "id": "+5491100009999" } }
            }),
            Some(TEST_ILK),
        );
        msg.payload = json!({
            "type": "text",
            "content": "soy Juana Secreta, juana.secreta@example.com"
        });
        let (logs, _guard) = LogCapture::start();
        node.on_message(msg)
            .await
            .expect("on_message should not fail")
            .expect("response should exist");

        let text = logs.text();
        assert_no_personal_data(&text);
        assert!(text.contains("incoming user message"));
        assert!(text.contains("whatsapp"));
    }
}
