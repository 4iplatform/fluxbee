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
    is_system_kind, Destination, Message, Meta, Routing, VaultSecretChangedPayload, VaultSecretOp,
    MSG_EDGE_ACME_CHALLENGE, MSG_EDGE_ACME_CHALLENGE_RESPONSE, MSG_EDGE_CLOSE_URL,
    MSG_EDGE_CLOSE_URL_RESPONSE, MSG_EDGE_LIST_URLS, MSG_EDGE_LIST_URLS_RESPONSE,
    MSG_EDGE_OPEN_URL, MSG_EDGE_OPEN_URL_RESPONSE, MSG_EDGE_PUBLISH_BLOB,
    MSG_EDGE_PUBLISH_BLOB_RESPONSE, MSG_EDGE_TLS_RELOAD, MSG_EDGE_TLS_RELOAD_RESPONSE,
    MSG_EDGE_TLS_STATUS, MSG_EDGE_TLS_STATUS_RESPONSE, MSG_EDGE_UNPUBLISH_BLOB,
    MSG_EDGE_UNPUBLISH_BLOB_RESPONSE, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_VAULT_SECRET_CHANGED,
    SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, is_node_config_get_message, is_node_config_set_message,
    parse_node_config_request, try_handle_default_node_status, NodeConfig,
    NodeConfigControlRequest, NodeSender, NodeUuidMode, OperationalRouteProfile, PendingMatcher,
    RouteMatch, RouteTarget, RouterDispatcher, RpcCommandReceiver, RpcError, RpcRequestLabels,
};
use json_router::tls_material::{self, ChainDiagnosis, MaterialError};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
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
/// The three TLS commands, taken off the system channel so the sequential loop never delays them.
const RPC_CH_TLS: &str = "tls";

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
        .with(tracing_subscriber::fmt::layer().with_ansi(fluxbee_sdk::logging::ansi()))
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

    // The public listener's TLS (docs/acme-edge-certificates-v1.md §4), shared by the acceptor,
    // the TLS command task, the vault notices of the loop below and the awaiting re-reads. `None`
    // on a role with no public door: such an edge still receives vault notices and must NOT react
    // to them, and it answers the TLS commands with an error.
    let mut edge_tls: Option<Arc<EdgeTls>> = None;

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
        // TLS: vault-sourced (preferred), on-disk PEM, or explicit plaintext. FAIL-CLOSED: a TLS
        // configuration that cannot work never falls back to plaintext on the public listener.
        // A vault edge never exits for its secret, though: it starts AWAITING (§4).
        let mode = TlsMode::resolve(
            config.tls_vault_key.as_deref(),
            config.tls_cert.as_deref(),
            config.tls_key.as_deref(),
        )
        .map_err(|err| {
            tracing::error!(error = %err, "sy-edge: invalid TLS configuration; refusing to bind the public listener");
            err
        })?;
        let tls = match mode {
            TlsMode::Plaintext => Arc::new(EdgeTls::plaintext()),
            TlsMode::Disk { cert, key } => match EdgeTls::from_disk(cert, key, now_unix_secs()) {
                Ok(tls) => Arc::new(tls),
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "sy-edge: TLS requested but the cert files did not load; refusing to bind the public listener in plaintext (fail-closed). Fix the files; systemd restarts in ~5s."
                    );
                    return Err(err.into());
                }
            },
            TlsMode::Vault { key } => {
                tracing::info!(vault_hive = %config.vault_hive, key = %key, ilk = %self_ilk, "sy-edge reading its TLS certificate from the vault");
                let source = VaultSecretSource {
                    client: fluxbee_sdk::VaultClient::new(
                        Arc::clone(&dispatcher),
                        config.vault_hive.clone(),
                        fluxbee_sdk::VaultCallerOwned::new(self_ilk.clone(), self_name.clone()),
                    ),
                    timeout: VAULT_TLS_READ_TIMEOUT,
                };
                let tls = Arc::new(EdgeTls::vault(key, Arc::new(source)));
                start_vault_tls(&tls, AWAITING_SCHEDULE).await;
                tls
            }
        };
        edge_tls = Some(Arc::clone(&tls));
        tokio::spawn(async move {
            if let Err(err) = run_frontend(listen, state, tls).await {
                tracing::error!(error = %err, "sy-edge public frontend exited");
            }
        });
    }

    // The TLS commands answer beside the sequential loop below (§4): EDGE_PUBLISH_BLOB can hold
    // that loop for a minute, and SY.admin's ACME order must not wait behind it.
    let tls_commands = dispatcher
        .take_command_receiver(RPC_CH_TLS)
        .await
        .map_err(|err| format!("sy-edge tls receiver: {err}"))?;
    tokio::spawn(run_tls_commands(
        tls_commands,
        sender.clone(),
        edge_tls.clone(),
        config.ttl,
    ));

    // FIX-6 (part 2): active reaper. Part 1 drops expired public-artifact rows at LOAD; this
    // periodically prunes them from the live registry AND persists the pruned ledger, so a
    // long-lived edge doesn't accumulate expired rows (bounds the served set + on-disk ledger).
    // Scope: the EDGE's own state only — reclaiming the public/ bytes and retiring the
    // authoritative admin ledger is SY.admin's expiry sweep (run_publication_expiry_sweep), which
    // drives MSG_EDGE_UNPUBLISH_BLOB (idempotent here — a row this reaper already dropped just
    // replies removed:false) + MSG_BLOB_RELEASE. On a non-ingress edge the registry is empty, so
    // this is a cheap no-op.
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
                    tracing::info!(
                        removed = before - guard.len(),
                        "sy-edge reaper: dropped expired public artifacts"
                    );
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
                        // The TLS certificate is NOT covered by resolve_secrets. A change to the
                        // edge's own key (put, rotate, rollback, or SY.vault re-announcing at its
                        // boot) does what EDGE_TLS_RELOAD does: one re-read, the shared verdict, a
                        // swap in place when it passes (PB-1, docs/acme-edge-certificates-v1.md
                        // §4). Nothing exits. It runs in its own task, so this loop never waits on
                        // the vault for it.
                        let is_bootstrap = msg.meta.action.as_deref()
                            == Some(fluxbee_sdk::protocol::VAULT_BOOTSTRAP_ACTION);
                        let live = edge_tls
                            .as_ref()
                            .map(|tls| tls.live_material())
                            .unwrap_or(LiveTlsMaterial::NotFromVault);
                        let tls_key = edge_tls
                            .as_ref()
                            .and_then(|tls| tls.mode.vault_key())
                            .map(str::to_string);
                        match tls_secret_change_action(
                            &msg.payload,
                            tls_key.as_deref(),
                            &live,
                            is_bootstrap,
                        ) {
                            TlsSecretChange::Reload {
                                key,
                                op,
                                version,
                                bootstrap,
                            } => {
                                let (live_version, live_fingerprint) = match &live {
                                    LiveTlsMaterial::FromVault {
                                        version,
                                        fingerprint,
                                    } => (*version, Some(fingerprint.as_str())),
                                    _ => (None, None),
                                };
                                tracing::info!(
                                    key = %key,
                                    op = %op,
                                    version,
                                    bootstrap,
                                    live_version = ?live_version,
                                    live_fingerprint = ?live_fingerprint,
                                    "sy-edge: TLS secret changed in the vault; re-reading it in place"
                                );
                                if let Some(tls) = edge_tls.clone() {
                                    let trigger = format!("VAULT_SECRET_CHANGED {op}");
                                    tokio::spawn(async move {
                                        let _ = tls.reload(&trigger).await;
                                    });
                                }
                            }
                            TlsSecretChange::DeletedKeepServing { key } => {
                                tracing::error!(
                                    key = %key,
                                    "sy-edge: TLS cert DELETED from the vault; still serving the certificate already loaded (nothing restarts). Restore the secret, or let SY.admin issue one."
                                );
                            }
                            TlsSecretChange::Malformed { error } => {
                                tracing::warn!(
                                    error = %error,
                                    "sy-edge: malformed VAULT_SECRET_CHANGED payload; cannot tell whether the TLS cert changed"
                                );
                            }
                            TlsSecretChange::Unrelated => {}
                        }
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
        .command_channel(RPC_CH_TLS)
        // The TLS commands get their own channel, consumed beside the sequential system loop
        // (`run_tls_commands`). Commands, never responses: they go before any pending waiter.
        .pre_pending_rule(
            RouteMatch::one_of(
                SYSTEM_KIND,
                [
                    MSG_EDGE_TLS_STATUS,
                    MSG_EDGE_TLS_RELOAD,
                    MSG_EDGE_ACME_CHALLENGE,
                ],
            ),
            RouteTarget::Command(RPC_CH_TLS),
        )
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
            // EVERY restart, growing it unbounded and pinning the referenced public/ bytes. This
            // bounds the edge's in-memory registry; the active reaper (part 2, below) also prunes
            // the on-disk ledger on a live edge. Reclaiming the public/ BYTES + retiring the
            // authoritative admin ledger is SY.admin's job — its expiry sweep drives the same
            // MSG_EDGE_UNPUBLISH_BLOB + MSG_BLOB_RELEASE path at TTL (run_publication_expiry_sweep).
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
        Some("") => Err(format!("{action} requires router-stamped source")),
        Some(src) => Err(format!("{action} not authorized from {src}")),
        None => Err(format!("{action} requires router-stamped source")),
    }
}

fn validate_endpoint_row_for_open(row: &EndpointRow) -> Result<(), String> {
    if row.ich.trim().is_empty() {
        return Err("ich must not be empty".to_string());
    }
    let family = row.inbound_family.trim();
    if family.is_empty() {
        return Err("inbound_family must not be empty".to_string());
    }
    if family.eq_ignore_ascii_case(SYSTEM_KIND) {
        return Err("inbound_family must not be system".to_string());
    }
    // A FANOUT row targets an IO.* family GLOB (no single owner); it needs a verify-token for the
    // connection-handshake challenge (or a ref to fetch it). auth is Public (the provider sends no
    // bearer; per-message auth is the downstream HMAC), so there is no secret requirement here.
    if let Some(fanout) = row
        .fanout_family
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !fanout.starts_with("IO.") || !fanout.contains('@') {
            return Err(format!(
                "fanout_family must be a fully-qualified IO.* glob, got '{fanout}'"
            ));
        }
        if !non_empty_str(row.verify_token.as_deref())
            && !non_empty_str(row.verify_token_ref.as_deref())
        {
            return Err("fanout endpoints require verify_token or verify_token_ref".to_string());
        }
        return Ok(());
    }
    // Ordinary unicast row: a single fully-qualified IO.* owner.
    let owner = row.owner_l2_name.trim();
    if owner.is_empty() || !owner.starts_with("IO.") || !owner.contains('@') {
        return Err(format!(
            "owner_l2_name must be a fully-qualified IO.* node, got '{owner}'"
        ));
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
/// change the TLS mode or its source, change NIC need a process restart; a new certificate under
/// the same vault key does not: EDGE_TLS_RELOAD). This is the edge configuring ITSELF — never the
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
/// needs from it. Other sections (wan/blob/…) are ignored — NO
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
    /// FANOUT endpoint (IO.wapp): instead of forwarding to a single `owner_l2_name`, the edge acks the
    /// external caller immediately and BROADCASTs the request to every node matching this L2 glob
    /// (e.g. `IO.wapp.*@motherbee`), which each verify + self-select. `None` = ordinary unicast row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fanout_family: Option<String>,
    /// The resolved webhook verify-token VALUE (RAM only, NEVER persisted — like `secret`). Used to
    /// answer a provider's connection-handshake challenge (WhatsApp GET `hub.challenge`) AT the edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verify_token: Option<String>,
    /// Vault key of the verify-token (persisted — just a name). At boot/open the edge re-fetches the
    /// VALUE into `verify_token`, so it never touches disk (mirrors `secret_ref`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verify_token_ref: Option<String>,
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
    tenant_id: Option<String>,
    /// Fanout L2 glob (e.g. `IO.wapp.*@motherbee`); `Some` = this is a fanout endpoint.
    fanout_family: Option<String>,
    /// Resolved verify-token VALUE (RAM only) for the connection-handshake challenge.
    verify_token: Option<String>,
    /// Vault key to (re-)fetch `verify_token` from — the durable, disk-safe form.
    verify_token_ref: Option<String>,
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
            fanout_family: row
                .fanout_family
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            verify_token: row.verify_token,
            verify_token_ref: row
                .verify_token_ref
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
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
            fanout_family: e.fanout_family.clone(),
            verify_token: None, // NEVER persist the verify-token VALUE (RAM only, like secret)
            verify_token_ref: e.verify_token_ref.clone(), // persist just the vault ref
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
    tls: Arc<EdgeTls>,
) -> Result<(), SyEdgeError> {
    let app = Router::new()
        .route("/e/:ich", any(invoke_root))
        .route("/e/:ich/*extra", any(invoke_extra))
        .route("/public/:key", any(serve_public_artifact))
        .route("/healthz", any(|| async { "ok" }))
        .with_state(state);
    let listener = TcpListener::bind(&listen).await?;
    let addr = listener.local_addr()?;
    if tls.mode == TlsMode::Plaintext {
        tracing::info!(addr = %addr, "sy-edge public HTTP frontend ready (plaintext)");
        axum::serve(listener, app).await?;
        return Ok(());
    }
    tracing::info!(
        addr = %addr,
        mode = tls.mode.as_str(),
        serving = tls.is_serving(),
        "sy-edge public HTTPS frontend ready (TLS)"
    );
    serve_tls_listener(
        listener,
        app,
        tls,
        DoorLimits::new(MAX_TLS_CONNECTIONS, HTTP_HEADER_READ_TIMEOUT),
    )
    .await
}

// ---------------------------------------------------------------------------
// TLS (docs/acme-edge-certificates-v1.md §4): the mode, one acceptor that chooses a
// configuration per connection, the ACME TLS-ALPN-01 challenge, hot reload (PB-1) and the
// AWAITING state of a vault edge with no usable secret.
// ---------------------------------------------------------------------------

/// ONE timeout for reading the ClientHello and finishing the handshake, on every path: a client
/// that connects and sends nothing, or stops halfway, never holds a task longer than this.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// The ALPN protocol of the TLS-ALPN-01 challenge (RFC 8737).
const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";
const ACME_CHALLENGE_DEFAULT_TTL_SECS: u64 = 600;
const ACME_CHALLENGE_MAX_TTL_SECS: u64 = 1800;
/// A reload reads the vault ONCE, with this timeout. No probing loop on this path: SY.admin, a
/// vault notice or the awaiting re-reads ask again.
const VAULT_TLS_READ_TIMEOUT: Duration = Duration::from_secs(15);
/// When an awaiting edge re-reads its secret: every 3 s for the first 2 minutes while the vault
/// does not answer (a cold boot; before AWAITING the unit restarted every 5 s), otherwise after
/// 30 s, then twice as late each time, up to 5 min.
const AWAITING_SCHEDULE: AwaitingSchedule = AwaitingSchedule {
    transient: Duration::from_secs(3),
    transient_window: Duration::from_secs(120),
    first: Duration::from_secs(30),
    max: Duration::from_secs(300),
};
/// The most connections the public TLS door holds at once, handshakes included; one more is
/// closed at accept.
const MAX_TLS_CONNECTIONS: usize = 4096;
/// HTTP/1: a request head must arrive within this, and so must the first byte after the
/// handshake. hyper applies it only with a timer (`serve_tls_connection`).
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// After a failed accept (out of file descriptors, say) the loop waits this long, never spins.
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
/// Connections refused at the cap are logged at most once per interval, with their count.
const REFUSED_CONNECTIONS_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// How the public listener does TLS. Only a `Vault` edge reloads its certificate and answers
/// ACME challenges; the other modes report themselves in `EDGE_TLS_STATUS`, and SY.admin leaves
/// them alone.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TlsMode {
    /// No TLS: an `allow_plaintext` ingress (`add_hive` refuses it on port 443), or a dev listener.
    Plaintext,
    /// `edge.tls_cert` + `edge.tls_key` from disk, read once at boot.
    Disk { cert: PathBuf, key: PathBuf },
    /// `{cert, key}` from SY.vault under `edge.tls_vault_key`, read with the edge's own ILK.
    Vault { key: String },
}

impl TlsMode {
    /// The mode the configuration asks for. A vault key wins over disk files. An empty vault key,
    /// or a disk pair with one half missing, is refused: never served as plaintext.
    fn resolve(
        vault_key: Option<&str>,
        cert: Option<&std::path::Path>,
        key: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        if let Some(vault_key) = vault_key {
            let vault_key = vault_key.trim();
            if vault_key.is_empty() {
                return Err("edge.tls_vault_key is empty".to_string());
            }
            return Ok(Self::Vault {
                key: vault_key.to_string(),
            });
        }
        match (cert, key) {
            (Some(cert), Some(key)) => Ok(Self::Disk {
                cert: cert.to_path_buf(),
                key: key.to_path_buf(),
            }),
            (None, None) => Ok(Self::Plaintext),
            (Some(_), None) => Err("edge.tls_cert is set without edge.tls_key".to_string()),
            (None, Some(_)) => Err("edge.tls_key is set without edge.tls_cert".to_string()),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Plaintext => "plaintext",
            Self::Disk { .. } => "disk",
            Self::Vault { .. } => "vault",
        }
    }

    fn vault_key(&self) -> Option<&str> {
        match self {
            Self::Vault { key } => Some(key),
            _ => None,
        }
    }
}

/// Why a vault edge has no usable certificate, typed so SY.admin can act on it (§4, §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TlsVerdict {
    /// The vault has no secret under the key (`KEY_NOT_FOUND`).
    NotFound,
    /// The vault refused this edge (`UNAUTHORIZED`): a wrong owner, or an inactive tenant.
    Forbidden,
    /// The secret itself is wrong: no `cert`/`key`, a chain that fails the shared verdict
    /// (truncated, expired, not yet valid, key mismatch, unparseable, leaf only when it would
    /// replace a served certificate), or a request or stored value the vault calls invalid.
    InvalidMaterial,
    /// No verdict on the secret: the vault did not answer (`vault_failure_is_transport`), or it
    /// could not do its job (`IDENTITY_UNAVAILABLE`, `STORAGE_ERROR`, `IO_ERROR`,
    /// `ENCRYPTION_ERROR`, `NODE_ERROR`, `MASTER_KEY_NOT_AVAILABLE`, any code it may add). A good
    /// certificate is never rolled back for a busy SQLite.
    Transient,
}

impl TlsVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Forbidden => "forbidden",
            Self::InvalidMaterial => "invalid_material",
            Self::Transient => "transient",
        }
    }
}

/// A refused read or reload: the typed verdict, and a line an operator can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TlsRefusal {
    verdict: TlsVerdict,
    detail: String,
}

impl TlsRefusal {
    fn material(detail: impl Into<String>) -> Self {
        Self {
            verdict: TlsVerdict::InvalidMaterial,
            detail: detail.into(),
        }
    }

    /// Keep the vault's verdict typed. Only an answer about the secret itself is `not_found`,
    /// `forbidden` or `invalid_material`; everything else says the vault could not do its job,
    /// which is `transient` (an unknown code included: a retry costs a delay, a wrong refusal
    /// costs SY.admin rolling back a good certificate).
    fn from_vault_error(secret_key: &str, err: fluxbee_sdk::VaultError) -> Self {
        let verdict = if fluxbee_sdk::vault_failure_is_transport(&err) {
            TlsVerdict::Transient
        } else {
            match &err {
                fluxbee_sdk::VaultError::Service { code, .. } => match code.as_str() {
                    "KEY_NOT_FOUND" => TlsVerdict::NotFound,
                    "UNAUTHORIZED" => TlsVerdict::Forbidden,
                    "INVALID_REQUEST" | "INVALID_VALUE" => TlsVerdict::InvalidMaterial,
                    _ => TlsVerdict::Transient,
                },
                // An answer that does not parse says nothing about the secret.
                fluxbee_sdk::VaultError::Json(_) => TlsVerdict::Transient,
                _ => TlsVerdict::InvalidMaterial,
            }
        };
        Self {
            verdict,
            detail: format!("vault get '{secret_key}' failed: {err}"),
        }
    }

    fn to_json(&self) -> Value {
        json!({ "reason": self.verdict.as_str(), "detail": self.detail })
    }
}

fn material_refusal(err: MaterialError) -> TlsRefusal {
    TlsRefusal::material(err.to_string())
}

/// The edge's `{cert, key}` secret as the vault holds it, with the identity of the write it came
/// from: SY.vault's rollback re-mints version numbers, so the version and the metadata's
/// `updated_at` together name one write.
#[derive(Debug, Clone)]
struct VaultTlsSecret {
    cert: String,
    key: String,
    version: Option<i64>,
    updated_at: Option<String>,
}

/// Where a vault edge reads its certificate: SY.vault in production, a fake in the tests.
#[async_trait::async_trait]
trait TlsSecretSource: Send + Sync {
    async fn read(&self, key: &str) -> Result<VaultTlsSecret, TlsRefusal>;
}

/// `SY.vault@<vault_hive>`, read with the edge's own ILK: the dedicated-owner match, so no
/// identity SHM is needed on the ingress hive.
struct VaultSecretSource {
    client: fluxbee_sdk::VaultClient,
    timeout: Duration,
}

#[async_trait::async_trait]
impl TlsSecretSource for VaultSecretSource {
    async fn read(&self, key: &str) -> Result<VaultTlsSecret, TlsRefusal> {
        let resp = self
            .client
            .get(key, self.timeout)
            .await
            .map_err(|err| TlsRefusal::from_vault_error(key, err))?;
        let value = resp.value.ok_or_else(|| {
            TlsRefusal::material("vault returned no value for the edge tls secret")
        })?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    TlsRefusal::material(format!("edge tls secret is missing a '{name}' PEM field"))
                })
        };
        // SY.vault answers a VAULT_GET with the record's metadata, the same a VAULT_GET_METADATA
        // returns (`handle_get` in sy_vault.rs), and every write sets its `updated_at`. A second
        // call could only repeat it, and the router's edge gate admits only VAULT_GET.
        let updated_at = resp
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.updated_at.clone());
        if updated_at.is_none() {
            tracing::warn!(key = %key, "sy-edge: the vault's answer carries no updated_at; vault_updated_at stays null");
        }
        Ok(VaultTlsSecret {
            cert: field("cert")?,
            key: field("key")?,
            version: resp.version,
            updated_at,
        })
    }
}

/// The certificate the main configuration serves, and what `EDGE_TLS_STATUS` says about it.
#[derive(Clone)]
struct ServedTls {
    config: Arc<rustls::ServerConfig>,
    leaf: tls_material::LeafInfo,
    leaf_pem: String,
    /// The whole chain served. A re-put that only fixes the intermediates keeps the leaf's
    /// fingerprint and is still a change.
    chain: Vec<CertificateDer<'static>>,
    /// The vault write it came from, as the last read found it: the secret's version and its
    /// metadata's `updated_at`. Both `None` from disk.
    vault_version: Option<i64>,
    vault_updated_at: Option<String>,
    /// Unix seconds.
    loaded_at: i64,
}

enum TlsState {
    /// Plaintext mode: no TLS at all.
    Off,
    Serving(Arc<ServedTls>),
    /// A vault edge with no usable secret: armed ACME handshakes are answered, every other TLS
    /// handshake is dropped, and nothing is ever served in plaintext.
    Awaiting(TlsRefusal),
}

struct ArmedChallenge {
    /// Lowercase hex, so a clear names exactly the arm it means.
    digest: String,
    config: Arc<rustls::ServerConfig>,
    /// Unix seconds.
    expires_at: i64,
}

/// The `EDGE_ACME_CHALLENGE` answer.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChallengeAnswer {
    name: String,
    armed: bool,
    expires_at: Option<i64>,
}

/// What one reload did.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReloadOutcome {
    /// The served chain changed (or the edge left AWAITING).
    changed: bool,
    refused: Option<TlsRefusal>,
}

/// The public listener's TLS: its mode, the main configuration it serves now (or why it has
/// none), and the armed ACME challenges. Shared by the acceptor, which reads it once per new
/// connection, the TLS command task, the vault notices of the system loop and the awaiting
/// re-reads.
struct EdgeTls {
    mode: TlsMode,
    /// Where a `Vault` edge reads its secret; `None` for the other modes.
    source: Option<Arc<dyn TlsSecretSource>>,
    state: RwLock<TlsState>,
    /// Armed TLS-ALPN-01 challenges by normalized name, in memory only.
    challenges: std::sync::Mutex<HashMap<String, ArmedChallenge>>,
    /// One reload at a time: a command, a vault notice and an awaiting re-read never race a swap.
    reload_lock: tokio::sync::Mutex<()>,
    /// Set once the first read finished, so its outcome is always logged at its full level.
    read_once: std::sync::atomic::AtomicBool,
    handshake_timeout: Duration,
}

impl EdgeTls {
    fn new(mode: TlsMode, source: Option<Arc<dyn TlsSecretSource>>, state: TlsState) -> Self {
        Self {
            mode,
            source,
            state: RwLock::new(state),
            challenges: std::sync::Mutex::new(HashMap::new()),
            reload_lock: tokio::sync::Mutex::new(()),
            read_once: std::sync::atomic::AtomicBool::new(false),
            handshake_timeout: TLS_HANDSHAKE_TIMEOUT,
        }
    }

    fn plaintext() -> Self {
        Self::new(TlsMode::Plaintext, None, TlsState::Off)
    }

    /// A disk edge: the files are read once, here, and judged as at any boot (a truncated or
    /// leaf-only chain is served loudly; anything else that fails the verdict is refused).
    fn from_disk(cert: PathBuf, key: PathBuf, now: i64) -> Result<Self, String> {
        let cert_pem =
            std::fs::read(&cert).map_err(|err| format!("read {}: {err}", cert.display()))?;
        let key_pem =
            std::fs::read(&key).map_err(|err| format!("read {}: {err}", key.display()))?;
        let (checked, diagnosis) =
            check_material(&cert_pem, &key_pem, now).map_err(|refusal| refusal.detail)?;
        log_tls_chain_diagnosis("disk", diagnosis);
        Ok(Self::new(
            TlsMode::Disk { cert, key },
            None,
            TlsState::Serving(Arc::new(served_tls(checked, None, None, now))),
        ))
    }

    /// A vault edge before its first read: awaiting until `reload` finds usable material.
    fn vault(key: String, source: Arc<dyn TlsSecretSource>) -> Self {
        Self::new(
            TlsMode::Vault { key },
            Some(source),
            TlsState::Awaiting(TlsRefusal {
                verdict: TlsVerdict::Transient,
                detail: "the secret has not been read yet".to_string(),
            }),
        )
    }

    #[cfg(test)]
    fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    fn read_state(&self) -> std::sync::RwLockReadGuard<'_, TlsState> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_state(&self) -> std::sync::RwLockWriteGuard<'_, TlsState> {
        self.state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_challenges(&self) -> std::sync::MutexGuard<'_, HashMap<String, ArmedChallenge>> {
        self.challenges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn served(&self) -> Option<Arc<ServedTls>> {
        match &*self.read_state() {
            TlsState::Serving(served) => Some(Arc::clone(served)),
            TlsState::Off | TlsState::Awaiting(_) => None,
        }
    }

    /// The main configuration, read once per new connection; `None` while awaiting.
    fn main_config(&self) -> Option<Arc<rustls::ServerConfig>> {
        self.served().map(|served| Arc::clone(&served.config))
    }

    fn is_serving(&self) -> bool {
        matches!(&*self.read_state(), TlsState::Serving(_))
    }

    /// The verdict an awaiting edge waits on; `None` once it serves.
    fn awaiting_verdict(&self) -> Option<TlsVerdict> {
        match &*self.read_state() {
            TlsState::Awaiting(refusal) => Some(refusal.verdict),
            TlsState::Off | TlsState::Serving(_) => None,
        }
    }

    /// What this edge serves from the vault, read from the shared state at the moment of the
    /// decision, so it can never lag a swap.
    fn live_material(&self) -> LiveTlsMaterial {
        if self.mode.vault_key().is_none() {
            return LiveTlsMaterial::NotFromVault;
        }
        match &*self.read_state() {
            TlsState::Serving(served) => LiveTlsMaterial::FromVault {
                version: served.vault_version,
                fingerprint: served.leaf.fingerprint.clone(),
            },
            TlsState::Off | TlsState::Awaiting(_) => LiveTlsMaterial::Awaiting,
        }
    }

    /// The `EDGE_TLS_STATUS` answer, also the body of `EDGE_TLS_RELOAD`'s.
    fn status_payload(&self) -> Value {
        let (state, refusal, served) = match &*self.read_state() {
            TlsState::Off => ("serving", None, None),
            TlsState::Serving(served) => ("serving", None, Some(Arc::clone(served))),
            TlsState::Awaiting(refusal) => ("awaiting", Some(refusal.clone()), None),
        };
        let served = served.as_deref();
        json!({
            "status": "ok",
            "mode": self.mode.as_str(),
            "tls_vault_key": self.mode.vault_key(),
            "state": state,
            "reason": refusal.as_ref().map(|refusal| refusal.verdict.as_str()),
            "detail": refusal.as_ref().map(|refusal| refusal.detail.as_str()),
            "leaf_pem": served.map(|served| served.leaf_pem.as_str()),
            "fingerprint": served.map(|served| served.leaf.fingerprint.as_str()),
            "not_before": served.map(|served| served.leaf.not_before),
            "not_after": served.map(|served| served.leaf.not_after),
            "names": served.map(|served| served.leaf.names.clone()),
            "issuer": served.map(|served| served.leaf.issuer.as_str()),
            "chain_len": served.map(|served| served.leaf.chain_len),
            "loaded_at": served.map(|served| served.loaded_at),
            "vault_version": served.and_then(|served| served.vault_version),
            "vault_updated_at": served.and_then(|served| served.vault_updated_at.as_deref()),
        })
    }

    /// The acceptor's choice for one ClientHello (§4):
    /// - ALPN exactly `["acme-tls/1"]` and a challenge armed for the SNI: that challenge;
    /// - ALPN exactly `["acme-tls/1"]` and nothing armed (or no SNI): drop;
    /// - anything else, `acme-tls/1` mixed with other protocols included: the main
    ///   configuration, or drop while awaiting.
    fn route_hello(&self, sni: Option<&str>, alpn: Option<&[&[u8]]>, now: i64) -> HelloRoute {
        if matches!(alpn, Some([only]) if *only == ACME_TLS_ALPN) {
            let name = sni.map(tls_material::normalize_name).unwrap_or_default();
            if name.is_empty() {
                return HelloRoute::Drop("acme-tls/1 without SNI");
            }
            return match self.challenge_config(&name, now) {
                Some(config) => HelloRoute::Challenge { name, config },
                None => HelloRoute::Drop("acme-tls/1 for a name with no armed challenge"),
            };
        }
        match self.main_config() {
            Some(config) => HelloRoute::Main(config),
            None => HelloRoute::Drop("awaiting a certificate: no main configuration"),
        }
    }

    /// Arm a TLS-ALPN-01 challenge for `name` until `now + ttl_secs`. A newer arm for the same
    /// name replaces the older one.
    fn arm_challenge(
        &self,
        name: &str,
        digest: &str,
        ttl_secs: u64,
        now: i64,
    ) -> Result<ChallengeAnswer, String> {
        let name = acme_challenge_name(name)?;
        let digest = acme_challenge_digest(digest)?;
        if !(1..=ACME_CHALLENGE_MAX_TTL_SECS).contains(&ttl_secs) {
            return Err(format!(
                "ttl_secs must be 1..={ACME_CHALLENGE_MAX_TTL_SECS}, got {ttl_secs}"
            ));
        }
        let config = acme_challenge_server_config(&name, &digest)?;
        let expires_at = now.saturating_add(ttl_secs as i64);
        let mut challenges = self.lock_challenges();
        challenges.retain(|_, armed| armed.expires_at > now);
        challenges.insert(
            name.clone(),
            ArmedChallenge {
                digest: hex::encode(digest),
                config,
                expires_at,
            },
        );
        tracing::info!(name = %name, expires_at, "sy-edge armed an ACME TLS-ALPN-01 challenge");
        Ok(ChallengeAnswer {
            name,
            armed: true,
            expires_at: Some(expires_at),
        })
    }

    /// Clear the challenge armed for `name` only when it carries `digest`, so a late clear never
    /// removes a newer arm. The answer says what stays armed.
    fn clear_challenge(
        &self,
        name: &str,
        digest: &str,
        now: i64,
    ) -> Result<ChallengeAnswer, String> {
        let name = acme_challenge_name(name)?;
        let digest = hex::encode(acme_challenge_digest(digest)?);
        let mut challenges = self.lock_challenges();
        challenges.retain(|_, armed| armed.expires_at > now);
        if challenges
            .get(&name)
            .is_some_and(|armed| armed.digest == digest)
        {
            challenges.remove(&name);
            tracing::info!(name = %name, "sy-edge cleared an ACME TLS-ALPN-01 challenge");
        }
        let remaining = challenges.get(&name).map(|armed| armed.expires_at);
        Ok(ChallengeAnswer {
            name,
            armed: remaining.is_some(),
            expires_at: remaining,
        })
    }

    /// The configuration of the challenge armed for `name` (normalized), unless it expired.
    fn challenge_config(&self, name: &str, now: i64) -> Option<Arc<rustls::ServerConfig>> {
        let mut challenges = self.lock_challenges();
        match challenges.get(name) {
            Some(armed) if armed.expires_at > now => Some(Arc::clone(&armed.config)),
            Some(_) => {
                challenges.remove(name);
                None
            }
            None => None,
        }
    }

    /// `EDGE_TLS_RELOAD`, a `VAULT_SECRET_CHANGED` for the edge's own key, and every awaiting
    /// re-read: read the secret ONCE and judge it with the shared verdict. The main configuration
    /// is swapped only when the material passes; connections already open finish on the one they
    /// started with. A refusal keeps what is served and says why. Nothing exits.
    async fn reload(&self, trigger: &str) -> Result<ReloadOutcome, String> {
        let (TlsMode::Vault { key }, Some(source)) = (&self.mode, self.source.as_ref()) else {
            return Err(format!(
                "this edge's TLS mode is {}: only a vault edge reloads its certificate",
                self.mode.as_str()
            ));
        };
        let _one_at_a_time = self.reload_lock.lock().await;
        let before = BeforeReload {
            serving: self.is_serving(),
            awaiting: self.awaiting_verdict(),
            first: !self
                .read_once
                .swap(true, std::sync::atomic::Ordering::SeqCst),
        };
        let outcome = match source.read(key).await {
            Ok(secret) => self.apply_material(
                secret.cert.as_bytes(),
                secret.key.as_bytes(),
                secret.version,
                secret.updated_at,
                now_unix_secs(),
            ),
            Err(refusal) => self.record_refusal(refusal),
        };
        log_reload_outcome(key, trigger, &before, &outcome, self.served().as_deref());
        Ok(outcome)
    }

    /// Judge PEM material and decide what it means for the main configuration:
    /// - the chain served already (a bootstrap re-announce, a rotate or rollback to the same
    ///   bytes): nothing swaps, and the vault write identity follows the read;
    /// - a truncated chain, or a leaf with no intermediates, that would REPLACE a served
    ///   certificate: refused. With nothing served (boot, awaiting) it is served, loudly: an edge
    ///   up on a weak chain beats an edge that is down;
    /// - anything else that passed: swapped in.
    fn apply_material(
        &self,
        cert_pem: &[u8],
        key_pem: &[u8],
        vault_version: Option<i64>,
        vault_updated_at: Option<String>,
        now: i64,
    ) -> ReloadOutcome {
        let (checked, diagnosis) = match check_material(cert_pem, key_pem, now) {
            Ok(checked) => checked,
            Err(refusal) => return self.record_refusal(refusal),
        };
        let mut state = self.write_state();
        if let TlsState::Serving(current) = &*state {
            if current.chain == checked.chain {
                let unchanged = ServedTls {
                    vault_version,
                    vault_updated_at,
                    ..(**current).clone()
                };
                *state = TlsState::Serving(Arc::new(unchanged));
                return ReloadOutcome {
                    changed: false,
                    refused: None,
                };
            }
            if let Some(complaint) = diagnosis.complaint() {
                return ReloadOutcome {
                    changed: false,
                    refused: Some(TlsRefusal::material(format!(
                        "not replacing the served certificate: {complaint}"
                    ))),
                };
            }
        }
        *state = TlsState::Serving(Arc::new(served_tls(
            checked,
            vault_version,
            vault_updated_at,
            now,
        )));
        drop(state);
        log_tls_chain_diagnosis("vault", diagnosis);
        ReloadOutcome {
            changed: true,
            refused: None,
        }
    }

    /// A failed read or a refused material: what is served stays; an awaiting edge records why.
    fn record_refusal(&self, refusal: TlsRefusal) -> ReloadOutcome {
        if let TlsState::Awaiting(reason) = &mut *self.write_state() {
            *reason = refusal.clone();
        }
        ReloadOutcome {
            changed: false,
            refused: Some(refusal),
        }
    }
}

/// What held before a reload's read, to report its outcome.
#[derive(Debug, Clone, Copy)]
struct BeforeReload {
    serving: bool,
    awaiting: Option<TlsVerdict>,
    /// The edge's first read (boot): its outcome is always reported in full.
    first: bool,
}

/// How a reload is reported, decided apart from the logging so it can be tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReloadReport {
    /// A new certificate is served.
    Loaded,
    /// The material read is the one served.
    Unchanged,
    /// A serving edge refused what the vault holds: ERROR, the operator fixes the secret.
    Refused,
    /// A serving edge got no verdict from the vault: WARN, the current certificate stays.
    VaultSilent,
    /// The edge awaits, at boot or for another reason than before: ERROR.
    Awaiting,
    /// The edge still awaits for the same reason: WARN (DEBUG for the fast transient re-reads).
    StillAwaiting,
}

fn reload_report(before: &BeforeReload, outcome: &ReloadOutcome) -> ReloadReport {
    match &outcome.refused {
        None if outcome.changed => ReloadReport::Loaded,
        None => ReloadReport::Unchanged,
        Some(refusal) if before.serving => {
            if refusal.verdict == TlsVerdict::Transient {
                ReloadReport::VaultSilent
            } else {
                ReloadReport::Refused
            }
        }
        Some(refusal) if before.first || before.awaiting != Some(refusal.verdict) => {
            ReloadReport::Awaiting
        }
        Some(_) => ReloadReport::StillAwaiting,
    }
}

fn log_reload_outcome(
    key: &str,
    trigger: &str,
    before: &BeforeReload,
    outcome: &ReloadOutcome,
    served: Option<&ServedTls>,
) {
    let report = reload_report(before, outcome);
    if let Some(refusal) = &outcome.refused {
        let reason = refusal.verdict.as_str();
        let detail = refusal.detail.as_str();
        match report {
            ReloadReport::Refused => tracing::error!(
                key = %key,
                trigger = %trigger,
                reason,
                detail,
                "sy-edge: REFUSED the TLS material in the vault; still serving the certificate already loaded. Fix the secret and re-put it (vault_rollback restores the last good version)."
            ),
            ReloadReport::VaultSilent => tracing::warn!(
                key = %key,
                trigger = %trigger,
                detail,
                "sy-edge: the vault did not answer for the TLS secret; the current certificate stays"
            ),
            ReloadReport::Awaiting => tracing::error!(
                key = %key,
                trigger = %trigger,
                reason,
                detail,
                "sy-edge: AWAITING a usable TLS certificate: armed ACME challenges are answered, every other TLS handshake is dropped, nothing is served in plaintext. The secret is re-read on its own."
            ),
            _ if refusal.verdict == TlsVerdict::Transient => tracing::debug!(
                key = %key,
                trigger = %trigger,
                detail,
                "sy-edge: still AWAITING: the vault did not answer"
            ),
            _ => tracing::warn!(
                key = %key,
                trigger = %trigger,
                reason,
                detail,
                "sy-edge: still AWAITING a usable TLS certificate"
            ),
        }
        return;
    }
    match served {
        Some(served) if report == ReloadReport::Loaded => tracing::info!(
            key = %key,
            trigger = %trigger,
            fingerprint = %served.leaf.fingerprint,
            not_after = served.leaf.not_after,
            vault_version = ?served.vault_version,
            vault_updated_at = ?served.vault_updated_at,
            "sy-edge: TLS certificate loaded in place (open connections finish on the previous one)"
        ),
        _ => tracing::info!(
            key = %key,
            trigger = %trigger,
            "sy-edge: TLS secret re-read; the material served is unchanged"
        ),
    }
}

/// The shared verdict on PEM material (`tls_material::inspect`): what will be served, and the
/// diagnosis of the PEM as given. A truncated chain is judged on exactly what the parser kept;
/// whether it, or a leaf with no intermediates, may be served depends on what is served already
/// (`EdgeTls::apply_material`).
fn check_material(
    cert_pem: &[u8],
    key_pem: &[u8],
    now: i64,
) -> Result<(tls_material::CheckedMaterial, ChainDiagnosis), TlsRefusal> {
    match tls_material::inspect(cert_pem, key_pem, now) {
        Ok(checked) => {
            let diagnosis = checked.diagnosis;
            Ok((checked, diagnosis))
        }
        Err(MaterialError::Truncated { .. }) => {
            let (chain, diagnosis) =
                tls_material::parse_chain(cert_pem).map_err(material_refusal)?;
            let kept: String = chain
                .iter()
                .map(|der| der_to_pem("CERTIFICATE", der.as_ref()))
                .collect();
            let checked =
                tls_material::inspect(kept.as_bytes(), key_pem, now).map_err(material_refusal)?;
            Ok((checked, diagnosis))
        }
        Err(err) => Err(material_refusal(err)),
    }
}

/// Checked material as the main configuration serves it.
fn served_tls(
    checked: tls_material::CheckedMaterial,
    vault_version: Option<i64>,
    vault_updated_at: Option<String>,
    now: i64,
) -> ServedTls {
    let leaf_pem = checked
        .chain
        .first()
        .map(|der| der_to_pem("CERTIFICATE", der.as_ref()))
        .unwrap_or_default();
    ServedTls {
        config: Arc::new(main_server_config(checked.certified_key)),
        leaf: checked.leaf,
        leaf_pem,
        chain: checked.chain,
        vault_version,
        vault_updated_at,
        loaded_at: now,
    }
}

/// The main configuration: the checked chain and key for every client, and an empty ALPN list,
/// as before (rustls then ignores what the client offers).
fn main_server_config(certified_key: rustls::sign::CertifiedKey) -> rustls::ServerConfig {
    let _ = rustls::crypto::ring::default_provider().install_default();
    rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
            certified_key,
        )))
}

/// A DER blob as PEM, 64 base64 characters per line.
fn der_to_pem(label: &str, der: &[u8]) -> String {
    let encoded = base64_encode(der);
    let mut pem = format!("-----BEGIN {label}-----\n");
    for line in encoded.as_bytes().chunks(64) {
        pem.push_str(&String::from_utf8_lossy(line));
        pem.push('\n');
    }
    pem.push_str(&format!("-----END {label}-----\n"));
    pem
}

/// Log what the chain diagnosis found for a chain being loaded. A complaint here (truncated, or a
/// leaf with no intermediates) means it is served anyway, because nothing else is served (boot,
/// awaiting, a disk edge); a reload that would replace a served certificate refuses such a chain.
fn log_tls_chain_diagnosis(source: &str, diagnosis: ChainDiagnosis) {
    match diagnosis.complaint() {
        Some(complaint) => {
            tracing::error!(
                source = %source,
                markers = diagnosis.markers,
                parsed = diagnosis.parsed,
                "sy-edge: {complaint} SERVING ANYWAY: nothing else is being served, and refusing would leave the public door down."
            );
        }
        None => {
            tracing::info!(
                source = %source,
                chain_len = diagnosis.parsed,
                "sy-edge: TLS chain loaded"
            );
        }
    }
}

/// The name a challenge is armed for: lowercase, no trailing dot, a DNS host name. Never a
/// wildcard (TLS-ALPN-01 cannot validate one, RFC 8737 §3) nor an IP literal.
fn acme_challenge_name(raw: &str) -> Result<String, String> {
    let name = tls_material::normalize_name(raw);
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    };
    if name.is_empty()
        || name.len() > 253
        || name.parse::<std::net::IpAddr>().is_ok()
        || !name.split('.').all(valid_label)
    {
        return Err(format!(
            "name must be a DNS host name (no wildcard, no IP literal), got '{raw}'"
        ));
    }
    Ok(name)
}

/// The TLS-ALPN-01 digest: the SHA-256 of the key authorization, 64 hex characters. Checked here,
/// before rcgen, which asserts its length.
fn acme_challenge_digest(raw: &str) -> Result<[u8; 32], String> {
    let raw = raw.trim();
    let mut digest = [0_u8; 32];
    if raw.len() != 64 || hex::decode_to_slice(raw, &mut digest).is_err() {
        return Err(
            "digest must be 64 hex characters (the SHA-256 of the key authorization)".to_string(),
        );
    }
    Ok(digest)
}

/// The TLS-ALPN-01 certificate (RFC 8737 §3), minted here so no private key crosses the mesh: a
/// fresh P-256 key, an empty subject, SAN = the name, and the critical `acmeIdentifier`
/// extension holding the digest.
fn acme_challenge_certificate(
    name: &str,
    digest: &[u8; 32],
) -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>), String> {
    let key = rcgen::KeyPair::generate().map_err(|err| format!("challenge key: {err}"))?;
    let mut params = rcgen::CertificateParams::new(vec![name.to_string()])
        .map_err(|err| format!("challenge certificate: {err}"))?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(digest)];
    let cert = params
        .self_signed(&key)
        .map_err(|err| format!("challenge certificate: {err}"))?;
    Ok((
        cert.der().clone(),
        PrivateKeyDer::Pkcs8(key.serialize_der().into()),
    ))
}

/// The configuration a challenge handshake finishes with: the challenge certificate and
/// `alpn_protocols = ["acme-tls/1"]`. Built from a `CertifiedKey` directly: rustls'
/// `with_single_cert` re-parses the leaf with webpki, which refuses the unknown critical
/// `acmeIdentifier` extension the certificate exists to carry.
fn acme_challenge_server_config(
    name: &str,
    digest: &[u8; 32],
) -> Result<Arc<rustls::ServerConfig>, String> {
    let (cert, key) = acme_challenge_certificate(name, digest)?;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let signing_key = rustls::crypto::ring::default_provider()
        .key_provider
        .load_private_key(key)
        .map_err(|err| format!("challenge key: {err}"))?;
    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(
            rustls::sign::CertifiedKey::new(vec![cert], signing_key),
        )));
    config.alpn_protocols = vec![ACME_TLS_ALPN.to_vec()];
    Ok(Arc::new(config))
}

/// What the acceptor does with one ClientHello.
#[derive(Debug)]
enum HelloRoute {
    /// Finish the handshake with the challenge's configuration, then close. HTTP is never
    /// reached.
    Challenge {
        name: String,
        config: Arc<rustls::ServerConfig>,
    },
    /// Today's path: the main configuration, then HTTP.
    Main(Arc<rustls::ServerConfig>),
    /// Close without a handshake.
    Drop(&'static str),
}

/// The public TLS door's limits against slow or abundant clients.
#[derive(Clone)]
struct DoorLimits {
    /// One permit per connection, its handshake included.
    connections: Arc<Semaphore>,
    max_connections: usize,
    /// The first request byte after the handshake, and every HTTP/1 request head, must arrive
    /// within this.
    header_read_timeout: Duration,
}

impl DoorLimits {
    fn new(max_connections: usize, header_read_timeout: Duration) -> Self {
        Self {
            connections: Arc::new(Semaphore::new(max_connections)),
            max_connections,
            header_read_timeout,
        }
    }
}

/// Counts the connections refused at the cap and says when to log them: at most once per
/// interval, with how many there were since the last line.
#[derive(Debug, Default)]
struct RefusedConnections {
    unlogged: u64,
    last_logged: Option<Instant>,
}

impl RefusedConnections {
    /// Note one refused connection; `Some(count)` when it is time to log `count` of them.
    fn note(&mut self, now: Instant, interval: Duration) -> Option<u64> {
        self.unlogged += 1;
        if self
            .last_logged
            .is_some_and(|last| now.duration_since(last) < interval)
        {
            return None;
        }
        self.last_logged = Some(now);
        Some(std::mem::take(&mut self.unlogged))
    }
}

/// The TLS accept loop: one task per connection, at most `limits.max_connections` at once. A
/// connection over the cap is closed at once; a failed accept waits `ACCEPT_ERROR_BACKOFF`.
async fn serve_tls_listener(
    listener: TcpListener,
    app: Router,
    tls: Arc<EdgeTls>,
    limits: DoorLimits,
) -> Result<(), SyEdgeError> {
    let mut refused = RefusedConnections::default();
    loop {
        let (tcp, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(error = %err, "sy-edge accept failed");
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                continue;
            }
        };
        let Ok(permit) = Arc::clone(&limits.connections).try_acquire_owned() else {
            drop(tcp);
            if let Some(count) = refused.note(Instant::now(), REFUSED_CONNECTIONS_LOG_INTERVAL) {
                tracing::warn!(
                    refused = count,
                    max_connections = limits.max_connections,
                    "sy-edge: at its connection cap; closing new connections"
                );
            }
            continue;
        };
        tokio::spawn(serve_tls_connection(
            tcp,
            app.clone(),
            Arc::clone(&tls),
            limits.header_read_timeout,
            permit,
        ));
    }
}

/// One connection, holding its permit to the end: the handshake (`accept_tls`), then hyper.
/// hyper-util's HTTP/1-or-2 sniffing waits for bytes with no timeout of its own, so the first
/// request byte must come within `header_read_timeout` here; after it, hyper's timer applies the
/// same timeout to every HTTP/1 request head, an idle keep-alive connection included.
async fn serve_tls_connection(
    tcp: tokio::net::TcpStream,
    app: Router,
    tls: Arc<EdgeTls>,
    header_read_timeout: Duration,
    _permit: OwnedSemaphorePermit,
) {
    let Some(tls_stream) = accept_tls(tcp, &tls).await else {
        return;
    };
    // The bytes read here stay buffered for hyper; writes pass straight through.
    let mut stream = tokio::io::BufReader::new(tls_stream);
    match tokio::time::timeout(
        header_read_timeout,
        tokio::io::AsyncBufReadExt::fill_buf(&mut stream),
    )
    .await
    {
        Ok(Ok(first)) if !first.is_empty() => {}
        Ok(Ok(_)) => return,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "sy-edge tls connection error");
            return;
        }
        Err(_) => {
            tracing::debug!("sy-edge closed a connection that sent nothing after its handshake");
            return;
        }
    }
    let io = hyper_util::rt::TokioIo::new(stream);
    let service = hyper_util::service::TowerToHyperService::new(app);
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(header_read_timeout);
    if let Err(err) = builder.serve_connection_with_upgrades(io, service).await {
        tracing::debug!(error = %err, "sy-edge tls connection error");
    }
}

/// Read the ClientHello, choose a configuration and finish the handshake, all under ONE timeout.
/// `Some` only for the main path: a challenge is answered and closed here, and a dropped hello
/// never gets a handshake.
async fn accept_tls<IO>(io: IO, tls: &EdgeTls) -> Option<tokio_rustls::server::TlsStream<IO>>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match tokio::time::timeout(tls.handshake_timeout, handshake_by_hello(io, tls)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "tls handshake failed");
            None
        }
        Err(_) => {
            tracing::debug!(
                timeout_ms = tls.handshake_timeout.as_millis() as u64,
                "tls handshake timed out"
            );
            None
        }
    }
}

async fn handshake_by_hello<IO>(
    io: IO,
    tls: &EdgeTls,
) -> std::io::Result<Option<tokio_rustls::server::TlsStream<IO>>>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let start =
        tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), io).await?;
    let route = {
        let hello = start.client_hello();
        let alpn: Option<Vec<&[u8]>> = hello.alpn().map(Iterator::collect);
        tls.route_hello(hello.server_name(), alpn.as_deref(), now_unix_secs())
    };
    match route {
        HelloRoute::Main(config) => start.into_stream(config).await.map(Some),
        HelloRoute::Challenge { name, config } => {
            let mut stream = start.into_stream(config).await?;
            let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
            tracing::info!(name = %name, "sy-edge answered an ACME TLS-ALPN-01 challenge");
            Ok(None)
        }
        HelloRoute::Drop(reason) => {
            tracing::debug!(
                reason,
                "sy-edge dropped a TLS connection at its ClientHello"
            );
            Ok(None)
        }
    }
}

/// A vault edge's boot read. Anything but usable material leaves it AWAITING instead of exiting
/// (a missing secret used to crash-loop the unit, so a new ingress could never answer the
/// challenge for its first certificate), and a task re-reads the secret until it serves.
async fn start_vault_tls(tls: &Arc<EdgeTls>, schedule: AwaitingSchedule) {
    match tls.reload("boot").await {
        Ok(outcome) if outcome.refused.is_some() => {
            tokio::spawn(run_awaiting_rereads(Arc::clone(tls), schedule));
        }
        Ok(_) => {}
        Err(err) => tracing::error!(error = %err, "sy-edge: boot read of the TLS secret failed"),
    }
}

/// When an awaiting edge re-reads its secret (`AWAITING_SCHEDULE` in production).
#[derive(Debug, Clone, Copy)]
struct AwaitingSchedule {
    /// The delay while the vault gives no verdict (`transient`), during the first
    /// `transient_window` of awaiting.
    transient: Duration,
    transient_window: Duration,
    /// Otherwise: `first`, then twice as late each time, up to `max`.
    first: Duration,
    max: Duration,
}

/// The delays of one awaiting stretch.
struct AwaitingBackoff {
    schedule: AwaitingSchedule,
    next_long: Duration,
}

impl AwaitingBackoff {
    fn new(schedule: AwaitingSchedule) -> Self {
        Self {
            schedule,
            next_long: schedule.first,
        }
    }

    /// The delay before the next re-read, given the verdict the edge awaits on and how long it
    /// has been awaiting.
    fn next(&mut self, verdict: TlsVerdict, awaiting_for: Duration) -> Duration {
        if verdict == TlsVerdict::Transient && awaiting_for < self.schedule.transient_window {
            return self.schedule.transient;
        }
        let delay = self.next_long;
        self.next_long = delay.saturating_mul(2).min(self.schedule.max);
        delay
    }
}

/// While awaiting, re-read the secret on `schedule` and stop once the edge serves. A command or
/// a vault notice may get there first; the loop then just ends.
async fn run_awaiting_rereads(tls: Arc<EdgeTls>, schedule: AwaitingSchedule) {
    let started = Instant::now();
    let mut backoff = AwaitingBackoff::new(schedule);
    while let Some(verdict) = tls.awaiting_verdict() {
        tokio::time::sleep(backoff.next(verdict, started.elapsed())).await;
        if tls.is_serving() {
            return;
        }
        if let Err(err) = tls.reload("awaiting re-read").await {
            tracing::error!(error = %err, "sy-edge: awaiting re-read failed");
            return;
        }
    }
}

/// The three TLS commands (§4), from `SY.admin@motherbee` only. They arrive on their own channel
/// (`RPC_CH_TLS`) and each runs in its own task over the shared state, so they never wait behind
/// `EDGE_PUBLISH_BLOB` (up to 60 s) or a secret resolution in the sequential system loop.
async fn run_tls_commands(
    mut commands: RpcCommandReceiver,
    sender: NodeSender,
    tls: Option<Arc<EdgeTls>>,
    ttl: u8,
) {
    while let Some(msg) = commands.recv().await {
        let sender = sender.clone();
        let tls = tls.clone();
        tokio::spawn(async move {
            if let Some((response, payload)) = tls_command_reply(tls.as_deref(), &msg).await {
                send_edge_reply(&sender, &msg, response, payload, ttl).await;
            }
        });
    }
}

async fn tls_command_reply(tls: Option<&EdgeTls>, msg: &Message) -> Option<(&'static str, Value)> {
    match msg.meta.msg.as_deref() {
        Some(MSG_EDGE_TLS_STATUS) => {
            Some((MSG_EDGE_TLS_STATUS_RESPONSE, tls_status_answer(tls, msg)))
        }
        Some(MSG_EDGE_TLS_RELOAD) => Some((
            MSG_EDGE_TLS_RELOAD_RESPONSE,
            tls_reload_answer(tls, msg).await,
        )),
        Some(MSG_EDGE_ACME_CHALLENGE) => Some((
            MSG_EDGE_ACME_CHALLENGE_RESPONSE,
            acme_challenge_answer(tls, msg),
        )),
        other => {
            tracing::warn!(msg = ?other, "sy-edge: unexpected message on the TLS command channel");
            None
        }
    }
}

/// The edge a TLS command acts on, once its origin is checked.
fn tls_command_edge<'a>(
    tls: Option<&'a EdgeTls>,
    msg: &Message,
    action: &str,
) -> Result<&'a EdgeTls, String> {
    authorize_edge_service_command(msg, action)?;
    tls.ok_or_else(|| format!("{action}: this edge serves no public listener"))
}

/// The same, for the commands only a vault edge serves.
fn vault_tls_command_edge<'a>(
    tls: Option<&'a EdgeTls>,
    msg: &Message,
    action: &str,
) -> Result<&'a EdgeTls, String> {
    let tls = tls_command_edge(tls, msg, action)?;
    if tls.mode.vault_key().is_none() {
        return Err(format!(
            "{action}: this edge's TLS mode is {}; only a vault edge reloads its certificate and answers ACME challenges",
            tls.mode.as_str()
        ));
    }
    Ok(tls)
}

fn tls_command_error(action: &str, msg: &Message, err: String) -> Value {
    tracing::warn!(error = %err, trace_id = %msg.routing.trace_id, "sy-edge {action} failed");
    json!({ "status": "error", "error": err })
}

/// `EDGE_TLS_STATUS`: what the listener serves.
fn tls_status_answer(tls: Option<&EdgeTls>, msg: &Message) -> Value {
    match tls_command_edge(tls, msg, MSG_EDGE_TLS_STATUS) {
        Ok(tls) => tls.status_payload(),
        Err(err) => tls_command_error(MSG_EDGE_TLS_STATUS, msg, err),
    }
}

/// `EDGE_TLS_RELOAD`: one re-read and verdict; the STATUS fields plus `changed` and, when
/// refused, `refused: {reason, detail}`.
async fn tls_reload_answer(tls: Option<&EdgeTls>, msg: &Message) -> Value {
    let tls = match vault_tls_command_edge(tls, msg, MSG_EDGE_TLS_RELOAD) {
        Ok(tls) => tls,
        Err(err) => return tls_command_error(MSG_EDGE_TLS_RELOAD, msg, err),
    };
    match tls.reload(MSG_EDGE_TLS_RELOAD).await {
        Ok(outcome) => {
            let mut payload = tls.status_payload();
            if let Value::Object(fields) = &mut payload {
                fields.insert("changed".to_string(), json!(outcome.changed));
                if let Some(refusal) = &outcome.refused {
                    fields.insert("refused".to_string(), refusal.to_json());
                }
            }
            payload
        }
        Err(err) => tls_command_error(MSG_EDGE_TLS_RELOAD, msg, err),
    }
}

/// `EDGE_ACME_CHALLENGE`: `{name, digest, ttl_secs?}` arms, `{name, digest, clear: true}` clears.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcmeChallengeRequest {
    name: String,
    digest: String,
    #[serde(default)]
    ttl_secs: Option<u64>,
    #[serde(default)]
    clear: bool,
}

fn acme_challenge_answer(tls: Option<&EdgeTls>, msg: &Message) -> Value {
    let outcome: Result<ChallengeAnswer, String> = (|| {
        let tls = vault_tls_command_edge(tls, msg, MSG_EDGE_ACME_CHALLENGE)?;
        let request: AcmeChallengeRequest = serde_json::from_value(msg.payload.clone())
            .map_err(|err| format!("invalid EDGE_ACME_CHALLENGE payload: {err}"))?;
        let now = now_unix_secs();
        if request.clear {
            tls.clear_challenge(&request.name, &request.digest, now)
        } else {
            tls.arm_challenge(
                &request.name,
                &request.digest,
                request.ttl_secs.unwrap_or(ACME_CHALLENGE_DEFAULT_TTL_SECS),
                now,
            )
        }
    })();
    match outcome {
        Ok(answer) => json!({
            "status": "ok",
            "name": answer.name,
            "armed": answer.armed,
            "expires_at": answer.expires_at,
        }),
        Err(err) => tls_command_error(MSG_EDGE_ACME_CHALLENGE, msg, err),
    }
}

/// What this edge serves from the vault, for the `VAULT_SECRET_CHANGED` decision. Derived from
/// `EdgeTls` at the moment of the decision (`EdgeTls::live_material`), so it is consistent with
/// every swap by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LiveTlsMaterial {
    /// Not a vault edge: no public door on this role, a disk certificate, or plaintext. Nothing a
    /// vault event can invalidate.
    NotFromVault,
    /// A vault edge with no usable secret yet: any change to its key may be the one it awaits.
    Awaiting,
    /// A vault edge serving the leaf whose `fingerprint` is the SHA-256 of its DER, from vault
    /// `version` (reported for the log: a version number alone is not an identity, since a vault
    /// rollback re-mints numbers, sy_vault.rs).
    FromVault {
        version: Option<i64>,
        fingerprint: String,
    },
}

/// What a `VAULT_SECRET_CHANGED` means for this edge's TLS material.
#[derive(Debug, PartialEq, Eq)]
enum TlsSecretChange {
    /// The value under our key may have changed: do what `EDGE_TLS_RELOAD` does (one re-read,
    /// the shared verdict, a swap in place when it passes). A bootstrap re-announce is confirmed
    /// the same way: when the vault still holds the chain served, nothing moves.
    Reload {
        key: String,
        op: &'static str,
        version: i64,
        bootstrap: bool,
    },
    /// Our TLS key was deleted. Keep serving what is loaded; nothing restarts.
    DeletedKeepServing { key: String },
    /// The notice is about some other secret, only an assignment, or this edge does not serve
    /// TLS from the vault.
    Unrelated,
    /// The payload could not be parsed, so we cannot tell.
    Malformed { error: String },
}

/// Decide how to react to a `VAULT_SECRET_CHANGED`, given the vault key of this edge's TLS
/// (`None` when it is not a vault edge). Split out from the message loop so it is testable.
///
/// Matching is BY KEY, deliberately diverging from the `(resource_type, tenant_id, ilk)` guidance
/// on `VaultSecretChangedPayload`: that guidance is for consumers that resolve a secret by
/// interest, whereas the edge is configured with one explicit `tls_vault_key` and reads that exact
/// key, so key equality is precisely the condition that invalidates what it loaded.
fn tls_secret_change_action(
    payload: &serde_json::Value,
    tls_key: Option<&str>,
    live: &LiveTlsMaterial,
    is_bootstrap: bool,
) -> TlsSecretChange {
    let Some(tls_key) = tls_key else {
        return TlsSecretChange::Unrelated;
    };
    // Stays ahead of every other check: a payload we cannot read must never be swallowed.
    let parsed: VaultSecretChangedPayload = match serde_json::from_value(payload.clone()) {
        Ok(p) => p,
        Err(err) => {
            return TlsSecretChange::Malformed {
                error: err.to_string(),
            }
        }
    };
    if parsed.key != tls_key {
        return TlsSecretChange::Unrelated;
    }
    // Before the live-material check: a delete must keep serving whatever we hold, always.
    if matches!(parsed.op, VaultSecretOp::Delete) {
        return TlsSecretChange::DeletedKeepServing { key: parsed.key };
    }
    // Who may use the key changed, never its value: nothing to reload.
    if matches!(parsed.op, VaultSecretOp::Assign) {
        return TlsSecretChange::Unrelated;
    }
    if matches!(live, LiveTlsMaterial::NotFromVault) {
        return TlsSecretChange::Unrelated;
    }
    TlsSecretChange::Reload {
        key: parsed.key,
        op: parsed.op.as_str(),
        version: parsed.version,
        bootstrap: is_bootstrap,
    }
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
    // Fanout endpoints carry a verify_token_ref (and usually no secret_ref), resolved the same way.
    let pending_verify: Vec<(String, String)> = match registry.read() {
        Ok(guard) => guard
            .iter()
            .filter(|(_, e)| e.verify_token.is_none() && e.verify_token_ref.is_some())
            .map(|(ich, e)| (ich.clone(), e.verify_token_ref.clone().unwrap_or_default()))
            .filter(|(_, r)| !r.is_empty())
            .collect(),
        Err(_) => return,
    };
    if pending.is_empty() && pending_verify.is_empty() {
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
    for (ich, verify_ref) in pending_verify {
        match client.get(&verify_ref, Duration::from_secs(15)).await {
            Ok(resp) => {
                let token = resp
                    .value
                    .as_ref()
                    .and_then(|v| v.get("secret"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                match token {
                    Some(token) => {
                        if let Ok(mut guard) = registry.write() {
                            if let Some(entry) = guard.get_mut(&ich) {
                                entry.verify_token = Some(token);
                            }
                        }
                        tracing::info!(ich = %ich, verify_token_ref = %verify_ref, "sy-edge resolved fanout verify-token from vault");
                    }
                    None => {
                        tracing::warn!(ich = %ich, verify_token_ref = %verify_ref, "vault verify-token has no 'secret' field; the webhook challenge will fail")
                    }
                }
            }
            Err(err) => {
                tracing::warn!(ich = %ich, verify_token_ref = %verify_ref, error = %err, "sy-edge could not fetch fanout verify-token from vault")
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

    // 1b. FANOUT endpoint (IO.wapp): answer the provider's connection-handshake challenge AT the edge,
    //     BEFORE the method allowlist + auth — a provider's verification GET (Meta: hub.mode/
    //     hub.verify_token/hub.challenge) carries no bearer and the row may be POST-only. On a
    //     constant-time verify-token match, echo the raw hub.challenge as text/plain 200; else 403.
    //     This is the ONLY synchronous edge response for a fanout row; POST events are fire-and-forget.
    if entry.fanout_family.is_some() && method == Method::GET {
        if let Some(query) = uri.query() {
            let params = parse_query_params(query);
            if params.get("hub.mode").map(String::as_str) == Some("subscribe") {
                let want = entry.verify_token.as_deref().unwrap_or("");
                let got = params
                    .get("hub.verify_token")
                    .map(String::as_str)
                    .unwrap_or("");
                let challenge = params.get("hub.challenge").cloned().unwrap_or_default();
                if !want.is_empty() && constant_time_eq(want.as_bytes(), got.as_bytes()) {
                    return (
                        StatusCode::OK,
                        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                        challenge,
                    )
                        .into_response();
                }
                tracing::warn!(ich = %ich, "fanout webhook verify-token mismatch; rejecting challenge");
                return http_error(
                    StatusCode::FORBIDDEN,
                    "FORBIDDEN",
                    "webhook verify token mismatch",
                );
            }
        }
        // A GET that is not a verification challenge falls through to the method allowlist.
    }

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

    // 4b. FANOUT endpoint (IO.wapp events): ack the caller 200 IMMEDIATELY and BROADCAST the request to
    //     every node matching the family glob (they each verify the signature + self-select by
    //     phone_number_id). Fire-and-forget — the reply goes out-of-band via the provider API, so the
    //     edge never waits for a node reply (avoids provider retries). The RAW body + the
    //     X-Hub-Signature-256 header (stripped by filter_request_headers) ride in the payload so each
    //     node verifies the HMAC over the exact bytes. The edge holds NO app_secret — a thin frontier.
    if let Some(family_glob) = entry.fanout_family.clone() {
        let signature = headers
            .get("x-hub-signature-256")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut fanout_ctx = serde_json::Map::new();
        fanout_ctx.insert("method".to_string(), json!(method.as_str()));
        fanout_ctx.insert("path".to_string(), json!(normalize_extra_path(&extra)));
        if let Some(query) = uri.query() {
            fanout_ctx.insert("query".to_string(), json!(query));
        }
        let message = Message {
            routing: Routing {
                src: state.sender.uuid().to_string(),
                src_l2_name: Some(state.sender.full_name().to_string()),
                dst: Destination::Broadcast,
                ttl: state.ttl,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: entry.inbound_family.clone(),
                ich: Some(ich.clone()),
                // Narrow the broadcast to the IO.wapp family glob (else it would hit every node).
                target: Some(family_glob),
                context: Some(Value::Object(fanout_ctx)),
                ..Meta::default()
            },
            payload: json!({
                "raw_body_base64": base64_encode(&body),
                "signature": signature,
            }),
        };
        if let Err(err) = state.sender.send(message).await {
            tracing::warn!(ich = %ich, error = %err, "sy-edge fanout broadcast failed to enqueue");
            return http_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "FANOUT_SEND_FAILED",
                "could not fan out webhook",
            );
        }
        // Ack the provider immediately (WhatsApp best practice: 200 fast, process async).
        return (StatusCode::OK, "").into_response();
    }

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

/// Parse a URL query string into decoded key→value pairs (for a fanout endpoint's webhook-verification
/// GET: `hub.mode`, `hub.verify_token`, `hub.challenge`). No `url` crate dependency here.
fn parse_query_params(query: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut it = pair.splitn(2, '=');
        let key = it.next().unwrap_or("");
        let value = it.next().unwrap_or("");
        if !key.is_empty() {
            map.insert(percent_decode(key), percent_decode(value));
        }
    }
    map
}

/// Minimal `application/x-www-form-urlencoded` decode (`%XX` escapes + `+` → space).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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

/// Unix seconds, the unit of every TLS date the edge reports (`tls_material::LeafInfo`).
fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
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
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
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
    use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

    #[test]
    fn parse_query_params_decodes_webhook_challenge() {
        let m = parse_query_params(
            "hub.mode=subscribe&hub.verify_token=my%20token&hub.challenge=12345",
        );
        assert_eq!(m.get("hub.mode").map(String::as_str), Some("subscribe"));
        assert_eq!(
            m.get("hub.verify_token").map(String::as_str),
            Some("my token")
        );
        assert_eq!(m.get("hub.challenge").map(String::as_str), Some("12345"));
    }

    /// Build a VAULT_SECRET_CHANGED payload the way SY.vault emits it.
    fn vault_changed(key: &str, op: &str, version: i64) -> serde_json::Value {
        serde_json::json!({
            "op": op,
            "resource_type": "tls",
            "tenant_id": "tnt:00000000-0000-0000-0000-000000000000",
            "version": version,
            "key": key,
            "hive_id": "motherbee",
            "at_ms": 1_753_800_000_000i64,
        })
    }

    fn serving_live() -> LiveTlsMaterial {
        LiveTlsMaterial::FromVault {
            version: Some(1),
            fingerprint: "fp1".into(),
        }
    }

    /// Regression (PB-1): a rotated TLS cert used to be invisible to a running edge, and then
    /// cost a process restart. A put, rotate or rollback of the edge's own key now reloads in
    /// place, and so does a change while the edge awaits its first certificate.
    #[test]
    fn a_tls_secret_change_in_the_vault_reloads_in_place() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "put", 2),
                Some("edge_tls"),
                &serving_live(),
                false,
            ),
            TlsSecretChange::Reload {
                key: "edge_tls".into(),
                op: "put",
                version: 2,
                bootstrap: false,
            }
        );
        for op in ["rotate", "rollback"] {
            assert!(matches!(
                tls_secret_change_action(
                    &vault_changed("edge_tls", op, 3),
                    Some("edge_tls"),
                    &serving_live(),
                    false,
                ),
                TlsSecretChange::Reload { .. }
            ));
        }
        assert!(matches!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "put", 1),
                Some("edge_tls"),
                &LiveTlsMaterial::Awaiting,
                false,
            ),
            TlsSecretChange::Reload { .. }
        ));
    }

    /// An assignment changes who may use the key, never its value: nothing to reload.
    #[test]
    fn tls_key_assignment_never_reloads() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "assign", 1),
                Some("edge_tls"),
                &serving_live(),
                false,
            ),
            TlsSecretChange::Unrelated
        );
    }

    /// A delete keeps serving what is loaded, whatever the edge holds; nothing restarts.
    #[test]
    fn tls_cert_delete_keeps_serving() {
        for live in [
            serving_live(),
            LiveTlsMaterial::Awaiting,
            LiveTlsMaterial::NotFromVault,
        ] {
            assert_eq!(
                tls_secret_change_action(
                    &vault_changed("edge_tls", "delete", 0),
                    Some("edge_tls"),
                    &live,
                    false,
                ),
                TlsSecretChange::DeletedKeepServing {
                    key: "edge_tls".into()
                },
                "{live:?}"
            );
        }
    }

    /// Other secrets rotating (postgres, slack, …) never touch the edge's TLS, and an edge that
    /// is not a vault edge ignores the notice entirely.
    #[test]
    fn unrelated_secret_changes_leave_the_edge_alone() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("pg_main", "put", 7),
                Some("edge_tls"),
                &serving_live(),
                false,
            ),
            TlsSecretChange::Unrelated
        );
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "put", 2),
                None,
                &LiveTlsMaterial::NotFromVault,
                false,
            ),
            TlsSecretChange::Unrelated
        );
    }

    /// PB-9: SY.vault re-announces every secret at its own boot. That is a reload like any
    /// other: one re-read, and when the vault still holds the chain served nothing moves
    /// (`a_reread_of_the_material_already_served_changes_nothing`). The version number decides
    /// nothing, since a vault rollback re-mints numbers.
    #[test]
    fn a_vault_bootstrap_re_announce_is_confirmed_by_a_reload() {
        for version in [3, 4, 9] {
            assert_eq!(
                tls_secret_change_action(
                    &vault_changed("edge_tls", "put", version),
                    Some("edge_tls"),
                    &LiveTlsMaterial::FromVault {
                        version: Some(4),
                        fingerprint: "fp".into(),
                    },
                    true,
                ),
                TlsSecretChange::Reload {
                    key: "edge_tls".into(),
                    op: "put",
                    version,
                    bootstrap: true,
                }
            );
        }
    }

    /// A role with no public door, a disk certificate or plaintext holds nothing from the vault.
    #[test]
    fn an_edge_serving_no_vault_tls_never_reacts() {
        for bootstrap in [true, false] {
            assert_eq!(
                tls_secret_change_action(
                    &vault_changed("edge_tls", "put", 2),
                    Some("edge_tls"),
                    &LiveTlsMaterial::NotFromVault,
                    bootstrap,
                ),
                TlsSecretChange::Unrelated
            );
        }
    }

    /// PB-2's empirical premise, pinned with real certificates: on a glued END/BEGIN,
    /// `rustls_pemfile` (behind `tls_material::parse_chain`) does NOT error; it returns Ok with
    /// only the leaf, while openssl reads zero certs from the same bytes. If a rustls bump starts
    /// erroring instead, this fails and the marker gate can be reconsidered.
    #[test]
    fn rustls_silently_drops_the_intermediate_of_a_glued_chain() {
        let ca = json_router::mesh_tls::MeshCa::generate().expect("ca");
        let leaf = ca.issue_leaf("edge-test").expect("leaf");

        let clean = format!("{}{}", leaf.cert_pem, ca.ca_cert_pem());
        let (chain, diagnosis) = tls_material::parse_chain(clean.as_bytes()).expect("clean");
        assert_eq!((chain.len(), diagnosis.markers), (2, 2));
        assert!(!diagnosis.truncated());

        let glued = format!(
            "{}{}",
            leaf.cert_pem.trim_end_matches('\n'),
            ca.ca_cert_pem()
        );
        let (chain, diagnosis) = tls_material::parse_chain(glued.as_bytes())
            .expect("glued STILL parses — that is the whole problem");
        assert_eq!(diagnosis.markers, 2, "both markers are in the bytes");
        assert_eq!(
            chain.len(),
            1,
            "rustls kept only the leaf, with no error — the silent failure PB-2 is about"
        );
        assert!(
            diagnosis.truncated(),
            "and that is exactly what the gate must catch"
        );
    }

    /// The vault's verdict stays typed. Transport is no verdict on the secret: the edge's vault
    /// is cross-hive by default, and a WAN blip makes the ROUTER answer UNREACHABLE at once.
    #[test]
    fn the_vault_verdict_stays_typed() {
        let service = |code: &str| fluxbee_sdk::VaultError::Service {
            code: code.to_string(),
            message: "scripted".to_string(),
        };
        let cases = [
            (
                fluxbee_sdk::VaultError::Unreachable {
                    reason: "GATEWAY_UNAVAILABLE".into(),
                    original_dst: "SY.vault@motherbee".into(),
                },
                TlsVerdict::Transient,
            ),
            (
                fluxbee_sdk::VaultError::TtlExceeded {
                    original_dst: "SY.vault@motherbee".into(),
                    last_hop: "ingress1".into(),
                },
                TlsVerdict::Transient,
            ),
            (
                fluxbee_sdk::VaultError::ActionTimeout {
                    action: "VAULT_GET".into(),
                    trace_id: "t".into(),
                    target: "SY.vault@motherbee".into(),
                    timeout_ms: 15_000,
                },
                TlsVerdict::Transient,
            ),
            (service("KEY_NOT_FOUND"), TlsVerdict::NotFound),
            (service("UNAUTHORIZED"), TlsVerdict::Forbidden),
            // The secret's own fault: the request names an invalid key, or the stored value is.
            (service("INVALID_REQUEST"), TlsVerdict::InvalidMaterial),
            (service("INVALID_VALUE"), TlsVerdict::InvalidMaterial),
            (
                fluxbee_sdk::VaultError::EmptyValue {
                    key: "edge_tls".into(),
                },
                TlsVerdict::InvalidMaterial,
            ),
            // The vault could not do its job: never a reason to roll a good certificate back.
            (service("IDENTITY_UNAVAILABLE"), TlsVerdict::Transient),
            (service("STORAGE_ERROR"), TlsVerdict::Transient),
            (service("IO_ERROR"), TlsVerdict::Transient),
            (service("ENCRYPTION_ERROR"), TlsVerdict::Transient),
            (service("NODE_ERROR"), TlsVerdict::Transient),
            (service("MASTER_KEY_NOT_AVAILABLE"), TlsVerdict::Transient),
            (service("VAULT_RPC_ERROR"), TlsVerdict::Transient),
            (service("A_CODE_ADDED_LATER"), TlsVerdict::Transient),
            (
                fluxbee_sdk::VaultError::Json(
                    serde_json::from_str::<Value>("{").expect_err("not JSON"),
                ),
                TlsVerdict::Transient,
            ),
        ];
        for (err, verdict) in cases {
            let refusal = TlsRefusal::from_vault_error("edge_tls", err);
            assert_eq!(refusal.verdict, verdict, "{}", refusal.detail);
            assert!(refusal.detail.contains("edge_tls"));
        }
    }

    /// An unparseable payload must be reported, never silently treated as
    /// "not my key" — that would resurrect the original stale-cert bug.
    #[test]
    fn malformed_payload_is_reported_not_swallowed() {
        assert!(matches!(
            tls_secret_change_action(
                &serde_json::json!({"op": "put"}),
                Some("edge_tls"),
                &serving_live(),
                false,
            ),
            TlsSecretChange::Malformed { .. }
        ));
    }

    #[test]
    fn percent_decode_handles_escapes_and_plus() {
        assert_eq!(percent_decode("a%2Bb"), "a+b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("bad%zz"), "bad%zz"); // invalid escape passes through
    }

    #[test]
    fn validate_accepts_fanout_row_and_enforces_verify_token_and_glob() {
        let base = EndpointRow {
            ich: "ich:wapp".into(),
            owner_l2_name: String::new(), // fanout rows have no single owner
            inbound_family: "io.wapp.inbound.v1".into(),
            auth_mode: AuthMode::Public,
            secret: None,
            secret_ref: None,
            methods: Some(vec!["GET".into(), "POST".into()]),
            tenant_id: None,
            fanout_family: Some("IO.wapp.*@motherbee".into()),
            verify_token: None,
            verify_token_ref: Some("edge_wapp_verify:ich:wapp".into()),
        };
        assert!(validate_endpoint_row_for_open(&base).is_ok());
        // missing both verify_token and ref => rejected
        let mut no_vt = base.clone();
        no_vt.verify_token_ref = None;
        assert!(validate_endpoint_row_for_open(&no_vt).is_err());
        // a fanout_family that is not a fully-qualified IO.* glob => rejected
        let mut bad_glob = base.clone();
        bad_glob.fanout_family = Some("wapp".into());
        assert!(validate_endpoint_row_for_open(&bad_glob).is_err());
    }

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
                fanout_family: None,
                verify_token: None,
                verify_token_ref: None,
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
                fanout_family: None,
                verify_token: None,
                verify_token_ref: None,
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
            fanout_family: None,
            verify_token: None,
            verify_token_ref: None,
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
        headers.insert("host", "edge.example.com".parse().unwrap());
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
        const { assert!(DEFAULT_MAX_INFLIGHT > 0, "the default cap must be positive") };
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

    // -----------------------------------------------------------------------
    // TLS (docs/acme-edge-certificates-v1.md §4, §10 "Edge")
    // -----------------------------------------------------------------------

    const EDGE: &str = "edge.example.com";

    /// A self-signed leaf with rcgen's wide default validity: (cert PEM, key PEM).
    fn self_signed(names: &[&str]) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("key");
        let params = rcgen::CertificateParams::new(
            names
                .iter()
                .map(|name| name.to_string())
                .collect::<Vec<_>>(),
        )
        .expect("params");
        (
            params.self_signed(&key).expect("cert").pem(),
            key.serialize_pem(),
        )
    }

    /// A self-signed leaf for `EDGE`, valid only between two dates.
    fn self_signed_between(from: (i32, u8, u8), to: (i32, u8, u8)) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("key");
        let mut params = rcgen::CertificateParams::new(vec![EDGE.to_string()]).expect("params");
        params.not_before = rcgen::date_time_ymd(from.0, from.1, from.2);
        params.not_after = rcgen::date_time_ymd(to.0, to.1, to.2);
        (
            params.self_signed(&key).expect("cert").pem(),
            key.serialize_pem(),
        )
    }

    /// A leaf for `EDGE` issued by a fresh CA: (leaf PEM, CA PEM, leaf key PEM).
    fn issued_by_ca() -> (String, String, String) {
        let ca_key = rcgen::KeyPair::generate().expect("ca key");
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).expect("ca");
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Edge Test CA");
        let ca = rcgen::CertifiedIssuer::self_signed(ca_params, ca_key).expect("ca cert");
        let leaf_key = rcgen::KeyPair::generate().expect("leaf key");
        let leaf = rcgen::CertificateParams::new(vec![EDGE.to_string()])
            .expect("leaf")
            .signed_by(&leaf_key, &ca)
            .expect("leaf cert");
        (leaf.pem(), ca.pem(), leaf_key.serialize_pem())
    }

    /// The operator's PB-2 file: the leaf's END glued to the next BEGIN on one line.
    fn glue(leaf: &str, next: &str) -> String {
        format!("{}{}", leaf.trim_end_matches('\n'), next)
    }

    fn leaf_der(cert_pem: &str) -> Vec<u8> {
        let (chain, _) = tls_material::parse_chain(cert_pem.as_bytes()).expect("chain");
        chain.first().expect("a leaf").to_vec()
    }

    type Reply = Result<VaultTlsSecret, TlsRefusal>;

    /// A scripted vault: each read takes the next reply.
    struct FakeSource {
        replies: std::sync::Mutex<std::collections::VecDeque<Reply>>,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl FakeSource {
        fn new(replies: Vec<Reply>) -> Arc<Self> {
            Arc::new(Self {
                replies: std::sync::Mutex::new(replies.into()),
                reads: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn reads(&self) -> usize {
            self.reads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl TlsSecretSource for FakeSource {
        async fn read(&self, _key: &str) -> Result<VaultTlsSecret, TlsRefusal> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.replies
                .lock()
                .expect("replies")
                .pop_front()
                .unwrap_or_else(|| refused(TlsVerdict::Transient))
        }
    }

    fn secret(material: (String, String)) -> Reply {
        secret_at(material, 1, "2026-10-21T10:00:00+00:00")
    }

    /// A secret as one particular vault write holds it.
    fn secret_at((cert, key): (String, String), version: i64, updated_at: &str) -> Reply {
        Ok(VaultTlsSecret {
            cert,
            key,
            version: Some(version),
            updated_at: Some(updated_at.to_string()),
        })
    }

    /// A complete chain for `EDGE` (leaf + CA) and its key: what a reload may swap in.
    fn chained() -> (String, String) {
        let (leaf, ca, key) = issued_by_ca();
        (format!("{leaf}{ca}"), key)
    }

    fn refused(verdict: TlsVerdict) -> Reply {
        Err(TlsRefusal {
            verdict,
            detail: format!("scripted {}", verdict.as_str()),
        })
    }

    fn vault_edge(source: &Arc<FakeSource>) -> Arc<EdgeTls> {
        Arc::new(EdgeTls::vault(
            "edge_tls".to_string(),
            Arc::clone(source) as _,
        ))
    }

    /// A vault edge already serving `material`.
    async fn serving_edge(material: (String, String)) -> Arc<EdgeTls> {
        let tls = vault_edge(&FakeSource::new(vec![secret(material)]));
        assert!(tls.reload("boot").await.expect("a vault edge").changed);
        tls
    }

    /// Two PEM files in a fresh directory, for a disk edge.
    fn disk_files((cert, key): (String, String)) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("sy-edge-tls-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        let (cert_path, key_path) = (dir.join("edge.crt"), dir.join("edge.key"));
        std::fs::write(&cert_path, cert).expect("cert file");
        std::fs::write(&key_path, key).expect("key file");
        (cert_path, key_path)
    }

    fn edge_command(src: Option<&str>, msg: &str, payload: Value) -> Message {
        Message {
            routing: Routing {
                src: "src-uuid".to_string(),
                src_l2_name: src.map(ToString::to_string),
                dst: Destination::Unicast("SY.edge@ingress1".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(msg.to_string()),
                ..Meta::default()
            },
            payload,
        }
    }

    fn admin_command(msg: &str, payload: Value) -> Message {
        edge_command(Some("SY.admin@motherbee"), msg, payload)
    }

    async fn answer(tls: Option<&EdgeTls>, msg: &str, payload: Value) -> Value {
        tls_command_reply(tls, &admin_command(msg, payload))
            .await
            .expect("a TLS command")
            .1
    }

    /// The `acmeIdentifier` extension of a DER certificate: (critical, raw value).
    fn acme_identifier(der: &[u8]) -> Option<(bool, Vec<u8>)> {
        let (_, cert) = X509Certificate::from_der(der).ok()?;
        cert.extensions()
            .iter()
            .find(|ext| ext.oid.to_id_string() == "1.3.6.1.5.5.7.1.31")
            .map(|ext| (ext.critical, ext.value.to_vec()))
    }

    /// The tests read what the edge presents; they never judge it. Not even the signatures:
    /// rustls checks those through webpki, which refuses the challenge certificate's unknown
    /// critical extension (an ACME validator parses that certificate itself).
    #[derive(Debug)]
    struct AcceptAnything;

    impl rustls::client::danger::ServerCertVerifier for AcceptAnything {
        fn verify_server_cert(
            &self,
            _: &CertificateDer<'_>,
            _: &[CertificateDer<'_>],
            _: &rustls::pki_types::ServerName<'_>,
            _: &[u8],
            _: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn verify_tls13_signature(
            &self,
            _: &[u8],
            _: &CertificateDer<'_>,
            _: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }

    fn client_config(alpn: &[&[u8]], sni: bool) -> Arc<rustls::ClientConfig> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnything))
            .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|protocol| protocol.to_vec()).collect();
        config.enable_sni = sni;
        Arc::new(config)
    }

    fn server_name(name: &str) -> rustls::pki_types::ServerName<'static> {
        rustls::pki_types::ServerName::try_from(name.to_string()).expect("server name")
    }

    /// What a client saw of one handshake with the edge's acceptor.
    struct Seen {
        /// The leaf presented, when the handshake completed.
        leaf: Option<Vec<u8>>,
        alpn: Option<Vec<u8>>,
        /// The acceptor handed the connection on to HTTP: the main path.
        reached_http: bool,
    }

    /// One handshake over an in-process pipe: a rustls client against `accept_tls`.
    async fn handshake(tls: &Arc<EdgeTls>, name: &str, sni: bool, alpn: &[&[u8]]) -> Seen {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server_tls = Arc::clone(tls);
        let server = tokio::spawn(async move { accept_tls(server_io, &server_tls).await });
        let client = tokio_rustls::TlsConnector::from(client_config(alpn, sni))
            .connect(server_name(name), client_io)
            .await;
        let (leaf, alpn) = match &client {
            Ok(stream) => {
                let (_, connection) = stream.get_ref();
                (
                    connection
                        .peer_certificates()
                        .and_then(|chain| chain.first())
                        .map(|leaf| leaf.to_vec()),
                    connection.alpn_protocol().map(<[u8]>::to_vec),
                )
            }
            Err(_) => (None, None),
        };
        let reached_http = server.await.expect("acceptor task").is_some();
        Seen {
            leaf,
            alpn,
            reached_http,
        }
    }

    /// The bytes of a ClientHello, for a client that stops right after it.
    fn client_hello(alpn: &[&[u8]]) -> Vec<u8> {
        let mut connection =
            rustls::ClientConnection::new(client_config(alpn, true), server_name(EDGE))
                .expect("client");
        let mut hello = Vec::new();
        connection.write_tls(&mut hello).expect("hello");
        hello
    }

    async fn try_connect_tls(
        addr: std::net::SocketAddr,
    ) -> std::io::Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>> {
        let tcp = tokio::net::TcpStream::connect(addr).await?;
        tokio_rustls::TlsConnector::from(client_config(&[], true))
            .connect(server_name(EDGE), tcp)
            .await
    }

    async fn connect_tls(
        addr: std::net::SocketAddr,
    ) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
        try_connect_tls(addr).await.expect("tls handshake")
    }

    /// A TLS door on a loopback port, serving `/healthz` with `tls` under `limits`.
    async fn loopback_door(tls: &Arc<EdgeTls>, limits: DoorLimits) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = Router::new().route("/healthz", any(|| async { "ok" }));
        tokio::spawn(serve_tls_listener(listener, app, Arc::clone(tls), limits));
        addr
    }

    /// How long until the server closes `stream`: a read that ends (EOF or error) within
    /// `limit`, after any bytes the server still sends.
    async fn closed_within<S>(stream: &mut S, limit: Duration) -> Duration
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        let started = Instant::now();
        let mut chunk = [0_u8; 1024];
        loop {
            match tokio::time::timeout(limit, stream.read(&mut chunk)).await {
                Err(_) => panic!("the server kept the connection open past {limit:?}"),
                Ok(Ok(0)) | Ok(Err(_)) => return started.elapsed(),
                Ok(Ok(_)) => {}
            }
        }
    }

    /// Write `bytes` in ten pieces spread over `over`, then keep the pipe open.
    async fn trickle(
        mut io: tokio::io::DuplexStream,
        bytes: Vec<u8>,
        over: Duration,
    ) -> tokio::io::DuplexStream {
        let piece = bytes.len().div_ceil(10).max(1);
        for chunk in bytes.chunks(piece) {
            tokio::time::sleep(over / 10).await;
            tokio::io::AsyncWriteExt::write_all(&mut io, chunk)
                .await
                .expect("trickle");
        }
        io
    }

    fn presented_leaf(stream: &tokio_rustls::client::TlsStream<tokio::net::TcpStream>) -> Vec<u8> {
        stream
            .get_ref()
            .1
            .peer_certificates()
            .and_then(|chain| chain.first())
            .map(|leaf| leaf.to_vec())
            .expect("a leaf")
    }

    /// `GET /healthz` over an open connection; the body.
    async fn get_healthz<S>(stream: &mut S) -> String
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        tokio::io::AsyncWriteExt::write_all(
            stream,
            b"GET /healthz HTTP/1.1\r\nhost: edge.example.com\r\n\r\n",
        )
        .await
        .expect("request");
        let mut response = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk))
                .await
                .expect("a response in time")
                .expect("read");
            assert!(read > 0, "the connection closed before the response");
            response.extend_from_slice(&chunk[..read]);
            let text = String::from_utf8_lossy(&response).to_string();
            if let Some((head, body)) = text.split_once("\r\n\r\n") {
                let length = head.lines().find_map(|line| {
                    let line = line.to_ascii_lowercase();
                    line.strip_prefix("content-length:")
                        .map(|value| value.trim().parse::<usize>().expect("length"))
                });
                if let Some(length) = length {
                    if body.len() >= length {
                        return body[..length].to_string();
                    }
                }
            }
        }
    }

    #[test]
    fn the_tls_mode_is_explicit_and_never_falls_back_to_plaintext() {
        let cert = Some(std::path::Path::new("edge.crt"));
        let key = Some(std::path::Path::new("edge.key"));
        assert_eq!(
            TlsMode::resolve(Some(" edge_tls "), cert, key),
            Ok(TlsMode::Vault {
                key: "edge_tls".into()
            }),
            "a vault key wins over disk files"
        );
        assert_eq!(
            TlsMode::resolve(None, cert, key),
            Ok(TlsMode::Disk {
                cert: "edge.crt".into(),
                key: "edge.key".into()
            })
        );
        assert_eq!(TlsMode::resolve(None, None, None), Ok(TlsMode::Plaintext));
        assert!(TlsMode::resolve(Some("  "), None, None).is_err());
        assert!(TlsMode::resolve(None, cert, None).is_err());
        assert!(TlsMode::resolve(None, None, key).is_err());
    }

    /// RFC 8737 §3: the challenge certificate holds the critical acmeIdentifier extension whose
    /// value is the 32-byte digest as an OCTET STRING, and the name as its only SAN.
    #[test]
    fn the_challenge_certificate_carries_the_critical_digest_and_only_the_name() {
        let digest = [0xab_u8; 32];
        let (cert, _) = acme_challenge_certificate(EDGE, &digest).expect("challenge certificate");
        let (critical, value) = acme_identifier(cert.as_ref()).expect("acmeIdentifier");
        assert!(critical, "the acmeIdentifier extension MUST be critical");
        let mut octet_string = vec![0x04, 0x20];
        octet_string.extend_from_slice(&digest);
        assert_eq!(value, octet_string);
        let (_, parsed) = X509Certificate::from_der(cert.as_ref()).expect("parses");
        let san = parsed
            .subject_alternative_name()
            .expect("SAN parses")
            .expect("SAN present");
        assert!(matches!(
            san.value.general_names.as_slice(),
            [GeneralName::DNSName(name)] if *name == EDGE
        ));
        assert_eq!(parsed.subject().iter().count(), 0, "an empty subject");
        assert_eq!(
            parsed.public_key().algorithm.algorithm.to_id_string(),
            "1.2.840.10045.2.1",
            "an EC (P-256) key"
        );
    }

    /// rcgen asserts the digest's length; a bad digest must be refused before it gets there.
    #[test]
    fn a_digest_that_is_not_32_bytes_of_hex_is_refused_before_rcgen() {
        let tls = EdgeTls::vault("edge_tls".to_string(), FakeSource::new(vec![]));
        for digest in [
            String::new(),
            "ab".to_string(),
            "a".repeat(63),
            "a".repeat(65),
            "zz".repeat(32),
        ] {
            assert!(
                tls.arm_challenge(EDGE, &digest, 600, 1_000).is_err(),
                "{digest:?}"
            );
        }
        assert!(
            tls.arm_challenge(EDGE, &"AB".repeat(32), 600, 1_000)
                .is_ok(),
            "upper-case hex is hex"
        );
    }

    #[test]
    fn an_armed_challenge_expires_and_a_clear_removes_only_its_own_digest() {
        let tls = EdgeTls::vault("edge_tls".to_string(), FakeSource::new(vec![]));
        let first = "aa".repeat(32);
        let newer = "BB".repeat(32);
        assert_eq!(
            tls.arm_challenge("Edge.Example.COM.", &first, 600, 1_000),
            Ok(ChallengeAnswer {
                name: EDGE.into(),
                armed: true,
                expires_at: Some(1_600)
            }),
            "the name is normalized"
        );
        assert!(tls.challenge_config(EDGE, 1_599).is_some());
        assert!(
            tls.challenge_config(EDGE, 1_600).is_none(),
            "gone at its TTL"
        );

        // A late clear of an older arm never removes the newer one.
        tls.arm_challenge(EDGE, &first, 600, 2_000).expect("arm");
        tls.arm_challenge(EDGE, &newer, 600, 2_010).expect("re-arm");
        assert_eq!(
            tls.clear_challenge(EDGE, &first, 2_020),
            Ok(ChallengeAnswer {
                name: EDGE.into(),
                armed: true,
                expires_at: Some(2_610)
            })
        );
        assert!(tls.challenge_config(EDGE, 2_020).is_some());
        assert_eq!(
            tls.clear_challenge(EDGE, &newer.to_lowercase(), 2_030),
            Ok(ChallengeAnswer {
                name: EDGE.into(),
                armed: false,
                expires_at: None
            }),
            "its own digest, in any case, clears it"
        );
        assert!(tls.challenge_config(EDGE, 2_030).is_none());

        assert!(tls.arm_challenge(EDGE, &first, 0, 3_000).is_err());
        assert!(tls
            .arm_challenge(EDGE, &first, ACME_CHALLENGE_MAX_TTL_SECS + 1, 3_000)
            .is_err());
        assert!(tls
            .arm_challenge(EDGE, &first, ACME_CHALLENGE_MAX_TTL_SECS, 3_000)
            .is_ok());
        for name in [
            "",
            "*.example.com",
            "192.0.2.1",
            "edge_tls.example.com",
            "-edge.example.com",
            "edge..example.com",
        ] {
            assert!(
                tls.arm_challenge(name, &first, 600, 3_000).is_err(),
                "{name:?}"
            );
        }
    }

    #[tokio::test]
    async fn the_acceptor_routes_each_client_hello_as_the_design_table_says() {
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let main_leaf = tls.served().expect("serving").chain[0].to_vec();
        let digest = [0x5a_u8; 32];
        tls.arm_challenge(EDGE, &hex::encode(digest), 600, now_unix_secs())
            .expect("armed");
        let mut challenge_value = vec![0x04, 0x20];
        challenge_value.extend_from_slice(&digest);

        // ALPN exactly acme-tls/1, a challenge armed for the SNI: the challenge certificate, the
        // acme-tls/1 protocol, and the connection closed, never handed to HTTP.
        for name in [EDGE, "EDGE.Example.com"] {
            let seen = handshake(&tls, name, true, &[ACME_TLS_ALPN]).await;
            assert_eq!(seen.alpn.as_deref(), Some(ACME_TLS_ALPN), "{name}");
            assert_eq!(
                seen.leaf.as_deref().and_then(acme_identifier),
                Some((true, challenge_value.clone())),
                "{name}"
            );
            assert!(!seen.reached_http, "a challenge never reaches HTTP");
        }

        // ALPN exactly acme-tls/1 and nothing armed for the SNI, or no SNI at all: dropped.
        for (name, sni) in [("other.example.com", true), (EDGE, false)] {
            let seen = handshake(&tls, name, sni, &[ACME_TLS_ALPN]).await;
            assert!(seen.leaf.is_none(), "{name} sni={sni}");
            assert!(!seen.reached_http);
        }

        // Anything else, acme-tls/1 mixed with other protocols included: the main configuration
        // with no ALPN negotiated, then HTTP.
        for (alpn, sni) in [
            (vec![ACME_TLS_ALPN, b"h2".as_slice()], true),
            (vec![b"h2".as_slice(), b"http/1.1".as_slice()], true),
            (vec![], true),
            (vec![], false),
        ] {
            let seen = handshake(&tls, EDGE, sni, &alpn).await;
            assert_eq!(
                seen.leaf.as_deref(),
                Some(main_leaf.as_slice()),
                "{alpn:?} sni={sni}"
            );
            assert_eq!(
                seen.alpn, None,
                "{alpn:?}: the main configuration has no ALPN"
            );
            assert!(seen.reached_http);
        }
    }

    #[tokio::test]
    async fn an_awaiting_edge_answers_armed_challenges_and_drops_everything_else() {
        let tls = vault_edge(&FakeSource::new(vec![refused(TlsVerdict::NotFound)]));
        tls.reload("boot").await.expect("a vault edge");
        tls.arm_challenge(EDGE, &"ef".repeat(32), 600, now_unix_secs())
            .expect("armed");
        let challenge = handshake(&tls, EDGE, true, &[ACME_TLS_ALPN]).await;
        assert_eq!(challenge.alpn.as_deref(), Some(ACME_TLS_ALPN));
        for alpn in [
            vec![],
            vec![b"h2".as_slice(), b"http/1.1".as_slice()],
            vec![ACME_TLS_ALPN, b"h2".as_slice()],
        ] {
            let seen = handshake(&tls, EDGE, true, &alpn).await;
            assert!(
                seen.leaf.is_none() && !seen.reached_http,
                "{alpn:?}: an awaiting edge serves nothing else"
            );
        }
    }

    /// ONE timeout covers the ClientHello and the handshake. A client trickles its hello over
    /// 0.7 T and then stalls the handshake: two per-phase timeouts would hold it about 1.7 T.
    #[tokio::test]
    async fn one_timeout_covers_the_client_hello_and_the_handshake() {
        let timeout = Duration::from_secs(1);
        let source = FakeSource::new(vec![secret(self_signed(&[EDGE]))]);
        let tls = Arc::new(
            EdgeTls::vault("edge_tls".to_string(), source).with_handshake_timeout(timeout),
        );
        tls.reload("boot").await.expect("a vault edge");
        tls.arm_challenge(EDGE, &"cd".repeat(32), 600, now_unix_secs())
            .expect("armed");

        let silent = async {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            let started = Instant::now();
            assert!(accept_tls(server_io, &tls).await.is_none());
            drop(client_io);
            started.elapsed()
        };
        let trickled = |alpn: Vec<&'static [u8]>| {
            let tls = Arc::clone(&tls);
            async move {
                let (client_io, server_io) = tokio::io::duplex(64 * 1024);
                let started = Instant::now();
                let (_client_io, accepted) = tokio::join!(
                    trickle(client_io, client_hello(&alpn), timeout.mul_f32(0.7)),
                    accept_tls(server_io, &tls)
                );
                assert!(accepted.is_none());
                started.elapsed()
            }
        };
        let (silent, main_path, challenge_path) =
            tokio::join!(silent, trickled(Vec::new()), trickled(vec![ACME_TLS_ALPN]));
        for (case, elapsed) in [
            ("sends nothing", silent),
            ("main path", main_path),
            ("challenge path", challenge_path),
        ] {
            assert!(elapsed >= timeout, "{case}: {elapsed:?}");
            assert!(elapsed < timeout.mul_f32(1.35), "{case}: {elapsed:?}");
        }
        assert_eq!(TLS_HANDSHAKE_TIMEOUT, Duration::from_secs(10));
    }

    #[tokio::test]
    async fn a_reload_swaps_new_connections_and_keeps_the_open_ones() {
        let (first, second) = (chained(), chained());
        let tls = vault_edge(&FakeSource::new(vec![
            secret(first.clone()),
            secret(second.clone()),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        let addr = loopback_door(
            &tls,
            DoorLimits::new(MAX_TLS_CONNECTIONS, HTTP_HEADER_READ_TIMEOUT),
        )
        .await;

        let mut open = connect_tls(addr).await;
        assert_eq!(presented_leaf(&open), leaf_der(&first.0));
        assert_eq!(get_healthz(&mut open).await, "ok");

        assert_eq!(
            tls.reload(MSG_EDGE_TLS_RELOAD).await,
            Ok(ReloadOutcome {
                changed: true,
                refused: None
            })
        );

        let mut fresh = connect_tls(addr).await;
        assert_eq!(presented_leaf(&fresh), leaf_der(&second.0));
        assert_eq!(get_healthz(&mut fresh).await, "ok");
        // The connection opened before the swap carries on.
        assert_eq!(get_healthz(&mut open).await, "ok");
    }

    #[tokio::test]
    async fn a_reload_refuses_bad_material_and_keeps_serving_the_current_certificate() {
        let (leaf, ca, key) = issued_by_ca();
        let good = (format!("{leaf}{ca}"), key.clone());
        let cases: Vec<(&str, Reply, TlsVerdict)> = vec![
            (
                "truncated",
                secret((glue(&leaf, &ca), key.clone())),
                TlsVerdict::InvalidMaterial,
            ),
            (
                "expired",
                secret(self_signed_between((2020, 1, 1), (2021, 1, 1))),
                TlsVerdict::InvalidMaterial,
            ),
            (
                "not yet valid",
                secret(self_signed_between((2090, 1, 1), (2091, 1, 1))),
                TlsVerdict::InvalidMaterial,
            ),
            (
                "key mismatch",
                secret((good.0.clone(), self_signed(&[EDGE]).1)),
                TlsVerdict::InvalidMaterial,
            ),
            (
                "leaf only",
                secret(self_signed(&[EDGE])),
                TlsVerdict::InvalidMaterial,
            ),
            (
                "deleted",
                refused(TlsVerdict::NotFound),
                TlsVerdict::NotFound,
            ),
            (
                "vault silent",
                refused(TlsVerdict::Transient),
                TlsVerdict::Transient,
            ),
        ];
        for (case, reply, verdict) in cases {
            let tls = vault_edge(&FakeSource::new(vec![secret(good.clone()), reply]));
            tls.reload("boot").await.expect("a vault edge");
            let before = tls.served().expect("serving");
            let outcome = tls.reload(MSG_EDGE_TLS_RELOAD).await.expect("a vault edge");
            assert!(!outcome.changed, "{case}");
            assert_eq!(
                outcome.refused.map(|refusal| refusal.verdict),
                Some(verdict),
                "{case}"
            );
            let after = tls.served().expect("still serving");
            assert!(
                Arc::ptr_eq(&before.config, &after.config),
                "{case}: the main configuration was kept"
            );
            assert_eq!(tls.status_payload()["state"], "serving", "{case}");
        }
    }

    /// Today's rule (PB-2) when nothing is served yet, at boot or while awaiting: a truncated
    /// chain is served as parsed, loudly. Every other check still applies.
    #[tokio::test]
    async fn a_truncated_chain_is_served_when_nothing_else_is() {
        let (leaf, ca, key) = issued_by_ca();
        let tls = vault_edge(&FakeSource::new(vec![secret((
            glue(&leaf, &ca),
            key.clone(),
        ))]));
        assert_eq!(
            tls.reload("boot").await,
            Ok(ReloadOutcome {
                changed: true,
                refused: None
            })
        );
        let status = tls.status_payload();
        assert_eq!(status["state"], "serving");
        assert_eq!(status["chain_len"], 1, "what the parser kept: the leaf");

        let (cert_path, key_path) = disk_files((glue(&leaf, &ca), key));
        let disk = EdgeTls::from_disk(cert_path, key_path, now_unix_secs()).expect("disk edge");
        assert_eq!(disk.status_payload()["chain_len"], 1);

        let (cert_path, key_path) = disk_files(self_signed_between((2020, 1, 1), (2021, 1, 1)));
        assert!(EdgeTls::from_disk(cert_path, key_path, now_unix_secs()).is_err());
    }

    #[tokio::test]
    async fn a_reread_of_the_material_already_served_changes_nothing() {
        let material = self_signed(&[EDGE]);
        let tls = vault_edge(&FakeSource::new(vec![
            secret_at(material.clone(), 2, "2026-10-21T10:00:00+00:00"),
            // A rollback re-mints version 2 for the same bytes; only updated_at tells them apart.
            secret_at(material, 2, "2026-10-21T11:30:00+00:00"),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        let first = tls.served().expect("serving");
        assert_eq!(
            tls.reload("VAULT_SECRET_CHANGED put").await,
            Ok(ReloadOutcome {
                changed: false,
                refused: None
            })
        );
        let second = tls.served().expect("serving");
        assert!(Arc::ptr_eq(&first.config, &second.config));
        assert_eq!(first.loaded_at, second.loaded_at);
        let status = tls.status_payload();
        assert_eq!(status["vault_version"], 2);
        assert_eq!(
            status["vault_updated_at"], "2026-10-21T11:30:00+00:00",
            "the write identity follows the read"
        );
    }

    #[tokio::test]
    async fn fixing_the_intermediates_of_the_same_leaf_is_a_change() {
        let (leaf, ca, key) = issued_by_ca();
        let tls = vault_edge(&FakeSource::new(vec![
            secret((leaf.clone(), key.clone())),
            secret((format!("{leaf}{ca}"), key)),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        let leaf_only = tls.status_payload();
        assert_eq!(leaf_only["chain_len"], 1);
        assert_eq!(
            tls.reload(MSG_EDGE_TLS_RELOAD).await,
            Ok(ReloadOutcome {
                changed: true,
                refused: None
            })
        );
        let full = tls.status_payload();
        assert_eq!(full["chain_len"], 2);
        assert_eq!(
            full["fingerprint"], leaf_only["fingerprint"],
            "the same leaf"
        );
    }

    #[tokio::test]
    async fn the_live_material_follows_every_swap_and_fingerprints_the_leaf_der() {
        let (first, second) = (chained(), chained());
        let tls = vault_edge(&FakeSource::new(vec![
            refused(TlsVerdict::NotFound),
            secret(first.clone()),
            secret(second.clone()),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        assert_eq!(tls.live_material(), LiveTlsMaterial::Awaiting);
        for (cert, _) in [first, second] {
            tls.reload(MSG_EDGE_TLS_RELOAD).await.expect("a vault edge");
            let fingerprint = hex::encode(Sha256::digest(leaf_der(&cert)));
            assert_ne!(
                fingerprint,
                hex::encode(Sha256::digest(cert.as_bytes())),
                "the leaf's DER, not the PEM"
            );
            assert_eq!(
                tls.live_material(),
                LiveTlsMaterial::FromVault {
                    version: Some(1),
                    fingerprint: fingerprint.clone()
                }
            );
            assert_eq!(tls.status_payload()["fingerprint"], fingerprint);
        }
        assert_eq!(
            EdgeTls::plaintext().live_material(),
            LiveTlsMaterial::NotFromVault
        );
    }

    #[tokio::test]
    async fn every_vault_verdict_at_boot_awaits_and_the_rereads_leave_it() {
        let mismatched = (self_signed(&[EDGE]).0, self_signed(&[EDGE]).1);
        let cases: Vec<(TlsVerdict, Reply)> = vec![
            (TlsVerdict::NotFound, refused(TlsVerdict::NotFound)),
            (TlsVerdict::Forbidden, refused(TlsVerdict::Forbidden)),
            (TlsVerdict::Transient, refused(TlsVerdict::Transient)),
            (
                TlsVerdict::InvalidMaterial,
                secret(self_signed_between((2020, 1, 1), (2021, 1, 1))),
            ),
            (TlsVerdict::InvalidMaterial, secret(mismatched)),
            (
                TlsVerdict::InvalidMaterial,
                secret(("not a certificate".into(), "not a key".into())),
            ),
        ];
        for (verdict, first) in cases {
            let source = FakeSource::new(vec![first.clone(), first, secret(self_signed(&[EDGE]))]);
            let tls = vault_edge(&source);
            start_vault_tls(
                &tls,
                AwaitingSchedule {
                    transient: Duration::from_millis(5),
                    transient_window: Duration::from_secs(60),
                    first: Duration::from_millis(10),
                    max: Duration::from_millis(40),
                },
            )
            .await;
            let status = tls.status_payload();
            assert_eq!(status["state"], "awaiting", "{verdict:?}");
            assert_eq!(status["reason"], verdict.as_str());
            assert!(status["fingerprint"].is_null());
            assert!(
                tls.main_config().is_none(),
                "awaiting has no main configuration"
            );

            // The re-reads (one more refusal, then a good secret) leave AWAITING on their own.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !tls.is_serving() {
                assert!(Instant::now() < deadline, "{verdict:?}: still awaiting");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(source.reads(), 3, "{verdict:?}");
            assert_eq!(tls.status_payload()["state"], "serving");
        }
    }

    /// A vault that does not answer at boot is retried every 3 s for 2 minutes (the old unit
    /// restarted every 5 s); a missing, forbidden or invalid secret waits 30 s doubling to 5 min.
    #[test]
    fn awaiting_rereads_follow_the_verdict() {
        let secs = |durations: Vec<Duration>| -> Vec<u64> {
            durations.iter().map(Duration::as_secs).collect()
        };
        let mut backoff = AwaitingBackoff::new(AWAITING_SCHEDULE);
        let mut elapsed = Duration::ZERO;
        let mut delays = Vec::new();
        while elapsed < Duration::from_secs(120) {
            let delay = backoff.next(TlsVerdict::Transient, elapsed);
            delays.push(delay);
            elapsed += delay;
        }
        assert_eq!(delays.len(), 40, "every 3 s for the first 2 minutes");
        assert!(delays.iter().all(|delay| *delay == Duration::from_secs(3)));
        let later = (0..6)
            .map(|_| backoff.next(TlsVerdict::Transient, Duration::from_secs(121)))
            .collect();
        assert_eq!(secs(later), vec![30, 60, 120, 240, 300, 300]);

        for verdict in [
            TlsVerdict::NotFound,
            TlsVerdict::Forbidden,
            TlsVerdict::InvalidMaterial,
        ] {
            let mut backoff = AwaitingBackoff::new(AWAITING_SCHEDULE);
            let delays = (0..6)
                .map(|_| backoff.next(verdict, Duration::ZERO))
                .collect();
            assert_eq!(
                secs(delays),
                vec![30, 60, 120, 240, 300, 300],
                "{verdict:?}"
            );
        }
    }

    #[tokio::test]
    async fn status_reports_each_tls_mode_and_only_a_vault_edge_reloads_or_arms() {
        let plaintext = EdgeTls::plaintext();
        let status = answer(Some(&plaintext), MSG_EDGE_TLS_STATUS, json!({})).await;
        assert_eq!(status["status"], "ok");
        assert_eq!(status["mode"], "plaintext");
        assert_eq!(status["state"], "serving");
        assert!(status["tls_vault_key"].is_null() && status["fingerprint"].is_null());
        assert!(status["vault_version"].is_null() && status["vault_updated_at"].is_null());

        let (cert_path, key_path) = disk_files(self_signed(&[EDGE]));
        let disk = EdgeTls::from_disk(cert_path, key_path, now_unix_secs()).expect("disk edge");
        let status = answer(Some(&disk), MSG_EDGE_TLS_STATUS, json!({})).await;
        assert_eq!(status["mode"], "disk");
        assert_eq!(status["state"], "serving");
        assert!(status["tls_vault_key"].is_null());
        assert_eq!(status["names"], json!([EDGE]));
        assert_eq!(status["chain_len"], 1);
        assert_eq!(status["fingerprint"].as_str().map(str::len), Some(64));
        assert!(status["leaf_pem"]
            .as_str()
            .is_some_and(|pem| pem.starts_with("-----BEGIN CERTIFICATE-----\n")));
        assert!(status["loaded_at"].is_i64());
        assert!(status["vault_version"].is_null() && status["vault_updated_at"].is_null());

        for edge in [&plaintext, &disk] {
            for (command, payload) in [
                (MSG_EDGE_TLS_RELOAD, json!({})),
                (
                    MSG_EDGE_ACME_CHALLENGE,
                    json!({"name": EDGE, "digest": "ab".repeat(32)}),
                ),
            ] {
                let reply = answer(Some(edge), command, payload).await;
                assert_eq!(reply["status"], "error", "{command}");
                assert!(reply["error"]
                    .as_str()
                    .is_some_and(|err| err.contains("only a vault edge")));
            }
        }

        let serving = serving_edge(self_signed(&[EDGE])).await;
        let status = answer(Some(&serving), MSG_EDGE_TLS_STATUS, json!({})).await;
        assert_eq!(status["mode"], "vault");
        assert_eq!(status["tls_vault_key"], "edge_tls");
        assert_eq!(status["state"], "serving");
        assert!(status["reason"].is_null());
        assert_eq!(status["vault_version"], 1);
        assert_eq!(status["vault_updated_at"], "2026-10-21T10:00:00+00:00");
        assert!(status["not_before"].as_i64() < status["not_after"].as_i64());
        assert!(status["issuer"].is_string());

        let awaiting = vault_edge(&FakeSource::new(vec![refused(TlsVerdict::NotFound)]));
        awaiting.reload("boot").await.expect("a vault edge");
        let status = answer(Some(&awaiting), MSG_EDGE_TLS_STATUS, json!({})).await;
        assert_eq!(status["mode"], "vault");
        assert_eq!(status["state"], "awaiting");
        assert_eq!(status["reason"], "not_found");
        assert!(status["detail"].is_string());
        assert!(status["fingerprint"].is_null() && status["leaf_pem"].is_null());
        assert!(status["vault_version"].is_null() && status["vault_updated_at"].is_null());

        // An edge with no public listener answers every TLS command with an error.
        for command in [
            MSG_EDGE_TLS_STATUS,
            MSG_EDGE_TLS_RELOAD,
            MSG_EDGE_ACME_CHALLENGE,
        ] {
            let reply = answer(None, command, json!({})).await;
            assert_eq!(reply["status"], "error", "{command}");
        }
    }

    #[tokio::test]
    async fn reload_answers_the_status_fields_with_changed_and_refused() {
        let tls = vault_edge(&FakeSource::new(vec![
            secret_at(chained(), 3, "2026-10-21T10:00:00+00:00"),
            secret_at(chained(), 4, "2026-10-21T11:00:00+00:00"),
            refused(TlsVerdict::Forbidden),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        let swapped = answer(Some(&tls), MSG_EDGE_TLS_RELOAD, json!({})).await;
        assert_eq!(swapped["status"], "ok");
        assert_eq!(swapped["changed"], true);
        assert!(swapped.get("refused").is_none());
        assert_eq!(swapped["state"], "serving");
        assert_eq!(swapped["vault_version"], 4);
        assert_eq!(swapped["vault_updated_at"], "2026-10-21T11:00:00+00:00");

        let refused = answer(Some(&tls), MSG_EDGE_TLS_RELOAD, json!({})).await;
        assert_eq!(refused["status"], "ok");
        assert_eq!(refused["changed"], false);
        assert_eq!(refused["refused"]["reason"], "forbidden");
        assert!(refused["refused"]["detail"].is_string());
        assert_eq!(
            refused["fingerprint"], swapped["fingerprint"],
            "still serving what it served"
        );
        assert_eq!(
            refused["vault_version"], 4,
            "and naming the write it came from"
        );
        assert_eq!(refused["vault_updated_at"], "2026-10-21T11:00:00+00:00");
    }

    #[tokio::test]
    async fn the_challenge_command_arms_for_600_s_by_default_and_clears_by_digest() {
        // An ingress with no certificate yet still answers ACME: that is how it gets its first.
        let tls = vault_edge(&FakeSource::new(vec![refused(TlsVerdict::NotFound)]));
        tls.reload("boot").await.expect("a vault edge");
        let digest = "cd".repeat(32);
        let before = now_unix_secs();
        let armed = answer(
            Some(&tls),
            MSG_EDGE_ACME_CHALLENGE,
            json!({"name": EDGE, "digest": digest}),
        )
        .await;
        assert_eq!(armed["status"], "ok");
        assert_eq!(armed["name"], EDGE);
        assert_eq!(armed["armed"], true);
        let expires_at = armed["expires_at"].as_i64().expect("expires_at");
        assert!((before + 600..=now_unix_secs() + 600).contains(&expires_at));

        let typo = answer(
            Some(&tls),
            MSG_EDGE_ACME_CHALLENGE,
            json!({"name": EDGE, "digest": digest, "ttl": 60}),
        )
        .await;
        assert_eq!(typo["status"], "error", "unknown fields are refused");

        let cleared = answer(
            Some(&tls),
            MSG_EDGE_ACME_CHALLENGE,
            json!({"name": EDGE, "digest": digest, "clear": true}),
        )
        .await;
        assert_eq!(cleared["status"], "ok");
        assert_eq!(cleared["armed"], false);
        assert!(cleared["expires_at"].is_null());
    }

    #[tokio::test]
    async fn tls_commands_come_only_from_the_motherbee_admin() {
        let source = FakeSource::new(vec![secret(self_signed(&[EDGE]))]);
        let tls = vault_edge(&source);
        tls.reload("boot").await.expect("a vault edge");
        let challenge = json!({"name": EDGE, "digest": "ab".repeat(32)});
        for command in [
            MSG_EDGE_TLS_STATUS,
            MSG_EDGE_TLS_RELOAD,
            MSG_EDGE_ACME_CHALLENGE,
        ] {
            for origin in [
                Some("SY.admin@ingress1"),
                Some("IO.cloud@motherbee"),
                Some(""),
                None,
            ] {
                let (_, reply) = tls_command_reply(
                    Some(&tls),
                    &edge_command(origin, command, challenge.clone()),
                )
                .await
                .expect("a TLS command");
                assert_eq!(reply["status"], "error", "{command} from {origin:?}");
            }
        }
        assert_eq!(source.reads(), 1, "no reload ran");
        assert!(
            tls.challenge_config(EDGE, now_unix_secs()).is_none(),
            "nothing was armed"
        );
        assert_eq!(
            answer(Some(&tls), MSG_EDGE_TLS_STATUS, json!({})).await["status"],
            "ok"
        );
    }

    #[tokio::test]
    async fn tls_commands_are_answered_while_the_system_loop_is_busy() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        // The sequential loop is stuck on an EDGE_PUBLISH_BLOB: nobody drains its channel.
        let mut system = dispatcher
            .take_command_receiver(RPC_CH_SYSTEM)
            .await
            .expect("system channel");
        router
            .inject(admin_command(MSG_EDGE_PUBLISH_BLOB, json!({})))
            .await
            .expect("blob command");
        let commands = dispatcher
            .take_command_receiver(RPC_CH_TLS)
            .await
            .expect("tls channel");
        let tls = serving_edge(self_signed(&[EDGE])).await;
        tokio::spawn(run_tls_commands(
            commands,
            dispatcher.sender_snapshot(),
            Some(tls),
            16,
        ));

        let status = admin_command(MSG_EDGE_TLS_STATUS, json!({}));
        router.inject(status.clone()).await.expect("status command");
        let reply = router
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("STATUS answered while the loop is busy");
        assert_eq!(
            reply.meta.msg.as_deref(),
            Some(MSG_EDGE_TLS_STATUS_RESPONSE)
        );
        assert_eq!(reply.routing.trace_id, status.routing.trace_id);
        assert!(matches!(
            &reply.routing.dst,
            Destination::Unicast(dst) if dst == "SY.admin@motherbee"
        ));
        assert_eq!(reply.payload["status"], "ok");
        assert_eq!(reply.payload["state"], "serving");

        // The blob command still waits for the loop, and no TLS command went its way.
        assert_eq!(
            system.try_recv().and_then(|msg| msg.meta.msg),
            Some(MSG_EDGE_PUBLISH_BLOB.to_string())
        );
        assert!(system.try_recv().is_none());
    }

    #[tokio::test]
    async fn the_vault_source_keeps_the_vault_verdict_typed() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let source = Arc::new(VaultSecretSource {
            client: fluxbee_sdk::VaultClient::new(
                Arc::clone(&dispatcher),
                "motherbee",
                fluxbee_sdk::VaultCallerOwned::new(
                    fluxbee_sdk::deterministic_system_ilk_id("SY.edge@ingress1"),
                    "SY.edge@ingress1",
                ),
            ),
            timeout: Duration::from_secs(5),
        });
        let (cert, key) = self_signed(&[EDGE]);
        let written = "2026-10-21T10:00:00.123456789+00:00";
        type Expected = Result<(Option<i64>, Option<String>), TlsVerdict>;
        let cases: Vec<(Value, Expected)> = vec![
            (
                json!({"status": "error", "error_code": "KEY_NOT_FOUND", "message": "key not found"}),
                Err(TlsVerdict::NotFound),
            ),
            (
                json!({"status": "error", "error_code": "UNAUTHORIZED", "message": "unauthorized"}),
                Err(TlsVerdict::Forbidden),
            ),
            (
                json!({"status": "error", "error_code": "IDENTITY_UNAVAILABLE", "message": "caller"}),
                Err(TlsVerdict::Transient),
            ),
            (
                json!({"status": "error", "error_code": "STORAGE_ERROR", "message": "database is locked"}),
                Err(TlsVerdict::Transient),
            ),
            (
                json!({"status": "ok", "key": "edge_tls", "value": {"cert": cert}}),
                Err(TlsVerdict::InvalidMaterial),
            ),
            (
                json!({
                    "status": "ok",
                    "key": "edge_tls",
                    "value": {"cert": cert, "key": key},
                    "version": 3,
                    "metadata": {"tenant_id": "tnt:00000000-0000-0000-0000-000000000000", "updated_at": written},
                }),
                Ok((Some(3), Some(written.to_string()))),
            ),
            (
                json!({"status": "ok", "key": "edge_tls", "value": {"cert": cert, "key": key}, "version": 4}),
                Ok((Some(4), None)),
            ),
        ];
        for (vault_reply, expected) in cases {
            let reader = Arc::clone(&source);
            let read = tokio::spawn(async move { reader.read("edge_tls").await });
            let request = router
                .next_outgoing_within(Duration::from_secs(5))
                .await
                .expect("a VAULT_GET");
            assert_eq!(
                request.meta.msg.as_deref(),
                Some(fluxbee_sdk::MSG_VAULT_GET)
            );
            assert_eq!(request.payload["key"], "edge_tls");
            router
                .inject(Message {
                    routing: Routing {
                        src: "vault-uuid".to_string(),
                        src_l2_name: Some("SY.vault@motherbee".to_string()),
                        dst: Destination::Unicast(request.routing.src.clone()),
                        ttl: 16,
                        trace_id: request.routing.trace_id.clone(),
                    },
                    meta: Meta {
                        msg_type: SYSTEM_KIND.to_string(),
                        msg: Some(fluxbee_sdk::MSG_VAULT_GET_RESPONSE.to_string()),
                        ..Meta::default()
                    },
                    payload: vault_reply,
                })
                .await
                .expect("vault reply");
            let got = read
                .await
                .expect("read task")
                .map(|secret| (secret.version, secret.updated_at))
                .map_err(|refusal| refusal.verdict);
            assert_eq!(got, expected);
        }
    }

    /// STATUS and RELOAD name the vault write served: the version AND the metadata's
    /// `updated_at`, because a vault rollback re-mints version numbers.
    #[tokio::test]
    async fn status_and_reload_name_the_vault_write_served() {
        let (first, second) = (chained(), chained());
        let tls = vault_edge(&FakeSource::new(vec![
            secret_at(first.clone(), 7, "2026-10-21T10:00:00+00:00"),
            secret_at(second, 8, "2026-10-21T11:00:00+00:00"),
            // A rollback: the first material again, under the re-minted version 7.
            secret_at(first, 7, "2026-10-21T12:00:00+00:00"),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        let status = answer(Some(&tls), MSG_EDGE_TLS_STATUS, json!({})).await;
        assert_eq!(
            (&status["vault_version"], &status["vault_updated_at"]),
            (&json!(7), &json!("2026-10-21T10:00:00+00:00"))
        );
        let put = answer(Some(&tls), MSG_EDGE_TLS_RELOAD, json!({})).await;
        assert_eq!(put["changed"], true);
        assert_eq!(
            (&put["vault_version"], &put["vault_updated_at"]),
            (&json!(8), &json!("2026-10-21T11:00:00+00:00"))
        );
        let rollback = answer(Some(&tls), MSG_EDGE_TLS_RELOAD, json!({})).await;
        assert_eq!(rollback["changed"], true);
        assert_eq!(
            (&rollback["vault_version"], &rollback["vault_updated_at"]),
            (&json!(7), &json!("2026-10-21T12:00:00+00:00")),
            "the same version number as at boot, another write"
        );
    }

    /// A served edge that gets no verdict from the vault keeps its certificate and only warns:
    /// the "REFUSED … vault_rollback" error is for material the vault holds and the edge refuses.
    #[test]
    fn a_reload_report_tells_a_silent_vault_from_bad_material() {
        let refused = |verdict| ReloadOutcome {
            changed: false,
            refused: Some(TlsRefusal {
                verdict,
                detail: "scripted".to_string(),
            }),
        };
        let serving = BeforeReload {
            serving: true,
            awaiting: None,
            first: false,
        };
        assert_eq!(
            reload_report(&serving, &refused(TlsVerdict::Transient)),
            ReloadReport::VaultSilent
        );
        for verdict in [
            TlsVerdict::NotFound,
            TlsVerdict::Forbidden,
            TlsVerdict::InvalidMaterial,
        ] {
            assert_eq!(
                reload_report(&serving, &refused(verdict)),
                ReloadReport::Refused,
                "{verdict:?}"
            );
        }
        let boot = BeforeReload {
            serving: false,
            awaiting: Some(TlsVerdict::Transient),
            first: true,
        };
        assert_eq!(
            reload_report(&boot, &refused(TlsVerdict::Transient)),
            ReloadReport::Awaiting,
            "the boot read is always reported in full"
        );
        let awaiting = BeforeReload {
            first: false,
            ..boot
        };
        assert_eq!(
            reload_report(&awaiting, &refused(TlsVerdict::Transient)),
            ReloadReport::StillAwaiting
        );
        assert_eq!(
            reload_report(&awaiting, &refused(TlsVerdict::NotFound)),
            ReloadReport::Awaiting,
            "a new reason is reported in full"
        );
        let loaded = ReloadOutcome {
            changed: true,
            refused: None,
        };
        assert_eq!(reload_report(&awaiting, &loaded), ReloadReport::Loaded);
        let same = ReloadOutcome {
            changed: false,
            refused: None,
        };
        assert_eq!(reload_report(&serving, &same), ReloadReport::Unchanged);
    }

    /// A leaf with no intermediates never REPLACES a served chain; with nothing served it is
    /// served (loudly), and re-reading the leaf-only chain already served changes nothing.
    #[tokio::test]
    async fn a_leaf_only_chain_never_replaces_a_served_certificate() {
        let full = chained();
        let leaf_only = self_signed(&[EDGE]);
        let tls = vault_edge(&FakeSource::new(vec![
            secret(full.clone()),
            secret(leaf_only.clone()),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        let outcome = tls.reload(MSG_EDGE_TLS_RELOAD).await.expect("a vault edge");
        assert_eq!(
            outcome.refused.as_ref().map(|refusal| refusal.verdict),
            Some(TlsVerdict::InvalidMaterial)
        );
        assert!(outcome
            .refused
            .is_some_and(|refusal| refusal.detail.contains("NO INTERMEDIATES")));
        assert_eq!(tls.status_payload()["chain_len"], 2, "still the full chain");

        let tls = vault_edge(&FakeSource::new(vec![
            refused(TlsVerdict::NotFound),
            secret(leaf_only.clone()),
            secret(leaf_only),
        ]));
        tls.reload("boot").await.expect("a vault edge");
        assert_eq!(
            tls.reload("awaiting re-read").await,
            Ok(ReloadOutcome {
                changed: true,
                refused: None
            }),
            "nothing was served: the leaf is"
        );
        assert_eq!(
            tls.reload("VAULT_SECRET_CHANGED put").await,
            Ok(ReloadOutcome {
                changed: false,
                refused: None
            }),
            "the leaf already served is not refused"
        );
    }

    #[tokio::test]
    async fn the_door_closes_connections_over_its_cap() {
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let addr = loopback_door(&tls, DoorLimits::new(1, HTTP_HEADER_READ_TIMEOUT)).await;
        let mut first = connect_tls(addr).await;
        assert_eq!(get_healthz(&mut first).await, "ok");
        assert!(
            try_connect_tls(addr).await.is_err(),
            "a second connection is closed while the first holds the only permit"
        );
        drop(first);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut third) = try_connect_tls(addr).await {
                assert_eq!(get_healthz(&mut third).await, "ok");
                break;
            }
            assert!(Instant::now() < deadline, "the permit came back");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn a_connection_that_sends_nothing_is_closed_at_the_header_timeout() {
        let header_timeout = Duration::from_millis(300);
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let addr = loopback_door(&tls, DoorLimits::new(16, header_timeout)).await;

        // The handshake completes, then nothing: hyper-util's protocol sniffing has no timeout.
        let mut silent = connect_tls(addr).await;
        let closed = closed_within(&mut silent, Duration::from_secs(5)).await;
        assert!(closed >= header_timeout.mul_f32(0.9), "{closed:?}");

        // One request, then an idle keep-alive connection: hyper's timer closes it.
        let mut idle = connect_tls(addr).await;
        assert_eq!(get_healthz(&mut idle).await, "ok");
        let closed = closed_within(&mut idle, Duration::from_secs(5)).await;
        assert!(closed >= header_timeout.mul_f32(0.9), "{closed:?}");
    }

    #[test]
    fn refused_connections_are_logged_at_most_once_per_interval() {
        let mut refused = RefusedConnections::default();
        let start = Instant::now();
        let interval = Duration::from_secs(10);
        assert_eq!(
            refused.note(start, interval),
            Some(1),
            "the first is logged"
        );
        for second in 1..10 {
            assert_eq!(
                refused.note(start + Duration::from_secs(second), interval),
                None
            );
        }
        assert_eq!(
            refused.note(start + interval, interval),
            Some(10),
            "then once per interval, with the count since"
        );
        assert_eq!(MAX_TLS_CONNECTIONS, 4096);
        assert_eq!(ACCEPT_ERROR_BACKOFF, Duration::from_millis(100));
    }
}
