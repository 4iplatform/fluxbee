#![forbid(unsafe_code)]

//! IO.wapp — WhatsApp Cloud API node. Phase-1 SKELETON: boots (degraded when unconfigured), runs the
//! SDK single-config control plane (CONFIG_GET/SET/PING/STATUS), and resolves credentials from
//! SY.vault via the family vault_ref pattern (mirrors io.slack) with a poll refresh loop + a
//! VAULT_SECRET_CHANGED fast-path wake. Webhook inbound (SY.edge) and Graph API outbound land in later
//! phases — see docs/io-wapp-design.md.

use anyhow::Result;
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, Message as WireMessage, Meta, Routing, MSG_VAULT_SECRET_CHANGED,
    SYSTEM_KIND,
};
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, NodeUuidMode, OperationalRouteProfile, RouteMatch,
    RouteTarget, RouterDispatcher, VaultCallerOwned, VaultClient, VaultError, FLUXBEE_NODE_NAME_ENV,
};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
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
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, RwLock};

const RPC_CH_SYSTEM: &str = "system";
const IO_WAPP_VAULT_REFRESH_INTERVAL_SECS: u64 = 60;
/// Canonical resource_type for the WhatsApp secret in SY.vault (design D4).
const WAPP_RESOURCE_TYPE: &str = "whatsapp";

struct Config {
    node_name: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
}

impl Config {
    fn from_env() -> Self {
        Self {
            node_name: env(FLUXBEE_NODE_NAME_ENV).unwrap_or_else(|| "IO.wapp.local".to_string()),
            node_version: env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET").unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string())),
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
}

impl WappClients {
    fn new() -> Self {
        Self {
            runtime: Arc::new(RwLock::new(WappRuntimeState {
                credentials: None,
                config_generation: 0,
            })),
        }
    }

    async fn config_generation(&self) -> u64 {
        self.runtime.read().await.config_generation
    }

    async fn credentials_configured(&self) -> bool {
        self.runtime.read().await.credentials.is_some()
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

async fn resolve_wapp_credentials_from_vault(vault: &VaultClient, key: &str) -> WappVaultResolution {
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
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,io_wapp=info,fluxbee_sdk=info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

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
        (Some(self_ilk), Some(hive_id)) => Some(Arc::new(VaultClient::new(
            dispatcher.clone(),
            hive_id.to_string(),
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

    let clients = WappClients::new();

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
            None => tracing::warn!(node_name = %config.node_name, "no wapp.auth.key in effective config; running degraded until configured"),
        }
    } else {
        tracing::warn!(node_name = %config.node_name, "FLUXBEE_NODE_ILK_ID / hive suffix missing; vault lookup skipped");
    }

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

    run_router_control_loop(
        dispatcher,
        config.node_name.clone(),
        control_plane,
        control_metrics,
        adapter_contract,
        clients,
        vault_change_notify,
    )
    .await
}

fn build_io_wapp_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}

#[allow(clippy::too_many_arguments)]
async fn run_router_control_loop(
    dispatcher: Arc<RouterDispatcher>,
    node_name: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    clients: WappClients,
    vault_change_notify: Arc<Notify>,
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

        if try_handle_default_node_status(&sender, &msg).await? {
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
        )
        .await
        {
            sender.send(response).await?;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    clients: &WappClients,
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
        let payload = serde_json::json!({
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
        return Some(build_system_reply(msg, control_src, "STATUS_RESPONSE", payload));
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
                build_io_adapter_contract_payload(adapter_contract, state.effective_config.as_ref()),
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

    if let Err(err) = ensure_config_version_advances(state.config_version, payload.config_version) {
        log_config_set_stale_rejected(node_name, payload.config_version, state.config_version);
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            state.config_version,
            "stale_config",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(node_name, &redacted, "stale_config", err.to_string());
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
            return build_io_config_set_error_payload(node_name, &redacted, err.code(), err.to_string());
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
        log_config_set_persist_error(node_name, payload.schema_version, payload.config_version, &message);
        control_metrics.record_config_set_error(state.current_state.as_str(), previous_version, code);
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(node_name, &redacted, code, message);
    }

    // A CONFIG_SET that changes wapp.auth.key is picked up by the refresh loop (reads the key from the
    // live control plane each tick); no explicit reload needed here.
    control_metrics.record_config_set_ok(state.current_state.as_str(), state.config_version);
    log_config_set_applied(node_name, payload.schema_version, payload.config_version, &[], &[], &[]);
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
        assert_eq!(wapp_vault_key(Some(&cfg)).as_deref(), Some("wapp/IO.wapp@motherbee"));
        assert_eq!(wapp_vault_key(None), None);
        assert_eq!(wapp_vault_key(Some(&json!({"wapp":{"auth":{"key":"   "}}}))), None);
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
        assert!(extract_wapp_creds_from_vault_value(&json!({"access_token":"A","app_secret":"S"})).is_none());
        // a blank field => None
        assert!(extract_wapp_creds_from_vault_value(&json!({"access_token":"A","app_secret":"S","verify_token":"  "})).is_none());
        // bare string => None (ambiguous)
        assert!(extract_wapp_creds_from_vault_value(&json!("A")).is_none());
    }

    #[test]
    fn vault_change_is_wapp_matches_only_whatsapp() {
        assert!(vault_change_is_wapp(&json!({"resource_type":"whatsapp","op":"put"})));
        assert!(vault_change_is_wapp(&json!({"resource_type":"WhatsApp"})));
        assert!(!vault_change_is_wapp(&json!({"resource_type":"slack"})));
        assert!(!vault_change_is_wapp(&json!({"op":"put"})));
    }

    #[tokio::test]
    async fn credentials_generation_is_change_driven_and_delete_aware() {
        let c = WappClients::new();
        assert_eq!(c.config_generation().await, 0);
        assert!(!c.credentials_configured().await);
        let creds = WappCredentials { access_token: "A".into(), app_secret: "S".into(), verify_token: "V".into() };
        c.reload_credentials(creds.clone()).await;
        assert_eq!(c.config_generation().await, 1);
        assert!(c.credentials_configured().await);
        // identical reload => no churn
        c.reload_credentials(creds).await;
        assert_eq!(c.config_generation().await, 1);
        // rotation bumps
        c.reload_credentials(WappCredentials { access_token: "A2".into(), app_secret: "S".into(), verify_token: "V".into() }).await;
        assert_eq!(c.config_generation().await, 2);
        // clear drops + bumps; clearing again is a no-op
        c.clear_credentials().await;
        assert_eq!(c.config_generation().await, 3);
        assert!(!c.credentials_configured().await);
        c.clear_credentials().await;
        assert_eq!(c.config_generation().await, 3);
    }
}
