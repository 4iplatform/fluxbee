#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_EDGE_CLOSE_URL, MSG_EDGE_CLOSE_URL_RESPONSE,
    MSG_EDGE_LIST_URLS, MSG_EDGE_LIST_URLS_RESPONSE, MSG_EDGE_OPEN_URL, MSG_EDGE_OPEN_URL_RESPONSE,
    MSG_EDGE_PUBLISH_BLOB, MSG_EDGE_PUBLISH_BLOB_RESPONSE, MSG_EDGE_UNPUBLISH_BLOB,
    is_system_kind, MSG_EDGE_UNPUBLISH_BLOB_RESPONSE, MSG_TTL_EXCEEDED, MSG_UNREACHABLE,
    MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, is_node_config_get_message, is_node_config_set_message,
    parse_node_config_request, try_handle_default_node_status, NodeConfig,
    NodeConfigControlRequest, NodeSender, NodeUuidMode, OperationalRouteProfile, PendingMatcher,
    RouteMatch, RouteTarget, RouterDispatcher, RpcError, RpcRequestLabels,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::net::TcpListener;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use http_ingress::{
    ensure_http_envelope_within_limit, HttpBodyEncoding, HttpHeader,
    HTTP_ENVELOPE_INLINE_LIMIT_BYTES,
};

type SyEdgeError = Box<dyn std::error::Error + Send + Sync>;

/// `http.req` / `http.res` is **RETIRED**. Under Option A/Z the edge forwards
/// each external request under the target's *own* declared family (`inbound_family`)
/// and never invents a request protocol — so there are no request/response payload
/// types here. What remains is the shared **envelope-size gate** (the router frame
/// limit is 128 KiB and an oversized frame tears down the node socket, so the edge
/// caps the FULL envelope at 64 KiB before send) plus the small header/body value
/// types the frontend reuses.
mod http_ingress {
    use fluxbee_sdk::protocol::Message;
    use serde::{Deserialize, Serialize};

    pub const HTTP_ENVELOPE_INLINE_LIMIT_BYTES: usize = 64 * 1024;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct HttpHeader {
        pub name: String,
        pub value: String,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum HttpBodyEncoding {
        Utf8,
        Base64,
    }

    #[derive(Debug, thiserror::Error)]
    pub enum HttpIngressError {
        #[error("json error: {0}")]
        Json(#[from] serde_json::Error),
        #[error("envelope too large: {size} bytes > {max} bytes")]
        EnvelopeTooLarge { size: usize, max: usize },
    }

    fn message_envelope_size(message: &Message) -> Result<usize, HttpIngressError> {
        Ok(serde_json::to_vec(message)?.len())
    }

    pub fn ensure_http_envelope_within_limit(message: &Message) -> Result<usize, HttpIngressError> {
        let size = message_envelope_size(message)?;
        if size > HTTP_ENVELOPE_INLINE_LIMIT_BYTES {
            return Err(HttpIngressError::EnvelopeTooLarge {
                size,
                max: HTTP_ENVELOPE_INLINE_LIMIT_BYTES,
            });
        }
        Ok(size)
    }
}

const RPC_CH_SYSTEM: &str = "system";

/// Request headers the edge forwards INWARD — a strict ALLOWLIST (fail-closed, §7.5/M2).
/// Only these named-safe headers reach the internal IO node; EVERYTHING else — the raw
/// `Authorization` bearer (already consumed at the door, §3), every hop-by-hop header, and
/// any attacker-settable header (`Cookie`, `X-Forwarded-*`, spoofed trust/identity headers) —
/// is dropped by default, so an external client can neither smuggle state nor forge trust
/// inward. New forward-worthy headers are added here deliberately, one at a time (I7).
const ALLOWED_REQUEST_HEADERS: &[&str] = &[
    "content-type",
    "accept",
    "accept-language",
    "user-agent",
    "x-request-id",
];

/// Default cap on concurrent in-flight edge requests (L1, §15.6). The edge holds one pending
/// entry per request until the inward handler replies (up to `handler_timeout_ms`); an
/// unbounded map is a DoS surface (slow/dead handler or a flood grows RAM without limit). At
/// the cap the next request is shed FAST with 503 instead of enqueued. Overridable via
/// `MAX_INFLIGHT`.
const DEFAULT_MAX_INFLIGHT: usize = 1024;
const DEFAULT_PUBLIC_MAX_INFLIGHT: usize = 128;
const DEFAULT_PUBLIC_READY_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone)]
struct Config {
    node_name: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    ttl: u8,
    handler_timeout_ms: u64,
    /// Max concurrent in-flight requests before the door sheds load with 503 (L1, §15.6).
    max_inflight: usize,
    /// Public HTTP frontend bind address. Present ONLY on the ingress-hive role
    /// (fail-closed role gate, `resolve_http_listen`). Absent ⇒ the node connects
    /// to the mesh but serves no public door.
    http_listen: Option<String>,
    /// Seed for the reverse-proxy table (`ich -> {...}`). Born-zero in production;
    /// live URLs then arrive one at a time via `EDGE_OPEN_URL` / `EDGE_CLOSE_URL` (§7).
    endpoints_path: Option<PathBuf>,
    publications_path: PathBuf,
    blob_public_root: PathBuf,
    public_max_inflight: usize,
    public_ready_timeout_ms: u64,
    /// Public TLS material from disk. When both are `Some` the frontend serves HTTPS.
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    /// Vault secret key holding the cert (preferred over the disk paths).
    tls_vault_key: Option<String>,
    /// Target hive for `SY.vault@<hive>` (default: the mesh's motherbee).
    vault_hive: String,
}

/// Reloads the tracing filter live (§9 node_config: `log_level`). Boxed so the caller never
/// names tracing-subscriber's `reload::Handle<_, _>` generics. Returns Err on a bad filter.
type LogReload = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), SyEdgeError> {
    use tracing_subscriber::prelude::*;
    let config = Config::load();
    let initial_log = std::env::var("RUST_LOG")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "info,sy_edge=debug,fluxbee_sdk=info".to_string());
    let env_filter = EnvFilter::try_new(&initial_log).unwrap_or_else(|_| EnvFilter::new("info"));
    let (filter_layer, reload_handle) = tracing_subscriber::reload::Layer::new(env_filter);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(tracing_subscriber::fmt::layer())
        .init();
    // Current level string (EnvFilter can't be read back) + a boxed reloader for node_config.
    let current_log = Arc::new(std::sync::Mutex::new(initial_log));
    let log_reload: LogReload = {
        let current = Arc::clone(&current_log);
        Arc::new(move |level: &str| {
            let filter = EnvFilter::try_new(level).map_err(|err| err.to_string())?;
            reload_handle
                .reload(filter)
                .map_err(|err| err.to_string())?;
            if let Ok(mut guard) = current.lock() {
                *guard = level.to_string();
            }
            Ok(())
        })
    };

    tracing::info!(
        node_name = %config.node_name,
        router_socket = %config.router_socket.display(),
        "sy-edge starting"
    );

    let node_config = NodeConfig {
        name: config.node_name.clone(),
        router_socket: config.router_socket.clone(),
        uuid_persistence_dir: config.uuid_persistence_dir.clone(),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config.config_dir.clone(),
        version: config.node_version.clone(),
    };
    let profile =
        build_sy_edge_rpc_profile().map_err(|err| format!("sy-edge rpc profile invalid: {err}"))?;
    let dispatcher =
        RouterDispatcher::connect_with_retry(node_config, Duration::from_secs(1), profile).await?;
    let sender = dispatcher.sender_snapshot();

    // Identity "a fuego": like every SY.* node, the edge computes its own system
    // ilk deterministically from its announced L2 name — SHA-256, no ILK_REGISTER,
    // no identity SHM (which the ingress hive does not run). Used to stamp
    // `src_ilk` on forwards and to authorize the vault TLS-cert fetch (dedicated
    // owner match). Its tenant is the fixed root tenant, also a fuego.
    let self_name = sender.full_name().to_string();
    let self_ilk = fluxbee_sdk::deterministic_system_ilk_id(&self_name);

    tracing::info!(full_name = %self_name, self_ilk = %self_ilk, "sy-edge connected to router");

    let mut system_rx = dispatcher
        .take_command_receiver(RPC_CH_SYSTEM)
        .await
        .map_err(|err| format!("sy-edge system receiver: {err}"))?;

    // The reverse-proxy table: seeded from disk (born-zero in prod), then mutated one
    // URL at a time by `EDGE_OPEN_URL` / `EDGE_CLOSE_URL` service commands (§7). Shared
    // (read) by the frontend and (write) by the system loop below.
    let registry: Arc<RwLock<HashMap<String, EndpointEntry>>> = Arc::new(RwLock::new(
        config
            .endpoints_path
            .as_deref()
            .map(load_registry)
            .unwrap_or_default(),
    ));
    let public_registry: Arc<RwLock<HashMap<String, PublicArtifactRow>>> =
        Arc::new(RwLock::new(load_public_registry(&config.publications_path)));

    // Warm-start step 2 (§8): rows reloaded from disk carry only a `secret_ref` (never the
    // secret value). Re-fetch each secret from vault now, so shared-secret channels come back
    // ready — before the door opens.
    resolve_secrets(
        &dispatcher,
        &self_ilk,
        &self_name,
        &config.vault_hive,
        &registry,
    )
    .await;

    // Ingress-hive role: run the public HTTPS door. Fail-closed — absent on any
    // other role (`resolve_http_listen`).
    if let Some(listen) = config.http_listen.clone() {
        tracing::info!(
            listen = %listen,
            endpoints = registry.read().map(|r| r.len()).unwrap_or(0),
            publications = public_registry.read().map(|r| r.len()).unwrap_or(0),
            "sy-edge starting public HTTP frontend"
        );
        let state = Arc::new(FrontendState {
            dispatcher: Arc::clone(&dispatcher),
            sender: sender.clone(),
            registry: Arc::clone(&registry),
            public_registry: Arc::clone(&public_registry),
            blob_public_root: config.blob_public_root.clone(),
            self_ilk: self_ilk.clone(),
            ttl: config.ttl,
            timeout: Duration::from_millis(config.handler_timeout_ms),
            inflight: Arc::new(Semaphore::new(config.max_inflight)),
            public_inflight: Arc::new(Semaphore::new(config.public_max_inflight)),
        });
        // TLS: vault-sourced (preferred) or on-disk PEM. FAIL-CLOSED — if TLS is
        // requested but the material can't be built, do NOT fall back to plaintext
        // on the public listener; skip the frontend entirely.
        let tls_requested = config.tls_vault_key.is_some()
            || (config.tls_cert.is_some() && config.tls_key.is_some());
        let tls: Option<Arc<rustls::ServerConfig>> = if let Some(vault_key) =
            config.tls_vault_key.clone()
        {
            tracing::info!(vault_hive = %config.vault_hive, key = %vault_key, ilk = %self_ilk, "sy-edge fetching TLS cert from vault");
            match fetch_tls_config_from_vault(
                Arc::clone(&dispatcher),
                self_ilk.clone(),
                self_name.clone(),
                config.vault_hive.clone(),
                &vault_key,
            )
            .await
            {
                Ok(cfg) => Some(Arc::new(cfg)),
                Err(err) => {
                    tracing::error!(error = %err, "sy-edge: TLS cert fetch from vault failed");
                    None
                }
            }
        } else if let (Some(cert), Some(key)) = (config.tls_cert.clone(), config.tls_key.clone()) {
            match load_tls_config(&cert, &key) {
                Ok(cfg) => Some(Arc::new(cfg)),
                Err(err) => {
                    tracing::error!(error = %err, "sy-edge: TLS cert load from disk failed");
                    None
                }
            }
        } else {
            None
        };
        if tls_requested && tls.is_none() {
            tracing::error!(
                "sy-edge: TLS requested but no valid cert loaded; refusing to bind the public \
                 listener in plaintext (fail-closed). Fix the vault secret / cert files and restart."
            );
            return Err("TLS requested but no valid cert loaded".into());
        } else {
            tokio::spawn(async move {
                if let Err(err) = run_frontend(listen, state, tls).await {
                    tracing::error!(error = %err, "sy-edge public frontend exited");
                }
            });
        }
    }

    // FIX-6 (part 2): active reaper. Part 1 drops expired public-artifact rows at LOAD; this
    // periodically prunes them from the live registry AND persists the pruned ledger, so a
    // long-lived edge doesn't accumulate expired rows (bounds the served set + on-disk ledger).
    // On a non-ingress edge the registry is empty, so this is a cheap no-op.
    {
        let reaper_registry = Arc::clone(&public_registry);
        let reaper_path = config.publications_path.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(600));
            tick.tick().await; // consume the immediate first tick
            loop {
                tick.tick().await;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let snapshot = {
                    let mut guard = match reaper_registry.write() {
                        Ok(guard) => guard,
                        Err(_) => continue,
                    };
                    let before = guard.len();
                    guard.retain(|_, row| row.expires_at > now);
                    if guard.len() == before {
                        continue; // nothing expired
                    }
                    tracing::info!(removed = before - guard.len(), "sy-edge reaper: dropped expired public artifacts");
                    guard.clone()
                };
                if let Err(err) = persist_public_registry(&reaper_path, &snapshot) {
                    tracing::warn!(error = %err, "sy-edge reaper: persist pruned ledger failed");
                }
            }
        });
    }

    // System channel: default node status + the URL-service command plane. Opening or
    // closing a public URL is a VERIFIED SERVICE DIRECTIVE from SY.admin, delivered as an
    // addressed request/response (`EDGE_OPEN_URL` / `EDGE_CLOSE_URL`, §7) — NOT a config
    // push. Each command mutates one row (upsert/remove) and is acked, so it routes
    // cross-hive like any node RPC and the caller stays blocked until it lands. This loop
    // is the process's only blocking await — it keeps main() alive.
    while let Some(msg) = system_rx.recv().await {
        match msg.meta.msg.as_deref() {
            Some(MSG_EDGE_OPEN_URL) => {
                apply_open_url(
                    &sender,
                    &registry,
                    config.endpoints_path.as_deref(),
                    &msg,
                    config.ttl,
                )
                .await;
                // If the just-opened row shipped a `secret_ref` (not the value), fetch it now.
                resolve_secrets(
                    &dispatcher,
                    &self_ilk,
                    &self_name,
                    &config.vault_hive,
                    &registry,
                )
                .await;
                continue;
            }
            Some(MSG_EDGE_CLOSE_URL) => {
                apply_close_url(
                    &sender,
                    &registry,
                    config.endpoints_path.as_deref(),
                    &msg,
                    config.ttl,
                )
                .await;
                continue;
            }
            Some(MSG_EDGE_LIST_URLS) => {
                apply_list_urls(&sender, &registry, &msg, config.ttl).await;
                continue;
            }
            Some(MSG_EDGE_PUBLISH_BLOB) => {
                apply_publish_blob(
                    &sender,
                    &public_registry,
                    &config.publications_path,
                    &config.blob_public_root,
                    &msg,
                    config.ttl,
                    Duration::from_millis(config.public_ready_timeout_ms),
                )
                .await;
                continue;
            }
            Some(MSG_EDGE_UNPUBLISH_BLOB) => {
                apply_unpublish_blob(
                    &sender,
                    &public_registry,
                    &config.publications_path,
                    &msg,
                    config.ttl,
                )
                .await;
                continue;
            }
            Some(MSG_VAULT_SECRET_CHANGED) => {
                // FIX-3: a degraded-vault edge boot leaves shared-secret channels with secret=None;
                // resolve_secrets runs only at boot + post-EDGE_OPEN_URL, so when the vault later
                // publishes the secret the edge never re-resolved and /e/<ich> 401'd indefinitely
                // while io.api still reported "published". Re-resolve on the broadcast (previously
                // dropped as unhandled). Fail-closed origin check: only THE VAULT THIS EDGE USES may
                // trigger it — src_l2_name is router-stamped. The edge's vault lives at
                // `config.vault_hive` (typically motherbee), NOT the edge's own (ingress) hive; the
                // vault broadcasts with src=None so the router stamps it `SY.vault@<vault_hive>`.
                // Comparing against the edge's OWN hive would reject it in the real multi-hive DMZ.
                let vault_hive = config.vault_hive.trim();
                let expected = format!("SY.vault@{vault_hive}");
                match msg.routing.src_l2_name.as_deref().map(str::trim) {
                    Some(origin) if !vault_hive.is_empty() && origin == expected => {
                        tracing::info!(origin = %origin, "sy-edge: VAULT_SECRET_CHANGED — re-resolving channel secrets");
                        resolve_secrets(
                            &dispatcher,
                            &self_ilk,
                            &self_name,
                            &config.vault_hive,
                            &registry,
                        )
                        .await;
                    }
                    other => {
                        tracing::warn!(
                            src_l2_name = %other.unwrap_or("<none>"),
                            expected = %expected,
                            "sy-edge: VAULT_SECRET_CHANGED from a non-vault / cross-hive origin; ignoring"
                        );
                    }
                }
                continue;
            }
            _ => {}
        }
        // node_config (§9): the edge's OWN config, live where it can. Distinct surface from the
        // URL commands above — this configures the edge itself, never other nodes' endpoints.
        if is_node_config_set_message(&msg) || is_node_config_get_message(&msg) {
            apply_node_config(&sender, &config.node_name, &log_reload, &current_log, &msg).await;
            continue;
        }
        match try_handle_default_node_status(&sender, &msg).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(
                    msg_type = %msg.meta.msg_type,
                    msg = ?msg.meta.msg,
                    trace_id = %msg.routing.trace_id,
                    "sy-edge dropping unhandled system message"
                );
            }
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    trace_id = %msg.routing.trace_id,
                    "sy-edge failed to handle system message"
                );
            }
        }
    }

    tracing::warn!("sy-edge system channel closed; exiting");
    Ok(())
}

fn build_sy_edge_rpc_profile() -> Result<OperationalRouteProfile, RpcError> {
    OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
}

/// `EDGE_OPEN_URL` (§7): SY.admin opens one public URL on this edge on behalf of an IO
/// node. The payload IS one endpoint row (`ich`, `owner_l2_name`, `inbound_family`, auth).
/// Upsert by `ich` (idempotent on retry), then ack — the caller (admin, and through it the
/// IO node) stays blocked until this lands. NOTE: this is a service directive, NOT config.
async fn apply_open_url(
    sender: &NodeSender,
    registry: &Arc<RwLock<HashMap<String, EndpointEntry>>>,
    endpoints_path: Option<&std::path::Path>,
    msg: &Message,
    ttl: u8,
) {
    let outcome: Result<(String, usize), String> = (|| {
        authorize_edge_service_command(msg, MSG_EDGE_OPEN_URL)?;
        let row: EndpointRow = serde_json::from_value(msg.payload.clone())
            .map_err(|err| format!("invalid EDGE_OPEN_URL payload: {err}"))?;
        validate_endpoint_row_for_open(&row)?;
        let (ich, mut entry) = row_to_entry(row);
        let mut guard = registry
            .write()
            .map_err(|_| "registry lock poisoned".to_string())?;
        // Grace-window bearer rotation (io.api residual #1): if this ich already had a live
        // shared secret, keep it valid for a grace period so a mid-flight external client is
        // NOT 401'd the instant the token rotates. The incoming entry arrives with secret=None
        // (admin ships only secret_ref; resolve_secrets fetches the NEW value right after this),
        // so we stash the OUTGOING value as the previous/grace secret here. If the new value
        // turns out identical (idempotent re-open), previous==current and the grace is a no-op.
        if entry.auth_mode == AuthMode::SharedSecret {
            if let Some(old_secret) = guard.get(&ich).and_then(|old| old.secret.clone()) {
                entry.previous_secret = Some(old_secret);
                entry.previous_secret_expires_at_ms = Some(now_epoch_ms() + SHARED_SECRET_GRACE_MS);
            }
        }
        guard.insert(ich.clone(), entry);
        let count = guard.len();
        Ok((ich, count))
    })();
    let payload = match &outcome {
        Ok((ich, count)) => {
            tracing::info!(ich = %ich, endpoints = count, trace_id = %msg.routing.trace_id, "sy-edge opened URL (EDGE_OPEN_URL)");
            persist_after_change(registry, endpoints_path);
            json!({ "status": "ok", "ich": ich, "url": format!("/e/{ich}") })
        }
        Err(err) => {
            tracing::warn!(error = %err, trace_id = %msg.routing.trace_id, "sy-edge EDGE_OPEN_URL failed");
            json!({ "status": "error", "error": err })
        }
    };
    send_edge_reply(sender, msg, MSG_EDGE_OPEN_URL_RESPONSE, payload, ttl).await;
}

/// Persist the route table after a successful open/close, outside the write lock (the
/// single-threaded system loop is the only writer, so no concurrent mutation races us).
fn persist_after_change(
    registry: &Arc<RwLock<HashMap<String, EndpointEntry>>>,
    endpoints_path: Option<&std::path::Path>,
) {
    if let (Some(path), Ok(guard)) = (endpoints_path, registry.read()) {
        persist_registry(path, &guard);
    }
}

/// `EDGE_CLOSE_URL` (§7): SY.admin closes a public URL. Payload is `{ich}`. Remove the row
/// (idempotent — closing an already-absent URL is `ok`), then ack.
async fn apply_close_url(
    sender: &NodeSender,
    registry: &Arc<RwLock<HashMap<String, EndpointEntry>>>,
    endpoints_path: Option<&std::path::Path>,
    msg: &Message,
    ttl: u8,
) {
    let outcome: Result<(String, bool), String> = (|| {
        authorize_edge_service_command(msg, MSG_EDGE_CLOSE_URL)?;
        let ich = msg
            .payload
            .get("ich")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "missing ich".to_string())?
            .to_string();
        let mut guard = registry
            .write()
            .map_err(|_| "registry lock poisoned".to_string())?;
        let existed = guard.remove(&ich).is_some();
        Ok((ich, existed))
    })();
    let payload = match &outcome {
        Ok((ich, existed)) => {
            tracing::info!(ich = %ich, existed, trace_id = %msg.routing.trace_id, "sy-edge closed URL (EDGE_CLOSE_URL)");
            if *existed {
                persist_after_change(registry, endpoints_path);
            }
            json!({ "status": "ok", "ich": ich, "closed": existed })
        }
        Err(err) => {
            tracing::warn!(error = %err, trace_id = %msg.routing.trace_id, "sy-edge EDGE_CLOSE_URL failed");
            json!({ "status": "error", "error": err })
        }
    };
    send_edge_reply(sender, msg, MSG_EDGE_CLOSE_URL_RESPONSE, payload, ttl).await;
}

async fn apply_list_urls(
    sender: &NodeSender,
    registry: &Arc<RwLock<HashMap<String, EndpointEntry>>>,
    msg: &Message,
    ttl: u8,
) {
    let payload = match authorize_edge_service_command(msg, MSG_EDGE_LIST_URLS) {
        Ok(()) => {
            let rows = registry
                .read()
                .map(|guard| registry_to_persisted_rows(&guard))
                .unwrap_or_default();
            let channels: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let ich = row.ich;
                    let url = format!("/e/{ich}");
                    json!({
                        "ich": ich,
                        "url": url,
                        "owner_l2_name": row.owner_l2_name,
                        "inbound_family": row.inbound_family,
                        "auth_mode": row.auth_mode,
                        "secret_ref": row.secret_ref,
                        "methods": row.methods,
                        "tenant_id": row.tenant_id,
                    })
                })
                .collect();
            let version = channels.len();
            json!({ "status": "ok", "channels": channels, "version": version })
        }
        Err(err) => {
            tracing::warn!(error = %err, trace_id = %msg.routing.trace_id, "sy-edge EDGE_LIST_URLS failed");
            json!({ "status": "error", "error": err })
        }
    };
    send_edge_reply(sender, msg, MSG_EDGE_LIST_URLS_RESPONSE, payload, ttl).await;
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PublicArtifactRow {
    key: String,
    publication_id: String,
    public_name: String,
    sha256: String,
    size: u64,
    content_type: String,
    presentation: String,
    expires_at: u64,
    content_policy: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct PublicArtifactsFile {
    #[serde(default)]
    publications: Vec<PublicArtifactRow>,
}

async fn apply_publish_blob(
    sender: &NodeSender,
    registry: &Arc<RwLock<HashMap<String, PublicArtifactRow>>>,
    publications_path: &std::path::Path,
    public_root: &std::path::Path,
    msg: &Message,
    ttl: u8,
    ready_timeout: Duration,
) {
    let outcome: Result<PublicArtifactRow, String> = async {
        authorize_edge_service_command(msg, MSG_EDGE_PUBLISH_BLOB)?;
        let row: PublicArtifactRow = serde_json::from_value(msg.payload.clone())
            .map_err(|err| format!("invalid EDGE_PUBLISH_BLOB payload: {err}"))?;
        validate_public_artifact_row(&row)?;
        wait_for_public_artifact_ready(public_root, &row, ready_timeout).await?;
        {
            let mut guard = registry
                .write()
                .map_err(|_| "public registry lock poisoned".to_string())?;
            if let Some(existing) = guard.get(&row.key) {
                if existing != &row {
                    return Err("public capability already exists with different facts".to_string());
                }
            }
            if guard.values().any(|existing| {
                existing.publication_id == row.publication_id && existing.key != row.key
            }) {
                return Err("publication_id already exists with a different capability".to_string());
            }
            let previous = guard.insert(row.key.clone(), row.clone());
            if let Err(err) = persist_public_registry(publications_path, &guard) {
                match previous {
                    Some(previous) => {
                        guard.insert(row.key.clone(), previous);
                    }
                    None => {
                        guard.remove(&row.key);
                    }
                }
                return Err(err);
            }
        }
        Ok(row)
    }
    .await;
    let payload = match outcome {
        Ok(row) => {
            tracing::info!(
                publication_id = %row.publication_id,
                key = %row.key,
                public_name = %row.public_name,
                "sy-edge published public artifact after local readiness verification"
            );
            json!({
                "status": "ok",
                "publication_id": row.publication_id,
                "url": format!("/public/{}", row.key),
                "ready": true,
            })
        }
        Err(err) => {
            tracing::warn!(error = %err, trace_id = %msg.routing.trace_id, "sy-edge EDGE_PUBLISH_BLOB failed");
            json!({"status": "error", "error": err})
        }
    };
    send_edge_reply(sender, msg, MSG_EDGE_PUBLISH_BLOB_RESPONSE, payload, ttl).await;
}

async fn apply_unpublish_blob(
    sender: &NodeSender,
    registry: &Arc<RwLock<HashMap<String, PublicArtifactRow>>>,
    publications_path: &std::path::Path,
    msg: &Message,
    ttl: u8,
) {
    let outcome: Result<(String, bool), String> = (|| {
        authorize_edge_service_command(msg, MSG_EDGE_UNPUBLISH_BLOB)?;
        let publication_id = msg
            .payload
            .get("publication_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| valid_prefixed_uuid(value, "pub"))
            .ok_or_else(|| "publication_id must be pub:<uuid>".to_string())?
            .to_string();
        let mut guard = registry
            .write()
            .map_err(|_| "public registry lock poisoned".to_string())?;
        let key = guard
            .iter()
            .find(|(_, row)| row.publication_id == publication_id)
            .map(|(key, _)| key.clone());
        let removed_row = key.as_ref().and_then(|key| guard.remove(key));
        let removed = removed_row.is_some();
        if removed {
            if let Err(err) = persist_public_registry(publications_path, &guard) {
                if let (Some(key), Some(row)) = (key, removed_row) {
                    guard.insert(key, row);
                }
                return Err(err);
            }
        }
        Ok((publication_id, removed))
    })();
    let payload = match outcome {
        Ok((publication_id, removed)) => {
            tracing::info!(publication_id = %publication_id, removed, "sy-edge unpublished public artifact");
            json!({"status": "ok", "publication_id": publication_id, "removed": removed})
        }
        Err(err) => json!({"status": "error", "error": err}),
    };
    send_edge_reply(sender, msg, MSG_EDGE_UNPUBLISH_BLOB_RESPONSE, payload, ttl).await;
}

fn validate_public_artifact_row(row: &PublicArtifactRow) -> Result<(), String> {
    if !is_lower_hex_64(&row.key) {
        return Err("key must be 64 lowercase hex characters".to_string());
    }
    if !valid_prefixed_uuid(&row.publication_id, "pub") {
        return Err("publication_id must be pub:<uuid>".to_string());
    }
    if !is_lower_hex_64(&row.public_name)
        || !is_lower_hex_64(&row.sha256)
        || row.public_name != row.sha256
    {
        return Err("public_name and sha256 must be the same lowercase SHA-256".to_string());
    }
    if !matches!(row.presentation.as_str(), "inline" | "attachment") {
        return Err("presentation must be inline or attachment".to_string());
    }
    match row.content_policy.as_str() {
        "sandboxed-html-v1" if row.content_type == "text/html; charset=utf-8" => {}
        "static-v1" if allowed_static_content_type(&row.content_type) => {}
        "download-v1"
            if row.content_type == "application/octet-stream"
                && row.presentation == "attachment" => {}
        _ => return Err("content_type/content_policy combination is not allowed".to_string()),
    }
    Ok(())
}

fn allowed_static_content_type(value: &str) -> bool {
    matches!(
        value,
        "text/plain; charset=utf-8"
            | "application/json; charset=utf-8"
            | "application/pdf"
            | "image/png"
            | "image/jpeg"
            | "image/webp"
            | "image/gif"
    )
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_prefixed_uuid(value: &str, prefix: &str) -> bool {
    value
        .strip_prefix(&format!("{prefix}:"))
        .and_then(|raw| Uuid::parse_str(raw).ok())
        .is_some()
}

async fn wait_for_public_artifact_ready(
    public_root: &std::path::Path,
    row: &PublicArtifactRow,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let root = public_root.to_path_buf();
        let row = row.clone();
        let last_error =
            match tokio::task::spawn_blocking(move || verify_public_artifact_file(&root, &row))
                .await
            {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(err)) => err,
                Err(err) => format!("readiness worker failed: {err}"),
            };
        if Instant::now() >= deadline {
            return Err(format!(
                "public artifact readiness timed out after {}ms: {last_error}",
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn verify_public_artifact_file(
    public_root: &std::path::Path,
    row: &PublicArtifactRow,
) -> Result<(), String> {
    let canonical_root = public_root
        .canonicalize()
        .map_err(|err| format!("public root unavailable: {err}"))?;
    let path = public_root.join(&row.public_name);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|err| format!("public artifact unavailable: {err}"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("public artifact must be a regular non-symlink file".to_string());
    }
    if metadata.len() != row.size {
        return Err(format!(
            "public artifact size mismatch: expected={} actual={}",
            row.size,
            metadata.len()
        ));
    }
    let canonical_path = path
        .canonicalize()
        .map_err(|err| format!("canonicalize public artifact: {err}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("public artifact resolves outside public root".to_string());
    }
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|err| format!("open public artifact: {err}"))?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|err| format!("read public artifact: {err}"))?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > row.size {
            return Err("public artifact grew while hashing".to_string());
        }
        hasher.update(&buffer[..count]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if total != row.size || actual != row.sha256 {
        return Err("public artifact SHA-256 verification failed".to_string());
    }
    Ok(())
}

fn load_public_registry(path: &std::path::Path) -> HashMap<String, PublicArtifactRow> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(err) => {
            tracing::error!(path = %path.display(), error = %err, "sy-edge public registry read failed; starting empty");
            return HashMap::new();
        }
    };
    let parsed: PublicArtifactsFile = match serde_json::from_slice(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::error!(path = %path.display(), error = %err, "sy-edge public registry invalid; starting empty");
            return HashMap::new();
        }
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    parsed
        .publications
        .into_iter()
        .filter(|row| validate_public_artifact_row(row).is_ok())
        .filter(|row| {
            // FIX-6: drop already-expired rows at load. They are un-servable (the serve path
            // rejects expires_at <= now) but were previously re-residented into the registry on
            // EVERY restart, growing it unbounded and pinning the referenced public/ bytes. NOTE:
            // this bounds the in-memory registry; an active reaper that also deletes the public/
            // bytes + prunes the on-disk ledger via the unpublish/MSG_BLOB_RELEASE path is the
            // complementary follow-up (part 2).
            if row.expires_at <= now {
                tracing::info!(key = %row.key, "sy-edge: dropping expired public artifact row at load");
                false
            } else {
                true
            }
        })
        .map(|row| (row.key.clone(), row))
        .collect()
}

fn persist_public_registry(
    path: &std::path::Path,
    registry: &HashMap<String, PublicArtifactRow>,
) -> Result<(), String> {
    let mut publications: Vec<_> = registry.values().cloned().collect();
    publications.sort_by(|left, right| left.key.cmp(&right.key));
    let data = serde_json::to_vec_pretty(&PublicArtifactsFile { publications })
        .map_err(|err| format!("serialize public registry: {err}"))?;
    let temp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create public registry directory: {err}"))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o640)
        .open(&temp)
        .map_err(|err| format!("open temporary public registry: {err}"))?;
    file.write_all(&data)
        .and_then(|_| file.sync_all())
        .map_err(|err| format!("write temporary public registry: {err}"))?;
    std::fs::rename(&temp, path).map_err(|err| format!("install public registry: {err}"))?;
    Ok(())
}

fn authorize_edge_service_command(msg: &Message, action: &str) -> Result<(), String> {
    match msg.routing.src_l2_name.as_deref().map(str::trim) {
        Some(src) if src == json_router::router::system_policy::EDGE_CONTROL_AUTHORITY => Ok(()),
        Some(src) if src.is_empty() => Err(format!("{action} requires router-stamped source")),
        Some(src) => Err(format!("{action} not authorized from {src}")),
        None => Err(format!("{action} requires router-stamped source")),
    }
}

fn validate_endpoint_row_for_open(row: &EndpointRow) -> Result<(), String> {
    if row.ich.trim().is_empty() {
        return Err("ich must not be empty".to_string());
    }
    let owner = row.owner_l2_name.trim();
    if owner.is_empty() || !owner.starts_with("IO.") || !owner.contains('@') {
        return Err(format!(
            "owner_l2_name must be a fully-qualified IO.* node, got '{owner}'"
        ));
    }
    let family = row.inbound_family.trim();
    if family.is_empty() {
        return Err("inbound_family must not be empty".to_string());
    }
    if family.eq_ignore_ascii_case(SYSTEM_KIND) {
        return Err("inbound_family must not be system".to_string());
    }
    if row.auth_mode == AuthMode::SharedSecret
        && !non_empty_str(row.secret.as_deref())
        && !non_empty_str(row.secret_ref.as_deref())
    {
        return Err("shared-secret endpoints require secret or secret_ref".to_string());
    }
    Ok(())
}

fn non_empty_str(value: Option<&str>) -> bool {
    value.map(str::trim).is_some_and(|value| !value.is_empty())
}

/// Reply to a service command by name (router-stamped `src_l2_name`, so it routes back
/// cross-hive), preserving the request's `trace_id` for the caller's pending matcher.
async fn send_edge_reply(
    sender: &NodeSender,
    req: &Message,
    resp_msg: &str,
    payload: Value,
    ttl: u8,
) {
    let dst = req
        .routing
        .src_l2_name
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| req.routing.src.clone());
    let reply = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(dst),
            ttl,
            trace_id: req.routing.trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(resp_msg.to_string()),
            ..Meta::default()
        },
        payload,
    };
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(error = %err, trace_id = %req.routing.trace_id, "sy-edge failed to send command reply");
    }
}

/// node_config (§9): apply the edge's OWN config live where possible, and reply. Today `log_level`
/// applies live (tracing reload); every other key is reported as `restart_required` (rebind :443,
/// swap TLS, change NIC need a process restart). This is the edge configuring ITSELF — never the
/// URL table (that's EDGE_OPEN_URL/CLOSE_URL).
async fn apply_node_config(
    sender: &NodeSender,
    node_name: &str,
    log_reload: &LogReload,
    current_log: &Arc<std::sync::Mutex<String>>,
    msg: &Message,
) {
    let current = || {
        current_log
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    };
    let payload = match parse_node_config_request(msg) {
        Ok(NodeConfigControlRequest::Get(_)) => json!({
            "ok": true,
            "node_name": node_name,
            "state": "ok",
            "effective_config": { "log_level": current() },
        }),
        Ok(NodeConfigControlRequest::Set(req)) => {
            let cfg = req.config.as_object().cloned().unwrap_or_default();
            let mut applied: Vec<String> = Vec::new();
            let mut restart_required: Vec<String> = Vec::new();
            let mut error: Option<String> = None;
            for (key, value) in &cfg {
                match key.as_str() {
                    "log_level" => match value.as_str() {
                        Some(level) => match log_reload(level) {
                            Ok(()) => {
                                applied.push("log_level".to_string());
                                tracing::info!(log_level = %level, "sy-edge applied log_level live (node_config §9)");
                            }
                            Err(err) => error = Some(format!("log_level: {err}")),
                        },
                        None => error = Some("log_level must be a string".to_string()),
                    },
                    other => restart_required.push(other.to_string()),
                }
            }
            let ok = error.is_none();
            json!({
                "ok": ok,
                "node_name": node_name,
                "state": if ok { "applied" } else { "error" },
                "config_version": req.config_version,
                "effective_config": { "log_level": current() },
                "applied": applied,
                "restart_required": restart_required,
                "error": error.map(|detail| json!({ "code": "CONFIG_APPLY_FAILED", "detail": detail })),
            })
        }
        Err(err) => json!({
            "ok": false,
            "node_name": node_name,
            "state": "error",
            "error": { "code": "INVALID_CONFIG", "detail": err.to_string() },
        }),
    };
    let reply = build_node_config_response_message(msg, sender.uuid(), payload);
    if let Err(err) = sender.send(reply).await {
        tracing::warn!(error = %err, "sy-edge failed to send node_config response");
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// The `role` + optional `edge:` section of `hive.yaml`, the ONLY fields sy_edge
/// needs from it. Other sections (wan/nats/blob/…) are ignored — NO
/// `deny_unknown_fields`, or the whole file would fail to parse.
#[derive(Debug, Clone, Deserialize)]
struct HiveEdgeFile {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    edge: Option<EdgeSection>,
}

#[derive(Debug, Clone, Deserialize)]
struct EdgeSection {
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    endpoints_path: Option<String>,
    #[serde(default)]
    publications_path: Option<String>,
    #[serde(default)]
    blob_public_root: Option<String>,
    #[serde(default)]
    public_max_inflight: Option<usize>,
    #[serde(default)]
    public_ready_timeout_ms: Option<u64>,
    /// Public TLS cert chain (PEM) + private key (PEM) from DISK. When both are
    /// set the frontend serves HTTPS. Alternatively `tls_vault_key` pulls the same
    /// material from SY.vault (preferred — no cert on the DMZ box's config).
    #[serde(default)]
    tls_cert: Option<String>,
    #[serde(default)]
    tls_key: Option<String>,
    /// Vault secret key holding `{ "cert": "<PEM>", "key": "<PEM>" }`. When set,
    /// SY.edge fetches its TLS material from SY.vault at boot (owned by the edge's
    /// deterministic ilk, so authz needs no identity SHM on the edge hive).
    #[serde(default)]
    tls_vault_key: Option<String>,
    /// Hive whose SY.vault holds the cert (target `SY.vault@<vault_hive>`).
    #[serde(default)]
    vault_hive: Option<String>,
}

/// Read `role` + `edge:` from `<config_dir>/hive.yaml`. Best-effort: a missing or
/// unparsable file yields `(None, None)` so the node still boots but never binds a
/// public listener (fail-closed).
fn load_hive_edge(config_dir: &std::path::Path) -> (Option<String>, Option<EdgeSection>) {
    let path = config_dir.join("hive.yaml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return (None, None),
    };
    match serde_yaml::from_str::<HiveEdgeFile>(&raw) {
        Ok(parsed) => (parsed.role, parsed.edge),
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sy-edge could not parse hive.yaml; no public listener");
            (None, None)
        }
    }
}

/// Fail-closed public-listener decision. An explicit env override wins (dev/test
/// escape hatch); otherwise the `edge.listen` is honored ONLY when the hive `role`
/// is `ingress`. Any other role (or no role, or no edge section) ⇒ `None` ⇒ never
/// binds a public `:443`. This is the invariant that keeps a stray `edge:` stanza
/// on motherbee from opening a public door.
fn resolve_http_listen(
    env_override: Option<String>,
    role: Option<&str>,
    edge_listen: Option<String>,
) -> Option<String> {
    if let Some(listen) = env_override {
        return Some(listen);
    }
    let is_ingress = role
        .map(|r| r.trim().eq_ignore_ascii_case("ingress"))
        .unwrap_or(false);
    if is_ingress {
        edge_listen
    } else {
        None
    }
}

impl Config {
    fn load() -> Self {
        let config_dir =
            PathBuf::from(env("CONFIG_DIR").unwrap_or_else(|| "/etc/fluxbee".to_string()));
        let (role, edge) = load_hive_edge(&config_dir);
        let http_listen = resolve_http_listen(
            env("SY_EDGE_HTTP_LISTEN"),
            role.as_deref(),
            edge.as_ref().and_then(|e| e.listen.clone()),
        );
        let endpoints_path = env("SY_EDGE_ENDPOINTS")
            .or_else(|| edge.as_ref().and_then(|e| e.endpoints_path.clone()))
            .unwrap_or_else(|| "/etc/fluxbee/edge.endpoints.json".to_string());
        let publications_path = env("SY_EDGE_PUBLICATIONS")
            .or_else(|| edge.as_ref().and_then(|e| e.publications_path.clone()))
            .unwrap_or_else(|| "/var/lib/fluxbee/state/sy-edge/publications.json".to_string());
        let blob_public_root = env("SY_EDGE_BLOB_PUBLIC_ROOT")
            .or_else(|| edge.as_ref().and_then(|e| e.blob_public_root.clone()))
            .unwrap_or_else(|| "/var/lib/fluxbee/blob/public".to_string());
        let tls_cert = env("SY_EDGE_TLS_CERT")
            .or_else(|| edge.as_ref().and_then(|e| e.tls_cert.clone()))
            .map(PathBuf::from);
        let tls_key = env("SY_EDGE_TLS_KEY")
            .or_else(|| edge.as_ref().and_then(|e| e.tls_key.clone()))
            .map(PathBuf::from);
        let tls_vault_key = env("SY_EDGE_TLS_VAULT_KEY")
            .or_else(|| edge.as_ref().and_then(|e| e.tls_vault_key.clone()));
        let vault_hive = env("SY_EDGE_VAULT_HIVE")
            .or_else(|| edge.as_ref().and_then(|e| e.vault_hive.clone()))
            .unwrap_or_else(|| "motherbee".to_string());
        Self {
            node_name: env("NODE_NAME").unwrap_or_else(|| "SY.edge".to_string()),
            node_version: env("NODE_VERSION").unwrap_or_else(|| "0.1".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET").unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir,
            ttl: env("TTL").and_then(|raw| raw.parse().ok()).unwrap_or(16),
            handler_timeout_ms: env("HANDLER_TIMEOUT_MS")
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(30_000),
            max_inflight: env("MAX_INFLIGHT")
                .and_then(|raw| raw.parse().ok())
                .filter(|&n| n > 0)
                .unwrap_or(DEFAULT_MAX_INFLIGHT),
            http_listen,
            endpoints_path: Some(PathBuf::from(endpoints_path)),
            publications_path: PathBuf::from(publications_path),
            blob_public_root: PathBuf::from(blob_public_root),
            public_max_inflight: env("SY_EDGE_PUBLIC_MAX_INFLIGHT")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.public_max_inflight))
                .filter(|&value| value > 0)
                .unwrap_or(DEFAULT_PUBLIC_MAX_INFLIGHT),
            public_ready_timeout_ms: env("SY_EDGE_PUBLIC_READY_TIMEOUT_MS")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.public_ready_timeout_ms))
                .filter(|&value| value > 0)
                .unwrap_or(DEFAULT_PUBLIC_READY_TIMEOUT_MS),
            tls_cert,
            tls_key,
            tls_vault_key,
            vault_hive,
        }
    }
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

// ---------------------------------------------------------------------------
// Public HTTP frontend (`:443` door). Runs only on the ingress-hive role.
// After TLS + hash lookup + door auth it forwards the request UNDER THE TARGET'S
// OWN family (Option A) BY NAME to the pre-resolved handler (Option Z, §3/§6).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AuthMode {
    Public,
    SharedSecret,
}

/// One row of the reverse-proxy table, opened by SY.admin via `EDGE_OPEN_URL`
/// (the edge is born with NONE — v6 §6). The public URL is
/// `/e/<ich>`; the `ICH` is the channel identity (v6 §4). The edge holds **no
/// `ilk`** — the frontier: it routes on `ICH -> owner_l2_name` handed to it
/// pre-resolved, and never resolves identity (I6).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct EndpointRow {
    /// The channel `ICH` — the URL is `/e/<ich>`. Opaque to the edge; the core
    /// minted/resolved it and pushed it down.
    ich: String,
    /// The owning node's L2 name (Option Z, pre-resolved by the core). The edge
    /// forwards to it by name and stamps `meta.ich` so the node knows its channel.
    owner_l2_name: String,
    /// The `msg_type`/subject the target speaks (Option A): the edge labels the
    /// forwarded message with exactly this family.
    inbound_family: String,
    auth_mode: AuthMode,
    /// The shared-secret VALUE. Held in RAM but NEVER persisted to disk (§8): the
    /// on-disk route table omits it, so a DMZ disk leak exposes no credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    /// Vault key of the shared-secret (§8). This IS persisted (it's just a name, not a
    /// credential); at boot / open the edge re-fetches the secret VALUE from vault by
    /// this ref (dedicated-owner read, like the TLS cert), so shared-secret channels
    /// warm-start without the secret ever touching disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    methods: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tenant_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EndpointsFile {
    #[serde(default)]
    endpoints: Vec<EndpointRow>,
}

#[derive(Debug, Clone)]
struct EndpointEntry {
    owner_l2_name: String,
    inbound_family: String,
    auth_mode: AuthMode,
    /// The resolved secret VALUE (RAM only). Populated either directly from an
    /// EDGE_OPEN_URL that carried it, or fetched from vault by `secret_ref`.
    secret: Option<String>,
    /// Vault key to (re-)fetch `secret` from — the durable, disk-safe form.
    secret_ref: Option<String>,
    /// Grace-window bearer rotation (io.api edge-publication residual #1): when a channel's
    /// shared secret is rotated (a re-`EDGE_OPEN_URL` for an existing ich lands a DIFFERENT
    /// value from vault), the PREVIOUS secret stays valid until `previous_secret_expires_at_ms`
    /// so a live external client is not hard-cut with a 401 the instant the token rotates. It
    /// is a credential: RAM-only, NEVER persisted (like `secret`); the grace is simply lost on
    /// an edge restart (rare, and the client re-auths with the current token then).
    previous_secret: Option<String>,
    previous_secret_expires_at_ms: Option<u64>,
    methods: Option<Vec<String>>,
    #[allow(dead_code)]
    tenant_id: Option<String>,
}

/// Index the rows by `ICH` (the URL key). No `ilk` anywhere — the edge is outside
/// the identity frontier (I6).
/// Split one row into its `ich` key and the entry stored in the registry.
fn row_to_entry(row: EndpointRow) -> (String, EndpointEntry) {
    let ich = row.ich.trim().to_string();
    (
        ich,
        EndpointEntry {
            owner_l2_name: row.owner_l2_name.trim().to_string(),
            inbound_family: row.inbound_family.trim().to_string(),
            auth_mode: row.auth_mode,
            secret: row.secret,
            secret_ref: row
                .secret_ref
                .map(|secret_ref| secret_ref.trim().to_string())
                .filter(|secret_ref| !secret_ref.is_empty()),
            // A freshly-parsed row has no grace secret; apply_open_url carries the outgoing
            // one forward when it replaces a live entry.
            previous_secret: None,
            previous_secret_expires_at_ms: None,
            methods: row.methods,
            tenant_id: row
                .tenant_id
                .map(|tenant_id| tenant_id.trim().to_string())
                .filter(|tenant_id| !tenant_id.is_empty()),
        },
    )
}

fn rows_to_registry(rows: Vec<EndpointRow>) -> HashMap<String, EndpointEntry> {
    let mut registry = HashMap::with_capacity(rows.len());
    for row in rows {
        let (ich, entry) = row_to_entry(row);
        registry.insert(ich, entry);
    }
    registry
}

/// Load the `ich -> endpoint` route table from disk — the edge's own persisted cache
/// (written by `persist_registry` on every open/close), so a restart/reboot warm-starts
/// instead of born-zero (no 404 window). A missing/empty file is fine (empty registry,
/// every request 404s) — that's the true first boot before any URL is opened. Secrets are
/// NOT on disk; shared-secret channels come back without their secret until re-opened
/// (or, later, re-fetched from vault by `secret_ref`).
fn load_registry(path: &std::path::Path) -> HashMap<String, EndpointEntry> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sy-edge endpoints file not read; starting with an empty registry");
            return HashMap::new();
        }
    };
    let parsed: EndpointsFile = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::error!(path = %path.display(), error = %err, "sy-edge endpoints file is invalid JSON; starting with an empty registry");
            return HashMap::new();
        }
    };
    rows_to_registry(parsed.endpoints)
}

/// Serialize the live registry back to rows for on-disk persistence, **omitting the
/// shared-secret value** (§8: credentials never touch the DMZ disk — only the route
/// survives; the secret is re-fetched from vault at boot in a later step).
fn registry_to_persisted_rows(reg: &HashMap<String, EndpointEntry>) -> Vec<EndpointRow> {
    let mut rows: Vec<EndpointRow> = reg
        .iter()
        .map(|(ich, e)| EndpointRow {
            ich: ich.clone(),
            owner_l2_name: e.owner_l2_name.clone(),
            inbound_family: e.inbound_family.clone(),
            auth_mode: e.auth_mode,
            secret: None,                     // NEVER persist the secret VALUE
            secret_ref: e.secret_ref.clone(), // persist the vault REF (just a name)
            methods: e.methods.clone(),
            tenant_id: e.tenant_id.clone(),
        })
        .collect();
    // Stable order so the file diffs cleanly and the write is deterministic.
    rows.sort_by(|a, b| a.ich.cmp(&b.ich));
    rows
}

/// Persist the route table to disk atomically (temp + rename) so the edge warm-starts
/// after a restart/reboot instead of born-zero (no 404 window). Best-effort: a failure
/// only costs the warm-start, never the in-memory update. The edge OWNS its routes; this
/// is its own operational cache, NOT authority over anyone (I3 relaxed only for routes).
fn persist_registry(path: &std::path::Path, reg: &HashMap<String, EndpointEntry>) {
    let file = EndpointsFile {
        endpoints: registry_to_persisted_rows(reg),
    };
    let json = match serde_json::to_vec_pretty(&file) {
        Ok(json) => json,
        Err(err) => {
            tracing::warn!(error = %err, "sy-edge could not serialize route table; skipping persist");
            return;
        }
    };
    let tmp = path.with_extension("json.tmp");
    if let Err(err) = std::fs::write(&tmp, &json).and_then(|_| std::fs::rename(&tmp, path)) {
        tracing::warn!(path = %path.display(), error = %err, "sy-edge route table persist failed (warm-start only; runtime unaffected)");
    }
}

struct FrontendState {
    dispatcher: Arc<RouterDispatcher>,
    sender: NodeSender,
    registry: Arc<RwLock<HashMap<String, EndpointEntry>>>,
    public_registry: Arc<RwLock<HashMap<String, PublicArtifactRow>>>,
    blob_public_root: PathBuf,
    /// The edge's own deterministic ("a fuego") system ilk, stamped as `src_ilk`.
    self_ilk: String,
    ttl: u8,
    timeout: Duration,
    /// In-flight request limiter (L1, §15.6): a permit is held for each request's whole
    /// lifetime; when none is free the door returns 503 instead of enqueuing unbounded.
    inflight: Arc<Semaphore>,
    public_inflight: Arc<Semaphore>,
}

async fn run_frontend(
    listen: String,
    state: Arc<FrontendState>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Result<(), SyEdgeError> {
    let app = Router::new()
        .route("/e/:ich", any(invoke_root))
        .route("/e/:ich/*extra", any(invoke_extra))
        .route("/public/:key", any(serve_public_artifact))
        .route("/healthz", any(|| async { "ok" }))
        .with_state(state);
    let listener = TcpListener::bind(&listen).await?;
    let addr = listener.local_addr()?;
    match tls {
        Some(tls_config) => {
            let acceptor = tokio_rustls::TlsAcceptor::from(tls_config);
            tracing::info!(addr = %addr, "sy-edge public HTTPS frontend ready (TLS)");
            loop {
                let (tcp, _peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(err) => {
                        tracing::warn!(error = %err, "sy-edge accept failed");
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let app = app.clone();
                tokio::spawn(async move {
                    let tls_stream = match acceptor.accept(tcp).await {
                        Ok(stream) => stream,
                        Err(err) => {
                            tracing::debug!(error = %err, "tls handshake failed");
                            return;
                        }
                    };
                    let io = hyper_util::rt::TokioIo::new(tls_stream);
                    let service = hyper_util::service::TowerToHyperService::new(app);
                    if let Err(err) = hyper_util::server::conn::auto::Builder::new(
                        hyper_util::rt::TokioExecutor::new(),
                    )
                    .serve_connection_with_upgrades(io, service)
                    .await
                    {
                        tracing::debug!(error = %err, "sy-edge tls connection error");
                    }
                });
            }
        }
        None => {
            tracing::info!(addr = %addr, "sy-edge public HTTP frontend ready (plaintext)");
            axum::serve(listener, app).await?;
            Ok(())
        }
    }
}

/// Build a rustls server config from in-memory PEM cert chain + private key.
fn tls_config_from_pem(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<rustls::ServerConfig, SyEdgeError> {
    // rustls 0.23 needs a process crypto provider before ServerConfig::builder()
    // (mirrors src/mesh_tls.rs). Idempotent: ignore "already installed".
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut cert_reader = cert_pem;
    let cert_chain: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_reader).collect::<Result<_, _>>()?;
    if cert_chain.is_empty() {
        return Err("no certificates found in PEM cert material".into());
    }
    let mut key_reader = key_pem;
    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or("no private key found in PEM key material")?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    Ok(config)
}

/// Build a rustls server config from PEM files on disk.
fn load_tls_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<rustls::ServerConfig, SyEdgeError> {
    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;
    tls_config_from_pem(&cert_pem, &key_pem)
}

/// Fetch the edge's TLS material from `SY.vault@<vault_hive>` (secret value
/// `{ "cert": "<PEM>", "key": "<PEM>" }`). Authorized by the dedicated-owner match
/// against the edge's deterministic ilk — no identity SHM needed on the edge hive.
async fn fetch_tls_config_from_vault(
    dispatcher: Arc<RouterDispatcher>,
    self_ilk: String,
    self_name: String,
    vault_hive: String,
    secret_key: &str,
) -> Result<rustls::ServerConfig, SyEdgeError> {
    let client = fluxbee_sdk::VaultClient::new(
        dispatcher,
        vault_hive,
        fluxbee_sdk::VaultCallerOwned::new(self_ilk, self_name),
    );
    let resp = client
        .get(secret_key, Duration::from_secs(15))
        .await
        .map_err(|err| format!("vault get '{secret_key}' failed: {err}"))?;
    let value = resp
        .value
        .ok_or("vault returned no value for the edge tls secret")?;
    let cert = value
        .get("cert")
        .and_then(|v| v.as_str())
        .ok_or("edge tls secret is missing a 'cert' PEM field")?;
    let key = value
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or("edge tls secret is missing a 'key' PEM field")?;
    tls_config_from_pem(cert.as_bytes(), key.as_bytes())
}

/// Resolve any registry entries that carry a `secret_ref` but not yet the secret VALUE by
/// fetching it from `SY.vault@<vault_hive>` (dedicated-owner read via the edge's deterministic
/// ilk — the same path as the TLS cert). Called at boot (warm-started rows carry only the ref)
/// and after an `EDGE_OPEN_URL` that shipped a ref. Best-effort: a failure leaves the channel
/// rejecting until the next resolve, never crashes the edge.
async fn resolve_secrets(
    dispatcher: &Arc<RouterDispatcher>,
    self_ilk: &str,
    self_name: &str,
    vault_hive: &str,
    registry: &Arc<RwLock<HashMap<String, EndpointEntry>>>,
) {
    let pending: Vec<(String, String)> = match registry.read() {
        Ok(guard) => guard
            .iter()
            .filter(|(_, e)| e.secret.is_none() && e.secret_ref.is_some())
            .map(|(ich, e)| (ich.clone(), e.secret_ref.clone().unwrap_or_default()))
            .filter(|(_, r)| !r.is_empty())
            .collect(),
        Err(_) => return,
    };
    if pending.is_empty() {
        return;
    }
    let client = fluxbee_sdk::VaultClient::new(
        Arc::clone(dispatcher),
        vault_hive.to_string(),
        fluxbee_sdk::VaultCallerOwned::new(self_ilk.to_string(), self_name.to_string()),
    );
    for (ich, secret_ref) in pending {
        match client.get(&secret_ref, Duration::from_secs(15)).await {
            Ok(resp) => {
                let secret = resp
                    .value
                    .as_ref()
                    .and_then(|v| v.get("secret"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                match secret {
                    Some(secret) => {
                        if let Ok(mut guard) = registry.write() {
                            if let Some(entry) = guard.get_mut(&ich) {
                                entry.secret = Some(secret);
                            }
                        }
                        tracing::info!(ich = %ich, secret_ref = %secret_ref, "sy-edge resolved channel secret from vault");
                    }
                    None => {
                        tracing::warn!(ich = %ich, secret_ref = %secret_ref, "vault secret has no 'secret' field; channel stays closed to auth")
                    }
                }
            }
            Err(err) => {
                tracing::warn!(ich = %ich, secret_ref = %secret_ref, error = %err, "sy-edge could not fetch channel secret from vault (channel rejects until resolved)")
            }
        }
    }
}

async fn invoke_root(
    State(state): State<Arc<FrontendState>>,
    AxumPath(ich): AxumPath<String>,
    req: Request,
) -> Response {
    invoke(state, ich, String::new(), req).await
}

async fn invoke_extra(
    State(state): State<Arc<FrontendState>>,
    AxumPath((ich, extra)): AxumPath<(String, String)>,
    req: Request,
) -> Response {
    invoke(state, ich, extra, req).await
}

async fn serve_public_artifact(
    State(state): State<Arc<FrontendState>>,
    AxumPath(key): AxumPath<String>,
    req: Request,
) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header(header::ALLOW, "GET, HEAD")
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    if !is_lower_hex_64(&key) {
        return public_not_found();
    }
    let row = match state
        .public_registry
        .read()
        .ok()
        .and_then(|guard| guard.get(&key).cloned())
    {
        Some(row) => row,
        None => return public_not_found(),
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if row.expires_at <= now {
        return public_not_found();
    }

    let etag = format!("\"{}\"", row.sha256);
    if request_etag_matches(req.headers(), &etag) {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .header(header::CACHE_CONTROL, "no-store")
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }
    let permit = match Arc::clone(&state.public_inflight).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "PUBLIC_BUSY",
                "public artifact capacity exhausted",
            )
        }
    };
    let root = state.blob_public_root.clone();
    let open_row = row.clone();
    let file = match tokio::task::spawn_blocking(move || {
        open_public_artifact_file(&root, &open_row)
    })
    .await
    {
        Ok(Ok(file)) => file,
        Ok(Err(err)) => {
            tracing::warn!(publication_id = %row.publication_id, error = %err, "public artifact file unavailable");
            return public_not_found();
        }
        Err(err) => {
            tracing::warn!(publication_id = %row.publication_id, error = %err, "public artifact open worker failed");
            return public_not_found();
        }
    };

    let (start, end, partial) = match parse_single_byte_range(
        req.headers()
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok()),
        row.size,
    ) {
        Ok(range) => range,
        Err(()) => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", row.size))
                .header(header::ACCEPT_RANGES, "bytes")
                .body(Body::empty())
                .unwrap_or_else(|_| Response::new(Body::empty()));
        }
    };
    let content_length = if row.size == 0 { 0 } else { end - start + 1 };
    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, row.content_type.as_str())
        .header(header::CONTENT_LENGTH, content_length.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .header("x-robots-tag", "noindex, nofollow, noarchive")
        .header("referrer-policy", "no-referrer")
        .header(
            header::CONTENT_DISPOSITION,
            if row.presentation == "attachment" {
                format!("attachment; filename=\"{}\"", row.public_name)
            } else {
                "inline".to_string()
            },
        );
    if partial {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", row.size),
        );
    }
    if row.content_policy == "sandboxed-html-v1" {
        builder = builder.header(
            header::CONTENT_SECURITY_POLICY,
            "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; \
             style-src 'unsafe-inline'; img-src data: blob:; font-src data:; \
             connect-src 'none'; form-action 'none'; object-src 'none'; base-uri 'none'",
        );
    }
    if req.method() == Method::HEAD || content_length == 0 {
        drop(permit);
        return builder
            .body(Body::empty())
            .unwrap_or_else(|_| Response::new(Body::empty()));
    }

    let mut file = tokio::fs::File::from_std(file);
    if let Err(err) = file.seek(SeekFrom::Start(start)).await {
        tracing::warn!(publication_id = %row.publication_id, error = %err, "public artifact seek failed");
        return public_not_found();
    }
    let stream = futures::stream::try_unfold(
        (file, permit, content_length),
        |(mut file, permit, remaining): (tokio::fs::File, OwnedSemaphorePermit, u64)| async move {
            if remaining == 0 {
                return Ok(None);
            }
            let mut buffer = vec![0_u8; remaining.min(64 * 1024) as usize];
            let count = file.read(&mut buffer).await?;
            if count == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "public artifact changed during response",
                ));
            }
            buffer.truncate(count);
            Ok(Some((
                Bytes::from(buffer),
                (file, permit, remaining - count as u64),
            )))
        },
    );
    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn public_not_found() -> Response {
    http_error(
        StatusCode::NOT_FOUND,
        "PUBLIC_ARTIFACT_NOT_FOUND",
        "public artifact not found",
    )
}

fn request_etag_matches(headers: &HeaderMap, etag: &str) -> bool {
    headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == "*" || candidate == etag)
        })
}

fn parse_single_byte_range(value: Option<&str>, size: u64) -> Result<(u64, u64, bool), ()> {
    let Some(value) = value else {
        return Ok((0, size.saturating_sub(1), false));
    };
    let raw = value.strip_prefix("bytes=").ok_or(())?;
    if raw.contains(',') || raw.is_empty() || size == 0 {
        return Err(());
    }
    let (start, end) = raw.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let start = size.saturating_sub(suffix.min(size));
        return Ok((start, size - 1, true));
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(size - 1)
    };
    if end < start {
        return Err(());
    }
    Ok((start, end, true))
}

fn open_public_artifact_file(
    public_root: &std::path::Path,
    row: &PublicArtifactRow,
) -> Result<std::fs::File, String> {
    let canonical_root = public_root
        .canonicalize()
        .map_err(|err| format!("public root unavailable: {err}"))?;
    let path = public_root.join(&row.public_name);
    let canonical_path = path
        .canonicalize()
        .map_err(|err| format!("public artifact unavailable: {err}"))?;
    if !canonical_path.starts_with(&canonical_root) {
        return Err("public artifact resolves outside public root".to_string());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|err| format!("open public artifact: {err}"))?;
    let metadata = file
        .metadata()
        .map_err(|err| format!("stat public artifact: {err}"))?;
    if !metadata.is_file() || metadata.len() != row.size {
        return Err("public artifact file facts changed".to_string());
    }
    Ok(file)
}

async fn invoke(state: Arc<FrontendState>, ich: String, extra: String, req: Request) -> Response {
    // 0. Backpressure (L1, §15.6): bound concurrent in-flight requests. Acquire a permit
    //    BEFORE any inward work; hold it for the whole request (dropped on return, after the
    //    reply resolves). When the edge is already at capacity, shed load fast with 503 so a
    //    slow/dead handler or a flood cannot grow the pending map unbounded (DoS).
    let _permit = match Arc::clone(&state.inflight).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                "edge at capacity; retry shortly",
            )
        }
    };

    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();

    // 1. Registry lookup — the ICH (URL) is the first capability gate. Clone the
    //    row out of the lock so nothing is held across the inward await.
    let entry = {
        let registry = match state.registry.read() {
            Ok(guard) => guard,
            Err(_) => {
                return http_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "REGISTRY_UNAVAILABLE",
                    "endpoint registry unavailable",
                )
            }
        };
        match registry.get(&ich) {
            Some(entry) => entry.clone(),
            None => return http_error(StatusCode::NOT_FOUND, "NOT_FOUND", "no such endpoint"),
        }
    };

    // 2. Method allowlist (if the row constrains methods).
    if let Some(allowed) = &entry.methods {
        if !allowed
            .iter()
            .any(|m| m.eq_ignore_ascii_case(method.as_str()))
        {
            return http_error(
                StatusCode::METHOD_NOT_ALLOWED,
                "METHOD_NOT_ALLOWED",
                "method not allowed for this endpoint",
            );
        }
    }

    // 3. Entry auth (§5.2/§5.3 — alpha shared-secret = static bearer).
    match entry.auth_mode {
        AuthMode::Public => {}
        AuthMode::SharedSecret => {
            // Accept the current secret, or a not-yet-expired previous one (rotation grace).
            if !shared_secret_bearer_ok(&entry, &headers, now_epoch_ms()) {
                return http_error(
                    StatusCode::UNAUTHORIZED,
                    "UNAUTHORIZED",
                    "missing or invalid bearer secret",
                );
            }
        }
    }

    // 4. Body — inline only, capped; the envelope check below is the hard gate.
    let body = match to_bytes(req.into_body(), HTTP_ENVELOPE_INLINE_LIMIT_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return http_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "BODY_TOO_LARGE",
                "request body exceeds the inline limit",
            )
        }
    };
    let (body_inline, body_encoding) = if body.is_empty() {
        (None, None)
    } else {
        match std::str::from_utf8(&body) {
            Ok(text) => (Some(text.to_string()), Some(HttpBodyEncoding::Utf8)),
            // Non-UTF-8 bodies ride as base64 so the node can recover raw bytes.
            Err(_) => (Some(base64_encode(&body)), Some(HttpBodyEncoding::Base64)),
        }
    };

    // 5. Forward UNDER THE TARGET'S DECLARED FAMILY, BY NAME (Option A + Z, §3).
    //    No request-time resolution: the handler name was resolved at publish
    //    time and cached in the row; the edge just forwards by Unicast(name).
    let mut ctx = serde_json::Map::new();
    ctx.insert("method".to_string(), json!(method.as_str()));
    ctx.insert("path".to_string(), json!(normalize_extra_path(&extra)));
    if let Some(query) = uri.query() {
        ctx.insert("query".to_string(), json!(query));
    }
    let filtered = filter_request_headers(&headers);
    if !filtered.is_empty() {
        ctx.insert("headers".to_string(), json!(filtered));
    }

    // Body passthrough: JSON body → parsed JSON; else opaque string (utf8 or,
    // for non-UTF-8, `{ "body_base64": ... }`).
    let payload = match (body_inline, body_encoding) {
        (Some(text), Some(HttpBodyEncoding::Utf8)) => {
            serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text))
        }
        (Some(b64), Some(HttpBodyEncoding::Base64)) => json!({ "body_base64": b64 }),
        _ => Value::Null,
    };

    let message = Message {
        routing: Routing {
            src: state.sender.uuid().to_string(),
            // REQUIRED: the handler's reply routes back cross-hive BY NAME.
            src_l2_name: Some(state.sender.full_name().to_string()),
            // Option Z: forward by the pre-resolved owner L2 name (LSA cross-hive).
            dst: Destination::Unicast(entry.owner_l2_name.clone()),
            ttl: state.ttl,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            // Option A: the target's own declared family.
            msg_type: entry.inbound_family.clone(),
            // The channel identity (v6 §4): the owning node disambiguates which of
            // its channels this request is for. Opaque tag; the edge never resolves it.
            ich: Some(ich.clone()),
            // The edge's own a-fuego self label. Core is the first trusted identity
            // boundary and may re-derive/ignore it (v6 §6). NO dst_ilk (frontier, I6).
            src_ilk: Some(state.self_ilk.clone()),
            context: Some(Value::Object(ctx)),
            ..Meta::default()
        },
        payload,
    };
    if let Err(err) = ensure_http_envelope_within_limit(&message) {
        return http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "REQ_TOO_LARGE",
            &err.to_string(),
        );
    }

    // 6. Await the reply correlated PURELY by trace_id — the reply family is
    //    whatever the handler speaks, so match on the trace, not a fixed type.
    let labels = RpcRequestLabels::new("edge-invoke", &entry.inbound_family, "*");
    match state
        .dispatcher
        .send_with_matcher(message, edge_reply_matcher(), labels, state.timeout)
        .await
    {
        Ok(reply) => {
            // Transport-error shadowing guard: RouteMatch::Any success also matches
            // the router's own SYSTEM_KIND UNREACHABLE/TTL frames on this trace_id,
            // so they arrive as Ok(reply). Detect them before treating the reply as
            // a handler response.
            if is_system_kind(&reply.meta.msg_type)
                && matches!(
                    reply.meta.msg.as_deref(),
                    Some(MSG_UNREACHABLE) | Some(MSG_TTL_EXCEEDED)
                )
            {
                return http_error(
                    StatusCode::BAD_GATEWAY,
                    "HANDLER_UNREACHABLE",
                    "target unreachable or ttl exceeded",
                );
            }
            // Wrap the raw handler payload as the HTTP 200 body.
            let body = serde_json::to_vec(&reply.payload).unwrap_or_default();
            (
                StatusCode::OK,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
                .into_response()
        }
        Err(err) => {
            let status = StatusCode::from_u16(rpc_error_to_http_status(&err))
                .unwrap_or(StatusCode::BAD_GATEWAY);
            http_error(status, rpc_error_code(&err), &err.to_string())
        }
    }
}

/// Await the handler's reply correlated purely by `trace_id`. The reply family is
/// unknown (the handler answers in its own family), so success matches ANY type;
/// the router's transport frames are listed as terminal (and also caught by the
/// shadowing guard in `invoke`, since `Any` matches them first).
fn edge_reply_matcher() -> PendingMatcher {
    PendingMatcher::new(
        vec![RouteMatch::Any],
        vec![
            RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
            RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
        ],
        vec![],
    )
}

fn rpc_error_to_http_status(err: &RpcError) -> u16 {
    match err {
        RpcError::Timeout { .. } => 504,
        RpcError::Unreachable { .. }
        | RpcError::TtlExceeded { .. }
        | RpcError::Disconnected
        | RpcError::Node(_) => 502,
        _ => 502,
    }
}

fn rpc_error_code(err: &RpcError) -> &'static str {
    match err {
        RpcError::Timeout { .. } => "HANDLER_TIMEOUT",
        RpcError::Unreachable { .. } => "HANDLER_UNREACHABLE",
        RpcError::TtlExceeded { .. } => "HANDLER_TTL_EXCEEDED",
        RpcError::Disconnected => "ROUTER_DISCONNECTED",
        RpcError::Node(_) => "ROUTER_ERROR",
        RpcError::InvalidResponse(_) => "INVALID_HANDLER_RESPONSE",
        _ => "HANDLER_ERROR",
    }
}

/// Constant-time byte equality (length-independent) — FIX-7: the shared-secret bearer is compared
/// at the public DMZ frontier, where a short-circuiting `==` leaks the token prefix via timing.
/// No external crate: fold any length mismatch into the accumulator, then OR every byte diff.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff: u8 = if a.len() == b.len() { 0 } else { 1 };
    let n = a.len().max(b.len());
    for i in 0..n {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(|token| constant_time_eq(token.trim().as_bytes(), expected.as_bytes()))
        .unwrap_or(false)
}

/// How long a rotated shared-secret bearer stays accepted after a NEW secret lands, so a
/// live external client is not hard-cut with a 401 the instant the channel token rotates
/// (io.api edge-publication residual #1 — bearer rotation). RAM-only, best-effort.
const SHARED_SECRET_GRACE_MS: u64 = 10 * 60 * 1000; // 10 minutes

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A shared-secret entry authorizes the request iff its bearer matches the CURRENT secret,
/// or a not-yet-expired PREVIOUS secret (the rotation grace window). The previous secret is
/// only ever set for a brief window right after a rotation, so this widens acceptance for
/// minutes, not indefinitely — and only to the immediately-prior token, never an arbitrary one.
fn shared_secret_bearer_ok(entry: &EndpointEntry, headers: &HeaderMap, now_ms: u64) -> bool {
    // Reject empty secrets outright: an empty configured/rotated secret must NEVER authorize an
    // empty bearer (defensive — a vault value of {"secret":""} would otherwise open the channel
    // to `Authorization: Bearer `). Applies to both the current and the grace (previous) secret.
    if entry
        .secret
        .as_deref()
        .filter(|current| !current.is_empty())
        .is_some_and(|current| bearer_matches(headers, current))
    {
        return true;
    }
    matches!(
        (
            entry.previous_secret.as_deref(),
            entry.previous_secret_expires_at_ms,
        ),
        (Some(prev), Some(expiry))
            if !prev.is_empty() && now_ms < expiry && bearer_matches(headers, prev)
    )
}

/// Forward ONLY allowlisted request headers inward (§7.5/M2, fail-closed). Any header not
/// explicitly in `ALLOWED_REQUEST_HEADERS` — including `Authorization`, `Cookie`,
/// `X-Forwarded-*`, hop-by-hop headers, and any spoofed trust header — is dropped, so an
/// external caller can never influence or impersonate to the internal IO node. Names are
/// matched case-insensitively.
fn filter_request_headers(headers: &HeaderMap) -> Vec<HttpHeader> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            if !ALLOWED_REQUEST_HEADERS.contains(&name.as_str()) {
                return None;
            }
            let value = value.to_str().ok()?;
            Some(HttpHeader {
                name,
                value: value.to_string(),
            })
        })
        .collect()
}

fn normalize_extra_path(extra: &str) -> String {
    if extra.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", extra.trim_start_matches('/'))
    }
}

/// Minimal std-only base64 (standard alphabet, padded) for non-UTF-8 bodies.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn http_error(status: StatusCode, code: &str, message: &str) -> Response {
    let body = json!({ "ok": false, "error_code": code, "message": message }).to_string();
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_routes_round_trip_and_never_leak_the_secret() {
        let path =
            std::env::temp_dir().join(format!("sy-edge-persist-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut reg: HashMap<String, EndpointEntry> = HashMap::new();
        reg.insert(
            "ich:pub".into(),
            EndpointEntry {
                owner_l2_name: "IO.a@h".into(),
                inbound_family: "user".into(),
                auth_mode: AuthMode::Public,
                secret: None,
                secret_ref: None,
                previous_secret: None,
                previous_secret_expires_at_ms: None,
                methods: None,
                tenant_id: None,
            },
        );
        reg.insert(
            "ich:sec".into(),
            EndpointEntry {
                owner_l2_name: "IO.b@h".into(),
                inbound_family: "user".into(),
                auth_mode: AuthMode::SharedSecret,
                secret: Some("s3cr3t".into()),
                secret_ref: Some("edge_channel_secret:ich:sec".into()),
                previous_secret: None,
                previous_secret_expires_at_ms: None,
                methods: None,
                tenant_id: None,
            },
        );
        persist_registry(&path, &reg);

        // The routes survive a reload (warm-start). The secret VALUE never hit disk, but the
        // secret_ref (just a vault key name) DID — so the edge can re-fetch it at boot.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("s3cr3t"),
            "secret value leaked to the on-disk route table"
        );
        assert!(
            raw.contains("edge_channel_secret:ich:sec"),
            "secret_ref must be persisted"
        );
        let loaded = load_registry(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded["ich:pub"].owner_l2_name, "IO.a@h");
        assert_eq!(loaded["ich:sec"].auth_mode, AuthMode::SharedSecret);
        assert!(
            loaded["ich:sec"].secret.is_none(),
            "secret must reload as None (fetched from vault by ref)"
        );
        assert_eq!(
            loaded["ich:sec"].secret_ref.as_deref(),
            Some("edge_channel_secret:ich:sec")
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rpc_error_mapping_matches_spec_basics() {
        let err = RpcError::Disconnected;
        assert_eq!(rpc_error_to_http_status(&err), 502);
        assert_eq!(rpc_error_code(&err), "ROUTER_DISCONNECTED");
        let err = RpcError::Timeout {
            trace_id: "t".to_string(),
            target: "target".to_string(),
            request_msg: "req".to_string(),
            response_msg: "res".to_string(),
            timeout_ms: 30_000,
        };
        assert_eq!(rpc_error_to_http_status(&err), 504);
        assert_eq!(rpc_error_code(&err), "HANDLER_TIMEOUT");
    }

    #[test]
    fn registry_file_parses_ich_rows() {
        let raw = r#"{"endpoints":[
            {"ich":"ich:1111","owner_l2_name":"IO.cloud@motherbee","inbound_family":"user","auth_mode":"public","methods":["POST"]},
            {"ich":"ich:2222","owner_l2_name":"IO.web@motherbee","inbound_family":"text","auth_mode":"shared-secret","secret":"s3cr3t"}
        ]}"#;
        let parsed: EndpointsFile = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.endpoints.len(), 2);
        assert_eq!(parsed.endpoints[0].ich, "ich:1111");
        assert_eq!(parsed.endpoints[0].owner_l2_name, "IO.cloud@motherbee");
        assert_eq!(parsed.endpoints[0].inbound_family, "user");
        assert_eq!(parsed.endpoints[0].auth_mode, AuthMode::Public);
        assert_eq!(parsed.endpoints[1].auth_mode, AuthMode::SharedSecret);
        assert_eq!(parsed.endpoints[1].secret.as_deref(), Some("s3cr3t"));
    }

    #[test]
    fn row_to_entry_keys_by_ich_and_carries_fields() {
        // The EDGE_OPEN_URL payload IS one row; open_url upserts it by ich.
        let row: EndpointRow = serde_json::from_value(json!({
            "ich": " ich:1 ",
            "owner_l2_name": " IO.cloud@motherbee ",
            "inbound_family": " user ",
            "auth_mode": "shared-secret",
            "secret": "s3cr3t",
            "secret_ref": " edge_channel_secret:ich:1 ",
            "tenant_id": " tenant-a "
        }))
        .unwrap();
        let (ich, entry) = row_to_entry(row);
        assert_eq!(ich, "ich:1");
        assert_eq!(entry.owner_l2_name, "IO.cloud@motherbee");
        assert_eq!(entry.inbound_family, "user");
        assert_eq!(
            entry.secret_ref.as_deref(),
            Some("edge_channel_secret:ich:1")
        );
        assert_eq!(entry.tenant_id.as_deref(), Some("tenant-a"));

        // Two opens accumulate (upsert), not replace — a second URL does not wipe the first.
        let mut reg: HashMap<String, EndpointEntry> = HashMap::new();
        let (k1, e1) = row_to_entry(
            serde_json::from_value(json!({"ich":"ich:a","owner_l2_name":"IO.a@h","inbound_family":"user","auth_mode":"public"})).unwrap(),
        );
        reg.insert(k1, e1);
        let (k2, e2) = row_to_entry(
            serde_json::from_value(json!({"ich":"ich:b","owner_l2_name":"IO.b@h","inbound_family":"user","auth_mode":"public"})).unwrap(),
        );
        reg.insert(k2, e2);
        assert_eq!(reg.len(), 2);
        // close removes exactly one.
        reg.remove("ich:a");
        assert_eq!(reg.len(), 1);
        assert!(reg.contains_key("ich:b"));
    }

    #[test]
    fn rows_to_registry_indexes_by_ich() {
        let rows: Vec<EndpointRow> = serde_json::from_value(json!([
            {"ich":"ich:abc","owner_l2_name":"IO.cloud@motherbee","inbound_family":"user","auth_mode":"public"}
        ]))
        .unwrap();
        let reg = rows_to_registry(rows);
        let entry = reg.get("ich:abc").expect("indexed");
        assert_eq!(entry.owner_l2_name, "IO.cloud@motherbee");
        assert_eq!(entry.inbound_family, "user");
    }

    fn edge_command_from(src_l2_name: Option<&str>) -> Message {
        Message {
            routing: Routing {
                src: "src-uuid".to_string(),
                src_l2_name: src_l2_name.map(ToString::to_string),
                dst: Destination::Unicast("SY.edge@ingress1".to_string()),
                ttl: 16,
                trace_id: "trace".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_EDGE_OPEN_URL.to_string()),
                ..Meta::default()
            },
            payload: Value::Null,
        }
    }

    #[test]
    fn edge_service_commands_require_motherbee_admin_origin() {
        assert!(authorize_edge_service_command(
            &edge_command_from(Some("SY.admin@motherbee")),
            MSG_EDGE_OPEN_URL
        )
        .is_ok());
        assert!(authorize_edge_service_command(
            &edge_command_from(Some("SY.admin@ingress1")),
            MSG_EDGE_OPEN_URL
        )
        .is_err());
        assert!(authorize_edge_service_command(
            &edge_command_from(Some("IO.cloud@motherbee")),
            MSG_EDGE_OPEN_URL
        )
        .is_err());
        assert!(
            authorize_edge_service_command(&edge_command_from(None), MSG_EDGE_OPEN_URL).is_err()
        );
    }

    #[test]
    fn open_url_payload_rejects_non_io_owner_and_system_family() {
        let good: EndpointRow = serde_json::from_value(json!({
            "ich": "ich:ok",
            "owner_l2_name": "IO.cloud@motherbee",
            "inbound_family": "user",
            "auth_mode": "public"
        }))
        .unwrap();
        assert!(validate_endpoint_row_for_open(&good).is_ok());

        let bad_owner: EndpointRow = serde_json::from_value(json!({
            "ich": "ich:bad",
            "owner_l2_name": "SY.identity@motherbee",
            "inbound_family": "user",
            "auth_mode": "public"
        }))
        .unwrap();
        assert!(validate_endpoint_row_for_open(&bad_owner).is_err());

        let system_family: EndpointRow = serde_json::from_value(json!({
            "ich": "ich:bad",
            "owner_l2_name": "IO.cloud@motherbee",
            "inbound_family": " System ",
            "auth_mode": "public"
        }))
        .unwrap();
        assert!(validate_endpoint_row_for_open(&system_family).is_err());

        let no_secret: EndpointRow = serde_json::from_value(json!({
            "ich": "ich:bad",
            "owner_l2_name": "IO.cloud@motherbee",
            "inbound_family": "user",
            "auth_mode": "shared-secret",
            "secret_ref": "   "
        }))
        .unwrap();
        assert!(validate_endpoint_row_for_open(&no_secret).is_err());
    }

    #[test]
    fn bearer_matches_only_on_exact_secret() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer s3cr3t".parse().unwrap());
        assert!(bearer_matches(&headers, "s3cr3t"));
        assert!(!bearer_matches(&headers, "wrong"));
        let mut lower = HeaderMap::new();
        lower.insert("authorization", "bearer s3cr3t".parse().unwrap());
        assert!(bearer_matches(&lower, "s3cr3t"));
        assert!(!bearer_matches(&HeaderMap::new(), "s3cr3t"));
    }

    #[test]
    fn shared_secret_grace_window_accepts_previous_until_expiry() {
        // io.api residual #1: after a bearer rotation the PREVIOUS secret stays valid until
        // its expiry, so a live client is not hard-cut; after expiry only the current works.
        let entry = EndpointEntry {
            owner_l2_name: "IO.b@h".into(),
            inbound_family: "user".into(),
            auth_mode: AuthMode::SharedSecret,
            secret: Some("new-token".into()),
            secret_ref: Some("edge_channel_secret:ich:sec".into()),
            previous_secret: Some("old-token".into()),
            previous_secret_expires_at_ms: Some(1_000),
            methods: None,
            tenant_id: None,
        };
        let with = |bearer: &str| {
            let mut h = HeaderMap::new();
            h.insert("authorization", format!("Bearer {bearer}").parse().unwrap());
            h
        };

        // Current token: always accepted (before and after the grace expiry).
        assert!(shared_secret_bearer_ok(&entry, &with("new-token"), 500));
        assert!(shared_secret_bearer_ok(&entry, &with("new-token"), 5_000));
        // Previous token: accepted while now < expiry, rejected once expired.
        assert!(shared_secret_bearer_ok(&entry, &with("old-token"), 999));
        assert!(!shared_secret_bearer_ok(&entry, &with("old-token"), 1_000));
        assert!(!shared_secret_bearer_ok(&entry, &with("old-token"), 5_000));
        // An unrelated token is never accepted.
        assert!(!shared_secret_bearer_ok(&entry, &with("bogus"), 500));

        // With no previous secret, only the current one authorizes.
        let no_prev = EndpointEntry {
            previous_secret: None,
            previous_secret_expires_at_ms: None,
            ..entry.clone()
        };
        assert!(shared_secret_bearer_ok(&no_prev, &with("new-token"), 500));
        assert!(!shared_secret_bearer_ok(&no_prev, &with("old-token"), 500));

        // An EMPTY secret (current or previous) never authorizes an empty bearer.
        let empty = EndpointEntry {
            secret: Some(String::new()),
            previous_secret: Some(String::new()),
            previous_secret_expires_at_ms: Some(1_000),
            ..entry.clone()
        };
        let mut empty_bearer = HeaderMap::new();
        empty_bearer.insert("authorization", "Bearer ".parse().unwrap());
        assert!(!shared_secret_bearer_ok(&empty, &empty_bearer, 500));
    }

    #[test]
    fn filter_request_headers_allowlist_is_fail_closed() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer x".parse().unwrap());
        headers.insert("host", "edge.fluxbee.ai".parse().unwrap());
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("cookie", "session=abc".parse().unwrap());
        headers.insert("x-forwarded-for", "10.0.0.1".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-custom", "smuggled".parse().unwrap());
        let filtered = filter_request_headers(&headers);
        let names: Vec<&str> = filtered.iter().map(|h| h.name.as_str()).collect();
        // Allowlisted -> passes inward.
        assert!(names.contains(&"content-type"));
        // Everything NOT named — the raw bearer, hop-by-hop, and any attacker-settable
        // header — is dropped by default (fail-closed). x-custom must NOT smuggle through.
        assert!(!names.contains(&"authorization"));
        assert!(!names.contains(&"host"));
        assert!(!names.contains(&"connection"));
        assert!(!names.contains(&"cookie"));
        assert!(!names.contains(&"x-forwarded-for"));
        assert!(!names.contains(&"x-custom"));
    }

    #[test]
    fn inflight_bound_sheds_load_at_capacity() {
        // The edge holds one semaphore permit per in-flight request; at the cap the next
        // acquisition fails, which invoke() turns into a fast 503. This locks that contract
        // (the full HTTP-path 503 is exercised in the lab flood test). try_acquire_owned is
        // non-blocking, so no async runtime is needed here.
        let sem = Arc::new(Semaphore::new(2));
        let p1 = Arc::clone(&sem).try_acquire_owned().expect("permit 1");
        let _p2 = Arc::clone(&sem).try_acquire_owned().expect("permit 2");
        assert!(
            Arc::clone(&sem).try_acquire_owned().is_err(),
            "at capacity the (cap+1)th request must be shed (-> 503)"
        );
        drop(p1); // request completes -> slot frees -> next admitted
        assert!(Arc::clone(&sem).try_acquire_owned().is_ok());
        assert!(DEFAULT_MAX_INFLIGHT > 0, "the default cap must be positive");
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn normalize_extra_path_shapes_subpath() {
        assert_eq!(normalize_extra_path(""), "/");
        assert_eq!(normalize_extra_path("webhooks/stripe"), "/webhooks/stripe");
        assert_eq!(normalize_extra_path("/already"), "/already");
    }

    fn sample_public_row(data: &[u8]) -> PublicArtifactRow {
        let sha256 = format!("{:x}", Sha256::digest(data));
        PublicArtifactRow {
            key: "ab".repeat(32),
            publication_id: format!("pub:{}", Uuid::new_v4()),
            public_name: sha256.clone(),
            sha256,
            size: data.len() as u64,
            content_type: "text/html; charset=utf-8".to_string(),
            presentation: "inline".to_string(),
            expires_at: u64::MAX,
            content_policy: "sandboxed-html-v1".to_string(),
        }
    }

    #[test]
    fn public_registry_round_trips_and_file_facts_are_verified() {
        let root = std::env::temp_dir().join(format!("sy-edge-public-{}", Uuid::new_v4()));
        let public_root = root.join("public");
        let registry_path = root.join("state/publications.json");
        std::fs::create_dir_all(&public_root).unwrap();
        let data = b"<!doctype html><script>document.body.textContent='ok'</script>";
        let row = sample_public_row(data);
        std::fs::write(public_root.join(&row.public_name), data).unwrap();
        assert!(validate_public_artifact_row(&row).is_ok());
        verify_public_artifact_file(&public_root, &row).unwrap();

        let mut registry = HashMap::new();
        registry.insert(row.key.clone(), row.clone());
        persist_public_registry(&registry_path, &registry).unwrap();
        assert_eq!(
            load_public_registry(&registry_path).get(&row.key),
            Some(&row)
        );

        std::fs::write(public_root.join(&row.public_name), b"changed").unwrap();
        assert!(verify_public_artifact_file(&public_root, &row).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn public_byte_ranges_accept_one_bounded_range() {
        assert_eq!(parse_single_byte_range(None, 100), Ok((0, 99, false)));
        assert_eq!(
            parse_single_byte_range(Some("bytes=10-19"), 100),
            Ok((10, 19, true))
        );
        assert_eq!(
            parse_single_byte_range(Some("bytes=-10"), 100),
            Ok((90, 99, true))
        );
        assert_eq!(
            parse_single_byte_range(Some("bytes=95-"), 100),
            Ok((95, 99, true))
        );
        assert!(parse_single_byte_range(Some("bytes=0-1,4-5"), 100).is_err());
        assert!(parse_single_byte_range(Some("bytes=100-"), 100).is_err());
        assert!(parse_single_byte_range(Some("bytes=0-0"), 0).is_err());
    }

    #[test]
    fn resolve_http_listen_is_fail_closed_and_role_gated() {
        let l = || Some("0.0.0.0:443".to_string());
        // role=ingress + edge listen => bind
        assert_eq!(resolve_http_listen(None, Some("ingress"), l()), l());
        assert_eq!(resolve_http_listen(None, Some(" Ingress "), l()), l());
        // non-ingress roles never bind from config (fail-closed)
        assert_eq!(resolve_http_listen(None, Some("motherbee"), l()), None);
        assert_eq!(resolve_http_listen(None, Some("worker"), l()), None);
        assert_eq!(resolve_http_listen(None, None, l()), None);
        // ingress but no edge.listen => still None
        assert_eq!(resolve_http_listen(None, Some("ingress"), None), None);
        // env override wins regardless of role (dev/test escape hatch)
        assert_eq!(
            resolve_http_listen(Some("0.0.0.0:8443".to_string()), Some("motherbee"), None),
            Some("0.0.0.0:8443".to_string())
        );
    }
}
