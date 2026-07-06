#![forbid(unsafe_code)]

//! `IO.cloud` — the in-mesh adapter for the external system that is Fluxbee Cloud
//! (same species as `IO.linkedhelper`/`io-api`). It is the FIRST node to externalize
//! a channel on `SY.edge` (spec `edge-ingress-spec-v6.md` §10): it lives inside the
//! mesh, talks to `SY.admin` for scoped commands (its `externalize`/`unexternalize`
//! set), and is the only inbound path from Fluxbee Cloud into the mesh (I2).
//!
//! This first cut is deliberately lean and grows into the real thing:
//! - it CONNECTS to the mesh as `IO.cloud`;
//! - it proves the node → `SY.admin` capability with a scoped read (`list_admin_actions`)
//!   — the same channel it will later use to `externalize` its own channel;
//! - it HANDLES requests forwarded from the edge under its declared family, reading
//!   `meta.ich` (the channel the request is for) and echoing — so it is a valid edge
//!   handler for the end-to-end test. A real `IO.cloud` dispatches by `meta.ich` to the
//!   matching Fluxbee Cloud tenant/conversation.
//!
//! Env: `IO_CLOUD_NODE_NAME` (default `IO.cloud`), `IO_CLOUD_ROUTER_SOCKET_DIR`,
//! `IO_CLOUD_UUID_PERSISTENCE_DIR`, `IO_CLOUD_CONFIG_DIR`, `IO_CLOUD_NODE_VERSION`,
//! `IO_CLOUD_ADMIN_HIVE` (default: the edge's own hive).

use std::path::PathBuf;
use std::time::Duration;

use std::sync::Arc;

use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing};
use fluxbee_sdk::rpc::AdminCommandRequest;
use fluxbee_sdk::{
    managed_node_name, try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode,
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, RpcCommandReceiver,
};
use io_common::identity::ResolveOrCreateInput;
use io_common::provision::{ensure_own_ich, strict_provision_ilk, IdentityProvisionConfig};
use serde_json::json;
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;

type DynError = Box<dyn std::error::Error + Send + Sync>;

const RPC_CH_INCOMING: &str = "incoming";

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or(
            "JSR_LOG_LEVEL",
            "info,io_cloud=debug,fluxbee_sdk=info",
        )))
        .init();

    let cfg = build_node_config();
    tracing::info!(node = %cfg.name, "IO.cloud starting (in-mesh Fluxbee Cloud adapter)");

    // Broad catch-all: unsolicited traffic (edge-forwarded requests under any family,
    // plus system messages) lands on `incoming`. Admin responses to our own outbound
    // RPCs are correlated by trace_id in the pending map first, so they never leak here.
    let profile = OperationalRouteProfile::builder()
        .command_channel(RPC_CH_INCOMING)
        .post_pending_rule(RouteMatch::Any, RouteTarget::Command(RPC_CH_INCOMING))
        .build()?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(cfg, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();
    let full_name = sender.full_name().to_string();
    let hive_id = full_name
        .rsplit_once('@')
        .map(|(_, hive)| hive.to_string())
        .unwrap_or_else(|| "motherbee".to_string());
    tracing::info!(full_name = %full_name, "IO.cloud connected to router");

    // Register IO.cloud's own channel (ICH) in SY.identity so it becomes the durable,
    // externalize-able identity of the public URL (spec §4: the URL *is* an ICH; §7:
    // externalize resolves `ICH -> owner_l2_name`). The ICH is owned by us because the
    // identity handler stamps `owner_l2_name` from the router-verified `src_l2_name`
    // (sy_identity.rs add_channel), so only a request that genuinely came from IO.cloud
    // can register a channel owned by IO.cloud. `ILK_ADD_CHANNEL`/`ILK_PROVISION` are
    // both authorized for the `IO.` prefix (sy_identity.rs allowed_prefixes).
    let identity_hive = env_or("IO_CLOUD_IDENTITY_HIVE", &hive_id);
    let identity_target = format!("SY.identity@{identity_hive}");
    let self_tenant = fluxbee_sdk::read_self_tenant_from_env().unwrap_or_else(|| {
        env_or("IO_CLOUD_TENANT_ID", "tnt:00000000-0000-0000-0000-000000000001")
    });
    let channel_type = env_or("IO_CLOUD_CHANNEL_TYPE", "cloud");
    let channel_address = env_or("IO_CLOUD_CHANNEL_ADDRESS", "demo");
    // As an enabled boot unit, IO.cloud may come up before the mesh (SY.identity/SY.admin)
    // is fully ready. Retry the channel registration; if it can't land, exit non-zero so
    // systemd restarts us for a fresh attempt (a singleton with no ICH is useless anyway).
    let own_ich = match ensure_own_channel_with_retry(
        &dispatcher,
        &identity_target,
        &self_tenant,
        &channel_type,
        &channel_address,
    )
    .await
    {
        Ok(ich_id) => {
            tracing::info!(
                ich_id = %ich_id,
                tenant = %self_tenant,
                channel_type = %channel_type,
                address = %channel_address,
                "IO.cloud own channel ICH ready — externalize this ich on SY.edge to publish it"
            );
            ich_id
        }
        Err(err) => {
            tracing::error!(error = %err, "IO.cloud channel registration exhausted retries; exiting for systemd to restart");
            return Err(err.into());
        }
    };

    // Self-externalize (spec §7): IO.cloud asks SY.admin to publish its own channel as a
    // public URL on the edge. This is the same node→admin ADMIN_COMMAND path a real deploy
    // uses; it is self-service (requester owns the ICH). Opt-in via `IO_CLOUD_EDGE_NODE`
    // (the `SY.edge` L2 name to publish on) so an unconfigured IO.cloud just registers its
    // channel and waits. NOTE: the `externalize` authz gate is intentionally still open
    // (spec §11.1 — PENDING); admin accepts this without an origin/owner check for now.
    let admin_hive = env_or("IO_CLOUD_ADMIN_HIVE", &hive_id);
    let admin_target = format!("SY.admin@{admin_hive}");
    if let Some(edge_node) = env("IO_CLOUD_EDGE_NODE") {
        let inbound_family = env_or("IO_CLOUD_INBOUND_FAMILY", "user");
        let auth_mode = env_or("IO_CLOUD_AUTH_MODE", "public");
        // For a shared-secret channel, the caller supplies the secret; admin stores it in vault
        // owned by the edge and ships only a secret_ref (§8). Opt-in via IO_CLOUD_SECRET.
        let mut params = json!({
            "ich": own_ich,
            "edge_node": edge_node,
            "inbound_family": inbound_family,
            "auth_mode": auth_mode,
        });
        if let Some(secret) = env("IO_CLOUD_SECRET") {
            params["secret"] = json!(secret);
        }
        match dispatcher
            .send_admin_rpc(AdminCommandRequest {
                admin_target: &admin_target,
                action: "externalize",
                target: None,
                params: params.clone(),
                request_id: None,
                timeout: Duration::from_secs(15),
            })
            .await
        {
            Ok(res) if res.status.eq_ignore_ascii_case("ok") => {
                // For a shared-secret channel admin returns the entry token (§8); a real IO node
                // hands it to its external clients as `Authorization: Bearer <token>`.
                let token = res.payload.get("token").and_then(|v| v.as_str());
                tracing::info!(
                    target = %admin_target,
                    edge_node = %edge_node,
                    entry_token = ?token,
                    result = %res.payload,
                    "IO.cloud -> SY.admin externalize OK (public URL published on the edge)"
                );
            }
            // The RPC round-tripped but admin refused (e.g. the §11.1 authz gate): status != ok.
            Ok(res) => tracing::warn!(
                target = %admin_target,
                edge_node = %edge_node,
                status = %res.status,
                error_code = ?res.error_code,
                error_detail = ?res.error_detail,
                "IO.cloud -> SY.admin externalize REJECTED"
            ),
            Err(err) => tracing::warn!(
                target = %admin_target,
                edge_node = %edge_node,
                error = %err,
                "IO.cloud -> SY.admin externalize failed (transport)"
            ),
        }
    } else {
        tracing::info!(
            "IO.cloud not externalizing (set IO_CLOUD_EDGE_NODE to publish); handling inbound only"
        );
    }

    let mut incoming = dispatcher.take_command_receiver(RPC_CH_INCOMING).await?;
    run_loop(&sender, &full_name, &mut incoming).await
}

/// Retry `ensure_own_channel` while the mesh is still starting (SY.identity/SY.admin not yet
/// reachable). Bounded so a genuinely broken deploy exits (systemd restarts) rather than
/// hanging forever. Tunable via `IO_CLOUD_REGISTER_ATTEMPTS` (default 30) at 2s apart.
async fn ensure_own_channel_with_retry(
    dispatcher: &Arc<RouterDispatcher>,
    identity_target: &str,
    self_tenant: &str,
    channel_type: &str,
    channel_address: &str,
) -> Result<String, String> {
    let max_attempts: u32 = env("IO_CLOUD_REGISTER_ATTEMPTS")
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(30);
    let mut last_err = String::new();
    for attempt in 1..=max_attempts {
        match ensure_own_channel(
            dispatcher,
            identity_target,
            self_tenant,
            channel_type,
            channel_address,
        )
        .await
        {
            Ok(ich) => return Ok(ich),
            Err(err) => {
                tracing::warn!(attempt, max = max_attempts, error = %err, "IO.cloud channel registration failed; mesh may still be starting — retrying");
                last_err = err;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Err(format!(
        "channel registration failed after {max_attempts} attempts: {last_err}"
    ))
}

/// Ensure IO.cloud owns a channel/ICH in SY.identity, self-provisioning its ilk when the
/// orchestrator did not inject one (`FLUXBEE_NODE_ILK_ID`). Returns the ICH id — the stable
/// identity of the public URL that `externalize` will bind to an edge endpoint.
async fn ensure_own_channel(
    dispatcher: &Arc<RouterDispatcher>,
    identity_target: &str,
    self_tenant: &str,
    channel_type: &str,
    channel_address: &str,
) -> Result<String, String> {
    let cfg = IdentityProvisionConfig {
        target: identity_target.to_string(),
        timeout: Duration::from_secs(10),
    };
    // Our own ilk: injected by the orchestrator at spawn, else self-provisioned. IO nodes
    // are authorized for ILK_PROVISION (`IO.` prefix), so an unmanaged run can still stand
    // up a real registered ilk to hang the owned channel on.
    let self_ilk = match fluxbee_sdk::read_self_ilk_from_env() {
        Some(ilk) => {
            tracing::info!(ilk = %ilk, "IO.cloud using orchestrator-provided self ilk");
            ilk
        }
        None => {
            let input = ResolveOrCreateInput {
                channel: channel_type.to_string(),
                external_id: format!("{channel_address}-owner"),
                src_ilk_override: None,
                tenant_id: Some(self_tenant.to_string()),
                tenant_hint: None,
                attributes: json!({ "source": "io.cloud", "role": "self" }),
                ilk_type: Some("agent".to_string()),
            };
            let ilk = strict_provision_ilk(dispatcher, &cfg, identity_target, &input)
                .await
                .map_err(|e| format!("self-provision ilk failed: {e}"))?;
            tracing::info!(ilk = %ilk, "IO.cloud self-provisioned its ilk");
            ilk
        }
    };
    let result = ensure_own_ich(
        dispatcher,
        &cfg,
        identity_target,
        &self_ilk,
        self_tenant,
        channel_type,
        channel_address,
    )
    .await
    .map_err(|e| format!("ensure_own_ich failed: {e}"))?;
    Ok(result.ich_id)
}

async fn run_loop(
    sender: &NodeSender,
    full_name: &str,
    incoming: &mut RpcCommandReceiver,
) -> Result<(), DynError> {
    loop {
        let msg = match timeout(Duration::from_secs(300), incoming.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => return Ok(()),
            Err(_) => continue,
        };

        if try_handle_default_node_status(sender, &msg).await? {
            continue;
        }

        // A request the edge forwarded under our channel. `meta.ich` is which channel
        // it is for (the discriminator, spec §4/§7.5); alpha echoes, a real IO.cloud
        // dispatches by ich to the matching Fluxbee Cloud tenant.
        let ich = msg.meta.ich.clone().unwrap_or_default();
        tracing::info!(
            trace_id = %msg.routing.trace_id,
            ich = %ich,
            family = %msg.meta.msg_type,
            routed_from = %msg.routing.src,
            "IO.cloud received a channel request"
        );

        let reply = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(msg.routing.src.clone()),
                ttl: 16,
                trace_id: msg.routing.trace_id.clone(),
            },
            meta: Meta {
                // reply in the same family, echo the channel back
                msg_type: msg.meta.msg_type.clone(),
                ich: msg.meta.ich.clone(),
                ..Meta::default()
            },
            payload: json!({
                "status": "ok",
                "handled_by": full_name,
                "ich": ich,
                "echo": msg.payload,
            }),
        };
        sender.send(reply).await?;
    }
}

fn build_node_config() -> NodeConfig {
    NodeConfig {
        name: managed_node_name("IO.cloud", &["IO_CLOUD_NODE_NAME"]),
        router_socket: PathBuf::from(env_or(
            "IO_CLOUD_ROUTER_SOCKET_DIR",
            "/var/run/fluxbee/routers",
        )),
        uuid_persistence_dir: PathBuf::from(env_or(
            "IO_CLOUD_UUID_PERSISTENCE_DIR",
            "/var/lib/fluxbee/state/nodes",
        )),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: PathBuf::from(env_or("IO_CLOUD_CONFIG_DIR", "/etc/fluxbee")),
        version: env_or("IO_CLOUD_NODE_VERSION", "0.1.0"),
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}
