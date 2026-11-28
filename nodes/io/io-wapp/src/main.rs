#![forbid(unsafe_code)]

//! IO.wapp — WhatsApp Cloud API node. Phase-1 SKELETON: boots (degraded when unconfigured), runs the
//! SDK single-config control plane (CONFIG_GET/SET/PING/STATUS), and resolves credentials from
//! SY.vault via the family vault_ref pattern (mirrors io.slack) with a poll refresh loop + a
//! VAULT_SECRET_CHANGED fast-path wake. Webhook inbound (SY.edge) and Graph API outbound land in later
//! phases — see docs/io-wapp-design.md.

mod webhook;

use anyhow::Result;
use fluxbee_sdk::blob::BlobToolkit;
use fluxbee_sdk::payload::TextV1Payload;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, Message as WireMessage, Meta, Routing, MSG_NODE_STATUS_GET,
    MSG_NODE_STATUS_GET_RESPONSE, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile, RouteMatch, RouteTarget,
    RouterDispatcher, VaultCallerOwned, VaultClient, VaultError, FLUXBEE_NODE_NAME_ENV,
};
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_context::{extract_wapp_post_target, wapp_inbound_io_context};
use io_common::io_control_plane::{
    build_io_config_get_response_payload, build_io_config_response_message,
    build_io_config_set_error_payload, build_io_config_set_ok_payload,
    ensure_config_version_advances, parse_and_validate_io_control_plane_request, IoConfigSource,
    IoControlPlaneErrorInfo, IoControlPlaneRequest, IoControlPlaneState, IoNodeLifecycleState,
};
use io_common::io_control_plane_bootstrap::bootstrap_io_control_plane_state;
use io_common::io_control_plane_logging::{
    log_config_get_served, log_config_set_applied, log_config_set_persist_error,
    log_config_set_stale_rejected, log_control_plane_request_rejected,
};
use io_common::io_control_plane_metrics::IoControlPlaneMetrics;
use io_common::io_control_plane_store::persist_io_control_plane_state;
use io_common::io_wapp_adapter_config::IoWappAdapterConfigContract;
use io_common::person_assignment::{
    extract_runtime_person_ilk, PersonAssignmentStatus, PERSON_RECHECK_INTERVAL,
};
use io_common::provision::{FluxbeeIdentityProvisioner, IdentityProvisionConfig};
use io_common::text_v1_blob::{resolve_text_v1_text_only_for_outbound, IoBlobRuntimeConfig};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, PoisonError};
use std::time::Duration;
use tokio::sync::{Mutex, Notify, RwLock};
use webhook::{
    base64_decode, extract_inbound_messages, verify_webhook_signature, WappInboundMessage,
    WappMessageKind,
};

const RPC_CH_SYSTEM: &str = "system";
const RPC_CH_INBOUND: &str = "inbound";
const RPC_CH_OUTBOUND: &str = "outbound";
const IO_WAPP_VAULT_REFRESH_INTERVAL_SECS: u64 = 60;
/// Canonical resource_type for the WhatsApp secret in SY.vault (design D4).
const WAPP_RESOURCE_TYPE: &str = "whatsapp";
/// Graph API host + default version (design §2/§4). The version is overridable per node via
/// `io.graph_api_version`; the host is fixed (Meta's only Cloud API endpoint).
const WAPP_GRAPH_BASE_URL: &str = "https://graph.facebook.com";
const WAPP_GRAPH_DEFAULT_VERSION: &str = "v20.0";
/// Outbound HTTP timeout — a hung Graph call must not wedge the outbound loop.
const WAPP_GRAPH_HTTP_TIMEOUT_SECS: u64 = 30;
/// The msg_type the edge stamps on fanned-out webhooks (the externalize row's `inbound_family` must
/// match). Every IO.wapp node subscribes to this family; each self-selects by phone_number_id.
const IO_WAPP_INBOUND_FAMILY: &str = "io.wapp.inbound.v1";

struct Config {
    node_name: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    /// Hive of this node (from `<name>@<hive>`) — the SHM identity island.
    island_id: String,
    ttl: u32,
    dedup_ttl_ms: u64,
    dedup_max_entries: usize,
    identity_target: String,
    identity_timeout_ms: u64,
}

impl Config {
    fn from_env() -> Self {
        let node_name = env(FLUXBEE_NODE_NAME_ENV).unwrap_or_else(|| "IO.wapp.local".to_string());
        let island_id = node_name
            .split_once('@')
            .map(|(_, hive)| hive.trim().to_string())
            .filter(|hive| !hive.is_empty())
            .unwrap_or_else(|| "motherbee".to_string());
        Self {
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
            ttl: env("IO_WAPP_TTL")
                .and_then(|v| v.parse().ok())
                .unwrap_or(16),
            dedup_ttl_ms: env("IO_WAPP_DEDUP_TTL_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(600_000),
            dedup_max_entries: env("IO_WAPP_DEDUP_MAX_ENTRIES")
                .and_then(|v| v.parse().ok())
                .unwrap_or(50_000),
            // A provision is the primary's to answer: a replica refuses it with NOT_PRIMARY, so an
            // IO on a worker that asked its own hive's SY.identity gave every stranger no ILK
            // (FINDINGS A-372). io.api asks the primary the same way.
            identity_target: env("IO_WAPP_IDENTITY_TARGET")
                .unwrap_or_else(|| "SY.identity@motherbee".to_string()),
            identity_timeout_ms: env("IO_WAPP_IDENTITY_TIMEOUT_MS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10_000),
            island_id,
            node_name,
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

// --- Credentials (mirrors io.slack SlackRuntimeState) ---

/// WhatsApp Cloud API credentials resolved from SY.vault.
#[derive(Clone, PartialEq)]
struct WappCredentials {
    access_token: String,
    app_secret: String,
    verify_token: String,
}

// Never print the tokens (a leaked access_token / app_secret is an account compromise).
impl std::fmt::Debug for WappCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WappCredentials")
            .field("access_token", &"<redacted>")
            .field("app_secret", &"<redacted>")
            .field("verify_token", &"<redacted>")
            .finish()
    }
}

/// Holds the live credentials; the inbound/outbound phases read them. `config_generation` bumps only
/// when a credential actually changed so a refresh tick on an unchanged secret is a true no-op.
struct WappRuntimeState {
    credentials: Option<WappCredentials>,
    config_generation: u64,
}

#[derive(Clone)]
struct WappClients {
    runtime: Arc<RwLock<WappRuntimeState>>,
    http: reqwest::Client,
}

impl WappClients {
    fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(WAPP_GRAPH_HTTP_TIMEOUT_SECS))
            .build()
            .map_err(|err| anyhow::anyhow!("io-wapp HTTP client build failed: {err}"))?;
        Ok(Self {
            runtime: Arc::new(RwLock::new(WappRuntimeState {
                credentials: None,
                config_generation: 0,
            })),
            http,
        })
    }

    async fn config_generation(&self) -> u64 {
        self.runtime.read().await.config_generation
    }

    async fn credentials_configured(&self) -> bool {
        self.runtime.read().await.credentials.is_some()
    }

    /// Snapshot the live credentials (for inbound HMAC verification / outbound). `None` = degraded.
    async fn credentials(&self) -> Option<WappCredentials> {
        self.runtime.read().await.credentials.clone()
    }

    /// Install credentials; only bumps the generation when they actually CHANGED, so the periodic
    /// refresh on an unchanged secret is a no-op.
    async fn reload_credentials(&self, creds: WappCredentials) {
        let mut guard = self.runtime.write().await;
        if guard.credentials.as_ref() == Some(&creds) {
            return;
        }
        guard.credentials = Some(creds);
        guard.config_generation = guard.config_generation.wrapping_add(1);
    }

    /// Drop live credentials (vault secret deleted / no longer resolvable) so a stale token is never
    /// reused. No-op if already cleared.
    async fn clear_credentials(&self) {
        let mut guard = self.runtime.write().await;
        if guard.credentials.is_none() {
            return;
        }
        guard.credentials = None;
        guard.config_generation = guard.config_generation.wrapping_add(1);
    }

    /// Send a Graph API request with bounded 429 (rate-limit) retries honoring `Retry-After` (mirrors
    /// io.slack's `slack_send_with_retry`). `build` is re-invoked per attempt so the request (token +
    /// body) is rebuilt fresh. Only 429 is retried — any other status returns immediately for the caller
    /// to classify.
    async fn graph_send_with_retry<F>(&self, api: &str, build: F) -> Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        const MAX_RETRIES: u32 = 5;
        let mut attempt: u32 = 0;
        loop {
            let response = build().send().await?;
            if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Ok(response);
            }
            attempt += 1;
            if attempt > MAX_RETRIES {
                return Err(anyhow::anyhow!(
                    "{api}: Graph API rate-limited (429) after {MAX_RETRIES} retries"
                ));
            }
            let retry_after = parse_retry_after(response.headers());
            tracing::warn!(
                api = %api, attempt, retry_after_secs = retry_after.as_secs(),
                "Graph API rate-limited (429); backing off"
            );
            tokio::time::sleep(retry_after).await;
        }
    }

    /// POST a free-form text reply to `graph.facebook.com/<version>/<phone_number_id>/messages` as the
    /// WhatsApp business number, addressed to the customer `to_wa_id`. Returns the Graph message id on
    /// success. Degraded (no credentials) or a non-2xx Graph response is an error the caller logs; the
    /// access_token rides only in the bearer header (never the URL/logs). 24h-window / template handling
    /// is deferred (D6) — an out-of-window free-form send is rejected by Meta and surfaces as the error.
    async fn post_text(
        &self,
        version: &str,
        phone_number_id: &str,
        to_wa_id: &str,
        text: &str,
    ) -> Result<String> {
        let creds = self
            .credentials()
            .await
            .ok_or_else(|| anyhow::anyhow!("io-wapp degraded: no credentials for outbound send"))?;
        let url = format!("{WAPP_GRAPH_BASE_URL}/{version}/{phone_number_id}/messages");
        let body = build_wapp_text_message_body(to_wa_id, text);
        let response = self
            .graph_send_with_retry("messages", || {
                self.http
                    .post(&url)
                    .bearer_auth(&creds.access_token)
                    .json(&body)
            })
            .await?;
        let status = response.status();
        let value: Value = response.json().await.unwrap_or(Value::Null);
        if !status.is_success() {
            // Surface Meta's error code + message (no secrets in Graph error bodies), never the request.
            let err = value.get("error");
            let code = err
                .and_then(|e| e.get("code"))
                .cloned()
                .unwrap_or(Value::Null);
            let message = err
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("<no message>");
            return Err(anyhow::anyhow!(
                "Graph send failed: status={status} code={code} message={message}"
            ));
        }
        // Success envelope: { messages: [ { id: "wamid...." } ], ... }
        let message_id = value
            .get("messages")
            .and_then(|m| m.as_array())
            .and_then(|arr| arr.first())
            .and_then(|m| m.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(message_id)
    }
}

/// Parse a Graph `Retry-After` header (integer seconds) into a bounded backoff, clamped to [1s, 30s];
/// a missing/garbage header falls back to 1s (mirrors io.slack's `parse_retry_after`).
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    const DEFAULT: Duration = Duration::from_secs(1);
    const MAX: Duration = Duration::from_secs(30);
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT)
        .clamp(DEFAULT, MAX)
}

/// The Graph Cloud API free-form text message body (design §7). `messaging_product:"whatsapp"` +
/// `recipient_type:"individual"` are required by Meta; `preview_url:false` keeps link previews off.
fn build_wapp_text_message_body(to_wa_id: &str, text: &str) -> Value {
    serde_json::json!({
        "messaging_product": "whatsapp",
        "recipient_type": "individual",
        "to": to_wa_id,
        "type": "text",
        "text": { "preview_url": false, "body": text },
    })
}

// --- Vault resolution (mirrors io.slack) ---

/// Outcome of resolving WhatsApp credentials from SY.vault. `Absent` = DETERMINISTIC (key deleted /
/// wrong resource_type / malformed value) → clear live creds; `Transient` = timeout / node error →
/// keep whatever we have (a vault blip must not drop a live node).
enum WappVaultResolution {
    Found(WappCredentials),
    Absent,
    Transient,
}

/// The vault key that holds this node's WhatsApp credentials, from `wapp.auth.key` in the effective
/// config (family vault_ref pattern). `None` while unconfigured.
fn wapp_vault_key(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("wapp"))
        .and_then(|s| s.get("auth"))
        .and_then(|a| a.get("key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// Extract `{access_token, app_secret, verify_token}` from a vault `value`. All three must be present
/// and non-empty (a partial secret can't authenticate the webhook or the Graph API).
fn extract_wapp_creds_from_vault_value(value: &Value) -> Option<WappCredentials> {
    let obj = value.as_object()?;
    let field = |k: &str| {
        obj.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
    };
    Some(WappCredentials {
        access_token: field("access_token")?,
        app_secret: field("app_secret")?,
        verify_token: field("verify_token")?,
    })
}

async fn resolve_wapp_credentials_from_vault(
    vault: &VaultClient,
    key: &str,
) -> WappVaultResolution {
    match vault.get(key, Duration::from_secs(5)).await {
        Ok(response) => {
            let resource_type = response
                .metadata
                .as_ref()
                .and_then(|m| m.resource_type.as_deref());
            if resource_type != Some(WAPP_RESOURCE_TYPE) {
                tracing::warn!(
                    vault_key = %key,
                    resource_type = ?resource_type,
                    "vault key is not resource_type=whatsapp; treating as absent"
                );
                return WappVaultResolution::Absent;
            }
            match response
                .value
                .as_ref()
                .and_then(extract_wapp_creds_from_vault_value)
            {
                Some(creds) => WappVaultResolution::Found(creds),
                None => {
                    tracing::warn!(
                        vault_key = %key,
                        "vault whatsapp secret has no usable {{access_token, app_secret, verify_token}}; treating as absent"
                    );
                    WappVaultResolution::Absent
                }
            }
        }
        Err(VaultError::Service { code, message }) if code == "KEY_NOT_FOUND" => {
            tracing::info!(vault_key = %key, message = %message, "vault whatsapp secret not found (deleted); clearing credentials");
            WappVaultResolution::Absent
        }
        Err(err) => {
            tracing::warn!(vault_key = %key, error = %err, "io-wapp vault get failed (transient); keeping current credentials");
            WappVaultResolution::Transient
        }
    }
}

/// True when a VAULT_SECRET_CHANGED broadcast concerns the whatsapp resource (fast-path reload).
fn vault_change_is_wapp(payload: &Value) -> bool {
    payload
        .get("resource_type")
        .and_then(|v| v.as_str())
        .is_some_and(|rt| rt.eq_ignore_ascii_case(WAPP_RESOURCE_TYPE))
}

#[tokio::main]
async fn main() -> Result<()> {
    let config = Config::from_env();
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info,io_wapp=info,fluxbee_sdk=info")
    });
    fluxbee_sdk::logging::fmt()
        .with_env_filter(env_filter)
        .init();

    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        self_ilk_id = ?self_ilk_id,
        self_tenant_id = ?self_tenant_id,
        "io-wapp starting"
    );

    let node_config = NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    };
    let profile = build_io_wapp_rpc_profile()
        .map_err(|err| anyhow::anyhow!("io-wapp rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    tracing::info!(full_name = %dispatcher.sender_snapshot().full_name(), "connected to router");

    let vault_client = match (
        self_ilk_id.as_deref().filter(|value| !value.is_empty()),
        config
            .node_name
            .split('@')
            .nth(1)
            .map(str::trim)
            .filter(|value| !value.is_empty()),
    ) {
        // The hive only validates the L2 name: the vault is the motherbee's wherever we run.
        (Some(self_ilk), Some(_hive)) => Some(Arc::new(VaultClient::for_primary(
            dispatcher.clone(),
            VaultCallerOwned::new(self_ilk.to_string(), config.node_name.clone()),
        ))),
        _ => None,
    };

    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoWappAdapterConfigContract);
    let boot_state = bootstrap_io_control_plane_state(&config.node_name, adapter_contract.as_ref())
        .unwrap_or_else(|err| {
            tracing::warn!(error = %err, node_name = %config.node_name, "failed to bootstrap IO control-plane state; using UNCONFIGURED");
            IoControlPlaneState::default()
        });

    let clients = WappClients::new()?;

    // Boot credential resolution: a missing/not-yet-present secret is DEGRADED (a runtime signal), not
    // FAILED_CONFIG — structural config validity was already decided at bootstrap.
    if let Some(vault) = vault_client.as_ref() {
        match wapp_vault_key(boot_state.effective_config.as_ref()) {
            Some(key) => match resolve_wapp_credentials_from_vault(vault, &key).await {
                WappVaultResolution::Found(creds) => {
                    clients.reload_credentials(creds).await;
                    tracing::info!(node_name = %config.node_name, vault_key = %key, "io-wapp credentials loaded from vault");
                }
                WappVaultResolution::Absent | WappVaultResolution::Transient => tracing::warn!(
                    node_name = %config.node_name, vault_key = %key,
                    "io-wapp vault credentials not ready; running degraded (refresh loop will retry)"
                ),
            },
            None => {
                tracing::warn!(node_name = %config.node_name, "no wapp.auth.key in effective config; running degraded until configured")
            }
        }
    } else {
        tracing::warn!(node_name = %config.node_name, "FLUXBEE_NODE_ILK_ID / hive suffix missing; vault lookup skipped");
    }

    // The person this IO carries (io.person_ilk, docs/digital-executive-v1-stage-2.md §7), read at
    // boot as io.dst_node is: the inbound processor stamps it while identity confirms it.
    let boot_person_ilk = extract_runtime_person_ilk(boot_state.effective_config.as_ref());
    let control_plane = Arc::new(RwLock::new(boot_state));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        control_plane.read().await.current_state.as_str(),
        control_plane.read().await.config_version,
    ));
    let vault_change_notify = Arc::new(Notify::new());

    // Vault refresh loop (family vault_ref pattern) — poll tick OR VAULT_SECRET_CHANGED wake.
    if let Some(vault) = vault_client.clone() {
        tokio::spawn(run_wapp_vault_refresh_loop(
            config.node_name.clone(),
            vault,
            clients.clone(),
            control_plane.clone(),
            vault_change_notify.clone(),
        ));
    }

    // Fanout inbound plane (webhooks fanned out by SY.edge). Identity + provisioning mirror io.slack;
    // media download is deferred (Phase 3 is text + explicit non-text markers), so no blob runtime yet.
    let identity: Arc<dyn IdentityResolver> = Arc::new(ShmIdentityResolver::new(&config.island_id));
    let provisioner: Arc<dyn IdentityProvisioner> = Arc::new(FluxbeeIdentityProvisioner::new(
        dispatcher.clone(),
        IdentityProvisionConfig {
            target: config.identity_target.clone(),
            timeout: Duration::from_millis(config.identity_timeout_ms),
        },
    ));
    let inbound = Arc::new(Mutex::new(InboundProcessor::new(
        dispatcher.sender_snapshot().uuid().to_string(),
        InboundConfig {
            ttl: config.ttl,
            dedup_ttl: Duration::from_millis(config.dedup_ttl_ms),
            dedup_max_entries: config.dedup_max_entries,
            dst_node: None,
            person_ilk: boot_person_ilk,
            self_ilk_id: self_ilk_id.clone(),
            provision_on_miss: true,
            blob_runtime: None,
            self_tenant_id: self_tenant_id.clone(),
        },
    )));
    // Checked against identity at once, so the status says it from the start, and then every
    // PERSON_RECHECK_INTERVAL, so the status follows identity with no inbound message.
    let person_status = PersonStatusView::default();
    sync_person_ilk(&inbound, &control_plane, identity.as_ref(), &person_status).await;
    if let Some(status) = person_status.get() {
        tracing::info!(
            node_name = %config.node_name,
            person_ilk = %status.ilk,
            confirmed = status.confirmed,
            reason = ?status.reason,
            "the config assigns the person this IO carries (io.person_ilk); inbound messages name it as dst_ilk while identity confirms it"
        );
    }
    tokio::spawn(run_wapp_person_check_loop(
        inbound.clone(),
        control_plane.clone(),
        identity.clone(),
        person_status.clone(),
    ));
    tokio::spawn(run_wapp_inbound_loop(
        dispatcher.clone(),
        control_plane.clone(),
        clients.clone(),
        identity.clone(),
        provisioner,
        inbound.clone(),
        person_status.clone(),
        config.island_id.clone(),
        self_tenant_id.clone(),
    ));

    // Outbound plane (Graph API). Text replies may carry blob-backed content (`content_ref`) resolved
    // from the shared blob root — honor the family `BLOB_ROOT` env (same var + default as io.slack) so
    // an operator override applies here too. Media send is deferred, so the toolkit resolves text only.
    let mut blob_runtime = IoBlobRuntimeConfig::default();
    if let Some(root) = env("BLOB_ROOT") {
        blob_runtime.blob_root = PathBuf::from(root);
    }
    let blob_toolkit = Arc::new(
        blob_runtime
            .build_toolkit()
            .map_err(|err| anyhow::anyhow!("io-wapp blob toolkit build failed: {err}"))?,
    );
    tokio::spawn(run_wapp_outbound_loop(
        dispatcher.clone(),
        control_plane.clone(),
        clients.clone(),
        blob_toolkit,
    ));

    run_router_control_loop(
        dispatcher,
        config.node_name.clone(),
        control_plane,
        control_metrics,
        adapter_contract,
        clients,
        vault_change_notify,
        PersonSync {
            inbound,
            identity,
            status: person_status,
        },
    )
    .await
}

fn build_io_wapp_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .command_channel(RPC_CH_INBOUND)
        .command_channel(RPC_CH_OUTBOUND)
        // Control-plane / system traffic (matched first).
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        // Fanned-out webhooks (delivery mode is invisible to the dispatcher — a Broadcast with
        // meta.target matches on msg_type like any message). meta.msg is None, so match by type.
        .post_pending_rule(
            RouteMatch::any_msg_type(IO_WAPP_INBOUND_FAMILY),
            RouteTarget::Command(RPC_CH_INBOUND),
        )
        // Everything else routed back to this node is a reply to relay outbound (msg_type "user",
        // and any other reply kind). Rules are checked in order, so this catch-all only ever sees
        // non-system, non-inbound-family traffic — i.e. replies. Mirrors io.slack's catch-all.
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command(RPC_CH_OUTBOUND))
        .build()
}

/// This node's configured business number (`io.phone_number_id`) — the self-select key. `None` while
/// unconfigured, which parks inbound (nothing to match against).
fn wapp_phone_number_id(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("phone_number_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// The Graph API version this node targets (`io.graph_api_version`), pinned-default when unset.
fn wapp_graph_api_version(effective: Option<&Value>) -> String {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("graph_api_version"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(WAPP_GRAPH_DEFAULT_VERSION)
        .to_string()
}

/// Where inbound relays (`io.dst_node`); absence → `None` → router resolve (like io.slack / io.api).
fn extract_runtime_dst_node(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("io"))
        .and_then(|io| io.get("dst_node"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// The person this IO carries, as its status answers say it (docs/digital-executive-v1-stage-2.md
/// §7): the inbound processor's `person_ilk_status`, copied out after each check, so a status
/// answer never waits for the processor, which an inbound message holds through its identity
/// provisioning (up to `IO_WAPP_IDENTITY_TIMEOUT_MS`, 10 s).
#[derive(Clone, Default)]
struct PersonStatusView(Arc<std::sync::Mutex<Option<PersonAssignmentStatus>>>);

impl PersonStatusView {
    fn get(&self) -> Option<PersonAssignmentStatus> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, status: Option<PersonAssignmentStatus>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = status;
    }
}

/// What the control loop needs to hand a config's `io.person_ilk` to the inbound processor.
struct PersonSync {
    inbound: Arc<Mutex<InboundProcessor>>,
    identity: Arc<dyn IdentityResolver>,
    status: PersonStatusView,
}

/// Hands the effective config's `io.person_ilk` to the inbound processor (`set_person_ilk`, as at
/// boot), checks it against identity now (`refresh_person_ilk`) and copies the outcome to the
/// status. With no person in the config, nothing is read: the status says nothing of it.
async fn sync_person_ilk(
    inbound: &Mutex<InboundProcessor>,
    control_plane: &RwLock<IoControlPlaneState>,
    identity: &dyn IdentityResolver,
    status: &PersonStatusView,
) {
    let person_ilk =
        extract_runtime_person_ilk(control_plane.read().await.effective_config.as_ref());
    let mut processor = inbound.lock().await;
    processor.set_person_ilk(person_ilk);
    status.set(processor.refresh_person_ilk(identity));
}

/// Checks the person the config assigns against identity every `PERSON_RECHECK_INTERVAL` (10 s,
/// the AI SDK's `speaks_as` check's pace), so the status follows a relation set or cleared in
/// identity with no inbound message, and an inbound message finds a fresh check.
async fn run_wapp_person_check_loop(
    inbound: Arc<Mutex<InboundProcessor>>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    identity: Arc<dyn IdentityResolver>,
    status: PersonStatusView,
) {
    loop {
        tokio::time::sleep(PERSON_RECHECK_INTERVAL).await;
        sync_person_ilk(&inbound, &control_plane, identity.as_ref(), &status).await;
    }
}

/// `NODE_STATUS_GET`'s answer: the one the SDK's `try_handle_default_node_status` gives (`status`
/// `ok` and `health_state`, `HEALTHY` unless `NODE_STATUS_DEFAULT_HEALTH_STATE` names another;
/// none at all with `NODE_STATUS_DEFAULT_HANDLER_ENABLED` off), and while the config assigns a
/// person, `extensions.person_ilk` `{ilk, confirmed, reason?}`
/// (`PersonAssignmentStatus::add_to_status`, docs/digital-executive-v1-stage-2.md §7), its health
/// unchanged. None for any other message.
fn wapp_node_status_reply(
    sender: &NodeSender,
    incoming: &WireMessage,
    person: Option<&PersonAssignmentStatus>,
) -> Option<WireMessage> {
    if !is_system_kind(&incoming.meta.msg_type)
        || incoming.meta.msg.as_deref() != Some(MSG_NODE_STATUS_GET)
    {
        return None;
    }
    let mut payload = default_node_status_payload()?;
    if let Some(person) = person {
        person.add_to_status(&mut payload);
    }
    Some(WireMessage {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(incoming.routing.src.clone()),
            ttl: 16,
            trace_id: incoming.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_NODE_STATUS_GET_RESPONSE.to_string()),
            ..Meta::default()
        },
        payload,
    })
}

/// The SDK's default status payload (`fluxbee_sdk::try_handle_default_node_status`, with its
/// environment switches): None when `NODE_STATUS_DEFAULT_HANDLER_ENABLED` turns it off.
fn default_node_status_payload() -> Option<Value> {
    let enabled = std::env::var("NODE_STATUS_DEFAULT_HANDLER_ENABLED")
        .ok()
        .map(|raw| {
            !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true);
    if !enabled {
        return None;
    }
    let health_state = match std::env::var("NODE_STATUS_DEFAULT_HEALTH_STATE")
        .unwrap_or_default()
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "DEGRADED" => "DEGRADED",
        "ERROR" => "ERROR",
        "UNKNOWN" => "UNKNOWN",
        _ => "HEALTHY",
    };
    Some(serde_json::json!({"status": "ok", "health_state": health_state}))
}

async fn run_router_control_loop(
    dispatcher: Arc<RouterDispatcher>,
    node_name: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    clients: WappClients,
    vault_change_notify: Arc<Notify>,
    person: PersonSync,
) -> Result<()> {
    let control_src = dispatcher.sender_snapshot().uuid().to_string();
    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| anyhow::anyhow!("io-wapp system receiver: {err}"))?;
    loop {
        let Some(msg) = system_rx.recv().await else {
            tracing::warn!("io-wapp system channel closed; exiting control loop");
            return Ok(());
        };
        let sender = dispatcher.sender_snapshot();

        if let Some(status) = wapp_node_status_reply(&sender, &msg, person.status.get().as_ref()) {
            sender.send(status).await?;
            continue;
        }

        // Fast path: a VAULT_SECRET_CHANGED for the whatsapp resource wakes the refresh loop.
        if is_system_kind(&msg.meta.msg_type)
            && msg
                .meta
                .msg
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(MSG_VAULT_SECRET_CHANGED))
        {
            if vault_change_is_wapp(&msg.payload) {
                tracing::info!(trace_id = %msg.routing.trace_id, "VAULT_SECRET_CHANGED (whatsapp); waking credential refresh");
                vault_change_notify.notify_one();
            }
            continue;
        }

        if let Some(response) = handle_io_control_plane_message(
            &msg,
            &node_name,
            &control_src,
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            &clients,
            person.status.get(),
        )
        .await
        {
            sender.send(response).await?;
            // A CONFIG_SET hands its io.person_ilk to the inbound processor at once, as boot does
            // (io.dst_node is read from the live config at each message). Off the control loop:
            // an inbound message may hold the processor through its identity provisioning.
            if is_config_set(&msg) {
                let (inbound, control_plane, identity, status) = (
                    person.inbound.clone(),
                    control_plane.clone(),
                    person.identity.clone(),
                    person.status.clone(),
                );
                tokio::spawn(async move {
                    sync_person_ilk(&inbound, &control_plane, identity.as_ref(), &status).await;
                });
            }
        }
    }
}

/// Namespace the WhatsApp customer number by the business number that received the message: two
/// distinct IO.wapp nodes (two business numbers) messaged by the same customer must resolve to
/// distinct identities, mirroring io.slack's per-node external_id namespacing (`tenant_hint` is the
/// same `phone_number_id`, so identity stays bound to its business context).
fn wapp_external_id(phone_number_id: &str, from_wa_id: &str) -> String {
    format!("{phone_number_id}:{from_wa_id}")
}

/// Render an inbound message to relay text. Non-text kinds are relayed as an explicit marker (or their
/// caption) — NEVER silently dropped; actual media download lands in the media sub-phase.
fn wapp_message_to_content(kind: &WappMessageKind) -> String {
    match kind {
        WappMessageKind::Text { body } => body.clone(),
        WappMessageKind::Media {
            media_type,
            caption,
            ..
        } => match caption.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
            Some(caption) => format!("{caption}\n[{media_type} attachment]"),
            None => format!("[{media_type} attachment]"),
        },
        WappMessageKind::Other { message_type } => {
            format!("[unsupported message type: {message_type}]")
        }
    }
}

/// Attach a `raw.wapp` provenance stub (mirrors io.slack's raw stub): the fields downstream needs to
/// reply and — for media — the `media_id` the media sub-phase will fetch.
fn attach_wapp_raw_stub(payload: &mut Value, msg: &WappInboundMessage) {
    let Some(obj) = payload.as_object_mut() else {
        return;
    };
    let mut wapp = serde_json::json!({
        "phone_number_id": msg.phone_number_id,
        "waba_id": msg.waba_id,
        "from_wa_id": msg.from_wa_id,
        "message_id": msg.message_id,
        "timestamp": msg.timestamp,
        "profile_name": msg.profile_name,
    });
    if let (
        Some(wapp_obj),
        WappMessageKind::Media {
            media_type,
            media_id,
            mime_type,
            ..
        },
    ) = (wapp.as_object_mut(), &msg.kind)
    {
        wapp_obj.insert(
            "media".to_string(),
            serde_json::json!({
                "media_type": media_type,
                "media_id": media_id,
                "mime_type": mime_type,
            }),
        );
    }
    obj.insert("raw".to_string(), serde_json::json!({ "wapp": wapp }));
}

/// Build the `text/v1` inbound payload for one WhatsApp message. `None` if the content is empty (a text
/// with a blank body — Meta shouldn't send it, but never hand a downstream AI node an empty turn; this
/// mirrors io.slack's `build_slack_inbound_payload_from_parts` empty guard) or if serialization fails.
/// Non-text kinds always render a non-empty marker, so this guard only ever trips on empty text.
fn build_wapp_inbound_payload(msg: &WappInboundMessage) -> Option<Value> {
    let content = wapp_message_to_content(&msg.kind);
    if content.trim().is_empty() {
        return None;
    }
    match TextV1Payload::new(&content, vec![]).to_value() {
        Ok(mut payload) => {
            attach_wapp_raw_stub(&mut payload, msg);
            Some(payload)
        }
        Err(error) => {
            tracing::warn!(error = %error, "failed to build base text/v1 inbound payload");
            None
        }
    }
}

/// Ship (or dedup-drop) one resolved inbound message. Mirrors io.slack: the edge already ACKed Meta
/// with a fast 200 (Meta will not redeliver), so a router send failure here is a genuine, unrecoverable
/// LOSS — log it loud with the trace_id, never silently.
async fn dispatch_inbound_outcome(sender: &NodeSender, outcome: InboundOutcome) {
    match outcome {
        InboundOutcome::SendNow(msg) => {
            let trace_id = msg.routing.trace_id.clone();
            if let Err(e) = sender.send(msg).await {
                tracing::error!(
                    error = ?e, %trace_id,
                    "LOST inbound message: edge already ACKed Meta (no redelivery) but router send \
                     failed on a terminal disconnect"
                );
            } else {
                tracing::debug!(%trace_id, "inbound relayed to router");
            }
        }
        InboundOutcome::DroppedDuplicate => {
            tracing::debug!("dedup hit; dropping inbound");
        }
    }
}

/// The fanout inbound loop. Every IO.wapp node subscribes to the same `IO_WAPP_INBOUND_FAMILY`; the
/// edge Broadcasts each webhook to all of them. Per message this node: (1) verifies the Meta HMAC with
/// ITS OWN `app_secret` (default-deny — a degraded node or bad signature drops), (2) self-selects by
/// `phone_number_id` (the copy for another node's business number is silently skipped — that's the
/// fanout design, not a drop), then (3) dedup + relay via the shared InboundProcessor.
async fn run_wapp_inbound_loop(
    dispatcher: Arc<RouterDispatcher>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    clients: WappClients,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
    // The status's copy of the person this IO carries: refreshed after each message, whose
    // processing may check the person against identity again.
    person_status: PersonStatusView,
    // For the first-contact gate (io_common::frontdesk_gate): the identity SHM is looked up by
    // hive, and the tenant is the node's (the external user belongs to it).
    hive_id: String,
    self_tenant_id: Option<String>,
) -> Result<()> {
    let mut inbound_rx = dispatcher
        .take_command_receiver(RPC_CH_INBOUND)
        .await
        .map_err(|err| anyhow::anyhow!("io-wapp inbound receiver: {err}"))?;

    while let Some(msg) = inbound_rx.recv().await {
        let trace_id = msg.routing.trace_id.clone();

        // (1) Live credentials — a degraded node CANNOT verify the HMAC, so it must drop (default-deny).
        let Some(creds) = clients.credentials().await else {
            tracing::warn!(%trace_id, "inbound webhook dropped: node degraded (no credentials to verify signature)");
            continue;
        };

        // Self-select key + relay target from the VALIDATED effective config.
        let (my_phone_number_id, dst_node) = {
            let state = control_plane.read().await;
            (
                wapp_phone_number_id(state.effective_config.as_ref()),
                extract_runtime_dst_node(state.effective_config.as_ref()),
            )
        };
        let Some(my_phone_number_id) = my_phone_number_id else {
            tracing::warn!(%trace_id, "inbound webhook dropped: no io.phone_number_id configured");
            continue;
        };

        // (2) Verify the Meta signature over the EXACT raw bytes, then parse THOSE bytes (never a
        // re-serialization — the signature covers the wire body verbatim).
        let raw_b64 = msg.payload.get("raw_body_base64").and_then(|v| v.as_str());
        let signature = msg.payload.get("signature").and_then(|v| v.as_str());
        let (Some(raw_b64), Some(signature)) = (raw_b64, signature) else {
            tracing::warn!(%trace_id, "inbound webhook dropped: missing raw_body_base64 or signature");
            continue;
        };
        let Some(raw_body) = base64_decode(raw_b64) else {
            tracing::warn!(%trace_id, "inbound webhook dropped: raw_body_base64 not valid base64");
            continue;
        };
        if !verify_webhook_signature(&creds.app_secret, &raw_body, signature) {
            tracing::warn!(%trace_id, "inbound webhook dropped: signature verification failed (forged, tampered, or wrong app_secret)");
            continue;
        }
        let envelope: Value = match serde_json::from_slice(&raw_body) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(%trace_id, error = %error, "inbound webhook dropped: signed body is not valid JSON");
                continue;
            }
        };

        // (3) Self-select + dedup + relay each message addressed to THIS business number.
        let sender = dispatcher.sender_snapshot();
        for wapp_msg in extract_inbound_messages(&envelope) {
            if wapp_msg.phone_number_id != my_phone_number_id {
                // Not our number — normally a sibling io.wapp node owns this copy (fanout design, not a
                // drop). But the SAME path also swallows a message whose number matches NO node (an
                // operator typo in io.phone_number_id, or a Meta-subscribed number with no node yet), so
                // log at debug — like the io.slack binding-mismatch peer — carrying both ids so raising
                // the level surfaces the off-by-one instead of black-holing inbound invisibly.
                tracing::debug!(
                    %trace_id,
                    configured_phone_number_id = %my_phone_number_id,
                    event_phone_number_id = %wapp_msg.phone_number_id,
                    "dropping inbound WhatsApp message outside this node's phone_number_id"
                );
                continue;
            }
            let Some(payload) = build_wapp_inbound_payload(&wapp_msg) else {
                continue;
            };
            let io_ctx = wapp_inbound_io_context(
                &wapp_msg.phone_number_id,
                &wapp_msg.from_wa_id,
                &wapp_msg.message_id,
                wapp_msg.timestamp.as_deref(),
            );
            let mut processor = inbound.lock().await;
            let outcome = processor
                .process_inbound(
                    identity.as_ref(),
                    Some(provisioner.as_ref()),
                    ResolveOrCreateInput {
                        channel: "whatsapp".to_string(),
                        external_id: wapp_external_id(
                            &wapp_msg.phone_number_id,
                            &wapp_msg.from_wa_id,
                        ),
                        src_ilk_override: None,
                        tenant_id: None,
                        tenant_hint: Some(wapp_msg.phone_number_id.clone()),
                        attributes: serde_json::json!({
                            "phone_number_id": wapp_msg.phone_number_id,
                            "waba_id": wapp_msg.waba_id,
                            "profile_name": wapp_msg.profile_name,
                        }),
                        ilk_type: Some("human".to_string()),
                    },
                    dst_node.clone(),
                    io_ctx,
                    payload,
                )
                .await;
            person_status.set(processor.person_ilk_status());
            drop(processor);
            // First-contact gate (OFF by default, io.api's gate-then-forward pattern): if the
            // sender is not registered yet, it is introduced to the frontdesk and the message still
            // goes on to its destination. See io_common::frontdesk_gate.
            if let InboundOutcome::SendNow(ref relayed) = outcome {
                let gate = io_common::frontdesk_gate::FrontdeskGateConfig::from_effective_config(
                    control_plane.read().await.effective_config.as_ref(),
                );
                io_common::frontdesk_gate::gate_then_forward(
                    &sender,
                    &gate,
                    &hive_id,
                    "whatsapp",
                    &wapp_external_id(&wapp_msg.phone_number_id, &wapp_msg.from_wa_id),
                    self_tenant_id.as_deref(),
                    relayed.meta.src_ilk.as_deref(),
                    relayed.routing.trace_id.as_str(),
                )
                .await;
            }

            dispatch_inbound_outcome(&sender, outcome).await;
        }
    }
    tracing::warn!("io-wapp inbound channel closed; exiting inbound loop");
    Ok(())
}

/// The outbound loop. A reply relayed back to this node (msg_type "user", carrying the round-tripped
/// `meta.context.io.reply_target` of kind `wapp_post`) is turned into a Graph API text send: address
/// the customer `to_wa_id` FROM the reply target's `phone_number_id` (falling back to this node's
/// configured `io.phone_number_id`).
///
/// Media send is deferred (phase 4-media), so we resolve ONLY the text (`resolve_text_v1_text_only_*`)
/// and never resolve/hard-fail on attachment blobs the node would not upload anyway — an unsendable
/// attachment must not sink a deliverable text reply. A media-ONLY reply (attachments, no text) can't
/// be delivered yet, so it is surfaced at WARN (not silently dropped).
async fn run_wapp_outbound_loop(
    dispatcher: Arc<RouterDispatcher>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    clients: WappClients,
    blob_toolkit: Arc<BlobToolkit>,
) -> Result<()> {
    let mut outbound_rx = dispatcher
        .take_command_receiver(RPC_CH_OUTBOUND)
        .await
        .map_err(|err| anyhow::anyhow!("io-wapp outbound receiver: {err}"))?;

    while let Some(msg) = outbound_rx.recv().await {
        let trace_id = msg.routing.trace_id.clone();

        // The reply target (customer wa_id + optional business number) rides in meta.context.
        let Some(target) = msg.meta.context.as_ref().and_then(extract_wapp_post_target) else {
            tracing::debug!(%trace_id, "outbound: no wapp_post reply target in meta.context; dropping");
            continue;
        };

        // Business number + Graph version from the reply target, falling back to the node's config.
        let (fallback_pnid, version) = {
            let state = control_plane.read().await;
            (
                wapp_phone_number_id(state.effective_config.as_ref()),
                wapp_graph_api_version(state.effective_config.as_ref()),
            )
        };
        let Some(phone_number_id) = target.phone_number_id.clone().or(fallback_pnid) else {
            tracing::warn!(%trace_id, "outbound: no phone_number_id (reply target absent + node unconfigured); dropping");
            continue;
        };

        // Resolve ONLY the text (media deferred; attachment blobs are counted, never resolved).
        let resolved =
            match resolve_text_v1_text_only_for_outbound(blob_toolkit.as_ref(), &msg.payload, true)
                .await
            {
                Ok(resolved) => resolved,
                Err(err) => {
                    tracing::warn!(
                        %trace_id, code = %err.canonical_code(),
                        "outbound: failed to resolve text/v1 payload; dropping"
                    );
                    continue;
                }
            };

        if resolved.text.trim().is_empty() {
            if resolved.attachment_count > 0 {
                // A media-only reply the bot intended to send — we can't yet (media out deferred). Surface
                // it (WARN, not a silent debug drop) so the gap is visible; do not spam the customer.
                tracing::warn!(
                    %trace_id, attachments = resolved.attachment_count,
                    "outbound: media-only reply not delivered (WhatsApp media send deferred, phase 4-media)"
                );
            } else {
                tracing::debug!(%trace_id, "outbound: empty reply; nothing to send");
            }
            continue;
        }

        if resolved.attachment_count > 0 {
            tracing::warn!(
                %trace_id, attachments = resolved.attachment_count,
                "outbound: WhatsApp media send deferred (phase 4-media); sending text only"
            );
        }

        match clients
            .post_text(&version, &phone_number_id, &target.to_wa_id, &resolved.text)
            .await
        {
            Ok(message_id) => {
                tracing::debug!(%trace_id, wa_message_id = %message_id, "outbound: WhatsApp message sent")
            }
            Err(err) => {
                tracing::error!(%trace_id, error = %err, "outbound: WhatsApp Graph send failed")
            }
        }
    }
    tracing::warn!("io-wapp outbound channel closed; exiting outbound loop");
    Ok(())
}

async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    clients: &WappClients,
    person: Option<PersonAssignmentStatus>,
) -> Option<WireMessage> {
    let command = msg.meta.msg.as_deref().unwrap_or_default();
    if !is_system_kind(&msg.meta.msg_type) {
        return None;
    }

    if command.eq_ignore_ascii_case("PING") {
        let state = control_plane.read().await.clone();
        let payload = serde_json::json!({
            "ok": true, "node_name": node_name, "state": state.current_state.as_str(),
        });
        return Some(build_system_reply(msg, control_src, "PONG", payload));
    }

    if command.eq_ignore_ascii_case("STATUS") {
        let state = control_plane.read().await.clone();
        let mut payload = serde_json::json!({
            "ok": true,
            "node_name": node_name,
            "state": state.current_state.as_str(),
            "config_source": state.config_source.as_str(),
            "schema_version": state.schema_version,
            "config_version": state.config_version,
            "last_error": state.last_error,
            "metrics": { "control_plane": control_metrics.snapshot() },
            "runtime": {
                "credentials_configured": clients.credentials_configured().await,
                "config_generation": clients.config_generation().await,
            }
        });
        // The person the config assigns, as NODE_STATUS_GET says it: extensions.person_ilk.
        if let Some(person) = person.as_ref() {
            person.add_to_status(&mut payload);
        }
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
            build_io_config_get_response_payload(
                node_name,
                &redacted,
                build_io_adapter_contract_payload(
                    adapter_contract,
                    state.effective_config.as_ref(),
                ),
            )
        }
        Ok(IoControlPlaneRequest::Set(set_payload)) => {
            apply_wapp_config_set(
                &set_payload,
                node_name,
                control_plane.clone(),
                control_metrics.as_ref(),
                adapter_contract,
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

async fn apply_wapp_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: &IoControlPlaneMetrics,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> Value {
    let mut state = control_plane.write().await;

    if let Err(err) = ensure_config_version_advances(payload.config_version, state.config_version) {
        log_config_set_stale_rejected(node_name, payload.config_version, state.config_version);
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            state.config_version,
            "stale_config",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            "stale_config",
            err.to_string(),
        );
    }

    let candidate = match apply_adapter_config_replace(adapter_contract, &payload.config) {
        Ok(cfg) => cfg,
        Err(err) => {
            state.current_state = IoNodeLifecycleState::FailedConfig;
            state.last_error = Some(IoControlPlaneErrorInfo {
                code: err.code().to_string(),
                message: err.to_string(),
            });
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
    };

    let previous_version = state.config_version;
    state.current_state = IoNodeLifecycleState::Configured;
    state.config_source = IoConfigSource::Dynamic;
    state.schema_version = payload.schema_version;
    state.config_version = payload.config_version;
    state.effective_config = Some(candidate);
    state.last_error = None;

    if let Err(err) = persist_io_control_plane_state(node_name, &state) {
        let code = "persist_failed";
        let message = err.to_string();
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: code.to_string(),
            message: message.clone(),
        });
        log_config_set_persist_error(
            node_name,
            payload.schema_version,
            payload.config_version,
            &message,
        );
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            previous_version,
            code,
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(node_name, &redacted, code, message);
    }

    // A CONFIG_SET that changes wapp.auth.key is picked up by the refresh loop (reads the key from the
    // live control plane each tick); no explicit reload needed here.
    control_metrics.record_config_set_ok(state.current_state.as_str(), state.config_version);
    log_config_set_applied(
        node_name,
        payload.schema_version,
        payload.config_version,
        &[],
        &[],
        &[],
    );
    let redacted = redact_state(&state, adapter_contract);
    build_io_config_set_ok_payload(node_name, &redacted)
}

/// Reloads credentials from vault on the poll tick OR a VAULT_SECRET_CHANGED wake. Reads the CURRENT
/// vault key from the live control plane each pass, so a CONFIG_SET that changes wapp.auth.key is
/// honored without a restart.
async fn run_wapp_vault_refresh_loop(
    node_name: String,
    vault: Arc<VaultClient>,
    clients: WappClients,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    vault_change_notify: Arc<Notify>,
) {
    let mut ticker =
        tokio::time::interval(Duration::from_secs(IO_WAPP_VAULT_REFRESH_INTERVAL_SECS));
    ticker.tick().await; // skip the immediate first tick
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = vault_change_notify.notified() => {
                tracing::debug!(node_name = %node_name, "vault secret changed; refreshing whatsapp credentials now");
            }
        }
        let Some(key) = wapp_vault_key(control_plane.read().await.effective_config.as_ref()) else {
            tracing::debug!(node_name = %node_name, "no wapp.auth.key in effective config; vault refresh skipped");
            continue;
        };
        match resolve_wapp_credentials_from_vault(&vault, &key).await {
            WappVaultResolution::Found(creds) => clients.reload_credentials(creds).await,
            WappVaultResolution::Absent => clients.clear_credentials().await,
            WappVaultResolution::Transient => { /* keep current creds; retry next pass */ }
        }
    }
}

/// A `CONFIG_SET` to this node's control plane.
fn is_config_set(msg: &WireMessage) -> bool {
    is_system_kind(&msg.meta.msg_type)
        && msg
            .meta
            .msg
            .as_deref()
            .is_some_and(|command| command.eq_ignore_ascii_case("CONFIG_SET"))
}

fn redact_state(
    state: &IoControlPlaneState,
    adapter_contract: &dyn IoAdapterConfigContract,
) -> IoControlPlaneState {
    let mut redacted = state.clone();
    redacted.effective_config = redacted
        .effective_config
        .as_ref()
        .map(|effective| adapter_contract.redact_effective_config(effective));
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
            ..Meta::default()
        },
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wapp_vault_key_reads_auth_key() {
        let cfg = json!({"wapp":{"auth":{"type":"vault_ref","resource_type":"whatsapp","key":"wapp/IO.wapp@motherbee"}}});
        assert_eq!(
            wapp_vault_key(Some(&cfg)).as_deref(),
            Some("wapp/IO.wapp@motherbee")
        );
        assert_eq!(wapp_vault_key(None), None);
        assert_eq!(
            wapp_vault_key(Some(&json!({"wapp":{"auth":{"key":"   "}}}))),
            None
        );
    }

    #[test]
    fn extract_creds_requires_all_three_fields() {
        let ok = extract_wapp_creds_from_vault_value(
            &json!({"access_token":"A","app_secret":"S","verify_token":"V"}),
        )
        .expect("all present");
        assert_eq!(ok.access_token, "A");
        assert_eq!(ok.app_secret, "S");
        assert_eq!(ok.verify_token, "V");
        // a missing field => None
        assert!(
            extract_wapp_creds_from_vault_value(&json!({"access_token":"A","app_secret":"S"}))
                .is_none()
        );
        // a blank field => None
        assert!(extract_wapp_creds_from_vault_value(
            &json!({"access_token":"A","app_secret":"S","verify_token":"  "})
        )
        .is_none());
        // bare string => None (ambiguous)
        assert!(extract_wapp_creds_from_vault_value(&json!("A")).is_none());
    }

    #[test]
    fn vault_change_is_wapp_matches_only_whatsapp() {
        assert!(vault_change_is_wapp(
            &json!({"resource_type":"whatsapp","op":"put"})
        ));
        assert!(vault_change_is_wapp(&json!({"resource_type":"WhatsApp"})));
        assert!(!vault_change_is_wapp(&json!({"resource_type":"slack"})));
        assert!(!vault_change_is_wapp(&json!({"op":"put"})));
    }

    #[tokio::test]
    async fn credentials_generation_is_change_driven_and_delete_aware() {
        let c = WappClients::new().expect("http client");
        assert_eq!(c.config_generation().await, 0);
        assert!(!c.credentials_configured().await);
        let creds = WappCredentials {
            access_token: "A".into(),
            app_secret: "S".into(),
            verify_token: "V".into(),
        };
        c.reload_credentials(creds.clone()).await;
        assert_eq!(c.config_generation().await, 1);
        assert!(c.credentials_configured().await);
        // identical reload => no churn
        c.reload_credentials(creds).await;
        assert_eq!(c.config_generation().await, 1);
        // rotation bumps
        c.reload_credentials(WappCredentials {
            access_token: "A2".into(),
            app_secret: "S".into(),
            verify_token: "V".into(),
        })
        .await;
        assert_eq!(c.config_generation().await, 2);
        // clear drops + bumps; clearing again is a no-op
        c.clear_credentials().await;
        assert_eq!(c.config_generation().await, 3);
        assert!(!c.credentials_configured().await);
        c.clear_credentials().await;
        assert_eq!(c.config_generation().await, 3);
    }

    #[test]
    fn phone_number_id_and_dst_node_read_io_block() {
        let cfg = json!({"io":{"phone_number_id":" 1555 ","dst_node":" WF.router@motherbee "}});
        assert_eq!(wapp_phone_number_id(Some(&cfg)).as_deref(), Some("1555"));
        assert_eq!(
            extract_runtime_dst_node(Some(&cfg)).as_deref(),
            Some("WF.router@motherbee")
        );
        // absence / blank => None (router-resolve for dst_node; parks inbound for phone_number_id)
        assert_eq!(wapp_phone_number_id(None), None);
        assert_eq!(extract_runtime_dst_node(Some(&json!({"io":{}}))), None);
        assert_eq!(
            wapp_phone_number_id(Some(&json!({"io":{"phone_number_id":"  "}}))),
            None
        );
    }

    #[test]
    fn external_id_namespaces_customer_by_business_number() {
        // same customer, two business numbers => two distinct identities
        assert_eq!(wapp_external_id("111", "573001112222"), "111:573001112222");
        assert_ne!(
            wapp_external_id("111", "573001112222"),
            wapp_external_id("222", "573001112222")
        );
    }

    #[test]
    fn message_to_content_never_drops_non_text() {
        assert_eq!(
            wapp_message_to_content(&WappMessageKind::Text {
                body: "hello".into()
            }),
            "hello"
        );
        // media with caption => caption + marker; without => marker only
        assert_eq!(
            wapp_message_to_content(&WappMessageKind::Media {
                media_type: "image".into(),
                media_id: "MID".into(),
                caption: Some("look".into()),
                mime_type: Some("image/jpeg".into()),
            }),
            "look\n[image attachment]"
        );
        assert_eq!(
            wapp_message_to_content(&WappMessageKind::Media {
                media_type: "document".into(),
                media_id: "MID".into(),
                caption: None,
                mime_type: None,
            }),
            "[document attachment]"
        );
        // blank caption falls back to the marker (no empty-content relay)
        assert_eq!(
            wapp_message_to_content(&WappMessageKind::Media {
                media_type: "audio".into(),
                media_id: "MID".into(),
                caption: Some("   ".into()),
                mime_type: None,
            }),
            "[audio attachment]"
        );
        assert_eq!(
            wapp_message_to_content(&WappMessageKind::Other {
                message_type: "location".into()
            }),
            "[unsupported message type: location]"
        );
    }

    #[test]
    fn inbound_payload_carries_text_and_raw_stub() {
        let msg = WappInboundMessage {
            phone_number_id: "111".into(),
            waba_id: "WABA".into(),
            from_wa_id: "573001112222".into(),
            profile_name: Some("Ada".into()),
            message_id: "wamid.X".into(),
            timestamp: Some("1700000000".into()),
            kind: WappMessageKind::Text {
                body: "hello".into(),
            },
        };
        let payload = build_wapp_inbound_payload(&msg).expect("payload");
        // base text/v1 content preserved
        assert_eq!(payload["content"], json!("hello"));
        // provenance stub present with reply fields
        let wapp = &payload["raw"]["wapp"];
        assert_eq!(wapp["phone_number_id"], json!("111"));
        assert_eq!(wapp["from_wa_id"], json!("573001112222"));
        assert_eq!(wapp["message_id"], json!("wamid.X"));
        assert_eq!(wapp["profile_name"], json!("Ada"));
        // text message => no media sub-object
        assert!(wapp.get("media").is_none());
    }

    #[test]
    fn inbound_payload_media_stub_carries_media_id_for_fetch() {
        let msg = WappInboundMessage {
            phone_number_id: "111".into(),
            waba_id: "WABA".into(),
            from_wa_id: "573001112222".into(),
            profile_name: None,
            message_id: "wamid.Y".into(),
            timestamp: None,
            kind: WappMessageKind::Media {
                media_type: "image".into(),
                media_id: "MID-42".into(),
                caption: Some("foto".into()),
                mime_type: Some("image/png".into()),
            },
        };
        let payload = build_wapp_inbound_payload(&msg).expect("payload");
        assert_eq!(payload["content"], json!("foto\n[image attachment]"));
        let media = &payload["raw"]["wapp"]["media"];
        assert_eq!(media["media_id"], json!("MID-42"));
        assert_eq!(media["media_type"], json!("image"));
        assert_eq!(media["mime_type"], json!("image/png"));
    }

    #[test]
    fn inbound_payload_drops_empty_text_but_keeps_non_text_markers() {
        let base = |kind| WappInboundMessage {
            phone_number_id: "111".into(),
            waba_id: "WABA".into(),
            from_wa_id: "573001".into(),
            profile_name: None,
            message_id: "wamid.Z".into(),
            timestamp: None,
            kind,
        };
        // empty / whitespace text => no relay (mirrors io.slack empty guard)
        assert!(
            build_wapp_inbound_payload(&base(WappMessageKind::Text { body: "".into() })).is_none()
        );
        assert!(
            build_wapp_inbound_payload(&base(WappMessageKind::Text { body: "  ".into() }))
                .is_none()
        );
        // a non-text kind always renders a marker, so it is never dropped by the empty guard
        assert!(build_wapp_inbound_payload(&base(WappMessageKind::Other {
            message_type: "location".into()
        }))
        .is_some());
    }

    #[test]
    fn graph_api_version_defaults_and_overrides() {
        assert_eq!(wapp_graph_api_version(None), WAPP_GRAPH_DEFAULT_VERSION);
        assert_eq!(
            wapp_graph_api_version(Some(&json!({"io":{"graph_api_version":" v21.0 "}}))),
            "v21.0"
        );
        // blank => pinned default
        assert_eq!(
            wapp_graph_api_version(Some(&json!({"io":{"graph_api_version":"  "}}))),
            WAPP_GRAPH_DEFAULT_VERSION
        );
    }

    #[test]
    fn graph_text_body_matches_meta_contract() {
        let body = build_wapp_text_message_body("573001112222", "hello world");
        assert_eq!(body["messaging_product"], json!("whatsapp"));
        assert_eq!(body["recipient_type"], json!("individual"));
        assert_eq!(body["to"], json!("573001112222"));
        assert_eq!(body["type"], json!("text"));
        assert_eq!(body["text"]["body"], json!("hello world"));
        assert_eq!(body["text"]["preview_url"], json!(false));
    }

    #[test]
    fn retry_after_parses_clamps_and_defaults() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let with = |v: &str| {
            let mut h = HeaderMap::new();
            h.insert(RETRY_AFTER, HeaderValue::from_str(v).unwrap());
            parse_retry_after(&h)
        };
        // missing header => 1s default
        assert_eq!(parse_retry_after(&HeaderMap::new()), Duration::from_secs(1));
        // valid integer seconds
        assert_eq!(with("5"), Duration::from_secs(5));
        // clamp high to 30s
        assert_eq!(with("9000"), Duration::from_secs(30));
        // clamp low (0) up to 1s
        assert_eq!(with("0"), Duration::from_secs(1));
        // garbage (HTTP-date form we don't parse) => 1s default
        assert_eq!(
            with("Wed, 21 Oct 2026 07:28:00 GMT"),
            Duration::from_secs(1)
        );
    }

    #[tokio::test]
    async fn config_set_advances_from_current_version_not_rejected_as_stale() {
        // Regression: apply_wapp_config_set must pass (payload_version, current_version) to
        // ensure_config_version_advances (io.slack/io.api order). A swapped call rejected EVERY
        // config_set as stale, so io.wapp could never be configured. A payload at version 2 over a
        // node at version 1 must be ACCEPTED (config_version strictly advances).
        let control_plane = Arc::new(RwLock::new(IoControlPlaneState::default())); // config_version = 0
        let metrics = IoControlPlaneMetrics::with_initial_state("UNCONFIGURED", 0);
        let contract = IoWappAdapterConfigContract;
        let payload = fluxbee_sdk::node_config::NodeConfigSetPayload {
            node_name: "IO.wapp.default@motherbee".to_string(),
            schema_version: 1,
            config_version: 2,
            apply_mode: "replace".to_string(),
            config: serde_json::json!({
                "wapp": { "auth": { "type": "vault_ref", "resource_type": "whatsapp", "key": "wapp_test" } },
                "io": { "phone_number_id": "111", "waba_id": "WABA" }
            }),
            ..Default::default()
        };
        let resp = apply_wapp_config_set(
            &payload,
            "IO.wapp.default@motherbee",
            control_plane.clone(),
            &metrics,
            &contract,
        )
        .await;
        assert_ne!(
            resp.get("error_code").and_then(|v| v.as_str()),
            Some("stale_config"),
            "config_version 2 over 0 must not be stale: {resp}"
        );
        assert_eq!(
            control_plane.read().await.config_version,
            2,
            "version must advance to 2"
        );
    }

    // --- docs/digital-executive-v1-stage-2.md §7: the person this IO carries (io.person_ilk) ---

    const IO_ILK: &str = "ilk:7d1e0b2a-5c3f-4a8e-9b6d-2f4c8a1e3b50";
    const ED: &str = "ilk:3f0c6a52-9c1e-4f5e-8f43-6d2b1a7e9c10";
    const TENANT: &str = "tnt:6a1f3c2e-0b4d-4e8a-9c7f-2d5e8b1a3c40";

    /// An identity SHM of the test's own: the entries it holds.
    #[derive(Default)]
    struct Region(std::sync::Mutex<std::collections::HashMap<String, Value>>);

    impl Region {
        /// The IO's ILK working for the ED as its channel, and the ED in the IO's tenant.
        fn confirming() -> Self {
            let region = Self::default();
            region.put(IO_ILK, Some("channel"));
            region.put(ED, None);
            region
        }

        fn put(&self, ilk: &str, work_role: Option<&str>) {
            let entry = match work_role {
                Some(role) => json!({"ilk_id": ilk, "tenant_id": TENANT,
                    "registration_status": "complete", "ilk_type": "agent",
                    "works_for": ED, "work_role": role}),
                None => json!({"ilk_id": ilk, "tenant_id": TENANT,
                    "registration_status": "complete", "ilk_type": "human"}),
            };
            self.0.lock().unwrap().insert(ilk.to_string(), entry);
        }
    }

    impl IdentityResolver for Region {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn lookup(
            &self,
            _input: &ResolveOrCreateInput,
        ) -> Result<Option<String>, io_common::identity::IdentityError> {
            Ok(None)
        }

        fn ilk_entry(
            &self,
            ilk_id: &str,
        ) -> Result<
            Option<fluxbee_sdk::identity::IdentityIlkOption>,
            io_common::identity::IdentityError,
        > {
            Ok(self
                .0
                .lock()
                .unwrap()
                .get(ilk_id)
                .map(|entry| serde_json::from_value(entry.clone()).expect("an entry")))
        }
    }

    fn wapp_config(person_ilk: Option<&str>) -> Value {
        let mut io = json!({"phone_number_id": "1234567890", "waba_id": "2",
            "dst_node": "AI.ed4qm7k2xa@worker1"});
        if let Some(person) = person_ilk {
            io["person_ilk"] = json!(person);
        }
        json!({"wapp": {"auth": {"type": "vault_ref", "resource_type": "whatsapp",
            "key": "acme-laura-wapp"}}, "io": io})
    }

    fn processor(person_ilk: Option<&str>) -> Mutex<InboundProcessor> {
        Mutex::new(InboundProcessor::new(
            "io-wapp-test-uuid",
            InboundConfig {
                person_ilk: person_ilk.map(str::to_string),
                self_ilk_id: Some(IO_ILK.to_string()),
                self_tenant_id: Some(TENANT.to_string()),
                blob_runtime: None,
                ..InboundConfig::default()
            },
        ))
    }

    fn configured(config: Value) -> RwLock<IoControlPlaneState> {
        RwLock::new(IoControlPlaneState {
            current_state: IoNodeLifecycleState::Configured,
            effective_config: Some(config),
            ..IoControlPlaneState::default()
        })
    }

    /// The person the config assigns reaches the inbound processor at boot (from the effective
    /// config, as io.dst_node does) and at each config change, and the status says identity's
    /// word on it: confirmed, why not, or nothing once the config names none.
    #[tokio::test]
    async fn the_config_hands_its_person_to_the_inbound_processor_and_the_status_says_it() {
        let region = Region::confirming();
        let boot = wapp_config(Some(ED));
        let boot_person = extract_runtime_person_ilk(Some(&boot));
        assert_eq!(boot_person.as_deref(), Some(ED));
        let inbound = processor(boot_person.as_deref());
        let control_plane = configured(boot);
        let status = PersonStatusView::default();

        sync_person_ilk(&inbound, &control_plane, &region, &status).await;
        assert_eq!(
            status.get(),
            Some(PersonAssignmentStatus {
                ilk: ED.to_string(),
                confirmed: true,
                reason: None
            })
        );

        // Identity takes the relation away: the next check says why.
        region.put(IO_ILK, Some("conscious"));
        sync_person_ilk(&inbound, &control_plane, &region, &status).await;
        let now = status.get().expect("a status");
        assert!(!now.confirmed);
        assert!(
            now.reason
                .as_deref()
                .unwrap_or_default()
                .contains("the role conscious"),
            "{now:?}"
        );

        // A config change that names no person: nothing more is said of one.
        control_plane.write().await.effective_config = Some(wapp_config(None));
        sync_person_ilk(&inbound, &control_plane, &region, &status).await;
        assert_eq!(status.get(), None);
        assert_eq!(inbound.lock().await.person_ilk_status(), None);

        // And one that names it again is checked at once.
        region.put(IO_ILK, Some("channel"));
        control_plane.write().await.effective_config = Some(wapp_config(Some(ED)));
        sync_person_ilk(&inbound, &control_plane, &region, &status).await;
        assert_eq!(status.get().map(|status| status.confirmed), Some(true));
    }

    /// NODE_STATUS_GET's answer is the SDK's default one, field for field, and while the config
    /// assigns a person it adds `extensions.person_ilk`, its health unchanged. Any other message
    /// is not its to answer.
    #[tokio::test]
    async fn the_node_status_answer_is_the_sdks_default_with_the_person() {
        let (dispatcher, mut harness) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "IO.wapp.laura@worker1",
            build_io_wapp_rpc_profile().expect("profile"),
        );
        let sender = dispatcher.sender_snapshot();
        let request = WireMessage {
            routing: Routing {
                src: "orchestrator-uuid".to_string(),
                src_l2_name: Some("SY.orchestrator@worker1".to_string()),
                dst: Destination::Unicast(sender.uuid().to_string()),
                ttl: 8,
                trace_id: "trace-status".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_NODE_STATUS_GET.to_string()),
                ..Meta::default()
            },
            payload: json!({}),
        };
        assert!(
            fluxbee_sdk::try_handle_default_node_status(&sender, &request)
                .await
                .expect("the default answer")
        );
        let default = harness
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("the default answer");

        let plain = wapp_node_status_reply(&sender, &request, None).expect("an answer");
        assert_eq!(
            serde_json::to_value(&plain).expect("plain"),
            serde_json::to_value(&default).expect("default")
        );

        let person = PersonAssignmentStatus {
            ilk: ED.to_string(),
            confirmed: false,
            reason: Some("identity names no person this IO's ILK works for".to_string()),
        };
        let with_person =
            wapp_node_status_reply(&sender, &request, Some(&person)).expect("an answer");
        let mut expected = default.payload.clone();
        expected["extensions"] = json!({"person_ilk": {"ilk": ED, "confirmed": false,
            "reason": "identity names no person this IO's ILK works for"}});
        assert_eq!(with_person.payload, expected);
        assert_eq!(
            with_person.payload["health_state"],
            default.payload["health_state"]
        );
        assert_eq!(with_person.meta.msg, default.meta.msg);

        let mut config_get = request.clone();
        config_get.meta.msg = Some("CONFIG_GET".to_string());
        assert!(wapp_node_status_reply(&sender, &config_get, Some(&person)).is_none());
    }

    #[tokio::test]
    async fn credentials_snapshot_reflects_live_state() {
        let c = WappClients::new().expect("http client");
        assert!(c.credentials().await.is_none());
        let creds = WappCredentials {
            access_token: "A".into(),
            app_secret: "S".into(),
            verify_token: "V".into(),
        };
        c.reload_credentials(creds.clone()).await;
        assert_eq!(c.credentials().await, Some(creds));
        c.clear_credentials().await;
        assert!(c.credentials().await.is_none());
    }
}
