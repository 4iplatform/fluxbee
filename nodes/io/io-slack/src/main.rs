#![forbid(unsafe_code)]

mod not_taken;
mod slack_menu;
mod thread_records;

use anyhow::Result;
use fluxbee_sdk::protocol::{
    Destination, Message as WireMessage, Meta, Routing, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    managed_node_config_path, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    RouteMatch, RouteTarget, RouterDispatcher, VaultCallerOwned, VaultClient, VaultError,
    FLUXBEE_NODE_NAME_ENV,
};
use futures_util::{SinkExt, StreamExt};
use io_common::identity::{
    IdentityProvisioner, IdentityResolver, ResolveOrCreateInput, ShmIdentityResolver,
};
use io_common::inbound::{InboundConfig, InboundOutcome, InboundProcessor};
use io_common::io_adapter_config::{
    apply_adapter_config_replace, build_io_adapter_contract_payload, IoAdapterConfigContract,
};
use io_common::io_context::{
    extract_slack_post_target, slack_binding_id, slack_inbound_io_context, IoContext,
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
use io_common::io_slack_adapter_config::IoSlackAdapterConfigContract;
use io_common::provision::{ensure_own_ich, FluxbeeIdentityProvisioner, IdentityProvisionConfig};
use io_common::relay::{
    AssembledTurn, InMemoryRelayStore, RelayBuffer, RelayDecision, RelayFlushHints, RelayFragment,
    RelayPolicy,
};
use io_common::reliability::{dedup_key, DedupCache};
use io_common::text_v1_blob::{
    resolve_text_v1_text_only_for_outbound, InboundAttachmentInput, IoBlobContractError,
    IoBlobRuntimeConfig, IoTextBlobConfig,
};
use not_taken::RelayedMessages;
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use slack_menu::{
    close_forgotten_menus, close_subject_menu, handle_interactive, Interactions, WfPost,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use thread_records::{
    map_inbound_thread, open_node_thread_records, run_thread_records_expiry_loop, OutboundThread,
    SlackThreadRecords, Subject,
};
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio::sync::{Mutex, Notify, RwLock};
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

    fluxbee_sdk::logging::fmt()
        .with_env_filter(env_filter)
        .init();

    // Phase J'-0a: read self ILK + tenant injected by orchestrator via
    // FLUXBEE_NODE_ILK_ID / FLUXBEE_NODE_TENANT_ID at spawn time. IO nodes
    // don't consume vault secrets in this cycle (deferred to Phase K') but
    // we validate the env is present so the wiring is end-to-end exercised.
    let self_ilk_id = fluxbee_sdk::read_self_ilk_from_env();
    let self_tenant_id = fluxbee_sdk::read_self_tenant_from_env();
    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        dst_node = %config.dst_node.clone().unwrap_or_else(|| "(router-resolve)".to_string()),
        dev_mode = %config.dev_mode,
        workspace_id = ?config.workspace_id,
        conversation_id = ?config.conversation_id,
        self_ilk_id = ?self_ilk_id,
        self_tenant_id = ?self_tenant_id,
        "io-slack starting"
    );

    let node_config = NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    };
    let profile = build_io_slack_rpc_profile()
        .map_err(|err| anyhow::anyhow!("io-slack rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();

    tracing::info!(
        full_name = %sender.full_name(),
        "connected to router"
    );

    let slack = Arc::new(SlackClients::new()?);
    let identity: Arc<dyn IdentityResolver> = Arc::new(ShmIdentityResolver::new(&config.island_id));
    let provisioner: Arc<dyn IdentityProvisioner> = Arc::new(FluxbeeIdentityProvisioner::new(
        dispatcher.clone(),
        IdentityProvisionConfig {
            target: config.identity_target.clone(),
            timeout: Duration::from_millis(config.identity_timeout_ms),
        },
    ));
    let inbound = Arc::new(Mutex::new(InboundProcessor::new(
        sender.uuid().to_string(),
        slack_inbound_config(&config, self_tenant_id.clone()),
    )));
    let blob_toolkit = Arc::new(
        config
            .blob_runtime
            .build_toolkit()
            .map_err(|err| anyhow::anyhow!("invalid blob runtime config: {err}"))?,
    );
    let blob_payload_cfg = config.blob_runtime.text_v1.clone();
    let boot_state =
        bootstrap_io_control_plane_state(&config.node_name, &IoSlackAdapterConfigContract)
            .unwrap_or_else(|err| {
                tracing::warn!(
                    error = %err,
                    node_name = %config.node_name,
                    "failed to bootstrap IO control-plane state; using UNCONFIGURED"
                );
                IoControlPlaneState::default()
            });
    // dst_node from the VALIDATED effective config (io-api pattern); absence -> None ->
    // Destination::Resolve. No "resolve" literal (a bogus unicast target).
    inbound.lock().await.set_dst_node(extract_runtime_dst_node(
        boot_state.effective_config.as_ref(),
    ));

    // Credentials live in SY.vault (family vault_ref pattern); config carries only slack.auth.key, so
    // Configured/FAILED_CONFIG is a pure function of config STRUCTURAL validity (already decided by the
    // contract at bootstrap). A not-yet-present vault secret is a DEGRADED runtime signal, NOT
    // FAILED_CONFIG -- the refresh loop picks it up after vault_put / rotation without a restart.
    let vault_client_opt: Option<VaultClient> = match (
        self_ilk_id.as_deref().filter(|v| !v.is_empty()),
        config.node_name.split('@').nth(1).filter(|v| !v.is_empty()),
    ) {
        // The hive only validates the L2 name: the vault is the motherbee's wherever we run.
        (Some(self_ilk_id_ref), Some(_hive)) => Some(VaultClient::for_primary(
            dispatcher.clone(),
            VaultCallerOwned::new(self_ilk_id_ref.to_string(), config.node_name.clone()),
        )),
        _ => None,
    };
    if let Some(vault_client) = vault_client_opt.as_ref() {
        match slack_vault_key(boot_state.effective_config.as_ref()) {
            Some(key) => match resolve_slack_credentials_from_vault(vault_client, &key).await {
                SlackVaultResolution::Found(app_token, bot_token) => {
                    slack.reload_credentials(app_token, bot_token).await;
                    tracing::info!(node_name = %config.node_name, vault_key = %key, "io-slack credentials loaded from vault");
                }
                SlackVaultResolution::Absent | SlackVaultResolution::Transient => {
                    tracing::warn!(node_name = %config.node_name, vault_key = %key, "io-slack vault credentials not ready; running degraded (refresh loop will retry)")
                }
            },
            None => {
                tracing::warn!(node_name = %config.node_name, "no slack.auth.key in effective config; running degraded until configured")
            }
        }
    } else {
        tracing::warn!(node_name = %config.node_name, "FLUXBEE_NODE_ILK_ID / hive suffix missing; vault lookup skipped");
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
    let ensured_bindings = Arc::new(Mutex::new(HashSet::<String>::new()));
    let control_metrics = Arc::new(IoControlPlaneMetrics::with_initial_state(
        boot_state.current_state.as_str(),
        boot_state.config_version,
    ));
    let adapter_contract: Arc<dyn IoAdapterConfigContract> = Arc::new(IoSlackAdapterConfigContract);

    // The thread id of each Slack place this node handles, next to its config.json (see
    // thread_records). Without them the node works as it did before they existed.
    let thread_records = open_node_thread_records(&config.node_name).map(Arc::new);
    if let Some(records) = thread_records.clone() {
        tokio::spawn(run_thread_records_expiry_loop(records));
    }

    // The messages and clicks this node relays lately, so the WF engine's NOT_TAKEN for one is shown
    // to whoever wrote it (see not_taken).
    let relayed = Arc::new(RelayedMessages::new());

    // Woken by the SY.vault `VAULT_SECRET_CHANGED` broadcast (handled in run_outbound_loop) so a
    // Cloud-side token update (put/rotate/delete) reloads credentials IMMEDIATELY instead of waiting
    // for the next poll tick — this is the primary path now that Slack token rotation is disabled and
    // tokens flow Fluxbee Cloud -> vault -> broadcast.
    let vault_change_notify = Arc::new(Notify::new());

    // Vault refresh loop (family vault_ref pattern): re-reads slack.auth.key from the LIVE control
    // plane on each tick OR on a VAULT_SECRET_CHANGED wake, and hot-swaps credentials on
    // first-appearance / rotation / CONFIG_SET — no restart. Owns vault_client_opt from here on.
    if let Some(vault_client) = vault_client_opt {
        tokio::spawn(run_slack_vault_refresh_loop(
            config.node_name.clone(),
            vault_client,
            slack.clone(),
            control_plane.clone(),
            vault_change_notify.clone(),
        ));
    }

    if let Some(effective) = boot_state.effective_config.as_ref() {
        if let Ok(binding) = extract_runtime_slack_binding(Some(effective), &config) {
            if let Err(error) = ensure_io_slack_binding_ich_registered(
                &config.node_name,
                &binding,
                &dispatcher,
                &IdentityProvisionConfig {
                    target: config.identity_target.clone(),
                    timeout: Duration::from_millis(config.identity_timeout_ms),
                },
                self_ilk_id.as_deref(),
                self_tenant_id.as_deref(),
                ensured_bindings.clone(),
            )
            .await
            {
                let mut guard = control_plane.write().await;
                guard.current_state = IoNodeLifecycleState::FailedConfig;
                guard.last_error = Some(IoControlPlaneErrorInfo {
                    code: "own_ich_registration_failed".to_string(),
                    message: error.to_string(),
                });
                tracing::warn!(
                    node_name = %config.node_name,
                    workspace_id = %binding.workspace_id,
                    conversation_id = %binding.conversation_id,
                    error = %error,
                    "failed to ensure Slack binding ICH at boot; starting in FAILED_CONFIG"
                );
            }
        }
    }

    let outbound_task = tokio::spawn(run_outbound_loop(
        dispatcher.clone(),
        config.clone(),
        config.node_name.clone(),
        control_plane.clone(),
        control_metrics.clone(),
        adapter_contract.clone(),
        inbound.clone(),
        slack.clone(),
        relay.clone(),
        IdentityProvisionConfig {
            target: config.identity_target.clone(),
            timeout: Duration::from_millis(config.identity_timeout_ms),
        },
        self_ilk_id.clone(),
        self_tenant_id.clone(),
        ensured_bindings.clone(),
        blob_toolkit.clone(),
        blob_payload_cfg.clone(),
        vault_change_notify.clone(),
        thread_records.clone(),
        relayed.clone(),
    ));

    let inbound_task = tokio::spawn(run_inbound_socket_mode(
        config.clone(),
        dispatcher.clone(),
        IdentityProvisionConfig {
            target: config.identity_target.clone(),
            timeout: Duration::from_millis(config.identity_timeout_ms),
        },
        control_plane.clone(),
        self_ilk_id.clone(),
        self_tenant_id.clone(),
        slack.clone(),
        identity.clone(),
        provisioner.clone(),
        inbound.clone(),
        relay.clone(),
        ensured_bindings.clone(),
        blob_toolkit.clone(),
        blob_payload_cfg.clone(),
        thread_records,
        relayed.clone(),
    ));

    let relay_flush_task = tokio::spawn(run_relay_flush_loop(
        relay,
        dispatcher.clone(),
        identity,
        provisioner,
        inbound,
        relayed,
    ));

    let _ = tokio::join!(inbound_task, outbound_task, relay_flush_task);
    Ok(())
}

/// The config of an inbound processor, which takes a person's message to the mesh: the events' and
/// the clicks' (slack_menu). Its destination is the effective config's `io.dst_node`, set on the
/// processor or given with each message.
fn slack_inbound_config(config: &Config, self_tenant_id: Option<String>) -> InboundConfig {
    InboundConfig {
        ttl: config.ttl,
        dedup_ttl: Duration::from_millis(config.dedup_ttl_ms),
        dedup_max_entries: config.dedup_max_entries,
        dst_node: None,
        provision_on_miss: true,
        blob_runtime: Some(config.blob_runtime.clone()),
        self_tenant_id,
    }
}

const RPC_CH_INCOMING: &str = "incoming";

fn build_io_slack_rpc_profile() -> Result<OperationalRouteProfile, fluxbee_sdk::RpcError> {
    // io-slack receives admin/system messages (control-plane) and may also
    // see user/text replies routed back. A single catch-all command channel
    // is fine because the dispatcher matches our pending RPC waiters before
    // the post_pending rule runs.
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_INCOMING)
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command(RPC_CH_INCOMING))
        .build()
}

#[derive(Clone)]
struct Config {
    workspace_id: Option<String>,
    conversation_id: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct SlackBindingConfig {
    workspace_id: String,
    conversation_id: String,
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

        // Model D' — Slack credentials live entirely in SY.vault under
        // `resource_type=slack`. Local plaintext / spawn-doc / env var
        // fallbacks were removed in Phase J' / VA-K2. The boot path here
        // Credentials never come from env/config: they are resolved from SY.vault in main() after the
        // router is up (boot resolution + refresh loop), via slack.auth.key (family vault_ref pattern).
        let workspace_id = env("IO_SLACK_WORKSPACE_ID").or_else(|| {
            json_get_string_opt(
                spawn_doc,
                &["config.io.workspace_id", "io.workspace_id", "workspace_id"],
            )
        });
        let conversation_id = env("IO_SLACK_CONVERSATION_ID").or_else(|| {
            json_get_string_opt(
                spawn_doc,
                &[
                    "config.io.conversation_id",
                    "io.conversation_id",
                    "conversation_id",
                ],
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
            workspace_id,
            conversation_id,
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
        "text/html".to_string(),
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

const IO_SLACK_VAULT_REFRESH_INTERVAL_SECS: u64 = 60;

/// Model D' — resolve the Slack credentials by discovering the `slack`
/// resource in SY.vault (pool match: dedicated to caller ILK → caller tenant
/// pool → root tenant pool, i.e. `tnt:00000000-0000-0000-0000-000000000001`).
/// The vault value is expected to be an object with both `app_token` and
/// `bot_token` fields. Returns `Some((app_token, bot_token))` when both are
/// Outcome of resolving Slack credentials from SY.vault. Distinguishes a DETERMINISTIC absence (key
/// deleted, wrong resource_type, or malformed value) from a TRANSIENT failure (timeout / node error):
/// the refresh loop clears the live credentials on `Absent` but keeps them on `Transient`, so a vault
/// blip never knocks a healthy long-lived socket offline (M7).
enum SlackVaultResolution {
    Found(String, String),
    Absent,
    Transient,
}

/// Resolve `(app_token, bot_token)` from SY.vault by the explicit vault KEY declared in config
/// (`slack.auth.key`). Family pattern (mirrors io.linkedhelper / ai.generic's vault_key), routed
/// through the canonical `Arc<RouterDispatcher>` via `VaultClient`. The secret's
/// `metadata.resource_type` must be "slack" and its value an object `{app_token, bot_token}`.
async fn resolve_slack_credentials_from_vault(
    vault: &VaultClient,
    key: &str,
) -> SlackVaultResolution {
    match vault.get(key, Duration::from_secs(5)).await {
        Ok(response) => {
            let resource_type = response
                .metadata
                .as_ref()
                .and_then(|m| m.resource_type.as_deref());
            if resource_type != Some("slack") {
                tracing::warn!(
                    vault_key = %key,
                    resource_type = ?resource_type,
                    "vault key is not resource_type=slack; treating as absent"
                );
                return SlackVaultResolution::Absent;
            }
            match response
                .value
                .as_ref()
                .and_then(extract_slack_tokens_from_vault_value)
            {
                Some((app_token, bot_token)) => SlackVaultResolution::Found(app_token, bot_token),
                None => {
                    tracing::warn!(
                        vault_key = %key,
                        "vault slack secret has no usable {{app_token, bot_token}}; treating as absent"
                    );
                    SlackVaultResolution::Absent
                }
            }
        }
        // A deleted / never-present key is DETERMINISTIC absence -> clear live creds.
        Err(VaultError::Service { code, message }) if code == "KEY_NOT_FOUND" => {
            tracing::info!(
                vault_key = %key,
                message = %message,
                "vault slack secret not found (deleted); clearing credentials"
            );
            SlackVaultResolution::Absent
        }
        // The vault REJECTED the request itself. Also deterministic: the key we were configured
        // with is malformed (the vault charset is [a-z0-9:_-], so an operator who copies a key
        // with dots, slashes, uppercase or `@` lands here), or we are not allowed to read it.
        // Retrying cannot fix either. Without this arm both fell into the catch-all below and
        // were reported as "transient" forever — telling the operator to look at the network
        // when the actual problem is one character in their config.
        Err(VaultError::Service { code, message })
            if code == "INVALID_REQUEST" || code == "UNAUTHORIZED" =>
        {
            tracing::error!(
                vault_key = %key,
                code = %code,
                message = %message,
                "vault REJECTED the slack secret lookup; this is a CONFIG error, not a blip — \
                 fix slack.auth.key (vault keys allow only [a-z0-9:_-], first char [a-z0-9]) \
                 or the node's authorization, then CONFIG_SET again"
            );
            SlackVaultResolution::Absent
        }
        // Anything else (timeout, node error, other service code) is TRANSIENT -> keep current creds.
        Err(err) => {
            tracing::warn!(
                vault_key = %key,
                error = %err,
                "io-slack vault get failed (transient); keeping current credentials"
            );
            SlackVaultResolution::Transient
        }
    }
}

/// The subtypes of a `message` that are a person's plain message all the same: a reply also sent
/// to the channel ("Also send to #channel"), and a message that shares files, relayed with them.
/// Any other subtype is no person's message: a bot's post, a join, an edit, a deletion...
const PLAIN_MESSAGE_SUBTYPES: [&str; 2] = ["thread_broadcast", "file_share"];

/// Why an inbound Slack event is not relayed to the mesh, or `Ok(())` to relay it.
///
/// Pure on purpose: the anti-echo guards are the part that cannot be wrong, and inline in the
/// socket loop they were untestable. `me` is our own bot user id, `None` when unknown.
fn classify_slack_inbound(
    event: &Value,
    event_type: &str,
    me: Option<&str>,
) -> Result<(), &'static str> {
    match event_type {
        "app_mention" => Ok(()),
        "message" => {
            let subtype = event.get("subtype").and_then(|v| v.as_str());
            if subtype.is_some_and(|subtype| !PLAIN_MESSAGE_SUBTYPES.contains(&subtype)) {
                // bot_message, channel_join, message_changed, message_deleted...
                return Err("not a plain user message (has a subtype)");
            }
            if event.get("bot_id").is_some() {
                return Err("posted by a bot");
            }
            let author = event.get("user").and_then(|v| v.as_str()).unwrap_or("");
            match me {
                Some(me) if author == me => Err("our own post"),
                Some(_) => Ok(()),
                // FAIL CLOSED. Relaying while we cannot tell our own messages apart is exactly the
                // echo loop these guards exist to prevent.
                None => {
                    Err("cannot resolve our own bot user id, so a self-post cannot be ruled out")
                }
            }
        }
        _ => Err("not a message or a mention"),
    }
}

/// True when a `VAULT_SECRET_CHANGED` broadcast payload concerns the slack resource — the fast-path
/// signal to reload credentials (Fluxbee Cloud -> vault -> broadcast). Matched by `resource_type`
/// because the vault key is opaque and may differ across hives (broadcast contract).
fn vault_change_is_slack(payload: &Value) -> bool {
    payload
        .get("resource_type")
        .and_then(|v| v.as_str())
        .is_some_and(|rt| rt.eq_ignore_ascii_case("slack"))
}

/// The vault key that holds this node's Slack credentials, read from `slack.auth.key` in the effective
/// config (the family vault_ref pattern). `None` while unconfigured.
fn slack_vault_key(effective: Option<&Value>) -> Option<String> {
    effective
        .and_then(|c| c.get("slack"))
        .and_then(|s| s.get("auth"))
        .and_then(|a| a.get("key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(ToString::to_string)
}

/// Extract `(app_token, bot_token)` from a vault `value`. The value must
/// be an object with both fields non-empty; bare strings are not accepted
/// (would be ambiguous which token they represent).
fn extract_slack_tokens_from_vault_value(value: &Value) -> Option<(String, String)> {
    let obj = value.as_object()?;
    let app_token = obj
        .get("app_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    let bot_token = obj
        .get("bot_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())?
        .to_string();
    Some((app_token, bot_token))
}

/// Reloads the slack credentials from vault whenever either (a) the poll interval elapses
/// (IO_SLACK_VAULT_REFRESH_INTERVAL_SECS — the slow-path safety net) or (b) a `VAULT_SECRET_CHANGED`
/// broadcast wakes `vault_change_notify` (the fast path: a Cloud-side put/rotate/delete flows in
/// within a round-trip, no restart). Both cases run the same resolve+swap so post-boot vault_put and
/// rotations converge regardless of which signal fires.
async fn run_slack_vault_refresh_loop(
    node_name: String,
    vault: VaultClient,
    slack: Arc<SlackClients>,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    vault_change_notify: Arc<Notify>,
) {
    let mut ticker =
        tokio::time::interval(Duration::from_secs(IO_SLACK_VAULT_REFRESH_INTERVAL_SECS));
    ticker.tick().await; // skip first immediate tick
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = vault_change_notify.notified() => {
                tracing::debug!(node_name = %node_name, "vault secret changed; refreshing slack credentials now");
            }
        }
        // Read the CURRENT vault key from the live control plane, so a CONFIG_SET that changes
        // slack.auth.key is honored without a restart, and post-boot vault_put / rotations flow in.
        let Some(key) = slack_vault_key(control_plane.read().await.effective_config.as_ref())
        else {
            tracing::debug!(
                node_name = %node_name,
                "no slack.auth.key in effective config; vault refresh skipped"
            );
            continue;
        };
        match resolve_slack_credentials_from_vault(&vault, &key).await {
            SlackVaultResolution::Found(app_token, bot_token) => {
                // Idempotent: reload_credentials only bumps the generation (WS reconnect) when a
                // token actually changed, so unchanged secrets never churn the socket.
                slack.reload_credentials(app_token, bot_token).await;
            }
            SlackVaultResolution::Absent => {
                // Deterministic delete / unusable secret: drop the stale token so it is never reused;
                // the inbound socket then parks waiting for the secret to reappear (M7).
                slack.clear_credentials().await;
            }
            SlackVaultResolution::Transient => {
                // Vault blip: keep whatever credentials we already have and retry next tick.
            }
        }
    }
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut cur = root;
    for part in dotted_path.split('.') {
        cur = cur.get(part)?;
    }
    Some(cur)
}

/// Slack's Web API, the base of every method's URL.
const SLACK_WEB_API: &str = "https://slack.com/api";
/// Slack's file store, the only origin a shared file is downloaded from with the bot token. A file
/// shared from elsewhere (an external file, a Google Drive one, or one an app added with
/// `files.remote.add`) names its own host in `url_private`, and its download would hand that host
/// the token.
const SLACK_FILE_STORE: &str = "https://files.slack.com";

/// How long a shared file's download may take to connect, and in all (FINDINGS A-85).
const FILE_DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FILE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// The client a shared file is downloaded with (FINDINGS A-85): a connect timeout, a deadline for
/// the whole download, and a redirect followed only on the file's own origin, where the bot token
/// goes, three at most.
fn file_download_client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(FILE_DOWNLOAD_CONNECT_TIMEOUT)
        .timeout(FILE_DOWNLOAD_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let same_origin = attempt
                .previous()
                .first()
                .is_some_and(|first| first.origin() == attempt.url().origin());
            if same_origin && attempt.previous().len() <= 3 {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }))
}

/// A shared file's download: its bytes, or the size that made it too large.
enum Download {
    Bytes(Vec<u8>),
    TooLarge(u64),
}

/// Whether `url` has `store`'s origin: its scheme, its host and its port.
fn same_origin(url: &str, store: &str) -> bool {
    match (Url::parse(url), Url::parse(store)) {
        (Ok(url), Ok(store)) => url.origin() == store.origin(),
        _ => false,
    }
}

#[derive(Clone)]
struct SlackClients {
    http: reqwest::Client,
    /// The client of shared files' downloads (`file_download_client`).
    downloads: reqwest::Client,
    runtime: Arc<RwLock<SlackRuntimeState>>,
    /// The base of the Web API's method URLs: `SLACK_WEB_API`, or a test's fake (`fake_web_api`).
    web_api: String,
    /// The origin a shared file is downloaded from with the bot token: `SLACK_FILE_STORE`, or a
    /// test's fake. A file anywhere else is refused before any request (`download_private_file`).
    file_store: String,
}

#[derive(Clone)]
struct SlackRuntimeState {
    slack_app_token: Option<String>,
    slack_bot_token: Option<String>,
    config_generation: u64,
    /// Our OWN bot user id, resolved from `auth.test` when credentials load.
    ///
    /// Needed only since the node stopped being mention-only: once plain channel messages are
    /// processed, the bot's own posts come back as inbound events, and relaying them would feed
    /// whatever answers back into the channel — an echo loop in a customer's Slack. `bot_id` and
    /// `subtype` already catch that, but this is the one check that cannot be defeated by an
    /// unusual posting path, so it is worth the single API call.
    bot_user_id: Option<String>,
}

// Manual Debug that never prints the live tokens (mirrors io-api PublicationRuntime): a leaked
// xapp-/xoxb- token in a log line is a workspace compromise. Only presence + generation surface.
impl std::fmt::Debug for SlackRuntimeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackRuntimeState")
            .field(
                "slack_app_token",
                &self.slack_app_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "slack_bot_token",
                &self.slack_bot_token.as_ref().map(|_| "<redacted>"),
            )
            .field("config_generation", &self.config_generation)
            .finish()
    }
}

impl SlackClients {
    fn new() -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::new(),
            downloads: file_download_client().build()?,
            runtime: Arc::new(RwLock::new(SlackRuntimeState {
                // Credentials are resolved from SY.vault post-construction (boot resolution + refresh
                // loop, family vault_ref pattern) — never seeded from config.
                slack_app_token: None,
                slack_bot_token: None,
                config_generation: 0,
                bot_user_id: None,
            })),
            web_api: SLACK_WEB_API.to_string(),
            file_store: SLACK_FILE_STORE.to_string(),
        })
    }

    /// The URL of a Web API method.
    fn web_api_url(&self, api: &str) -> String {
        format!("{}/{api}", self.web_api)
    }

    async fn config_generation(&self) -> u64 {
        self.runtime.read().await.config_generation
    }

    /// Install credentials resolved from SY.vault. Only bumps `config_generation` (which forces the
    /// inbound WebSocket to reconnect) when a token actually CHANGED — so the periodic vault refresh
    /// is a true no-op on unchanged secrets and never churns a healthy long-lived socket.
    async fn reload_credentials(&self, slack_app_token: String, slack_bot_token: String) {
        let mut guard = self.runtime.write().await;
        let changed = guard.slack_app_token.as_deref() != Some(slack_app_token.as_str())
            || guard.slack_bot_token.as_deref() != Some(slack_bot_token.as_str());
        guard.slack_app_token = Some(slack_app_token);
        guard.slack_bot_token = Some(slack_bot_token);
        if changed {
            guard.config_generation = guard.config_generation.wrapping_add(1);
            // Identity belongs to the token: a new bot token can be a different bot.
            guard.bot_user_id = None;
        }
    }

    /// Who are we, per Slack. Cached; resolved once per credential generation.
    ///
    /// Returns `None` when `auth.test` cannot answer. Callers must treat that as "unknown", never
    /// as "not us" — the self-filter has to fail CLOSED or the echo loop it prevents comes back.
    async fn bot_user_id(&self) -> Option<String> {
        if let Some(known) = self.runtime.read().await.bot_user_id.clone() {
            return Some(known);
        }
        let slack_bot_token = self.slack_bot_token().await.ok()?;
        #[derive(Deserialize)]
        struct AuthTestResp {
            ok: bool,
            user_id: Option<String>,
            error: Option<String>,
        }
        let url = self.web_api_url("auth.test");
        let resp: AuthTestResp = self
            .slack_send_with_retry("auth.test", || {
                self.http.post(url.as_str()).bearer_auth(&slack_bot_token)
            })
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        if !resp.ok {
            tracing::warn!(
                error = %resp.error.unwrap_or_else(|| "unknown".to_string()),
                "auth.test failed; cannot resolve our own bot user id"
            );
            return None;
        }
        let user_id = resp.user_id?;
        self.runtime.write().await.bot_user_id = Some(user_id.clone());
        tracing::info!(bot_user_id = %user_id, "resolved our own Slack bot user id");
        Some(user_id)
    }

    /// Drop the live credentials (vault secret deleted / no longer resolvable). Bumps the generation
    /// so the inbound socket reconnects and then parks in "waiting for valid slack credentials" until
    /// the secret reappears — the stale token is never reused (M7).
    async fn clear_credentials(&self) {
        let mut guard = self.runtime.write().await;
        if guard.slack_app_token.is_none() && guard.slack_bot_token.is_none() {
            return; // already cleared; don't churn the generation
        }
        guard.slack_app_token = None;
        guard.slack_bot_token = None;
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
                    "missing Slack app token; expected an object {{app_token, bot_token}} at the \
                     vault key named by slack.auth.key (resource_type=slack)"
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
                    "missing Slack bot token; expected an object {{app_token, bot_token}} at the \
                     vault key named by slack.auth.key (resource_type=slack)"
                )
            })
    }

    /// Send a Slack Web API request with bounded 429 (rate-limit) retries honoring `Retry-After`
    /// (M3). Slack returns HTTP 429 with a `Retry-After: <secs>` header when a method is
    /// rate-limited; a bare `.json()` on that body fails, so every tier-limited Web API call goes
    /// through here. `build` is re-invoked per attempt so the request (token + body) is rebuilt fresh.
    async fn slack_send_with_retry<F>(&self, api: &str, build: F) -> Result<reqwest::Response>
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
                    "{api}: slack rate-limited (429) after {MAX_RETRIES} retries"
                ));
            }
            let retry_after = parse_retry_after(response.headers());
            tracing::warn!(
                api = %api,
                attempt,
                retry_after_secs = retry_after.as_secs(),
                "slack API rate-limited (429); backing off"
            );
            tokio::time::sleep(retry_after).await;
        }
    }

    async fn socket_mode_url(&self) -> Result<Url> {
        #[derive(Deserialize)]
        struct OpenResp {
            ok: bool,
            url: Option<String>,
            error: Option<String>,
        }
        let slack_app_token = self.slack_app_token().await?;

        let url = self.web_api_url("apps.connections.open");
        let resp: OpenResp = self
            .slack_send_with_retry("apps.connections.open", || {
                self.http.post(url.as_str()).bearer_auth(&slack_app_token)
            })
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

    /// Posts a message, its text escaped (`post_message_body`), and returns its `ts`, which Slack's
    /// answer carries: the thread records keep a new top-level post's (see thread_records).
    async fn post_message(
        &self,
        channel: &str,
        text: &str,
        thread_ts: Option<&str>,
        blocks: Option<serde_json::Value>,
    ) -> Result<Option<String>> {
        let body = post_message_body(channel, text, thread_ts, blocks);

        #[derive(Deserialize)]
        struct PostResp {
            ok: bool,
            ts: Option<String>,
            error: Option<String>,
        }
        let slack_bot_token = self.slack_bot_token().await?;

        let url = self.web_api_url("chat.postMessage");
        let resp: PostResp = self
            .slack_send_with_retry("chat.postMessage", || {
                self.http
                    .post(url.as_str())
                    .bearer_auth(&slack_bot_token)
                    .json(&body)
            })
            .await?
            .json()
            .await?;

        if !resp.ok {
            return Err(anyhow::anyhow!(
                "chat.postMessage failed: {}",
                resp.error.unwrap_or_else(|| "unknown".to_string())
            ));
        }
        Ok(resp.ts)
    }

    /// Replaces a post's blocks: a menu's post, closed (slack_menu).
    async fn update_message(&self, channel: &str, ts: &str, blocks: Value) -> Result<()> {
        self.call_web_api(
            "chat.update",
            &serde_json::json!({"channel": channel, "ts": ts, "blocks": blocks}),
        )
        .await
    }

    /// Posts a message only `user` sees, its text escaped (`post_ephemeral_body`): "Not taken"
    /// (not_taken), and a menu's "This is no longer open." (slack_menu).
    async fn post_ephemeral(
        &self,
        channel: &str,
        user: &str,
        text: &str,
        thread_ts: Option<&str>,
    ) -> Result<()> {
        self.call_web_api(
            "chat.postEphemeral",
            &post_ephemeral_body(channel, user, text, thread_ts),
        )
        .await
    }

    /// Opens a modal with a click's `trigger_id`, which Slack honors for 3 seconds: an option's
    /// params (slack_menu).
    async fn open_view(&self, trigger_id: &str, view: Value) -> Result<()> {
        self.call_web_api(
            "views.open",
            &serde_json::json!({"trigger_id": trigger_id, "view": view}),
        )
        .await
    }

    /// Calls a Web API method with a JSON body and the bot token, for an answer that is only `ok`:
    /// a refusal is an error, with Slack's code and its messages (which name a Block Kit field it
    /// refuses).
    async fn call_web_api(&self, api: &str, body: &Value) -> Result<()> {
        #[derive(Deserialize)]
        struct OkResp {
            ok: bool,
            error: Option<String>,
            response_metadata: Option<SlackResponseMetadata>,
        }

        #[derive(Deserialize)]
        struct SlackResponseMetadata {
            messages: Option<Vec<String>>,
        }
        let slack_bot_token = self.slack_bot_token().await?;
        let url = self.web_api_url(api);

        let resp: OkResp = self
            .slack_send_with_retry(api, || {
                self.http
                    .post(url.as_str())
                    .bearer_auth(&slack_bot_token)
                    .json(body)
            })
            .await?
            .json()
            .await?;
        if !resp.ok {
            let detail = resp
                .response_metadata
                .and_then(|m| m.messages)
                .map(|msgs| msgs.join(" | "))
                .unwrap_or_else(|| "-".to_string());
            return Err(anyhow::anyhow!(
                "{api} failed: {} detail={detail}",
                resp.error.unwrap_or_else(|| "unknown".to_string()),
            ));
        }
        Ok(())
    }

    /// Downloads a shared file from Slack's file store with the bot token, reading at most
    /// `max_bytes` (FINDINGS A-85): a file announced bigger is not read, and one that grows past it
    /// is dropped as it comes.
    async fn download_private_file(&self, url: &str, max_bytes: u64) -> Result<Download> {
        if !same_origin(url, &self.file_store) {
            return Err(anyhow::anyhow!(
                "the file is not on Slack's file store ({}), and the bot token goes nowhere else",
                self.file_store
            ));
        }
        let slack_bot_token = self.slack_bot_token().await?;
        let mut response = self
            .downloads
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
        if let Some(length) = response
            .content_length()
            .filter(|length| *length > max_bytes)
        {
            return Ok(Download::TooLarge(length));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            let reached = (bytes.len() + chunk.len()) as u64;
            if reached > max_bytes {
                return Ok(Download::TooLarge(reached));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Download::Bytes(bytes))
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
        let get_url = self.web_api_url("files.getUploadURLExternal");
        let get_resp: GetUploadUrlResp = self
            .slack_send_with_retry("files.getUploadURLExternal", || {
                self.http
                    .post(get_url.as_str())
                    .bearer_auth(&slack_bot_token)
                    .form(&[("filename", filename), ("length", length.as_str())])
            })
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

        // No `initial_comment`: the share carries no message text. A file's title is the file's
        // metadata, not message text, and Slack parses it for no mention or link, so the file's
        // name goes as it is (a message's text is escaped, `post_message_body`).
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

        let complete_url = self.web_api_url("files.completeUploadExternal");
        let complete_resp: CompleteUploadResp = self
            .slack_send_with_retry("files.completeUploadExternal", || {
                self.http
                    .post(complete_url.as_str())
                    .bearer_auth(&slack_bot_token)
                    .json(&complete_payload)
            })
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

/// The body of a `chat.postMessage` in `channel`, in the Slack thread `thread_ts` or at the top
/// level: `text` escaped (`escape_slack_text`), and `blocks` as they are.
fn post_message_body(
    channel: &str,
    text: &str,
    thread_ts: Option<&str>,
    blocks: Option<Value>,
) -> Value {
    let mut body = serde_json::json!({
        "channel": channel,
        "text": escape_slack_text(text),
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = Value::String(ts.to_string());
    }
    if let Some(blocks) = blocks {
        body["blocks"] = blocks;
    }
    body
}

/// The body of a `chat.postEphemeral` to `user` in `channel`, in the Slack thread `thread_ts` or at
/// the top level: `text` escaped (`escape_slack_text`), as a post's.
fn post_ephemeral_body(channel: &str, user: &str, text: &str, thread_ts: Option<&str>) -> Value {
    let mut body = serde_json::json!({
        "channel": channel,
        "user": user,
        "text": escape_slack_text(text),
    });
    if let Some(ts) = thread_ts {
        body["thread_ts"] = Value::String(ts.to_string());
    }
    body
}

/// `text` as Slack shows it, as written: `&`, `<` and `>` become `&amp;`, `&lt;` and `&gt;`, as
/// Slack's formatting rules ask of text that is not markup. The text the node posts is a payload's,
/// which a person or an agent wrote: unescaped, a `<!channel>`, `<!here>`, `<@U…>` or
/// `<!subteam^…>` in it pings, and a `<https://evil.example|https://sso.acme.com>` shows a false
/// link label. A bare URL still links. A person's text, decoded on the way in
/// (`decode_slack_text`), goes back escaped once.
fn escape_slack_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// A person's words as they wrote them, from a message's `text`. Slack sends a person's `&`, `<`
/// and `>` as `&amp;`, `&lt;` and `&gt;`, and those three are decoded, `&amp;` last, so `&amp;lt;`
/// is `&lt;`. Slack's own markup, a `<…>` with raw angle brackets such as a mention `<@U…>` or a
/// link `<https://…|label>`, stays as it is: a person's `<` is never raw. On the way out the text
/// is escaped again (`escape_slack_text`).
fn decode_slack_text(text: &str) -> String {
    let decode = |words: &str| {
        words
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    };
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('<') {
        let Some(len) = rest[start..].find('>') else {
            break;
        };
        let end = start + len + 1;
        decoded.push_str(&decode(&rest[..start]));
        decoded.push_str(&rest[start..end]);
        rest = &rest[end..];
    }
    decoded.push_str(&decode(rest));
    decoded
}

/// Parse a Slack `Retry-After` header (integer seconds) into a bounded backoff, clamped to
/// [1s, 30s]. Used by [`SlackClients::slack_send_with_retry`] on HTTP 429; a missing/garbage
/// header falls back to 1s.
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

#[derive(Debug, Deserialize)]
struct SocketEnvelope {
    envelope_id: String,
    /// What the envelope carries: `events_api` (the events the node relays), `interactive` (a click
    /// on a menu's button or a modal's submission, slack_menu), `slash_commands`.
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    payload: serde_json::Value,
}

/// The envelope of the events the node relays, which the event worker takes in order.
const ENVELOPE_EVENTS_API: &str = "events_api";
/// The envelope of a click or a modal's submission, which Slack sends once the app has
/// Interactivity on.
const ENVELOPE_INTERACTIVE: &str = "interactive";

/// Read-idle timeout for the Socket Mode WS. Slack pings the client ~every 30s and rotates the socket
/// every ~30-60min; if no frame arrives within this window the socket is treated as dead and we
/// reconnect (a bare `read.next().await` would otherwise wedge forever on a half-open TCP).
const SOCKET_READ_IDLE: Duration = Duration::from_secs(90);
/// Ceiling for the exponential reconnect backoff.
const SOCKET_RECONNECT_MAX_BACKOFF: Duration = Duration::from_secs(30);
/// The events the read loop handed the event worker and the worker has not taken yet, at most. Past
/// it an event is dropped, with an error line: it was acked, so Slack does not send it again.
const SOCKET_EVENTS_QUEUE: usize = 1_024;

/// Hands off an envelope the read loop acked, and never waits: an event goes to the event worker's
/// queue, which keeps the events' order (`run_slack_event_worker`); a click on a menu's button or a
/// modal's submission goes to a task of its own (slack_menu), so a modal opens within the 3 seconds
/// its trigger lasts, whatever the worker is busy with; any other envelope is ignored.
fn hand_off_envelope(
    env: SocketEnvelope,
    events: &mpsc::Sender<Value>,
    interactions: &Arc<Interactions>,
) {
    match env.kind.as_str() {
        ENVELOPE_EVENTS_API => {
            if let Err(err) = events.try_send(env.payload) {
                let reason = match err {
                    TrySendError::Full(_) => "the event worker's queue is full",
                    TrySendError::Closed(_) => "the event worker stopped",
                };
                // As M1 says of a send to the router: acked, the event is never sent again, so a
                // drop is a loss, logged at error.
                tracing::error!(
                    envelope_id = %env.envelope_id,
                    queue = SOCKET_EVENTS_QUEUE,
                    reason,
                    "LOST inbound Slack event: already ACKed to Slack (no redelivery) but not \
                     handed to the event worker"
                );
            }
        }
        ENVELOPE_INTERACTIVE => {
            tokio::spawn(handle_interactive(interactions.clone(), env.payload));
        }
        kind => tracing::debug!(kind, "ignoring a socket envelope the node does not take"),
    }
}

async fn run_inbound_socket_mode(
    config: Config,
    dispatcher: Arc<RouterDispatcher>,
    identity_provision_cfg: IdentityProvisionConfig,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
    slack: Arc<SlackClients>,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    ensured_bindings: Arc<Mutex<HashSet<String>>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
    thread_records: Option<Arc<SlackThreadRecords>>,
    relayed: Arc<RelayedMessages>,
) -> Result<()> {
    let sender = dispatcher.sender_snapshot();
    let mention_re = Regex::new(r"^<@[^>]+>\s*")?;
    // A click on a menu's button, or a modal's submission, goes to the mesh as a message from the
    // person who clicked, with what a message from Slack gets, through an inbound processor of its
    // own, never the events' (see slack_menu).
    let interactions = Arc::new(Interactions::new(
        config.clone(),
        control_plane.clone(),
        dispatcher.clone(),
        identity_provision_cfg.clone(),
        self_ilk_id.clone(),
        self_tenant_id.clone(),
        ensured_bindings.clone(),
        slack.clone(),
        thread_records.clone(),
        identity.clone(),
        provisioner.clone(),
        relayed.clone(),
    ));
    // The events go to one worker, which takes them in the order they came. The read loop below
    // only reads, acks and hands off: an event's files, or a first writer's provisioning, never
    // keep the envelopes behind it unread, and a click's modal must open within the 3 seconds its
    // trigger lasts.
    let (events, events_rx) = mpsc::channel(SOCKET_EVENTS_QUEUE);
    tokio::spawn(run_slack_event_worker(
        events_rx,
        config,
        dispatcher,
        identity_provision_cfg,
        control_plane,
        self_ilk_id,
        self_tenant_id,
        slack.clone(),
        identity,
        provisioner,
        inbound,
        relay,
        ensured_bindings,
        blob_toolkit,
        blob_payload_cfg,
        thread_records,
        relayed,
        sender,
        mention_re,
    ));

    // Reconnect backoff (exponential, capped). Reset to the floor after a successful connect so a
    // long-lived socket never inherits a stale large backoff.
    let mut backoff = Duration::from_secs(1);
    loop {
        let socket_generation = slack.config_generation().await;
        let url = match slack.socket_mode_url().await {
            Ok(url) => url,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    config_generation = socket_generation,
                    backoff_secs = backoff.as_secs(),
                    "socket mode not ready; waiting for valid slack credentials"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(SOCKET_RECONNECT_MAX_BACKOFF);
                continue;
            }
        };
        tracing::info!(
            host = url.host_str().unwrap_or(""),
            path = url.path(),
            config_generation = socket_generation,
            "socket mode connected"
        );

        // C1: a bare `?` here killed the whole (spawned) inbound task on ANY transient dial failure —
        // the process stayed alive and "healthy" while inbound was silently dead forever, and Slack
        // rotates the socket every ~30-60min so it was effectively inevitable. Retry with backoff so
        // the reconnect loop is actually resilient.
        let (ws, _) = match tokio_tungstenite::connect_async(url).await {
            Ok(conn) => conn,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    backoff_secs = backoff.as_secs(),
                    "socket mode dial failed; retrying (inbound stays alive)"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(SOCKET_RECONNECT_MAX_BACKOFF);
                continue;
            }
        };
        // Connected: reset the reconnect backoff.
        backoff = Duration::from_secs(1);
        let (mut write, mut read) = ws.split();

        loop {
            // H2: a bare read.next() wedges forever on a half-open TCP; a read-idle timeout turns a
            // dead socket into a reconnect and re-checks the config generation on idle too.
            let item = match tokio::time::timeout(SOCKET_READ_IDLE, read.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => {
                    tracing::warn!("socket mode stream ended; reconnecting");
                    break;
                }
                Err(_) => {
                    tracing::warn!("socket mode read idle timeout; reconnecting");
                    break;
                }
            };
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

            hand_off_envelope(env, &events, &interactions);
        }
    }
}

/// The event worker: takes the events the read loop hands it (`hand_off_envelope`) one at a time,
/// in the order they came, and relays each person's message in the bound channel, its files as
/// blobs, as the person who wrote it.
async fn run_slack_event_worker(
    mut events: mpsc::Receiver<Value>,
    config: Config,
    dispatcher: Arc<RouterDispatcher>,
    identity_provision_cfg: IdentityProvisionConfig,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
    slack: Arc<SlackClients>,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    ensured_bindings: Arc<Mutex<HashSet<String>>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
    thread_records: Option<Arc<SlackThreadRecords>>,
    relayed: Arc<RelayedMessages>,
    sender: NodeSender,
    mention_re: Regex,
) {
    // The messages taken lately, by their channel and `ts` (`message_seen`), for as long and as
    // many as the inbound processor keeps their events' ids.
    let mut seen_messages = DedupCache::new(
        Duration::from_millis(config.dedup_ttl_ms),
        config.dedup_max_entries,
    );
    while let Some(payload) = events.recv().await {
        // Parse Slack event callback payload (only what we need).
        let payload_type = payload.get("type").and_then(|v| v.as_str());
        if payload_type != Some("event_callback") {
            continue;
        }
        let binding = match extract_runtime_slack_binding(
            control_plane.read().await.effective_config.as_ref(),
            &config,
        ) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::warn!(
                    node_name = %config.node_name,
                    error = %error,
                    "dropping inbound Slack event: missing workspace_id/conversation_id binding"
                );
                continue;
            }
        };
        let team_id = match payload.get("team_id").and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => continue,
        };
        let event = match payload.get("event") {
            Some(v) => v.clone(),
            None => continue,
        };
        let channel = match event.get("channel").and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => continue,
        };
        if team_id != binding.workspace_id || channel != binding.conversation_id {
            // At info: two nodes on one Slack app split its events between their sockets, and each
            // drops the other's share here (FINDINGS A-129). Each node needs an app of its own.
            tracing::info!(
                node_name = %config.node_name,
                configured_workspace_id = %binding.workspace_id,
                configured_conversation_id = %binding.conversation_id,
                event_workspace_id = %team_id,
                event_conversation_id = %channel,
                "dropping inbound Slack event outside configured binding; if this node shares its \
                 Slack app with another io.slack node, that node never gets it"
            );
            continue;
        }
        if let Err(err) = ensure_io_slack_binding_ich_registered(
            &config.node_name,
            &binding,
            &dispatcher,
            &identity_provision_cfg,
            self_ilk_id.as_deref(),
            self_tenant_id.as_deref(),
            ensured_bindings.clone(),
        )
        .await
        {
            tracing::warn!(
                node_name = %config.node_name,
                workspace_id = %binding.workspace_id,
                conversation_id = %binding.conversation_id,
                error = %err,
                "dropping inbound Slack event: unable to ensure own binding ICH"
            );
            continue;
        }
        let message_id = payload
            .get("event_id")
            .and_then(|v| v.as_str())
            .or_else(|| event.get("event_id").and_then(|v| v.as_str()))
            .or_else(|| event.get("ts").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        // `app_mention` plus ordinary messages in the BOUND channel. The binding was already
        // checked above, so a `message` here is by definition in the configured conversation.
        //
        // Anything the node echoes back is posted BY the node, and Slack delivers that post
        // straight back as a `message` event. Relaying it would hand the mesh its own output
        // and whatever it answers gets posted again — an echo loop inside a customer's Slack.
        // Three independent guards, because one bad frame is a live incident:
        //   * `subtype` present  -> not a plain human message (bot_message, joins, edits,
        //                           deletions...), but for a reply also sent to the channel
        //                           and a file share (`PLAIN_MESSAGE_SUBTYPES`), which the
        //                           other two still check.
        //   * `bot_id` present   -> posted by some bot, ours included.
        //   * author == us       -> the airtight one; see `bot_user_id`.
        // The third FAILS CLOSED: if we cannot establish who we are, we do not relay.
        let me = if event_type == "message" {
            slack.bot_user_id().await
        } else {
            None
        };
        if let Err(reason) = classify_slack_inbound(&event, event_type, me.as_deref()) {
            // Say so. A bare `continue` here is invisible at EVERY level, which makes the two
            // explanations for "nothing happens" indistinguishable from the outside: Slack
            // delivering no events at all (app not subscribed) versus us dropping what it
            // does deliver. Diagnosing that cost a live session.
            tracing::debug!(
                node_name = %config.node_name,
                event_type = %event_type,
                reason = %reason,
                "ignoring inbound Slack event"
            );
            continue;
        }
        let user = match event.get("user").and_then(|v| v.as_str()) {
            Some(v) => v.to_string(),
            None => continue,
        };
        if message_seen(&mut seen_messages, &channel, &event) {
            tracing::debug!(
                node_name = %config.node_name,
                event_type = %event_type,
                message_id = %message_id,
                "dropping inbound Slack event: its message came already"
            );
            continue;
        }
        // A blank thread_ts is the channel's top level, like a missing one.
        let thread_ts = event
            .get("thread_ts")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let raw_text = event.get("text").and_then(|v| v.as_str()).unwrap_or("");
        // The person's words as written: what Slack escapes in them is decoded.
        let content = decode_slack_text(&mention_re.replace(raw_text, ""));

        tracing::debug!(
            team_id = %team_id,
            user = %user,
            channel = %channel,
            thread_ts = thread_ts.as_deref().unwrap_or(""),
            message_id = %message_id,
            content_len = content.len(),
            content_preview = %truncate(&content, 120),
            event_type = %event_type,
            "inbound Slack message"
        );

        let mut io_ctx = slack_inbound_io_context(
            &binding.workspace_id,
            &user,
            &binding.conversation_id,
            thread_ts.as_deref(),
            &message_id,
        );
        // A reply in the Slack thread of a recorded post takes that post's thread id (a WF's
        // question gets its answer back on the instance's thread); any other message keeps
        // the computed one and records its place. Before dispatch, so an answer finds it.
        map_inbound_thread(
            thread_records.as_deref(),
            &mut io_ctx,
            thread_ts.as_deref(),
            now_epoch_ms(),
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
            &binding.conversation_id,
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
            &binding.workspace_id,
            &user,
            &binding.conversation_id,
            thread_ts.as_deref(),
            &message_id,
            &content,
            attachments,
            payload.clone(),
            io_ctx.clone(),
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
                    &relayed,
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
                slack_identity_input(
                    &config.node_name,
                    &binding.workspace_id,
                    &binding.conversation_id,
                    &user,
                ),
                None,
                io_ctx,
                payload,
            )
            .await;

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
                &config.island_id,
                "slack",
                &slack_external_id(&config.node_name, &user),
                self_tenant_id.as_deref(),
                relayed.meta.src_ilk.as_deref(),
                relayed.routing.trace_id.as_str(),
            )
            .await;
        }

        dispatch_inbound_outcome(sender.clone(), outcome, "direct inbound", &relayed).await;
    }
    tracing::warn!("the inbound event queue closed; the event worker stops");
}

/// Whether an event's message came already, as another event: Slack may deliver one message that
/// mentions the bot as an `app_mention` and as a `message`, each with an `event_id` of its own,
/// which the inbound processor's dedup by event id lets through (FINDINGS A-130). By the message's
/// channel and `ts`, within `seen`'s window and bound; a new one is marked. An event with no `ts`
/// is never taken for another's, and its event id still deduplicates it.
fn message_seen(seen: &mut DedupCache, channel: &str, event: &Value) -> bool {
    let Some(ts) = event
        .get("ts")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|ts| !ts.is_empty())
    else {
        return false;
    };
    seen.is_duplicate_and_mark(&dedup_key("slack", channel, ts))
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
    workspace_id: &str,
    user: &str,
    conversation_id: &str,
    thread_ts: Option<&str>,
    message_id: &str,
    content: &str,
    attachments: Vec<InboundAttachmentInput>,
    raw_payload: Value,
    io_context: IoContext,
) -> RelayFragment {
    RelayFragment {
        relay_key: slack_relay_key(workspace_id, user, conversation_id, thread_ts),
        fragment_id: message_id.to_string(),
        received_at_ms: now_epoch_ms(),
        content_text: Some(content.to_string()),
        attachments: attachments
            .into_iter()
            .filter_map(|attachment| serde_json::to_value(attachment.blob_ref).ok())
            .collect(),
        raw_payload: Some(raw_payload),
        io_context,
        identity_input: slack_identity_input(node_name, workspace_id, conversation_id, user),
        dst_node_override: None,
        flush_hints: RelayFlushHints::default(),
    }
}

/// Who wrote a message in the bound channel, as identity resolves or provisions them: a Slack user
/// of this node, a human of the workspace. A click on a menu is resolved the same way (slack_menu).
fn slack_identity_input(
    node_name: &str,
    workspace_id: &str,
    conversation_id: &str,
    user: &str,
) -> ResolveOrCreateInput {
    ResolveOrCreateInput {
        channel: "slack".to_string(),
        external_id: slack_external_id(node_name, user),
        src_ilk_override: None,
        tenant_id: None,
        tenant_hint: Some(workspace_id.to_string()),
        attributes: serde_json::json!({
            "workspace_id": workspace_id,
            "conversation_id": conversation_id
        }),
        ilk_type: Some("human".to_string()),
    }
}

async fn ensure_io_slack_binding_ich_registered(
    node_name: &str,
    binding: &SlackBindingConfig,
    dispatcher: &Arc<RouterDispatcher>,
    identity_provision_cfg: &IdentityProvisionConfig,
    self_ilk_id: Option<&str>,
    self_tenant_id: Option<&str>,
    ensured_bindings: Arc<Mutex<HashSet<String>>>,
) -> Result<()> {
    let binding_id = slack_binding_id(&binding.workspace_id, &binding.conversation_id);
    {
        let guard = ensured_bindings.lock().await;
        if guard.contains(binding_id.as_str()) {
            return Ok(());
        }
    }
    let self_ilk_id = self_ilk_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing self_ilk_id for IO.slack own ICH registration"))?;
    let self_tenant_id = self_tenant_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("missing self_tenant_id for IO.slack own ICH registration")
        })?;
    let result = ensure_own_ich(
        dispatcher,
        identity_provision_cfg,
        identity_provision_cfg.target.as_str(),
        self_ilk_id,
        self_tenant_id,
        "slack_binding",
        binding_id.as_str(),
    )
    .await?;
    tracing::info!(
        node_name = %node_name,
        workspace_id = %binding.workspace_id,
        conversation_id = %binding.conversation_id,
        self_ilk_id = %result.ilk_id,
        ich_id = %result.ich_id,
        owner_l2_name = ?result.owner_l2_name,
        enabled = result.enabled,
        "io-slack own binding ICH ensured"
    );
    let mut guard = ensured_bindings.lock().await;
    guard.insert(binding_id);
    Ok(())
}

async fn dispatch_assembled_turn(
    sender: NodeSender,
    identity: &dyn IdentityResolver,
    provisioner: &dyn IdentityProvisioner,
    inbound: Arc<Mutex<InboundProcessor>>,
    turn: AssembledTurn,
    send_context: &str,
    relayed: &RelayedMessages,
) {
    let outcome = inbound
        .lock()
        .await
        .process_assembled_turn(identity, Some(provisioner), turn)
        .await;

    dispatch_inbound_outcome(sender, outcome, send_context, relayed).await;
}

async fn dispatch_inbound_outcome(
    sender: NodeSender,
    outcome: InboundOutcome,
    send_context: &str,
    relayed: &RelayedMessages,
) {
    match outcome {
        InboundOutcome::SendNow(msg) => {
            let trace_id = msg.routing.trace_id.clone();
            let has_src_ilk = msg.meta.src_ilk.as_deref().is_some_and(|v| !v.is_empty());
            // Remembered before it goes, so the WF engine's NOT_TAKEN, which may come back at once,
            // finds whoever wrote it (see not_taken).
            relayed.remember(&msg, now_epoch_ms());
            // M1: Socket Mode mandates ACKing the Slack envelope within ~3s, so we ack BEFORE this
            // handoff — Slack will not redeliver. NodeSender::send buffers through a transient router
            // reconnect (no loss there), so the only way this errors is a terminal disconnect. That is
            // a genuine, unrecoverable message LOSS, so log it at error with the trace_id (never
            // silently): a durable inbound queue would be the design-level remedy if we need more.
            if let Err(e) = sender.send(msg).await {
                tracing::error!(
                    error=?e, %trace_id, context = send_context,
                    "LOST inbound message: already ACKed to Slack (no redelivery) but router send \
                     failed on a terminal disconnect"
                );
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
    dispatcher: Arc<RouterDispatcher>,
    identity: Arc<dyn IdentityResolver>,
    provisioner: Arc<dyn IdentityProvisioner>,
    inbound: Arc<Mutex<InboundProcessor>>,
    relayed: Arc<RelayedMessages>,
) -> Result<()> {
    let mut interval = tokio::time::interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let turns = relay.lock().await.flush_expired(now_epoch_ms());
        for turn in turns {
            let sender = dispatcher.sender_snapshot();
            dispatch_assembled_turn(
                sender,
                identity.as_ref(),
                provisioner.as_ref(),
                inbound.clone(),
                turn,
                "relay scheduled flush",
                &relayed,
            )
            .await;
        }
    }
}

fn slack_relay_key(
    workspace_id: &str,
    user: &str,
    conversation_id: &str,
    thread_ts: Option<&str>,
) -> String {
    let thread_or_channel = thread_ts.unwrap_or(conversation_id);
    format!("slack:{workspace_id}:{conversation_id}:{thread_or_channel}:{user}")
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

        // What is left of the message's share, and never more than one file may take.
        let max_bytes = blob_payload_cfg.max_attachment_bytes.min(
            blob_payload_cfg
                .max_total_attachment_bytes
                .saturating_sub(total_size),
        );
        let bytes = match slack.download_private_file(download_url, max_bytes).await {
            Ok(Download::Bytes(bytes)) => bytes,
            Ok(Download::TooLarge(actual)) => {
                let err = IoBlobContractError::BlobTooLarge {
                    actual,
                    max: max_bytes,
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

fn slack_external_id(node_name: &str, user_id: &str) -> String {
    format!("{node_name}:{user_id}")
}

async fn run_outbound_loop(
    dispatcher: Arc<RouterDispatcher>,
    config: Config,
    node_name: String,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: Arc<dyn IoAdapterConfigContract>,
    inbound: Arc<Mutex<InboundProcessor>>,
    slack: Arc<SlackClients>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    identity_provision_cfg: IdentityProvisionConfig,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
    ensured_bindings: Arc<Mutex<HashSet<String>>>,
    blob_toolkit: Arc<fluxbee_sdk::blob::BlobToolkit>,
    blob_payload_cfg: IoTextBlobConfig,
    vault_change_notify: Arc<Notify>,
    thread_records: Option<Arc<SlackThreadRecords>>,
    relayed: Arc<RelayedMessages>,
) -> Result<()> {
    let mut incoming_rx = dispatcher
        .take_command_receiver(RPC_CH_INCOMING)
        .await
        .map_err(|err| anyhow::anyhow!("io-slack incoming receiver: {err}"))?;
    let sender = dispatcher.sender_snapshot();
    loop {
        let Some(msg) = incoming_rx.recv().await else {
            tracing::warn!("io-slack incoming channel closed; exiting outbound loop");
            return Ok(());
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
            control_plane.clone(),
            control_metrics.clone(),
            adapter_contract.as_ref(),
            inbound.clone(),
            relay.clone(),
            dispatcher.clone(),
            identity_provision_cfg.clone(),
            self_ilk_id.clone(),
            self_tenant_id.clone(),
            ensured_bindings.clone(),
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

        // Fast path for the "tokens flow Fluxbee Cloud -> vault -> broadcast" model (Slack token
        // rotation disabled): a VAULT_SECRET_CHANGED for the slack resource wakes the refresh loop to
        // reload THIS node's slack.auth.key immediately, instead of waiting for the poll tick. Filter
        // by resource_type (payload doc: match by resource_type, the key is opaque) so unrelated
        // secret churn (openai, postgres, ...) never triggers a vault round-trip.
        if is_control_plane_msg_type(&msg.meta.msg_type)
            && msg
                .meta
                .msg
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(MSG_VAULT_SECRET_CHANGED))
        {
            if vault_change_is_slack(&msg.payload) {
                let op = msg
                    .payload
                    .get("op")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                tracing::info!(
                    trace_id = %msg.routing.trace_id,
                    op = %op,
                    "VAULT_SECRET_CHANGED (slack); waking credential refresh"
                );
                vault_change_notify.notify_one();
            }
            continue;
        }

        // The WF engine's answer to a message it took nowhere (docs/wf-orchestration-v1.md §5.4.6):
        // whoever wrote it sees "Not taken", where they wrote. Only a fixed text, chosen by the
        // reason, is posted, never the answer's own words (see not_taken).
        if is_control_plane_msg_type(&msg.meta.msg_type)
            && meta_msg.eq_ignore_ascii_case(not_taken::MSG_NOT_TAKEN)
        {
            not_taken::show(&slack, &relayed, &msg).await;
            continue;
        }

        // Everything below this line POSTS TO A CUSTOMER'S SLACK CHANNEL. Mesh control traffic is
        // not user content and must never get there.
        //
        // Without this guard, any system message the loop does not handle by name fell straight
        // into the text/v1 resolver, failed to parse (it has no `type` field), and the failure
        // path posted io-slack's own internal error INTO the channel. Observed live: an operator
        // mentioned the bot, the router bounced the message back UNREACHABLE because no handler
        // was configured, and two seconds later the bot answered the operator with its error text
        // for an invalid_text_payload. Every routing failure became noise in the customer's Slack.
        if is_control_plane_msg_type(&msg.meta.msg_type) {
            // UNREACHABLE is worth its own line: it means inbound Slack traffic is being accepted
            // and then dropped by the mesh, which looks like "the bot ignores me" from Slack and
            // like nothing at all from here.
            if meta_msg.eq_ignore_ascii_case("UNREACHABLE") {
                tracing::warn!(
                    trace_id = %msg.routing.trace_id,
                    node_name = %config.node_name,
                    "inbound Slack message came back UNREACHABLE: nothing in the mesh is handling \
                     it. Set config.io.dst_node (or route the binding ICH) so relayed messages \
                     have a destination."
                );
            } else {
                tracing::debug!(
                    trace_id = %msg.routing.trace_id,
                    msg_type = %msg.meta.msg_type,
                    msg = %meta_msg,
                    "ignoring mesh control message on the outbound path (never posted to Slack)"
                );
            }
            continue;
        }

        let binding = match extract_runtime_slack_binding(
            control_plane.read().await.effective_config.as_ref(),
            &config,
        ) {
            Ok(binding) => binding,
            Err(error) => {
                tracing::warn!(
                    trace_id = %msg.routing.trace_id,
                    error = %error,
                    "skipping outbound: missing workspace_id/conversation_id binding"
                );
                continue;
            }
        };
        post_outbound(
            &slack,
            blob_toolkit.as_ref(),
            &blob_payload_cfg,
            config.blob_runtime.resolve_retry,
            thread_records.as_deref(),
            &binding,
            &msg,
        )
        .await;
    }
}

/// Posts an outbound message in the bound channel: its text, with the WF's facts and menu
/// (slack_menu), then its files.
///
/// - A message that ends its subject with nothing to post only closes the subject's menu.
/// - A message with nothing to post, no text, no blocks and no files, posts nothing, with a log
///   line: no fallback.
/// - A message with only files, on a new place, posts a first text that names them, whose `ts`
///   the records keep for its thread id, and the files go under it: an upload answers no `ts`.
/// - A file the node cannot send is left out alone (`outbound_content`): the text and the other
///   files go, and the text names it. One Slack refuses once the text went is named by a post
///   after the files (FINDINGS A-108).
async fn post_outbound(
    slack: &SlackClients,
    blob_toolkit: &fluxbee_sdk::blob::BlobToolkit,
    blob_payload_cfg: &IoTextBlobConfig,
    resolve_retry: fluxbee_sdk::blob::ResolveRetryConfig,
    thread_records: Option<&SlackThreadRecords>,
    binding: &SlackBindingConfig,
    msg: &WireMessage,
) {
    let channel = binding.conversation_id.as_str();
    // Where in the bound channel the message goes is the thread records' call: its thread id's
    // place when a person opened it; when the node opened its places, the thread of its post
    // for a message that continues or ends the post's subject (its mark, `same` or `end`), else
    // a new post at the top level, since each of its posts serves one message; else its
    // reply_target when that is this channel; else a new post at the top level, recorded.
    // Another channel's reply_target is never read: an agent's answer carries the one of the
    // message it answers, which can be another node's.
    let reply_target = msg
        .meta
        .context
        .as_ref()
        .and_then(extract_slack_post_target);
    // The WF's block: the message's mark, the facts under its text and a menu for people, read
    // only from a WF node (see slack_menu).
    let wf_post = WfPost::of_message(msg, thread_records.is_some());
    // A message that ends its subject with nothing to post (no text, no blocks, no files) only
    // closes it: it posts nothing, and no fallback (docs/wf-orchestration-v1.md §5.4.2).
    if wf_post.only_closes(&msg.payload) {
        tracing::debug!(
            trace_id = %msg.routing.trace_id,
            "an end mark with nothing to post: the subject only closes"
        );
        close_subject_menu(
            slack,
            thread_records,
            msg.meta.thread_id.as_deref(),
            channel,
        )
        .await;
        return;
    }
    // The span puts the message's trace_id on the resolution's log lines.
    let mut thread = tracing::debug_span!("slack_outbound", trace_id = %msg.routing.trace_id)
        .in_scope(|| {
            OutboundThread::resolve(
                thread_records,
                &binding.workspace_id,
                channel,
                msg.meta.thread_id.as_deref(),
                reply_target.as_ref(),
                wf_post.subject,
                now_epoch_ms(),
            )
        });

    // A payload with nothing in it has nothing to resolve; a WF's menu may still make a post.
    let content = if payload_has_nothing(&msg.payload) {
        Ok(OutboundContent::default())
    } else {
        outbound_content(blob_toolkit, blob_payload_cfg, resolve_retry, &msg.payload).await
    };
    match content {
        Ok(content) => {
            for (filename, why) in &content.left_out {
                tracing::warn!(
                    trace_id = %msg.routing.trace_id,
                    filename = %filename,
                    code = why.canonical_code(),
                    error = %why,
                    "an outbound file is left out: the text names it, and the rest goes"
                );
            }
            let own_blocks = msg.payload.get("blocks");
            let mut text = content.text;
            // Only files, on a new place: a first post names them, which records the place, and
            // they go under it. An upload answers no `ts`, so each file would be a post of its own
            // at the top level, and the thread id's next message would open yet another.
            if text.trim().is_empty()
                && !content.files.is_empty()
                && thread.opens_a_place()
                && !has_blocks(wf_post.blocks("", own_blocks).as_ref())
            {
                let names = content.files.iter().map(|file| file.name.clone());
                text = files_line(&names.collect::<Vec<_>>());
            }
            if !content.left_out.is_empty() {
                let names = content
                    .left_out
                    .iter()
                    .map(|(filename, _)| filename.clone());
                text = with_line(text, &not_sent_line(&names.collect::<Vec<_>>()));
            }
            // M4: a Block Kit message legitimately carries its content in `blocks` with an empty
            // top-level `text` (text is only the notification fallback). Post when EITHER is
            // present. The WF's facts and menu make the blocks (see slack_menu).
            let blocks = wf_post.blocks(&text, own_blocks);
            let has_text = !text.trim().is_empty();
            let has_blocks = has_blocks(blocks.as_ref());
            if !has_text && !has_blocks && content.files.is_empty() {
                tracing::info!(
                    trace_id = %msg.routing.trace_id,
                    "an outbound message with no text, no blocks and no files: nothing is posted"
                );
            }
            if has_text || has_blocks {
                tracing::debug!(
                    slack_channel = channel,
                    slack_thread_ts = thread.thread_ts().unwrap_or(""),
                    text_len = text.len(),
                    has_blocks,
                    text_preview = %truncate(&text, 120),
                    "sending slack message"
                );
                let post_thread_ts = thread.thread_ts().map(str::to_string);
                let posted = slack
                    .post_message(channel, &text, thread.thread_ts(), blocks.clone())
                    .await;
                match posted {
                    Ok(ts) => {
                        thread.posted(ts.as_deref(), now_epoch_ms());
                        tracing::debug!("slack message sent");
                        if let (Some(ts), Some(blocks)) = (ts.as_deref(), blocks.as_ref()) {
                            wf_post
                                .menu_posted(
                                    slack,
                                    thread_records,
                                    channel,
                                    ts,
                                    post_thread_ts.as_deref(),
                                    blocks,
                                )
                                .await;
                        }
                    }
                    Err(e) => tracing::warn!(error=?e, "failed to send slack message"),
                }
            }

            let mut refused = Vec::new();
            for file in content.files {
                let uploaded = slack
                    .upload_file(
                        channel,
                        thread.thread_ts(),
                        &file.name,
                        &file.mime,
                        file.bytes,
                    )
                    .await;
                match uploaded {
                    Ok(()) => {
                        thread.posted(None, now_epoch_ms());
                        tracing::debug!(
                            blob_name = %file.blob_name,
                            filename = %file.name,
                            "slack attachment uploaded"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            error = ?error,
                            blob_name = %file.blob_name,
                            "failed to upload attachment to Slack; a post after the files names it"
                        );
                        refused.push(file.name);
                    }
                }
            }
            // The text went before Slack refused a file: a post of its own names it.
            if !refused.is_empty() {
                let posted = slack
                    .post_message(channel, &not_sent_line(&refused), thread.thread_ts(), None)
                    .await;
                match posted {
                    Ok(ts) => thread.posted(ts.as_deref(), now_epoch_ms()),
                    Err(error) => tracing::warn!(
                        error = ?error,
                        "failed to post the names of the files Slack refused"
                    ),
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
                "Error ({}) while processing the content or attachments.",
                err.canonical_code()
            );
            let posted = slack
                .post_message(channel, &fallback, thread.thread_ts(), None)
                .await;
            match posted {
                Ok(ts) => thread.posted(ts.as_deref(), now_epoch_ms()),
                Err(error) => {
                    tracing::warn!(error=?error, "failed to send outbound fallback message")
                }
            }
        }
    }

    // A new post forgets the node's older posts on its thread id, and their open menus close
    // with them (see thread_records), after the message's posts.
    close_forgotten_menus(slack, thread.take_forgotten_menus()).await;

    // `end` ends the subject: its open menu closes, after the message's posts.
    if wf_post.subject == Subject::End {
        close_subject_menu(
            slack,
            thread_records,
            msg.meta.thread_id.as_deref(),
            channel,
        )
        .await;
    }
}

/// Whether a value says nothing: `null`, a blank text, an empty list or an empty object.
pub(crate) fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.trim().is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(fields) => fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Whether an outbound payload has nothing to post: no text, no `content_ref`, no files and no
/// blocks, or no payload at all.
pub(crate) fn payload_has_nothing(payload: &Value) -> bool {
    payload.is_null()
        || (payload.is_object()
            && ["content", "content_ref", "attachments", "blocks"]
                .into_iter()
                .all(|key| payload.get(key).is_none_or(is_blank)))
}

/// Whether a post's blocks are any.
fn has_blocks(blocks: Option<&Value>) -> bool {
    blocks
        .and_then(Value::as_array)
        .is_some_and(|blocks| !blocks.is_empty())
}

/// What an outbound message gives the node to post: its text, the files it can send, read from
/// their blobs, and the names of those it cannot, each with why.
#[derive(Default)]
struct OutboundContent {
    text: String,
    files: Vec<OutboundFile>,
    left_out: Vec<(String, IoBlobContractError)>,
}

/// A file of an outbound message, read: its name, its type, its blob and its bytes.
struct OutboundFile {
    name: String,
    mime: String,
    blob_name: String,
    bytes: Vec<u8>,
}

/// The content of an outbound `text/v1` payload. Its text, inline or its `content_ref`'s, fails the
/// whole when it does not resolve, as an invalid payload does. Its files are each read from their
/// blob, waiting `retry` for one that has not arrived; one the node cannot send is left out alone:
/// past the first `max_attachments`, of a type outside the allowed list, past the size of a file
/// or of them all, or a blob that is not there or does not read. The text and the other files
/// still go (FINDINGS A-108).
async fn outbound_content(
    blob_toolkit: &fluxbee_sdk::blob::BlobToolkit,
    cfg: &IoTextBlobConfig,
    retry: fluxbee_sdk::blob::ResolveRetryConfig,
    payload: &Value,
) -> Result<OutboundContent, IoBlobContractError> {
    let text = resolve_text_v1_text_only_for_outbound(blob_toolkit, payload, true)
        .await?
        .text;
    let attachments = fluxbee_sdk::payload::TextV1Payload::from_value(payload)?.attachments;
    let allowed_mimes = cfg
        .allowed_mimes
        .iter()
        .map(|mime| normalize_mime(mime))
        .collect::<HashSet<_>>();
    let count = attachments.len();
    let mut content = OutboundContent {
        text,
        ..OutboundContent::default()
    };
    let mut total_size: u64 = 0;
    for (index, blob_ref) in attachments.into_iter().enumerate() {
        let name = if blob_ref.filename_original.trim().is_empty() {
            blob_ref.blob_name.clone()
        } else {
            blob_ref.filename_original.clone()
        };
        let refused = if index >= cfg.max_attachments {
            Some(IoBlobContractError::TooManyAttachments {
                actual: count,
                max: cfg.max_attachments,
            })
        } else if !allowed_mimes.contains(&normalize_mime(&blob_ref.mime)) {
            Some(IoBlobContractError::UnsupportedMime {
                mime: blob_ref.mime.clone(),
            })
        } else if blob_ref.size > cfg.max_attachment_bytes {
            Some(IoBlobContractError::BlobTooLarge {
                actual: blob_ref.size,
                max: cfg.max_attachment_bytes,
            })
        } else if total_size.saturating_add(blob_ref.size) > cfg.max_total_attachment_bytes {
            Some(IoBlobContractError::BlobTooLarge {
                actual: total_size.saturating_add(blob_ref.size),
                max: cfg.max_total_attachment_bytes,
            })
        } else {
            None
        };
        if let Some(why) = refused {
            content.left_out.push((name, why));
            continue;
        }
        match blob_toolkit.read_blob_with_retry(&blob_ref, retry).await {
            Ok(bytes) => {
                total_size = total_size.saturating_add(blob_ref.size);
                content.files.push(OutboundFile {
                    name,
                    mime: blob_ref.mime,
                    blob_name: blob_ref.blob_name,
                    bytes,
                });
            }
            Err(error) => content
                .left_out
                .push((name, IoBlobContractError::Blob(error))),
        }
    }
    Ok(content)
}

/// The first post of a message with only files, on a new place: their names.
fn files_line(names: &[String]) -> String {
    let label = if names.len() == 1 { "File" } else { "Files" };
    format!("{label}: {}", names.join(", "))
}

/// The line that names the files of a message that do not go.
fn not_sent_line(names: &[String]) -> String {
    format!("Not sent: {}", names.join(", "))
}

/// `text`, then `line` after a blank line; `line` alone under a blank text.
fn with_line(text: String, line: &str) -> String {
    if text.trim().is_empty() {
        line.to_string()
    } else {
        format!("{}\n\n{line}", text.trim_end())
    }
}

async fn handle_io_control_plane_message(
    msg: &WireMessage,
    node_name: &str,
    control_src: &str,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    dispatcher: Arc<RouterDispatcher>,
    identity_provision_cfg: IdentityProvisionConfig,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
    ensured_bindings: Arc<Mutex<HashSet<String>>>,
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
                control_plane.clone(),
                control_metrics,
                adapter_contract,
                inbound,
                relay,
                dispatcher,
                identity_provision_cfg,
                self_ilk_id,
                self_tenant_id,
                ensured_bindings,
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

/// Whether a message's sender is a WF node, `WF.<name>@<hive>`, by the name its router stamps on
/// it (`routing.src_l2_name`), which no sender chooses. The node reads a WF's block, and shows a
/// `NOT_TAKEN`, only from a WF (slack_menu, not_taken).
fn is_wf_node(src_l2_name: Option<&str>) -> bool {
    src_l2_name.is_some_and(|name| name.starts_with("WF."))
}

async fn apply_io_config_set(
    payload: &fluxbee_sdk::node_config::NodeConfigSetPayload,
    node_name: &str,
    control_plane: Arc<RwLock<IoControlPlaneState>>,
    control_metrics: Arc<IoControlPlaneMetrics>,
    adapter_contract: &dyn IoAdapterConfigContract,
    inbound: Arc<Mutex<InboundProcessor>>,
    relay: Arc<Mutex<RelayBuffer<InMemoryRelayStore>>>,
    dispatcher: Arc<RouterDispatcher>,
    identity_provision_cfg: IdentityProvisionConfig,
    self_ilk_id: Option<String>,
    self_tenant_id: Option<String>,
    ensured_bindings: Arc<Mutex<HashSet<String>>>,
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

    state.current_state = IoNodeLifecycleState::Configured;
    state.config_source = IoConfigSource::Dynamic;
    state.schema_version = payload.schema_version;
    state.config_version = payload.config_version;
    state.effective_config = Some(effective.clone());
    state.last_error = None;

    if let Err(err) = persist_io_control_plane_state(node_name, &state) {
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
    if section_changed(
        previous_effective.as_ref(),
        &effective,
        &["io", "workspace_id"],
    ) {
        apply_hot.push("io.workspace_id".to_string());
    }
    if section_changed(
        previous_effective.as_ref(),
        &effective,
        &["io", "conversation_id"],
    ) {
        apply_hot.push("io.conversation_id".to_string());
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

    let binding = match extract_slack_binding_from_io_section(effective.get("io")) {
        Ok(binding) => binding,
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
    };
    if let Err(err) = ensure_io_slack_binding_ich_registered(
        node_name,
        &binding,
        &dispatcher,
        &identity_provision_cfg,
        self_ilk_id.as_deref(),
        self_tenant_id.as_deref(),
        ensured_bindings,
    )
    .await
    {
        let err_text = err.to_string();
        state.current_state = IoNodeLifecycleState::FailedConfig;
        state.last_error = Some(IoControlPlaneErrorInfo {
            code: "own_ich_registration_failed".to_string(),
            message: err_text.clone(),
        });
        control_metrics.record_config_set_error(
            state.current_state.as_str(),
            payload.config_version,
            "own_ich_registration_failed",
        );
        let redacted = redact_state(&state, adapter_contract);
        return build_io_config_set_error_payload(
            node_name,
            &redacted,
            "own_ich_registration_failed",
            err_text,
        );
    }
    apply_reinit.push("io.own_ich".to_string());
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

fn extract_slack_binding_from_io_section(io: Option<&Value>) -> Result<SlackBindingConfig> {
    let workspace_id = io
        .and_then(|section| section.get("workspace_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing config.io.workspace_id"))?;
    let conversation_id = io
        .and_then(|section| section.get("conversation_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| anyhow::anyhow!("missing config.io.conversation_id"))?;

    Ok(SlackBindingConfig {
        workspace_id,
        conversation_id,
    })
}

fn extract_runtime_slack_binding(
    effective_config: Option<&Value>,
    defaults: &Config,
) -> Result<SlackBindingConfig> {
    if let Ok(binding) =
        extract_slack_binding_from_io_section(effective_config.and_then(|cfg| cfg.get("io")))
    {
        return Ok(binding);
    }

    let workspace_id = defaults
        .workspace_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing config.io.workspace_id"))?;
    let conversation_id = defaults
        .conversation_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("missing config.io.conversation_id"))?;

    Ok(SlackBindingConfig {
        workspace_id,
        conversation_id,
    })
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

/// A fake Slack Web API, for the tests: an HTTP server on the loopback that answers each method
/// with `{"ok": true}`, or the answers its test gives, and keeps each call, in the order they came.
/// A client of it (`FakeWebApi::client`) has credentials, and no call of it reaches Slack. A file's
/// bytes go to it too: `files.getUploadURLExternal` answers its own URL `upload`, unless the test
/// gives that method's answers.
#[cfg(test)]
mod fake_web_api {
    use super::{file_download_client, SlackClients};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// The calls so far: each method, and its JSON body (`null` for a body that is no JSON).
    type Calls = Arc<Mutex<Vec<(String, Value)>>>;
    /// The answers of each method a test gives, in the order its calls get them.
    type Answers = Arc<HashMap<String, Vec<Value>>>;

    pub(crate) struct FakeWebApi {
        base: String,
        calls: Calls,
    }

    impl FakeWebApi {
        /// Starts the server: `answers` gives the answer of a method, in place of `{"ok": true}`. A
        /// method given more than once answers its calls in that order, and its last answer from
        /// then on.
        pub(crate) async fn start(answers: &[(&str, Value)]) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("a loopback port");
            let base = format!("http://{}/api", listener.local_addr().expect("its address"));
            let mut by_api = HashMap::<String, Vec<Value>>::new();
            for (api, answer) in answers {
                by_api
                    .entry(api.to_string())
                    .or_default()
                    .push(answer.clone());
            }
            by_api
                .entry("files.getUploadURLExternal".to_string())
                .or_insert_with(|| {
                    vec![json!({
                        "ok": true,
                        "upload_url": format!("{base}/upload"),
                        "file_id": "F0FILE"
                    })]
                });
            let answers: Answers = Arc::new(by_api);
            let calls = Calls::default();
            let kept = calls.clone();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    tokio::spawn(answer(stream, answers.clone(), kept.clone()));
                }
            });
            Self { base, calls }
        }

        /// A Slack client of this Web API, with credentials, and no proxy between them.
        pub(crate) async fn client(&self) -> SlackClients {
            let mut slack = SlackClients::new().expect("client");
            slack.http = reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("http client");
            slack.downloads = file_download_client()
                .no_proxy()
                .build()
                .expect("download client");
            slack.web_api = self.base.clone();
            slack.file_store = self.base.clone();
            slack
                .reload_credentials("test-app-token".to_string(), "test-bot-token".to_string())
                .await;
            slack
        }

        /// The URL of `path` on this server, which answers it as the method its last segment
        /// names: a file's download, say.
        pub(crate) fn url(&self, path: &str) -> String {
            format!("{}/{path}", self.base)
        }

        /// The calls so far, each method and its body, in the order they came.
        pub(crate) fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().expect("calls").clone()
        }

        /// The bodies of the calls to `api` so far.
        pub(crate) fn bodies(&self, api: &str) -> Vec<Value> {
            self.calls()
                .into_iter()
                .filter(|(called, _)| called == api)
                .map(|(_, body)| body)
                .collect()
        }

        /// The bodies of the calls to `api`, once there are `count` of them, or after 5 seconds: a
        /// spawned task calls when it calls.
        pub(crate) async fn wait_for(&self, api: &str, count: usize) -> Vec<Value> {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let bodies = self.bodies(api);
                if bodies.len() >= count || tokio::time::Instant::now() >= deadline {
                    return bodies;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    /// Reads one request, keeps its call, answers it, and closes the connection.
    async fn answer(mut stream: TcpStream, answers: Answers, calls: Calls) {
        let mut request = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut body_at = None;
        let mut body_len = 0;
        while body_at.is_none_or(|at| request.len() < at + body_len) {
            match stream.read(&mut chunk).await {
                Ok(read) if read > 0 => request.extend_from_slice(&chunk[..read]),
                _ => return,
            }
            if body_at.is_none() {
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    body_len = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|len| len.trim().parse().ok())
                        .unwrap_or(0);
                    body_at = Some(end + 4);
                }
            }
        }
        let body_at = body_at.unwrap_or(request.len());
        let head = String::from_utf8_lossy(&request[..body_at]);
        // The request line: `POST /api/<method> HTTP/1.1`.
        let api = head
            .split_whitespace()
            .nth(1)
            .and_then(|path| path.rsplit('/').next())
            .unwrap_or_default()
            .to_string();
        let body = serde_json::from_slice(&request[body_at..]).unwrap_or(Value::Null);
        let earlier = {
            let mut calls = calls.lock().expect("calls");
            let earlier = calls.iter().filter(|(called, _)| *called == api).count();
            calls.push((api.clone(), body));
            earlier
        };
        let answer = answers
            .get(&api)
            .and_then(|given| given.get(earlier).or(given.last()))
            .cloned()
            .unwrap_or_else(|| json!({"ok": true}))
            .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{answer}",
            answer.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_io_slack_rpc_profile, build_slack_inbound_payload_from_parts, build_system_reply,
        classify_slack_inbound, decode_slack_text, default_slack_allowed_mimes,
        extract_runtime_relay_config, extract_slack_tokens_from_vault_value,
        is_control_plane_msg_type, message_seen, now_epoch_ms, outbound_content, parse_retry_after,
        post_ephemeral_body, post_message_body, post_outbound, run_slack_event_worker, same_origin,
        slack_inbound_config, slack_relay_policy_from_config, slack_vault_key,
        vault_change_is_slack, Config, Download, SlackBindingConfig, SlackClients,
        SlackRelayConfig, SlackRuntimeState, SocketEnvelope, ENVELOPE_EVENTS_API,
        ENVELOPE_INTERACTIVE,
    };
    use crate::fake_web_api::FakeWebApi;
    use crate::not_taken::RelayedMessages;
    use crate::thread_records::{map_inbound_thread, SlackThreadRecords};
    use fluxbee_sdk::blob::{BlobConfig, BlobRef, BlobToolkit, ResolveRetryConfig};
    use fluxbee_sdk::RouterDispatcherTestHarness;
    use io_common::identity::{DisabledIdentityProvisioner, MockIdentityResolver};
    use io_common::inbound::InboundProcessor;
    use io_common::io_context::{slack_binding_id, slack_inbound_io_context};
    use io_common::io_control_plane::IoControlPlaneState;
    use io_common::provision::IdentityProvisionConfig;
    use io_common::relay::{InMemoryRelayStore, RelayBuffer};
    use io_common::reliability::DedupCache;
    use io_common::router_message::{build_user_message, DEFAULT_TTL};
    use io_common::text_v1_blob::{IoBlobRuntimeConfig, IoTextBlobConfig};
    use regex::Regex;
    use serde_json::Value;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{mpsc, Mutex, RwLock};

    const WORKSPACE: &str = "T0WORKSPACE";
    const CHANNEL: &str = "C0SALES";
    /// The sales io.slack node, and the WF it relays to (its `io.dst_node`).
    const NODE: &str = "IO.slack.sales@motherbee";
    const WF: &str = "WF.sales.invoice@motherbee";
    /// An agent that answers through the node, as any sender that is no WF.
    const AGENT: &str = "AI.sales@worker1";
    /// The thread id of a conversation the node has no place for yet.
    const CASE_THREAD: &str = "thread:sha256:ana-conversation";
    /// The `ts` Slack answers for the node's posts.
    const POST_TS: &str = "1793000000.000100";

    /// A blob store in a directory of its own, removed with it.
    struct BlobStore {
        blobs: BlobToolkit,
        root: PathBuf,
    }

    impl Drop for BlobStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn blob_store(label: &str) -> BlobStore {
        let root = std::env::temp_dir().join(format!(
            "io-slack-blobs-{label}-{}-{}",
            std::process::id(),
            now_epoch_ms()
        ));
        let blobs = BlobToolkit::new(BlobConfig {
            blob_root: root.clone(),
            name_max_chars: fluxbee_sdk::blob::BLOB_NAME_MAX_CHARS,
            max_blob_bytes: None,
        })
        .expect("blob store");
        BlobStore { blobs, root }
    }

    impl BlobStore {
        /// A file in the store, as a message's attachment refers to it.
        fn put(&self, bytes: &[u8], name: &str, mime: &str) -> BlobRef {
            let blob_ref = self.blobs.put_bytes(bytes, name, mime).expect("put");
            self.blobs.promote(&blob_ref).expect("promote");
            blob_ref
        }

        /// A file that never comes: its blob is staged, never promoted.
        fn never_comes(&self, bytes: &[u8], name: &str, mime: &str) -> BlobRef {
            self.blobs.put_bytes(bytes, name, mime).expect("put")
        }
    }

    /// The limits of the node's files: Slack's types (`default_slack_allowed_mimes`).
    fn slack_files() -> IoTextBlobConfig {
        IoTextBlobConfig {
            allowed_mimes: default_slack_allowed_mimes(),
            ..IoTextBlobConfig::default()
        }
    }

    /// How long a test waits for a blob that never comes.
    fn short_wait() -> ResolveRetryConfig {
        ResolveRetryConfig {
            max_wait_ms: 50,
            initial_delay_ms: 10,
            backoff_factor: 2.0,
        }
    }

    /// A message the node takes to post, from `from` (by its router's stamp), on `thread_id`.
    fn outbound(from: &str, thread_id: &str, context: Value, payload: Value) -> WireMessage {
        let mut msg = build_user_message(
            "sender-uuid",
            None,
            DEFAULT_TTL,
            "trace-out".to_string(),
            None,
            None,
            context,
            payload,
        );
        msg.routing.src_l2_name = Some(from.to_string());
        msg.meta.thread_id = Some(thread_id.to_string());
        msg
    }

    /// Posts `msg` as the node bound to `CHANNEL` does, with Slack's types for its files.
    async fn post(
        slack: &SlackClients,
        store: &BlobStore,
        records: Option<&SlackThreadRecords>,
        msg: &WireMessage,
    ) {
        let binding = SlackBindingConfig {
            workspace_id: WORKSPACE.to_string(),
            conversation_id: CHANNEL.to_string(),
        };
        post_outbound(
            slack,
            &store.blobs,
            &slack_files(),
            short_wait(),
            records,
            &binding,
            msg,
        )
        .await;
    }

    /// The node `NODE`, bound to `CHANNEL` of `WORKSPACE` with its own ICH ensured and `WF` as its
    /// `io.dst_node`, as its event worker sees it.
    fn node_config() -> Config {
        Config {
            workspace_id: None,
            conversation_id: None,
            node_name: NODE.to_string(),
            island_id: "motherbee".to_string(),
            node_version: "0.1".to_string(),
            router_socket: PathBuf::new(),
            uuid_persistence_dir: PathBuf::new(),
            config_dir: PathBuf::new(),
            dev_mode: false,
            ttl: DEFAULT_TTL,
            dedup_ttl_ms: 60_000,
            dedup_max_entries: 100,
            dst_node: None,
            identity_target: "SY.identity@motherbee".to_string(),
            identity_timeout_ms: 1_000,
            relay: SlackRelayConfig::default(),
            blob_runtime: IoBlobRuntimeConfig::default(),
        }
    }

    /// Runs the event worker of `node_config` on `events`, each the payload of an `events_api`
    /// envelope, until it took them all, with `api` as Slack and `store` for the files. What it
    /// relayed is at the router that comes back.
    async fn run_events(
        api: &FakeWebApi,
        store: &BlobStore,
        events: Vec<Value>,
    ) -> RouterDispatcherTestHarness {
        let (dispatcher, router) =
            RouterDispatcherTestHarness::new(NODE, build_io_slack_rpc_profile().expect("profile"));
        let config = node_config();
        let mut inbound = InboundProcessor::new(
            dispatcher.sender_snapshot().uuid().to_string(),
            slack_inbound_config(&config, Some("tnt:sales".to_string())),
        );
        inbound.set_dst_node(Some(WF.to_string()));
        let relay = RelayBuffer::new(
            slack_relay_policy_from_config(&SlackRelayConfig::default()).expect("policy"),
            InMemoryRelayStore::new(),
        )
        .expect("relay");
        let control_plane = IoControlPlaneState {
            effective_config: Some(json!({"io": {
                "workspace_id": WORKSPACE,
                "conversation_id": CHANNEL,
                "dst_node": WF
            }})),
            ..IoControlPlaneState::default()
        };
        let (queue, queued) = mpsc::channel(events.len().max(1));
        for event in events {
            queue.try_send(event).expect("queued");
        }
        drop(queue);
        run_slack_event_worker(
            queued,
            config,
            dispatcher.clone(),
            IdentityProvisionConfig {
                target: "SY.identity@motherbee".to_string(),
                timeout: Duration::from_secs(1),
            },
            Arc::new(RwLock::new(control_plane)),
            Some("ilk:io-slack-sales".to_string()),
            Some("tnt:sales".to_string()),
            Arc::new(api.client().await),
            Arc::new(MockIdentityResolver::new()),
            Arc::new(DisabledIdentityProvisioner::new()),
            Arc::new(Mutex::new(inbound)),
            Arc::new(Mutex::new(relay)),
            Arc::new(Mutex::new(HashSet::from([slack_binding_id(
                WORKSPACE, CHANNEL,
            )]))),
            Arc::new(store.blobs.clone()),
            slack_files(),
            Some(Arc::new(SlackThreadRecords::in_memory().expect("records"))),
            Arc::new(RelayedMessages::new()),
            dispatcher.sender_snapshot(),
            Regex::new(r"^<@[^>]+>\s*").expect("mention"),
        )
        .await;
        router
    }

    /// An `events_api` envelope's payload: `event`, in `WORKSPACE`, as the event `event_id`.
    fn event_callback(event_id: &str, event: Value) -> Value {
        json!({
            "type": "event_callback",
            "team_id": WORKSPACE,
            "event_id": event_id,
            "event": event
        })
    }

    /// What the router got, in the order it came.
    async fn relayed(router: &mut RouterDispatcherTestHarness) -> Vec<WireMessage> {
        let mut messages = Vec::new();
        while let Some(msg) = router
            .next_outgoing_within(Duration::from_millis(200))
            .await
        {
            messages.push(msg);
        }
        messages
    }

    #[test]
    fn vault_change_is_slack_matches_only_the_slack_resource() {
        assert!(vault_change_is_slack(
            &json!({"resource_type":"slack","op":"put"})
        ));
        assert!(vault_change_is_slack(&json!({"resource_type":"Slack"}))); // case-insensitive
        assert!(!vault_change_is_slack(&json!({"resource_type":"openai"})));
        assert!(!vault_change_is_slack(&json!({"op":"put"}))); // no resource_type
    }
    use fluxbee_sdk::protocol::{Destination, Message as WireMessage, Meta, Routing};
    use serde_json::json;

    #[test]
    fn slack_vault_key_reads_auth_key_and_ignores_blanks() {
        // Family vault_ref pattern: config carries only slack.auth.key (a vault reference).
        let cfg = json!({"slack":{"auth":{"type":"vault_ref","resource_type":"slack","key":"slack:io-slack:motherbee"}}});
        assert_eq!(
            slack_vault_key(Some(&cfg)).as_deref(),
            Some("slack:io-slack:motherbee")
        );
        assert_eq!(slack_vault_key(None), None);
        assert_eq!(slack_vault_key(Some(&json!({"slack":{"auth":{}}}))), None);
        assert_eq!(
            slack_vault_key(Some(&json!({"slack":{"auth":{"key":"   "}}}))),
            None
        );
    }

    #[test]
    fn extract_slack_tokens_requires_object_with_both_fields() {
        let ok = extract_slack_tokens_from_vault_value(
            &json!({"app_token":"xapp-1","bot_token":"xoxb-2"}),
        );
        assert_eq!(ok, Some(("xapp-1".to_string(), "xoxb-2".to_string())));
        // bare strings are ambiguous -> rejected
        assert_eq!(
            extract_slack_tokens_from_vault_value(&json!("xapp-1")),
            None
        );
        // a missing / blank half is not usable
        assert_eq!(
            extract_slack_tokens_from_vault_value(&json!({"app_token":"xapp-1"})),
            None
        );
        assert_eq!(
            extract_slack_tokens_from_vault_value(&json!({"app_token":"xapp-1","bot_token":"  "})),
            None
        );
    }

    #[test]
    fn parse_retry_after_reads_header_and_clamps() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        // absent header -> 1s floor
        assert_eq!(parse_retry_after(&HeaderMap::new()), Duration::from_secs(1));
        // honored within range
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("7"));
        assert_eq!(parse_retry_after(&h), Duration::from_secs(7));
        // clamped to 30s ceiling
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("600"));
        assert_eq!(parse_retry_after(&h), Duration::from_secs(30));
        // garbage / HTTP-date form -> 1s floor (we only accept integer seconds)
        let mut h = HeaderMap::new();
        h.insert(
            RETRY_AFTER,
            HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&h), Duration::from_secs(1));
    }

    #[tokio::test]
    async fn credential_generation_is_change_driven_and_delete_aware() {
        let slack = SlackClients::new().expect("client");
        assert_eq!(slack.config_generation().await, 0);

        // First install bumps (0 -> 1).
        slack
            .reload_credentials("xapp-1".into(), "xoxb-1".into())
            .await;
        assert_eq!(slack.config_generation().await, 1);

        // An identical reload (the periodic vault refresh on an UNCHANGED secret) must NOT churn the
        // generation — otherwise the inbound WebSocket would needlessly reconnect every tick.
        slack
            .reload_credentials("xapp-1".into(), "xoxb-1".into())
            .await;
        assert_eq!(slack.config_generation().await, 1);

        // A real rotation bumps (1 -> 2) so the socket reconnects with the new token.
        slack
            .reload_credentials("xapp-2".into(), "xoxb-1".into())
            .await;
        assert_eq!(slack.config_generation().await, 2);

        // Vault delete: clear drops the live tokens and bumps so the socket stops using the stale one.
        slack.clear_credentials().await;
        assert_eq!(slack.config_generation().await, 3);
        assert!(slack.slack_app_token().await.is_err());
        assert!(slack.slack_bot_token().await.is_err());

        // Clearing again is a no-op (already empty) — no generation churn.
        slack.clear_credentials().await;
        assert_eq!(slack.config_generation().await, 3);
    }

    #[test]
    fn slack_runtime_state_debug_never_leaks_tokens() {
        // A leaked xapp-/xoxb- token in a log line is a workspace compromise: Debug must redact.
        let state = SlackRuntimeState {
            slack_app_token: Some("xapp-super-secret".to_string()),
            slack_bot_token: Some("xoxb-super-secret".to_string()),
            config_generation: 3,
            bot_user_id: Some("U0BOT".to_string()),
        };
        let rendered = format!("{state:?}");
        assert!(!rendered.contains("xapp-super-secret"), "{rendered}");
        assert!(!rendered.contains("xoxb-super-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(rendered.contains("config_generation: 3"), "{rendered}");
        // absence renders as None, not <redacted>
        let empty = SlackRuntimeState {
            bot_user_id: None,
            slack_app_token: None,
            slack_bot_token: None,
            config_generation: 0,
        };
        assert!(format!("{empty:?}").contains("None"));
    }

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
                ich: Some("slack://T123".to_string()),
                thread_id: Some("thread:canonical-1".to_string()),
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

        assert_eq!(reply.meta.ich.as_deref(), Some("slack://T123"));
        assert_eq!(reply.meta.thread_id.as_deref(), Some("thread:canonical-1"));
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

    /// The mesh's control traffic can never reach the code that posts in Slack.
    ///
    /// It is pinned by reading the source because the property is one of ORDER: the guard has to
    /// sit between the control handlers by name and the binding lookup that opens the outbound
    /// path. A test on `is_control_plane_msg_type` alone passes just as happily with the guard
    /// deleted, which is how an UNREACHABLE bounce ended up posting "Error (invalid_text_payload)"
    /// in a customer's channel.
    ///
    /// The literals are split with `concat!`: if they were whole, this test could find ITSELF and
    /// measure its own text.
    #[test]
    fn control_traffic_never_reaches_the_path_out_to_slack() {
        let src = include_str!("main.rs");
        let guard_if = concat!("if is_control_plane_msg_type(&msg.meta.", "msg_type) {");
        let outbound = concat!("let binding = match extract_runtime_slack", "_binding(");
        let unreachable_warn = concat!("inbound Slack message came back ", "UNREACHABLE");

        let warn_at = src
            .find(unreachable_warn)
            .expect("the guard has to keep warning about the UNREACHABLE bounce");

        // The OUTBOUND path is the binding lookup AFTER the guard (there is another one before
        // it, on the inbound path).
        let outbound_at = warn_at
            + src[warn_at..]
                .find(outbound)
                .expect("the outbound path has to come after the guard");

        // The msg_type cut has to come before the warn, and its `continue` before the outbound
        // path.
        let guard_at = src[..warn_at]
            .rfind(guard_if)
            .expect("the msg_type cut has to precede the warn");
        assert!(
            guard_at < warn_at && warn_at < outbound_at,
            "broken order: guard={guard_at} warn={warn_at} outbound={outbound_at}"
        );
        assert!(
            src[guard_at..outbound_at].contains("continue;"),
            "the guard has to CUT (continue) before it gets to post in Slack"
        );

        assert!(is_control_plane_msg_type("system"));
        assert!(is_control_plane_msg_type("SYSTEM"));
        assert!(is_control_plane_msg_type("admin"));
        assert!(!is_control_plane_msg_type("data"));
    }

    /// The anti-echo guards. This is the part that cannot be wrong: if the node relays its own
    /// posts, whatever the mesh answers is posted again, and that is a loop inside a customer's
    /// Slack.
    #[test]
    fn the_node_never_relays_its_own_posts() {
        let me = "U0BOT";

        let person = serde_json::json!({"type":"message","user":"U0PERSON","text":"hello"});
        assert!(classify_slack_inbound(&person, "message", Some(me)).is_ok());

        let own_post = serde_json::json!({"type":"message","user":me,"text":"the bot's answer"});
        assert!(classify_slack_inbound(&own_post, "message", Some(me)).is_err());

        let with_bot_id =
            serde_json::json!({"type":"message","user":"U0OTHER","bot_id":"B123","text":"x"});
        assert!(classify_slack_inbound(&with_bot_id, "message", Some(me)).is_err());

        let with_subtype = serde_json::json!({"type":"message","subtype":"bot_message","text":"x"});
        assert!(classify_slack_inbound(&with_subtype, "message", Some(me)).is_err());

        // FAILS CLOSED: without knowing who we are, nothing is relayed.
        assert!(
            classify_slack_inbound(&person, "message", None).is_err(),
            "without bot_user_id a self-post cannot be ruled out: nothing is relayed"
        );

        let mention_event =
            serde_json::json!({"type":"app_mention","user":"U0PERSON","text":"<@U0BOT> hello"});
        assert!(classify_slack_inbound(&mention_event, "app_mention", None).is_ok());

        let other = serde_json::json!({"type":"reaction_added"});
        assert!(classify_slack_inbound(&other, "reaction_added", Some(me)).is_err());
    }

    /// A payload's text is posted as Slack shows plain text: its `&`, `<` and `>` escaped. What a
    /// person wrote, and an agent passed on (a WF's question to the supervisors' channel), never
    /// pings the channel, a person or a group, and never shows a false link label. A bare URL is
    /// left to link, and `blocks` go as they are.
    #[test]
    fn a_payloads_text_is_posted_escaped_and_its_blocks_as_they_are() {
        let blocks = json!([
            {"type": "section", "text": {"type": "mrkdwn", "text": "<!here> *Approved*"}}
        ]);
        let body = post_message_body(
            "C0SUPERVISORS",
            "<!channel> <!here> <@U0BOSS> <!subteam^S0SALES|@sales> 20% for 10 Pro? Go to \
             <https://evil.example|https://sso.acme.com> or https://fluxbee.example/public/k3y \
             (Q&A: 1 < 2 > 0)",
            Some("1793000000.000100"),
            Some(blocks.clone()),
        );
        assert_eq!(
            body,
            json!({
                "channel": "C0SUPERVISORS",
                "text": "&lt;!channel&gt; &lt;!here&gt; &lt;@U0BOSS&gt; \
                         &lt;!subteam^S0SALES|@sales&gt; 20% for 10 Pro? Go to \
                         &lt;https://evil.example|https://sso.acme.com&gt; or \
                         https://fluxbee.example/public/k3y (Q&amp;A: 1 &lt; 2 &gt; 0)",
                "thread_ts": "1793000000.000100",
                "blocks": blocks,
            })
        );

        // At the top level, without blocks; a text with nothing to escape goes as it is.
        assert_eq!(
            post_message_body("C0SUPERVISORS", "Approved: 15%.", None, None),
            json!({"channel": "C0SUPERVISORS", "text": "Approved: 15%."})
        );
    }

    /// What Slack escapes in a person's text is decoded on the way in, and escaped once on the way
    /// out: a person's `Q&A <x>`, which Slack sends as `Q&amp;A &lt;x&gt;`, reaches the mesh as
    /// written and goes back as Slack sent it. Slack's own markup, a mention or a link, stays as it
    /// is, and `&amp;` is decoded last, so `&amp;lt;` is `&lt;`.
    #[test]
    fn a_persons_text_reaches_the_mesh_as_written_and_goes_back_escaped_once() {
        let content = decode_slack_text("Q&amp;A &lt;x&gt;");
        assert_eq!(content, "Q&A <x>");
        let payload = build_slack_inbound_payload_from_parts(
            "T0WORKSPACE",
            "U0PERSON",
            "C0SALES",
            None,
            "Ev0001",
            &content,
            &[],
        )
        .expect("a payload");
        assert_eq!(payload["content"], "Q&A <x>");
        assert_eq!(
            post_message_body("C0SALES", &content, None, None)["text"],
            "Q&amp;A &lt;x&gt;"
        );

        for (sent, written) in [
            ("&amp;lt;b&amp;gt;", "&lt;b&gt;"),
            ("R&amp;D &amp;amp; P&amp;L", "R&D &amp; P&L"),
            ("<@U0BOSS> 1 &lt; 2", "<@U0BOSS> 1 < 2"),
            (
                "<!here> see <https://acme.example/?a=1&amp;b=2|Q&amp;A> and R&amp;D",
                "<!here> see <https://acme.example/?a=1&amp;b=2|Q&amp;A> and R&D",
            ),
            (
                "How are you? Nothing to decode.",
                "How are you? Nothing to decode.",
            ),
            // A raw `<` with no `>` after it is no markup.
            ("1 < 2 &gt; 0", "1 < 2 > 0"),
        ] {
            assert_eq!(decode_slack_text(sent), written, "{sent}");
        }
    }

    /// A message only one person sees ("Not taken", "This is no longer open.") is posted as any
    /// other: its text escaped, in the person's Slack thread or at the top level.
    #[test]
    fn an_ephemeral_message_is_posted_escaped_where_the_person_wrote() {
        assert_eq!(
            post_ephemeral_body(
                "C0SUPERVISORS",
                "U0JUAN",
                "Not taken: <!here> & more",
                Some("1793000000.000100"),
            ),
            json!({
                "channel": "C0SUPERVISORS",
                "user": "U0JUAN",
                "text": "Not taken: &lt;!here&gt; &amp; more",
                "thread_ts": "1793000000.000100",
            })
        );
        assert_eq!(
            post_ephemeral_body("C0SUPERVISORS", "U0JUAN", "This is no longer open.", None),
            json!({"channel": "C0SUPERVISORS", "user": "U0JUAN", "text": "This is no longer open."})
        );
    }

    /// A Socket Mode envelope names what it carries: the events the node relays, which the read
    /// loop hands to the event worker, or a click on a menu (`interactive`), which it hands to
    /// slack_menu (`hand_off_envelope`).
    #[test]
    fn a_socket_envelope_names_what_it_carries() {
        let interactive: SocketEnvelope = serde_json::from_value(json!({
            "envelope_id": "e1",
            "type": "interactive",
            "accepts_response_payload": true,
            "payload": {"type": "block_actions"}
        }))
        .expect("envelope");
        assert_eq!(interactive.kind, ENVELOPE_INTERACTIVE);
        assert_eq!(interactive.payload["type"], "block_actions");
        let event: SocketEnvelope = serde_json::from_value(json!({
            "envelope_id": "e2",
            "type": "events_api",
            "payload": {"type": "event_callback"}
        }))
        .expect("envelope");
        assert_eq!(event.kind, ENVELOPE_EVENTS_API);
        let bare: SocketEnvelope =
            serde_json::from_value(json!({"envelope_id": "e3"})).expect("envelope");
        assert_eq!(bare.kind, "");
    }

    /// A reply also sent to the channel (`thread_broadcast`) and a message that shares files
    /// (`file_share`) are a person's plain messages, with the checks a plain message gets: the
    /// node's own, by its `bot_id` or by its author, is never relayed, and none is while the node
    /// cannot tell its own. Every other subtype stays dropped.
    #[test]
    fn a_reply_sent_to_the_channel_and_a_file_share_are_plain_messages() {
        let me = "U0BOT";
        for subtype in ["thread_broadcast", "file_share"] {
            let person = json!({"type": "message", "subtype": subtype, "user": "U0ANA"});
            assert_eq!(
                classify_slack_inbound(&person, "message", Some(me)),
                Ok(()),
                "{subtype}"
            );
            assert!(
                classify_slack_inbound(&person, "message", None).is_err(),
                "{subtype}"
            );
            let own = json!({"type": "message", "subtype": subtype, "user": me});
            assert!(
                classify_slack_inbound(&own, "message", Some(me)).is_err(),
                "{subtype}"
            );
            let a_bots = json!({"type": "message", "subtype": subtype, "user": "U0ANA",
                                "bot_id": "B0BOT"});
            assert!(
                classify_slack_inbound(&a_bots, "message", Some(me)).is_err(),
                "{subtype}"
            );
        }
        for subtype in [
            "bot_message",
            "message_changed",
            "message_deleted",
            "channel_join",
        ] {
            let event = json!({"type": "message", "subtype": subtype, "user": "U0ANA"});
            assert!(
                classify_slack_inbound(&event, "message", Some(me)).is_err(),
                "{subtype}"
            );
        }
    }

    /// One message that mentions the bot may come as an `app_mention` and as a `message`, each with
    /// an event id of its own: the second is its message again, by its channel and `ts`. Another
    /// `ts`, or the same `ts` in another channel, is another message. The memory is bounded: past
    /// its entries the oldest go. An event with no `ts` is never taken for another's.
    #[test]
    fn a_message_that_comes_as_two_events_is_seen_once() {
        let mut seen = DedupCache::new(Duration::from_secs(60), 2);
        let mention = json!({"type": "app_mention", "user": "U0ANA", "text": "<@U0BOT> hello",
                             "ts": POST_TS});
        let message = json!({"type": "message", "user": "U0ANA", "text": "<@U0BOT> hello",
                             "ts": POST_TS});
        assert!(!message_seen(&mut seen, CHANNEL, &mention));
        assert!(message_seen(&mut seen, CHANNEL, &message));
        assert!(!message_seen(&mut seen, "C0OTHER", &message));
        let next = json!({"type": "message", "user": "U0ANA", "ts": "1793000000.000200"});
        assert!(!message_seen(&mut seen, CHANNEL, &next));
        assert!(
            !message_seen(&mut seen, CHANNEL, &mention),
            "two entries at most: the first went"
        );
        for no_ts in [
            json!({"type": "message"}),
            json!({"type": "message", "ts": " "}),
        ] {
            assert!(!message_seen(&mut seen, CHANNEL, &no_ts));
            assert!(!message_seen(&mut seen, CHANNEL, &no_ts));
        }
    }

    /// The bot token goes only to Slack's file store: a file shared from elsewhere names its own
    /// host, and its download is refused before any request, its token never read (the review of
    /// WF Case 5: since a file share in the channel reaches the mesh, anyone there could share one).
    #[tokio::test]
    async fn the_bot_token_goes_only_to_slacks_file_store() {
        let store = "https://files.slack.com";
        for slacks in [
            "https://files.slack.com/files-pri/T0-F0/download/invoice.pdf",
            "https://files.slack.com:443/files-pri/T0-F0/invoice.pdf",
        ] {
            assert!(same_origin(slacks, store), "{slacks}");
        }
        for foreign in [
            "http://files.slack.com/files-pri/T0-F0/invoice.pdf",
            "https://files.slack.com:8443/files-pri/T0-F0/invoice.pdf",
            "https://files.slack.com.example.com/invoice.pdf",
            "https://files.slack.com@example.com/invoice.pdf",
            "https://drive.google.com/file/d/F0/view",
            "https://example.com/?files.slack.com",
            "not a url",
            "",
        ] {
            assert!(!same_origin(foreign, store), "{foreign}");
        }
        let slack = SlackClients::new().expect("client");
        let refused = slack
            .download_private_file("https://drive.google.com/file/d/F0/view", 1024)
            .await
            .err()
            .expect("a file elsewhere is refused");
        assert!(
            refused.to_string().contains("not on Slack's file store"),
            "{refused}"
        );
    }

    /// A-85: a shared file is read only within its share: one announced bigger is not read.
    #[tokio::test]
    async fn a_shared_file_is_downloaded_within_its_share() {
        let body = json!("x".repeat(2000));
        let api = FakeWebApi::start(&[("F0BIG", body.clone())]).await;
        let slack = api.client().await;
        let url = api.url("files/F0BIG");
        let expected = body.to_string().into_bytes();
        match slack.download_private_file(&url, 1000).await {
            Ok(Download::TooLarge(size)) => assert_eq!(size, expected.len() as u64),
            Ok(Download::Bytes(_)) => panic!("a file over its share must not be read"),
            Err(err) => panic!("download: {err}"),
        }
        match slack.download_private_file(&url, 5000).await {
            Ok(Download::Bytes(bytes)) => assert_eq!(bytes, expected),
            Ok(Download::TooLarge(size)) => panic!("a file within its share refused at {size}"),
            Err(err) => panic!("download: {err}"),
        }
    }

    /// A person's messages reach the mesh as the plain messages they are, once each (FINDINGS
    /// A-130): a reply also sent to the channel, on its Slack thread's id; a message that shares a
    /// file, with the file; and a message that mentions the bot, which Slack sends as an
    /// `app_mention` and as a `message`. The node's own file shares, an edit and another channel's
    /// message never do, and the node's own files are never downloaded.
    #[tokio::test]
    async fn a_persons_messages_reach_the_mesh_once_each_and_the_nodes_own_never() {
        let api =
            FakeWebApi::start(&[("auth.test", json!({"ok": true, "user_id": "U0BOT"}))]).await;
        let store = blob_store("events");
        let invoice = json!([{
            "id": "F0INVOICE",
            "name": "invoice.pdf",
            "mimetype": "application/pdf",
            "url_private_download": api.url("files/F0INVOICE")
        }]);
        let own_file = json!([{
            "id": "F0OWN",
            "name": "quote.pdf",
            "mimetype": "application/pdf",
            "url_private_download": api.url("files/F0OWN")
        }]);
        let in_channel = |event: Value| {
            let mut event = event;
            event["channel"] = json!(CHANNEL);
            event
        };
        let mut router = run_events(
            &api,
            &store,
            vec![
                event_callback(
                    "Ev01",
                    in_channel(json!({"type": "message", "subtype": "thread_broadcast",
                        "user": "U0ANA", "text": "For everyone too", "ts": "1793000000.000200",
                        "thread_ts": POST_TS})),
                ),
                event_callback(
                    "Ev02",
                    in_channel(json!({"type": "message", "subtype": "file_share",
                        "user": "U0ANA", "text": "The invoice", "ts": "1793000000.000300",
                        "files": invoice})),
                ),
                event_callback(
                    "Ev03",
                    in_channel(json!({"type": "app_mention", "user": "U0ANA",
                        "text": "<@U0BOT> hello", "ts": "1793000000.000400"})),
                ),
                event_callback(
                    "Ev04",
                    in_channel(json!({"type": "message", "user": "U0ANA",
                        "text": "<@U0BOT> hello", "ts": "1793000000.000400"})),
                ),
                event_callback(
                    "Ev05",
                    in_channel(json!({"type": "message", "subtype": "file_share",
                        "user": "U0BOT", "bot_id": "B0BOT", "text": "", "ts": "1793000000.000500",
                        "files": own_file.clone()})),
                ),
                event_callback(
                    "Ev06",
                    in_channel(json!({"type": "message", "subtype": "file_share",
                        "user": "U0BOT", "text": "", "ts": "1793000000.000600",
                        "files": own_file})),
                ),
                event_callback(
                    "Ev07",
                    in_channel(json!({"type": "message", "subtype": "message_changed",
                        "user": "U0ANA", "ts": "1793000000.000700"})),
                ),
                event_callback(
                    "Ev08",
                    json!({"type": "message", "user": "U0ANA", "text": "elsewhere",
                        "ts": "1793000000.000800", "channel": "C0OTHER"}),
                ),
            ],
        )
        .await;

        let messages = relayed(&mut router).await;
        let ids = messages
            .iter()
            .map(|msg| msg.meta.context.as_ref().expect("context")["io"]["message"]["id"].clone())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![json!("Ev01"), json!("Ev02"), json!("Ev03")]);
        for msg in &messages {
            assert!(
                matches!(&msg.routing.dst, Destination::Unicast(node) if node == WF),
                "{:?}",
                msg.routing.dst
            );
        }

        let broadcast = &messages[0];
        assert_eq!(broadcast.payload["content"], "For everyone too");
        let thread = slack_inbound_io_context(WORKSPACE, "U0ANA", CHANNEL, Some(POST_TS), "Ev01")
            .conversation
            .thread_id;
        assert_eq!(broadcast.meta.thread_id, thread);

        let shared = &messages[1];
        assert_eq!(shared.payload["content"], "The invoice");
        let attachments = shared.payload["attachments"].as_array().expect("a file");
        assert_eq!(attachments.len(), 1);
        let file: BlobRef = serde_json::from_value(attachments[0].clone()).expect("a blob ref");
        assert_eq!(file.filename_original, "invoice.pdf");
        assert_eq!(file.mime, "application/pdf");
        // The fake's download answers its own `{"ok":true}`.
        assert_eq!(
            store.blobs.read_blob(&file).expect("the file's blob"),
            br#"{"ok":true}"#
        );

        assert_eq!(messages[2].payload["content"], "hello");
        let downloads = |id: &str| api.calls().iter().filter(|(api, _)| api == id).count();
        assert_eq!(downloads("F0INVOICE"), 1);
        assert_eq!(downloads("F0OWN"), 0);
    }

    /// A message with no text, no blocks and no files posts nothing, whatever its payload's shape,
    /// where it posted a fallback before (FINDINGS A-130). A WF's facts alone make no post; its
    /// menu alone does, as buttons.
    #[tokio::test]
    async fn a_message_with_nothing_to_post_posts_nothing() {
        let api = FakeWebApi::start(&[]).await;
        let slack = api.client().await;
        let records = SlackThreadRecords::in_memory().expect("records");
        let store = blob_store("nothing");
        for payload in [
            json!({"type": "text", "content": ""}),
            json!({"type": "text", "content": " \n ", "attachments": [], "blocks": []}),
            json!({"type": "text"}),
            json!({}),
            json!(null),
        ] {
            let plain = outbound(AGENT, CASE_THREAD, json!({}), payload.clone());
            post(&slack, &store, Some(&records), &plain).await;
            let facts = outbound(
                WF,
                CASE_THREAD,
                json!({"wf": {"vars": {"person": "Ana"}}}),
                payload,
            );
            post(&slack, &store, Some(&records), &facts).await;
        }
        assert_eq!(api.calls(), vec![]);

        let menu = json!({"wf": {"menu": {"options": [
            {"name": "handback", "description": "Hand the conversation back."}
        ]}}});
        post(
            &slack,
            &store,
            Some(&records),
            &outbound(WF, CASE_THREAD, menu, json!(null)),
        )
        .await;
        let posts = api.bodies("chat.postMessage");
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0]["text"], "");
        assert_eq!(posts[0]["blocks"][0]["block_id"], "fluxbee_wf_menu");
    }

    /// A message with only files, on a thread id with no place, posts their names first: its `ts`
    /// is recorded for the thread id, and the files go into its thread (FINDINGS A-130). A reply
    /// there keeps the thread id, and so does the WF's next message on the subject. On a place the
    /// node knows, the files go there, with no names; without the records, as before them.
    #[tokio::test]
    async fn a_message_with_only_files_on_a_new_place_names_them_first() {
        let api =
            FakeWebApi::start(&[("chat.postMessage", json!({"ok": true, "ts": POST_TS}))]).await;
        let slack = api.client().await;
        let records = SlackThreadRecords::in_memory().expect("records");
        let store = blob_store("files-only");
        let report = store.put(b"%PDF-1.7", "report.pdf", "application/pdf");
        let chart = store.put(b"\x89PNG", "chart.png", "image/png");
        let files_only = json!({"type": "text", "content": "", "attachments": [report, chart]});
        post(
            &slack,
            &store,
            Some(&records),
            &outbound(AGENT, CASE_THREAD, json!({}), files_only.clone()),
        )
        .await;

        let shared_in = |thread_ts: Option<&str>, title: &str| {
            let mut body = json!({"files": [{"id": "F0FILE", "title": title}],
                                  "channel_id": CHANNEL});
            if let Some(thread_ts) = thread_ts {
                body["thread_ts"] = json!(thread_ts);
            }
            body
        };
        assert_eq!(
            api.bodies("chat.postMessage"),
            vec![json!({"channel": CHANNEL, "text": "Files: report.pdf, chart.png"})]
        );
        assert_eq!(
            api.bodies("files.completeUploadExternal"),
            vec![
                shared_in(Some(POST_TS), "report.pdf"),
                shared_in(Some(POST_TS), "chart.png")
            ]
        );
        let mut reply =
            slack_inbound_io_context(WORKSPACE, "U0SUPERVISOR", CHANNEL, Some(POST_TS), "Ev10");
        map_inbound_thread(Some(&records), &mut reply, Some(POST_TS), now_epoch_ms());
        assert_eq!(reply.conversation.thread_id.as_deref(), Some(CASE_THREAD));
        let same = outbound(
            WF,
            CASE_THREAD,
            json!({"wf": {"subject": "same"}}),
            json!({"type": "text", "content": "And the total."}),
        );
        post(&slack, &store, Some(&records), &same).await;
        assert_eq!(
            api.bodies("chat.postMessage")[1],
            json!({"channel": CHANNEL, "text": "And the total.", "thread_ts": POST_TS})
        );

        // A place a person opened: the files go there, with no names.
        let person_ts = "1793000000.000900";
        let mut person =
            slack_inbound_io_context(WORKSPACE, "U0ANA", CHANNEL, Some(person_ts), "Ev11");
        map_inbound_thread(Some(&records), &mut person, Some(person_ts), now_epoch_ms());
        let person_thread = person.conversation.thread_id.expect("a thread id");
        let one_file = json!({"type": "text", "content": "", "attachments": [report]});
        post(
            &slack,
            &store,
            Some(&records),
            &outbound(AGENT, &person_thread, json!({}), one_file.clone()),
        )
        .await;
        // Without the records, as before them: at the top level, with no names.
        post(
            &slack,
            &store,
            None,
            &outbound(AGENT, CASE_THREAD, json!({}), one_file),
        )
        .await;
        assert_eq!(api.bodies("chat.postMessage").len(), 2);
        assert_eq!(
            api.bodies("files.completeUploadExternal")[2..].to_vec(),
            vec![
                shared_in(Some(person_ts), "report.pdf"),
                shared_in(None, "report.pdf")
            ]
        );
    }

    /// A file the node cannot send no longer fails the message (FINDINGS A-108): an HTML page
    /// goes; a type the node does not take and a blob that never came are left out, and the text
    /// names them; the other files go. A file Slack refuses, once the text went, is named by a post
    /// after the files.
    #[tokio::test]
    async fn a_file_left_out_is_named_and_the_rest_goes() {
        let api = FakeWebApi::start(&[
            ("chat.postMessage", json!({"ok": true, "ts": POST_TS})),
            ("files.completeUploadExternal", json!({"ok": true})),
            (
                "files.completeUploadExternal",
                json!({"ok": false, "error": "file_uploads_except_images_disabled"}),
            ),
        ])
        .await;
        let slack = api.client().await;
        let records = SlackThreadRecords::in_memory().expect("records");
        let store = blob_store("left-out");
        let page = store.put(b"<p>The quote</p>", "quote.html", "text/html");
        let setup = store.put(b"MZ", "setup.exe", "application/x-msdownload");
        let gone = store.never_comes(b"%PDF-1.7", "terms.pdf", "application/pdf");
        let totals = store.put(b"plan,total\npro,1200", "totals.csv", "text/csv");
        let payload = json!({"type": "text", "content": "Here is the quote.\n",
                             "attachments": [page, setup, gone, totals]});
        post(
            &slack,
            &store,
            Some(&records),
            &outbound(AGENT, CASE_THREAD, json!({}), payload),
        )
        .await;

        let calls = api.calls();
        let order = calls
            .iter()
            .map(|(api, _)| api.as_str())
            .filter(|api| !api.starts_with("files.getUploadURL") && *api != "upload")
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            vec![
                "chat.postMessage",
                "files.completeUploadExternal",
                "files.completeUploadExternal",
                "chat.postMessage"
            ]
        );
        let posts = api.bodies("chat.postMessage");
        assert_eq!(
            posts[0],
            json!({"channel": CHANNEL,
                   "text": "Here is the quote.\n\nNot sent: setup.exe, terms.pdf"})
        );
        let shared = api.bodies("files.completeUploadExternal");
        assert_eq!(shared[0]["files"][0]["title"], "quote.html");
        assert_eq!(shared[1]["files"][0]["title"], "totals.csv");
        assert_eq!(shared[1]["thread_ts"], POST_TS);
        assert_eq!(
            posts[1],
            json!({"channel": CHANNEL, "text": "Not sent: totals.csv", "thread_ts": POST_TS})
        );
    }

    /// Each file is checked alone: past the first `max_attachments`, past a file's size or all of
    /// theirs, or of a type the node does not take, a file is left out with why, and the others
    /// are read. A payload that is no `text/v1` fails the whole.
    #[tokio::test]
    async fn each_outbound_file_is_checked_alone() {
        let store = blob_store("limits");
        let cfg = IoTextBlobConfig {
            max_attachments: 5,
            max_attachment_bytes: 8,
            max_total_attachment_bytes: 12,
            ..slack_files()
        };
        let attachments = [
            store.put(b"12345678", "a.txt", "text/plain"),
            store.put(b"123456789", "big.txt", "text/plain"),
            store.put(b"PK", "archive.zip", "application/zip"),
            store.put(b"12345", "b.txt", "text/plain"),
            store.put(b"1234", "c.txt", "text/plain"),
            store.put(b"1", "sixth.txt", "text/plain"),
        ];
        let payload = json!({"type": "text", "content": "Hi.", "attachments": attachments});
        let content = outbound_content(&store.blobs, &cfg, short_wait(), &payload)
            .await
            .expect("content");
        assert_eq!(content.text, "Hi.");
        assert_eq!(
            content
                .files
                .iter()
                .map(|file| (file.name.as_str(), file.bytes.as_slice()))
                .collect::<Vec<_>>(),
            vec![
                ("a.txt", b"12345678".as_slice()),
                ("c.txt", b"1234".as_slice())
            ]
        );
        assert_eq!(
            content
                .left_out
                .iter()
                .map(|(name, why)| (name.as_str(), why.canonical_code()))
                .collect::<Vec<_>>(),
            vec![
                ("big.txt", "BLOB_TOO_LARGE"),
                ("archive.zip", "unsupported_attachment_mime"),
                ("b.txt", "BLOB_TOO_LARGE"),
                ("sixth.txt", "too_many_attachments"),
            ]
        );
        assert!(outbound_content(
            &store.blobs,
            &cfg,
            short_wait(),
            &json!({"type": "card", "content": "Hi."})
        )
        .await
        .is_err());
    }
}
