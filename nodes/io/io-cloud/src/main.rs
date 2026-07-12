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
        env_or(
            "IO_CLOUD_TENANT_ID",
            "tnt:00000000-0000-0000-0000-000000000001",
        )
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
                "IO.cloud own channel ICH enabled — ready to externalize on SY.edge"
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
    // channel and waits. SY.admin authorizes this by router-stamped IO.* origin plus
    // `requester == ICH owner`.
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
        tokio::spawn(publish_channel_on_edge_with_retry(
            dispatcher.clone(),
            admin_target.clone(),
            edge_node,
            params,
        ));
    } else {
        tracing::info!(
            "IO.cloud not externalizing (set IO_CLOUD_EDGE_NODE to publish); handling inbound only"
        );
    }

    let mut incoming = dispatcher.take_command_receiver(RPC_CH_INCOMING).await?;
    run_loop(&sender, &full_name, &dispatcher, &admin_target, &mut incoming).await
}

/// Keep IO.cloud running while the ingress hive is still converging. A boot race where
/// SY.admin is ready before SY.edge should not require manually restarting IO.cloud.
async fn publish_channel_on_edge_with_retry(
    dispatcher: Arc<RouterDispatcher>,
    admin_target: String,
    edge_node: String,
    params: serde_json::Value,
) {
    let max_attempts: u64 = env("IO_CLOUD_EXTERNALIZE_ATTEMPTS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let retry_delay_secs: u64 = env("IO_CLOUD_EXTERNALIZE_RETRY_SECONDS")
        .and_then(|v| v.parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(5);
    let mut attempt = 0_u64;

    loop {
        attempt = attempt.saturating_add(1);
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
                    attempts = attempt,
                    "IO.cloud -> SY.admin externalize OK (public URL published on the edge)"
                );
                return;
            }
            // The RPC round-tripped but admin refused. Authz/schema errors are terminal; edge
            // reachability races keep retrying.
            Ok(res) => {
                let detail = res
                    .error_detail
                    .as_ref()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| res.payload.to_string());
                let code = res.error_code.clone().unwrap_or_default();
                if !externalize_rejection_is_retryable(&code, &detail)
                    || externalize_attempts_exhausted(attempt, max_attempts)
                {
                    tracing::warn!(
                        target = %admin_target,
                        edge_node = %edge_node,
                        status = %res.status,
                        error_code = ?res.error_code,
                        error_detail = ?res.error_detail,
                        attempts = attempt,
                        "IO.cloud -> SY.admin externalize REJECTED"
                    );
                    return;
                }
                tracing::warn!(
                    target = %admin_target,
                    edge_node = %edge_node,
                    status = %res.status,
                    error_code = ?res.error_code,
                    error_detail = ?res.error_detail,
                    attempts = attempt,
                    "IO.cloud -> SY.admin externalize rejected by a retryable edge condition; retrying"
                );
            }
            Err(err) => {
                if externalize_attempts_exhausted(attempt, max_attempts) {
                    tracing::warn!(
                        target = %admin_target,
                        edge_node = %edge_node,
                        error = %err,
                        attempts = attempt,
                        "IO.cloud -> SY.admin externalize failed (transport); retries exhausted"
                    );
                    return;
                }
                tracing::warn!(
                    target = %admin_target,
                    edge_node = %edge_node,
                    error = %err,
                    attempts = attempt,
                    "IO.cloud -> SY.admin externalize failed (transport); retrying"
                );
            }
        }

        tokio::time::sleep(Duration::from_secs(retry_delay_secs)).await;
    }
}

fn externalize_attempts_exhausted(attempt: u64, max_attempts: u64) -> bool {
    max_attempts != 0 && attempt >= max_attempts
}

fn externalize_rejection_is_retryable(error_code: &str, detail: &str) -> bool {
    let combined = format!("{error_code} {detail}").to_ascii_lowercase();
    combined.contains("edge unreachable")
        || combined.contains("open_url failed")
        || combined.contains("node_not_found")
        || combined.contains("timed out")
        || combined.contains("timeout")
        || combined.contains("unreachable")
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
    tracing::info!(
        ich_id = %result.ich_id,
        owner_l2_name = ?result.owner_l2_name,
        enabled = result.enabled,
        "IO.cloud own ICH ensured"
    );
    Ok(result.ich_id)
}

async fn run_loop(
    sender: &NodeSender,
    full_name: &str,
    dispatcher: &Arc<RouterDispatcher>,
    admin_target: &str,
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

        // A request the edge forwarded under our channel. `meta.ich` = which channel (§4/§7.5).
        // IO.cloud is the internal Fluxbee Cloud relay (spec §3): the body is `{op, tenant_id,
        // params}` and we translate it into the matching ADMIN_COMMAND(s), returning the result.
        let ich = msg.meta.ich.clone().unwrap_or_default();
        let op = msg
            .payload
            .get("op")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        tracing::info!(
            trace_id = %msg.routing.trace_id,
            ich = %ich,
            op = %op,
            routed_from = %msg.routing.src,
            "IO.cloud received a Cloud request"
        );

        let mut response = dispatch_cloud_op(dispatcher, admin_target, &msg.payload).await;
        if let Some(obj) = response.as_object_mut() {
            obj.insert("handled_by".to_string(), json!(full_name));
            obj.insert("ich".to_string(), json!(ich));
        }

        let reply = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(msg.routing.src.clone()),
                ttl: 16,
                trace_id: msg.routing.trace_id.clone(),
            },
            meta: Meta {
                // reply in the same family, carry the channel back
                msg_type: msg.meta.msg_type.clone(),
                ich: msg.meta.ich.clone(),
                ..Meta::default()
            },
            payload: response,
        };
        sender.send(reply).await?;
    }
}

/// Error response payload for a rejected/malformed Cloud request.
fn cloud_error(detail: &str) -> serde_json::Value {
    json!({ "status": "error", "error_detail": detail })
}

/// Pure translation of a Cloud op into the admin `(action, params)`, injecting the tenant claim.
/// Returns `Err(error_payload)` for a missing/unknown op or a missing tenant. Kept pure (no admin
/// call) so it is unit-testable; the ops mirror IO_CLOUD_EXPOSED_ACTIONS (admin's single source).
fn translate_cloud_op(
    op: &str,
    tenant_id: Option<&str>,
    params: &serde_json::Value,
) -> Result<(&'static str, serde_json::Value), serde_json::Value> {
    match op {
        "put_token" => {
            let tenant_id = tenant_id.ok_or_else(|| cloud_error("put_token requires tenant_id"))?;
            Ok((
                "vault_put",
                json!({
                    "key": params.get("key"),
                    "value": params.get("value"),
                    "metadata": {
                        "resource_type": params.get("resource_type"),
                        "owner_node": params.get("owner_node"),
                        "tenant_id": tenant_id,
                    },
                }),
            ))
        }
        "provision_node" => {
            let tenant_id =
                tenant_id.ok_or_else(|| cloud_error("provision_node requires tenant_id"))?;
            let mut p = json!({
                "node_name": params.get("node_name"),
                "tenant_id": tenant_id,
            });
            if let Some(rt) = params.get("runtime") {
                p["runtime"] = rt.clone();
            }
            Ok(("run_node", p))
        }
        "" => Err(cloud_error("missing 'op'")),
        other => Err(cloud_error(&format!("unknown op '{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_put_token_injects_tenant_and_builds_vault_put() {
        let params = json!({
            "key": "wapp_token:acme",
            "value": {"token": "abc"},
            "resource_type": "bearer_token",
            "owner_node": "IO.wapp@motherbee"
        });
        let (action, p) = translate_cloud_op("put_token", Some("tnt:acme"), &params).unwrap();
        assert_eq!(action, "vault_put");
        assert_eq!(p["key"], "wapp_token:acme");
        assert_eq!(p["value"]["token"], "abc");
        assert_eq!(p["metadata"]["resource_type"], "bearer_token");
        assert_eq!(p["metadata"]["owner_node"], "IO.wapp@motherbee");
        assert_eq!(p["metadata"]["tenant_id"], "tnt:acme"); // injected from the claim
    }

    #[test]
    fn translate_provision_node_requires_tenant_and_passes_runtime() {
        let params = json!({"node_name": "IO.wapp@motherbee", "runtime": "io.generic"});
        let (action, p) = translate_cloud_op("provision_node", Some("tnt:acme"), &params).unwrap();
        assert_eq!(action, "run_node");
        assert_eq!(p["node_name"], "IO.wapp@motherbee");
        assert_eq!(p["tenant_id"], "tnt:acme");
        assert_eq!(p["runtime"], "io.generic");
    }

    #[test]
    fn translate_rejects_missing_tenant_unknown_and_empty_op() {
        assert!(translate_cloud_op("put_token", None, &json!({})).is_err());
        assert!(translate_cloud_op("provision_node", None, &json!({})).is_err());
        assert!(translate_cloud_op("", Some("tnt:x"), &json!({})).is_err());
        let err = translate_cloud_op("frobnicate", Some("tnt:x"), &json!({})).unwrap_err();
        assert_eq!(err["status"], "error");
    }
}

/// Translate a Cloud request (`{op, tenant_id, params}`) into the matching ADMIN_COMMAND and
/// return the response payload. IO.cloud is the internal Fluxbee Cloud relay (spec §3): it injects
/// the (trusted for the MVP — §1.2) tenant claim into each admin call and relays over the same
/// `send_admin_rpc` seam it already uses for externalize. The ops mirror IO_CLOUD_EXPOSED_ACTIONS
/// (SY.admin's single source):
///   put_token       -> vault_put   (store a provider token owned by the target IO node)
///   provision_node  -> run_node    (spawn IO.<provider>@<tenant>; requires tenant_id)
/// For an owner-scoped token the caller must provision_node BEFORE put_token (Caveat B: the
/// `owner_node -> ilk` resolution needs the target node registered in identity SHM first).
async fn dispatch_cloud_op(
    dispatcher: &Arc<RouterDispatcher>,
    admin_target: &str,
    request: &serde_json::Value,
) -> serde_json::Value {
    let op = request.get("op").and_then(|v| v.as_str()).unwrap_or("");
    let tenant_id = request.get("tenant_id").and_then(|v| v.as_str());
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    // The hive the admin action targets (MVP: the admin's own hive, e.g. motherbee).
    let hive = admin_target.split_once('@').map(|(_, h)| h.to_string());

    let (action, admin_params) = match translate_cloud_op(op, tenant_id, &params) {
        Ok(pair) => pair,
        Err(error_payload) => return error_payload,
    };

    match dispatcher
        .send_admin_rpc(AdminCommandRequest {
            admin_target,
            action,
            target: hive.as_deref(),
            params: admin_params,
            request_id: None,
            timeout: Duration::from_secs(20),
        })
        .await
    {
        Ok(res) if res.status.eq_ignore_ascii_case("ok") => {
            json!({ "status": "ok", "op": op, "result": res.payload })
        }
        Ok(res) => json!({
            "status": "error",
            "op": op,
            "error_code": res.error_code,
            "error_detail": res.error_detail.unwrap_or(res.payload),
        }),
        Err(e) => cloud_error(&format!("admin call failed: {e}")),
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
