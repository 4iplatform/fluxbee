#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::fs::OpenOptions;
use std::io::{Read, SeekFrom, Write};
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, PoisonError, RwLock};
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body, Bytes, HttpBody};
use axum::extract::{ConnectInfo, Path as AxumPath, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use fluxbee_sdk::mesh::{
    install_crypto_provider, is_valid_hive_id, MeshTlsMaterial, IO_WEB_PORT, IO_WEB_SERVER_NAME,
    MESH_TLS_DIR,
};
use fluxbee_sdk::protocol::{
    is_system_kind, Destination, Message, Meta, Routing, VaultSecretChangedPayload, VaultSecretOp,
    MSG_EDGE_ACME_CHALLENGE, MSG_EDGE_ACME_CHALLENGE_RESPONSE, MSG_EDGE_CLOSE_URL,
    MSG_EDGE_CLOSE_URL_RESPONSE, MSG_EDGE_LIST_URLS, MSG_EDGE_LIST_URLS_RESPONSE,
    MSG_EDGE_OPEN_URL, MSG_EDGE_OPEN_URL_RESPONSE, MSG_EDGE_PUBLISH_BLOB,
    MSG_EDGE_PUBLISH_BLOB_RESPONSE, MSG_EDGE_TLS_RELOAD, MSG_EDGE_TLS_RELOAD_RESPONSE,
    MSG_EDGE_TLS_STATUS, MSG_EDGE_TLS_STATUS_RESPONSE, MSG_EDGE_UNPUBLISH_BLOB,
    MSG_EDGE_UNPUBLISH_BLOB_RESPONSE, MSG_EDGE_WEB_ROUTES, MSG_EDGE_WEB_ROUTES_RESPONSE,
    MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, is_node_config_get_message, is_node_config_set_message,
    parse_node_config_request, try_handle_default_node_status, NodeConfig,
    NodeConfigControlRequest, NodeSender, NodeUuidMode, OperationalRouteProfile, PendingMatcher,
    RouteMatch, RouteTarget, RouterDispatcher, RpcCommandReceiver, RpcError, RpcRequestLabels,
};
use json_router::tls_material::{self, ChainDiagnosis, MaterialError};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, ReadBuf};
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
/// The door's own commands (the three TLS ones and `EDGE_WEB_ROUTES`), taken off the system
/// channel so the sequential loop never delays them.
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
/// A-86: the connections one peer address may hold at once, of the door's 4096 (0: no cap).
const DEFAULT_MAX_CONNECTIONS_PER_IP: usize = 256;
/// A-86: the downloads one peer address may hold at once, of `public_max_inflight` (0: no cap).
const DEFAULT_PUBLIC_MAX_INFLIGHT_PER_IP: usize = 8;
/// io-web-v2 §11.2: the requests the proxy holds at once to io.web, each with its own connection
/// to the motherbee until its response ends (0: no cap). io.web serves a few operators.
const DEFAULT_WEB_MAX_INFLIGHT: usize = 128;

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
    /// The web hosts SY.admin declared (`EDGE_WEB_ROUTES`), kept so a restart serves them at once.
    web_routes_path: PathBuf,
    blob_public_root: PathBuf,
    /// Connections one peer address may hold at once (0: no cap).
    max_connections_per_ip: usize,
    public_max_inflight: usize,
    /// Downloads one peer address may hold at once (0: no cap).
    public_max_inflight_per_ip: usize,
    public_ready_timeout_ms: u64,
    /// Proxied requests in flight to io.web at once (0: no cap).
    web_max_inflight: usize,
    /// This hive's id, which names its mesh TLS material (`MESH_TLS_DIR/<hive_id>/`).
    hive_id: Option<String>,
    /// The motherbee's address as this hive's router dials it (`wan.uplinks[0].address`): its host
    /// is where io.web listens.
    motherbee_uplink: Option<String>,
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
    // The web hosts (io-web-v2 §11.1): from disk, so a restart proxies them at once, before
    // SY.admin's next push.
    let web_routes = Arc::new(WebRouteTable::load(config.web_routes_path.clone()));

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
    // The door's proxy to io.web, whose upstream the answer to EDGE_WEB_ROUTES reports. `None`
    // with no public door, like `edge_tls`.
    let mut web_proxy: Option<Arc<WebProxy>> = None;

    // Ingress-hive role: run the public HTTPS door. Fail-closed — absent on any
    // other role (`resolve_http_listen`).
    if let Some(listen) = config.http_listen.clone() {
        tracing::info!(
            listen = %listen,
            endpoints = registry.read().map(|r| r.len()).unwrap_or(0),
            publications = public_registry.read().map(|r| r.len()).unwrap_or(0),
            web_hosts = web_routes.snapshot().routes.len(),
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
            public_peer_slots: PeerSlots::new(config.public_max_inflight_per_ip),
            refused_downloads: std::sync::Mutex::new(RefusedConnections::default()),
            body_timeout: INLINE_BODY_TIMEOUT,
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
        // The web hosts were read before any certificate: now the edge can tell its main name,
        // which a web host never is (an awaiting edge tells it at each request, `host_route`).
        if let Some(served) = tls.served() {
            web_routes.forget_main_name(&served.leaf.names);
        }
        // The proxy to io.web (io-web-v2 §11.2), from this hive's mesh TLS material and the
        // motherbee's address, read once. Without them every web host answers 502: never
        // plaintext, never a verifier that skips io.web's name.
        let upstream = IoWebClient::for_hive(
            std::path::Path::new(MESH_TLS_DIR),
            config.hive_id.as_deref(),
            config.motherbee_uplink.as_deref(),
        );
        match &upstream {
            Ok(client) => tracing::info!(
                upstream = %client.authority,
                server_name = IO_WEB_SERVER_NAME,
                web_max_inflight = config.web_max_inflight,
                "sy-edge web proxy ready: web hosts go to io.web"
            ),
            Err(reason) => tracing::error!(
                reason = %reason,
                "sy-edge: the proxy to io.web has no upstream; every web host answers 502 until this is fixed and the edge restarts"
            ),
        }
        let proxy = Arc::new(WebProxy::new(upstream, config.web_max_inflight));
        web_proxy = Some(Arc::clone(&proxy));
        let door_web_routes = Arc::clone(&web_routes);
        let max_connections_per_ip = config.max_connections_per_ip;
        tokio::spawn(async move {
            if let Err(err) = run_frontend(
                listen,
                state,
                tls,
                door_web_routes,
                proxy,
                max_connections_per_ip,
            )
            .await
            {
                tracing::error!(error = %err, "sy-edge public frontend exited");
            }
        });
    }

    // The door's commands answer beside the sequential loop below (§4): EDGE_PUBLISH_BLOB can
    // hold that loop for a minute, and neither SY.admin's ACME order nor its push of the web hosts
    // may wait behind it.
    let tls_commands = dispatcher
        .take_command_receiver(RPC_CH_TLS)
        .await
        .map_err(|err| format!("sy-edge tls receiver: {err}"))?;
    tokio::spawn(run_tls_commands(
        tls_commands,
        sender.clone(),
        edge_tls.clone(),
        Arc::clone(&web_routes),
        web_proxy,
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
                reap_public_registry(&reaper_registry, &reaper_path, now);
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
        // The door's commands get their own channel, consumed beside the sequential system loop
        // (`run_tls_commands`). Commands, never responses: they go before any pending waiter.
        .pre_pending_rule(
            RouteMatch::one_of(
                SYSTEM_KIND,
                [
                    MSG_EDGE_TLS_STATUS,
                    MSG_EDGE_TLS_RELOAD,
                    MSG_EDGE_ACME_CHALLENGE,
                    MSG_EDGE_WEB_ROUTES,
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

/// One public artifact as the edge keeps it, in memory and in `publications.json`. Of the
/// capability key SY.admin sends (`EDGE_PUBLISH_BLOB`) it keeps only the SHA-256, and a GET looks
/// the hash of the key it is asked for up (A-82): neither the disk nor a log line of the DMZ holds
/// a key.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PublicArtifactRow {
    /// SHA-256 of the capability key, lowercase hex.
    key_sha256: String,
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

/// SHA-256 of `text`, lowercase hex.
fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// A row that carries the capability `key` itself (an `EDGE_PUBLISH_BLOB`, or `publications.json`
/// before 0.1.84) gets `key_sha256` in its place, the form the edge keeps (A-82). `Ok(true)` when
/// the row carried a key; `Ok(false)` when it had none.
fn hash_capability_key(row: &mut Value) -> Result<bool, String> {
    let fields = row
        .as_object_mut()
        .ok_or_else(|| "a public artifact row must be an object".to_string())?;
    let Some(key) = fields.remove("key") else {
        return Ok(false);
    };
    let key = key
        .as_str()
        .filter(|key| is_lower_hex_64(key))
        .ok_or_else(|| "key must be 64 lowercase hex characters".to_string())?;
    if fields.contains_key("key_sha256") {
        return Err("a public artifact row carries both key and key_sha256".to_string());
    }
    fields.insert("key_sha256".to_string(), json!(sha256_hex(key)));
    Ok(true)
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
    let outcome: Result<(PublicArtifactRow, String), String> = async {
        authorize_edge_service_command(msg, MSG_EDGE_PUBLISH_BLOB)?;
        // The answer still names the key's URL, to SY.admin, which holds the key; the edge keeps
        // only its hash (A-82).
        let url = format!(
            "/public/{}",
            msg.payload.get("key").and_then(Value::as_str).unwrap_or("")
        );
        let mut payload = msg.payload.clone();
        if !hash_capability_key(&mut payload)? {
            return Err("invalid EDGE_PUBLISH_BLOB payload: missing field `key`".to_string());
        }
        let row: PublicArtifactRow = serde_json::from_value(payload)
            .map_err(|err| format!("invalid EDGE_PUBLISH_BLOB payload: {err}"))?;
        validate_public_artifact_row(&row)?;
        wait_for_public_artifact_ready(public_root, &row, ready_timeout).await?;
        {
            let mut guard = registry
                .write()
                .map_err(|_| "public registry lock poisoned".to_string())?;
            if let Some(existing) = guard.get(&row.key_sha256) {
                if existing != &row {
                    return Err("public capability already exists with different facts".to_string());
                }
            }
            if guard.values().any(|existing| {
                existing.publication_id == row.publication_id
                    && existing.key_sha256 != row.key_sha256
            }) {
                return Err("publication_id already exists with a different capability".to_string());
            }
            let previous = guard.insert(row.key_sha256.clone(), row.clone());
            if let Err(err) = persist_public_registry(publications_path, &guard) {
                match previous {
                    Some(previous) => {
                        guard.insert(row.key_sha256.clone(), previous);
                    }
                    None => {
                        guard.remove(&row.key_sha256);
                    }
                }
                return Err(err);
            }
        }
        Ok((row, url))
    }
    .await;
    let payload = match outcome {
        Ok((row, url)) => {
            tracing::info!(
                publication_id = %row.publication_id,
                key_sha256 = %row.key_sha256,
                public_name = %row.public_name,
                "sy-edge published public artifact after local readiness verification"
            );
            json!({
                "status": "ok",
                "publication_id": row.publication_id,
                "url": url,
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
        let key_sha256 = guard
            .iter()
            .find(|(_, row)| row.publication_id == publication_id)
            .map(|(key_sha256, _)| key_sha256.clone());
        let removed_row = key_sha256
            .as_ref()
            .and_then(|key_sha256| guard.remove(key_sha256));
        let removed = removed_row.is_some();
        if removed {
            if let Err(err) = persist_public_registry(publications_path, &guard) {
                if let (Some(key_sha256), Some(row)) = (key_sha256, removed_row) {
                    guard.insert(key_sha256, row);
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
    if !is_lower_hex_64(&row.key_sha256) {
        return Err("key_sha256 must be 64 lowercase hex characters".to_string());
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

/// `publications.json` as read: each row stays raw until `hash_capability_key` has seen it.
#[derive(Debug, Deserialize)]
struct StoredPublicArtifacts {
    #[serde(default)]
    publications: Vec<Value>,
}

/// The public table as `publications.json` left it, keyed by `key_sha256`, without its invalid
/// or expired rows. A file written before 0.1.84 holds the capability keys themselves: they are
/// hashed here, the file is rewritten without them, and one line says how many rows carried one
/// (A-82). The next start finds none.
fn load_public_registry(path: &std::path::Path) -> HashMap<String, PublicArtifactRow> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(err) => {
            tracing::error!(path = %path.display(), error = %err, "sy-edge public registry read failed; starting empty");
            return HashMap::new();
        }
    };
    let parsed: StoredPublicArtifacts = match serde_json::from_slice(&raw) {
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
    let mut carried_keys = 0_usize;
    let mut invalid = 0_usize;
    let mut registry = HashMap::new();
    for mut stored in parsed.publications {
        match hash_capability_key(&mut stored) {
            Ok(true) => carried_keys += 1,
            Ok(false) => {}
            Err(_) => {
                invalid += 1;
                continue;
            }
        }
        let row = match serde_json::from_value::<PublicArtifactRow>(stored) {
            Ok(row) if validate_public_artifact_row(&row).is_ok() => row,
            _ => {
                invalid += 1;
                continue;
            }
        };
        // FIX-6: drop already-expired rows at load. They are un-servable (the serve path
        // rejects expires_at <= now) but were previously re-residented into the registry on
        // EVERY restart, growing it unbounded and pinning the referenced public/ bytes. This
        // bounds the edge's in-memory registry; the active reaper (part 2, below) also prunes
        // the on-disk ledger on a live edge. Reclaiming the public/ BYTES + retiring the
        // authoritative admin ledger is SY.admin's job — its expiry sweep drives the same
        // MSG_EDGE_UNPUBLISH_BLOB + MSG_BLOB_RELEASE path at TTL (run_publication_expiry_sweep).
        if row.expires_at <= now {
            tracing::info!(
                publication_id = %row.publication_id,
                key_sha256 = %row.key_sha256,
                "sy-edge: dropping expired public artifact row at load"
            );
            continue;
        }
        registry.insert(row.key_sha256.clone(), row);
    }
    if invalid > 0 {
        tracing::warn!(path = %path.display(), invalid, "sy-edge: dropped invalid public artifact rows at load");
    }
    if carried_keys > 0 {
        let rewritten = persist_public_registry(path, &registry);
        tracing::info!(
            path = %path.display(),
            converted = carried_keys,
            rewritten = rewritten.is_ok(),
            "sy-edge: the public registry held capability keys; it keeps only their SHA-256 now (A-82)"
        );
        if let Err(err) = rewritten {
            tracing::error!(path = %path.display(), error = %err, "sy-edge could not rewrite the public registry without its keys; the next start converts it again");
        }
    }
    registry
}

/// Drop the rows expired at `now` and write the table; how many went. Like a publication and an
/// unpublication, it writes `publications.json` while it holds the table's write lock: one writer
/// of the file at a time, and always the latest table.
fn reap_public_registry(
    registry: &RwLock<HashMap<String, PublicArtifactRow>>,
    path: &std::path::Path,
    now: u64,
) -> usize {
    let mut guard = registry.write().unwrap_or_else(PoisonError::into_inner);
    let before = guard.len();
    guard.retain(|_, row| row.expires_at > now);
    let removed = before - guard.len();
    if removed > 0 {
        tracing::info!(removed, "sy-edge reaper: dropped expired public artifacts");
        if let Err(err) = persist_public_registry(path, &guard) {
            tracing::warn!(error = %err, "sy-edge reaper: persist pruned ledger failed");
        }
    }
    removed
}

/// Every write of `publications.json` goes through here, under the public table's write lock (the
/// conversion at load runs before the table is shared).
fn persist_public_registry(
    path: &std::path::Path,
    registry: &HashMap<String, PublicArtifactRow>,
) -> Result<(), String> {
    let mut publications: Vec<_> = registry.values().cloned().collect();
    publications.sort_by(|left, right| left.key_sha256.cmp(&right.key_sha256));
    let data = serde_json::to_vec_pretty(&PublicArtifactsFile { publications })
        .map_err(|err| format!("serialize public registry: {err}"))?;
    write_state_file(path, &data)
}

/// Write one of the edge's state files whole: a synced temporary file with mode 0640, then a
/// rename, so a crash leaves the old file or the new one, never half of one.
fn write_state_file(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o640)
        .open(&temp)
        .map_err(|err| format!("open {}: {err}", temp.display()))?;
    file.write_all(data)
        .and_then(|_| file.sync_all())
        .map_err(|err| format!("write {}: {err}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|err| format!("install {}: {err}", path.display()))?;
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

/// The fields of `hive.yaml` sy_edge needs: `role` and the optional `edge:` section, and for the
/// proxy to io.web the hive's id (its mesh TLS material) and `wan:` (the motherbee's address).
/// Other sections (blob/…) are ignored — NO `deny_unknown_fields`, or the whole file would fail
/// to parse.
#[derive(Debug, Clone, Default, Deserialize)]
struct HiveEdgeFile {
    #[serde(default)]
    hive_id: Option<String>,
    #[serde(default)]
    role: Option<String>,
    /// The router's section, read as a plain value (`first_uplink_address`): a shape the edge does
    /// not expect leaves the proxy without its upstream, never the door without its listener.
    #[serde(default)]
    wan: Option<serde_yaml::Value>,
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
    web_routes_path: Option<String>,
    #[serde(default)]
    blob_public_root: Option<String>,
    #[serde(default)]
    max_connections_per_ip: Option<usize>,
    #[serde(default)]
    public_max_inflight: Option<usize>,
    #[serde(default)]
    public_max_inflight_per_ip: Option<usize>,
    #[serde(default)]
    public_ready_timeout_ms: Option<u64>,
    #[serde(default)]
    web_max_inflight: Option<usize>,
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

/// Read `<config_dir>/hive.yaml` (`HiveEdgeFile`). Best-effort: a missing or unparsable file
/// yields no role and no `edge:` section, so the node still boots but never binds a public
/// listener (fail-closed).
fn load_hive_edge(config_dir: &std::path::Path) -> HiveEdgeFile {
    let path = config_dir.join("hive.yaml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return HiveEdgeFile::default(),
    };
    match serde_yaml::from_str::<HiveEdgeFile>(&raw) {
        Ok(parsed) => parsed,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "sy-edge could not parse hive.yaml; no public listener");
            HiveEdgeFile::default()
        }
    }
}

/// The address this hive's router dials, `wan.uplinks[0].address`: on an ingress, the motherbee's.
fn first_uplink_address(wan: Option<&serde_yaml::Value>) -> Option<String> {
    wan?.get("uplinks")?
        .get(0)?
        .get("address")?
        .as_str()
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(str::to_string)
}

/// The host of an uplink address (`10.10.10.10:9000`, `[fd00::10]:9000`, `motherbee.lan:9000`, or
/// one with no port); an IPv6 literal keeps its brackets. `None` when it names no host.
fn uplink_host(address: &str) -> Option<String> {
    let authority: axum::http::uri::Authority = address.trim().parse().ok()?;
    if authority.as_str().contains('@') {
        return None;
    }
    let host = authority.host();
    (!host.is_empty()).then(|| host.to_string())
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
        let hive = load_hive_edge(&config_dir);
        let edge = hive.edge.clone();
        let http_listen = resolve_http_listen(
            env("SY_EDGE_HTTP_LISTEN"),
            hive.role.as_deref(),
            edge.as_ref().and_then(|e| e.listen.clone()),
        );
        let endpoints_path = env("SY_EDGE_ENDPOINTS")
            .or_else(|| edge.as_ref().and_then(|e| e.endpoints_path.clone()))
            .unwrap_or_else(|| "/etc/fluxbee/edge.endpoints.json".to_string());
        let publications_path = env("SY_EDGE_PUBLICATIONS")
            .or_else(|| edge.as_ref().and_then(|e| e.publications_path.clone()))
            .unwrap_or_else(|| "/var/lib/fluxbee/state/sy-edge/publications.json".to_string());
        let web_routes_path = env("SY_EDGE_WEB_ROUTES")
            .or_else(|| edge.as_ref().and_then(|e| e.web_routes_path.clone()))
            .unwrap_or_else(|| "/var/lib/fluxbee/state/sy-edge/web_routes.json".to_string());
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
            web_routes_path: PathBuf::from(web_routes_path),
            blob_public_root: PathBuf::from(blob_public_root),
            // 0 turns a per-address cap off, so it is taken as given.
            max_connections_per_ip: env("SY_EDGE_MAX_CONNECTIONS_PER_IP")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.max_connections_per_ip))
                .unwrap_or(DEFAULT_MAX_CONNECTIONS_PER_IP),
            public_max_inflight: env("SY_EDGE_PUBLIC_MAX_INFLIGHT")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.public_max_inflight))
                .filter(|&value| value > 0)
                .unwrap_or(DEFAULT_PUBLIC_MAX_INFLIGHT),
            public_max_inflight_per_ip: env("SY_EDGE_PUBLIC_MAX_INFLIGHT_PER_IP")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.public_max_inflight_per_ip))
                .unwrap_or(DEFAULT_PUBLIC_MAX_INFLIGHT_PER_IP),
            public_ready_timeout_ms: env("SY_EDGE_PUBLIC_READY_TIMEOUT_MS")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.public_ready_timeout_ms))
                .filter(|&value| value > 0)
                .unwrap_or(DEFAULT_PUBLIC_READY_TIMEOUT_MS),
            // 0 turns the cap off, so it is taken as given.
            web_max_inflight: env("SY_EDGE_WEB_MAX_INFLIGHT")
                .and_then(|raw| raw.parse().ok())
                .or_else(|| edge.as_ref().and_then(|e| e.web_max_inflight))
                .unwrap_or(DEFAULT_WEB_MAX_INFLIGHT),
            hive_id: hive
                .hive_id
                .as_deref()
                .map(str::trim)
                .filter(|hive_id| !hive_id.is_empty())
                .map(str::to_string),
            motherbee_uplink: first_uplink_address(hive.wan.as_ref()),
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
    /// A-86: the downloads each peer address holds, out of `public_inflight`.
    public_peer_slots: Arc<PeerSlots>,
    /// Downloads refused at a peer's cap, logged at most once per interval.
    refused_downloads: std::sync::Mutex<RefusedConnections>,
    /// The longest an `/e/` body may take to arrive whole (`INLINE_BODY_TIMEOUT`).
    body_timeout: Duration,
}

async fn run_frontend(
    listen: String,
    state: Arc<FrontendState>,
    tls: Arc<EdgeTls>,
    web_routes: Arc<WebRouteTable>,
    proxy: Arc<WebProxy>,
    max_connections_per_ip: usize,
) -> Result<(), SyEdgeError> {
    let door = Door {
        main: main_routes(state),
        web: web_routes,
        proxy,
        tls: Arc::clone(&tls),
    };
    let listener = TcpListener::bind(&listen).await?;
    let addr = listener.local_addr()?;
    if tls.mode == TlsMode::Plaintext {
        tracing::info!(addr = %addr, "sy-edge public HTTP frontend ready (plaintext)");
    } else {
        tracing::info!(
            addr = %addr,
            mode = tls.mode.as_str(),
            serving = tls.is_serving(),
            "sy-edge public HTTPS frontend ready (TLS)"
        );
    }
    serve_door(
        listener,
        door,
        tls,
        DoorLimits::new(
            MAX_CONNECTIONS,
            max_connections_per_ip,
            HTTP_HEADER_READ_TIMEOUT,
            DOOR_PACE,
        ),
    )
    .await
}

/// The door's own routes, for the hosts that are the door's own (`host_route`).
fn main_routes(state: Arc<FrontendState>) -> Router {
    Router::new()
        .route("/e/:ich", any(invoke_root))
        .route("/e/:ich/*extra", any(invoke_extra))
        .route("/public/:key", any(serve_public_artifact))
        .route("/healthz", any(|| async { "ok" }))
        .with_state(state)
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
/// The most connections the public door holds at once, TLS handshakes included; one more is
/// closed at accept.
const MAX_CONNECTIONS: usize = 4096;
/// A request head must arrive within this, timed from the connection's start (after its TLS
/// handshake) or from the previous answer: one byte, or an h2 preface, never holds a connection
/// longer (A-87). hyper applies it only with a timer (`serve_http`).
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// A-86: how the door's connections must keep up while the edge waits on their peer (`Pace`):
/// nothing moving for 30 s, or a pace under 1 KiB/s, ends the connection.
const DOOR_PACE: PaceLimits = PaceLimits {
    idle: Duration::from_secs(30),
    min_rate: 1024,
};
/// An `/e/` body (64 KiB at most) must arrive whole within this. The pace alone would let a
/// chunked body that stays just above it hold an `/e/` slot for minutes.
const INLINE_BODY_TIMEOUT: Duration = Duration::from_secs(30);
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
    if !is_dns_host_name(&name) {
        return Err(format!(
            "name must be a DNS host name (no wildcard, no IP literal), got '{raw}'"
        ));
    }
    Ok(name)
}

/// A DNS host name as the edge takes one: lowercase labels of letters, digits and inner hyphens.
/// Never a wildcard, an IP literal, a port or a trailing dot.
fn is_dns_host_name(name: &str) -> bool {
    let valid_label = |label: &str| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    };
    !name.is_empty()
        && name.len() <= 253
        && name.parse::<std::net::IpAddr>().is_err()
        && name.split('.').all(valid_label)
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

/// The public door's limits against slow or abundant clients.
#[derive(Clone)]
struct DoorLimits {
    /// One permit per connection, its handshake included.
    connections: Arc<Semaphore>,
    max_connections: usize,
    /// The connections each peer address holds, taken at accept like the permit (A-86).
    peer_connections: Arc<PeerSlots>,
    /// Every HTTP/1 request head must arrive within this, the first one included (A-87).
    header_read_timeout: Duration,
    /// What each connection must keep up while the edge waits on its peer (A-86).
    pace: PaceLimits,
}

impl DoorLimits {
    fn new(
        max_connections: usize,
        max_connections_per_ip: usize,
        header_read_timeout: Duration,
        pace: PaceLimits,
    ) -> Self {
        Self {
            connections: Arc::new(Semaphore::new(max_connections)),
            max_connections,
            peer_connections: PeerSlots::new(max_connections_per_ip),
            header_read_timeout,
            pace,
        }
    }
}

/// How many connections, or downloads, one peer address may hold at once (A-86). An IPv6 peer
/// counts by its /64, the block one subscriber gets; an IPv4-mapped address as its IPv4.
struct PeerSlots {
    /// 0: no cap.
    max: usize,
    held: std::sync::Mutex<HashMap<std::net::IpAddr, usize>>,
}

/// One slot of a `PeerSlots`, given back when dropped.
struct PeerSlot {
    slots: Arc<PeerSlots>,
    /// `None` when there is no cap to count against.
    key: Option<std::net::IpAddr>,
}

impl PeerSlots {
    fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            held: std::sync::Mutex::new(HashMap::new()),
        })
    }

    /// A slot for `peer`, or `None` when it holds `max` already.
    fn try_take(self: &Arc<Self>, peer: std::net::IpAddr) -> Option<PeerSlot> {
        if self.max == 0 {
            return Some(PeerSlot {
                slots: Arc::clone(self),
                key: None,
            });
        }
        let key = match peer.to_canonical() {
            std::net::IpAddr::V6(v6) => {
                let [a, b, c, d, ..] = v6.segments();
                std::net::IpAddr::V6(std::net::Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
            v4 => v4,
        };
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let count = held.entry(key).or_insert(0);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(PeerSlot {
            slots: Arc::clone(self),
            key: Some(key),
        })
    }

    #[cfg(test)]
    fn held_by(&self, peer: std::net::IpAddr) -> usize {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.get(&peer).copied().unwrap_or(0)
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let Some(key) = self.key else {
            return;
        };
        let mut held = self
            .slots
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = held.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                held.remove(&key);
            }
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

/// The door's accept loop: one task per connection, at most `limits.max_connections` at once and
/// at most `max_connections_per_ip` from one peer address, both counted before the handshake. A
/// connection over either cap is closed at once; a failed accept waits `ACCEPT_ERROR_BACKOFF`.
async fn serve_door(
    listener: TcpListener,
    door: Door,
    tls: Arc<EdgeTls>,
    limits: DoorLimits,
) -> Result<(), SyEdgeError> {
    let mut refused = RefusedConnections::default();
    let mut refused_peers = RefusedConnections::default();
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(error = %err, "sy-edge accept failed");
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
                continue;
            }
        };
        let Some(peer_slot) = limits.peer_connections.try_take(peer.ip()) else {
            drop(tcp);
            if let Some(count) =
                refused_peers.note(Instant::now(), REFUSED_CONNECTIONS_LOG_INTERVAL)
            {
                tracing::warn!(
                    peer = %peer.ip(),
                    refused = count,
                    max_connections_per_ip = limits.peer_connections.max,
                    "sy-edge: a peer address at its connection cap; closing its new connections"
                );
            }
            continue;
        };
        let Ok(permit) = Arc::clone(&limits.connections).try_acquire_owned() else {
            drop(tcp);
            if let Some(count) = refused.note(Instant::now(), REFUSED_CONNECTIONS_LOG_INTERVAL) {
                tracing::warn!(
                    peer = %peer.ip(),
                    refused = count,
                    max_connections = limits.max_connections,
                    "sy-edge: at its connection cap; closing new connections"
                );
            }
            continue;
        };
        tokio::spawn(serve_door_connection(
            tcp,
            peer,
            door.clone(),
            Arc::clone(&tls),
            limits.clone(),
            (permit, peer_slot),
        ));
    }
}

/// One connection, holding its permit and its peer's slot to the end: the handshake
/// (`accept_tls`) unless the door is plaintext, then HTTP (`serve_http`).
async fn serve_door_connection(
    tcp: tokio::net::TcpStream,
    peer: SocketAddr,
    door: Door,
    tls: Arc<EdgeTls>,
    limits: DoorLimits,
    _held: (OwnedSemaphorePermit, PeerSlot),
) {
    track_delivery(&tcp);
    if tls.mode == TlsMode::Plaintext {
        serve_http(tcp, peer, door, &limits).await;
        return;
    }
    if let Some(stream) = accept_tls(tcp, &tls).await {
        serve_http(stream, peer, door, &limits).await;
    }
}

/// A-86: an accepted socket reports itself writable only once less than 16 KiB of what was written
/// is still unsent (`TCP_NOTSENT_LOWAT`), so the write side of `Pace` follows what reaches the
/// peer. Otherwise Linux wakes a writer only when its send queue drains to two thirds of a buffer
/// that grows with the congestion window, and one wait on a slow mobile link could outlast the
/// pace's whole credit. The ingress runs Linux; elsewhere nothing changes.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn track_delivery(tcp: &tokio::net::TcpStream) {
    const NOTSENT_LOWAT: u32 = 16 * 1024;
    if let Err(err) = socket2::SockRef::from(tcp).set_tcp_notsent_lowat(NOTSENT_LOWAT) {
        tracing::debug!(error = %err, "sy-edge could not set TCP_NOTSENT_LOWAT");
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn track_delivery(_tcp: &tokio::net::TcpStream) {}

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

/// The door's commands, from `SY.admin@motherbee` only: the three TLS ones (§4) and
/// `EDGE_WEB_ROUTES`. They arrive on their own channel (`RPC_CH_TLS`) and each runs in its own
/// task over the shared state, so they never wait behind `EDGE_PUBLISH_BLOB` (up to 60 s) or a
/// secret resolution in the sequential system loop.
async fn run_tls_commands(
    mut commands: RpcCommandReceiver,
    sender: NodeSender,
    tls: Option<Arc<EdgeTls>>,
    web: Arc<WebRouteTable>,
    proxy: Option<Arc<WebProxy>>,
    ttl: u8,
) {
    while let Some(msg) = commands.recv().await {
        let sender = sender.clone();
        let tls = tls.clone();
        let web = Arc::clone(&web);
        let proxy = proxy.clone();
        tokio::spawn(async move {
            if let Some((response, payload)) =
                tls_command_reply(tls.as_deref(), &web, proxy.as_deref(), &msg).await
            {
                send_edge_reply(&sender, &msg, response, payload, ttl).await;
            }
        });
    }
}

async fn tls_command_reply(
    tls: Option<&EdgeTls>,
    web: &WebRouteTable,
    proxy: Option<&WebProxy>,
    msg: &Message,
) -> Option<(&'static str, Value)> {
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
        Some(MSG_EDGE_WEB_ROUTES) => Some((
            MSG_EDGE_WEB_ROUTES_RESPONSE,
            web_routes_answer(tls, web, proxy, msg),
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

// ---------------------------------------------------------------------------
// Web hosts (docs/io-web-v2.md §11.1): the hosts the door serves only through io.web, never
// through its own routes. SY.admin pushes the full set (`EDGE_WEB_ROUTES`) every minute; the edge
// keeps it on disk, so a restart serves it at once.
// ---------------------------------------------------------------------------

/// The only upstream a web host may name: io.web on the motherbee (io-web-v2 §3). The edge takes
/// no other, so a route can never point the door at a host or a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum WebUpstream {
    #[serde(rename = "io.web")]
    IoWeb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebRoute {
    host: String,
    upstream: WebUpstream,
}

/// The full set of web hosts: the `EDGE_WEB_ROUTES` payload, the set its answer names, and the
/// file the edge keeps.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebRoutes {
    routes: Vec<WebRoute>,
}

impl WebRoutes {
    fn hosts(&self) -> Vec<&str> {
        self.routes
            .iter()
            .map(|route| route.host.as_str())
            .collect()
    }
}

/// What one `EDGE_WEB_ROUTES` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WebRoutesApplied {
    /// The set differs from the one served before.
    changed: bool,
    /// The file holds the set. When a write fails, the set is served anyway and the next push
    /// writes it again.
    persisted: bool,
}

/// The web hosts the door serves: read by every request, replaced whole by `EDGE_WEB_ROUTES`,
/// kept at `web_routes_path`.
struct WebRouteTable {
    path: PathBuf,
    routes: RwLock<WebRoutes>,
    /// What the file holds; `None` when unknown (unreadable, invalid, or a write that failed), so
    /// the next push writes it. Held for a whole replace: one at a time.
    on_disk: std::sync::Mutex<Option<WebRoutes>>,
}

impl WebRouteTable {
    /// The set the file holds. No file is no web host. An unreadable or corrupt file is an ERROR
    /// and starts empty, until SY.admin pushes the set again, within a minute; meanwhile a web
    /// host is an unknown host to the door (`host_route`), which answers it 404. The main name is
    /// checked once a certificate is served (`forget_main_name`).
    fn load(path: PathBuf) -> Self {
        let read = match std::fs::read(&path) {
            Ok(raw) => serde_json::from_slice::<WebRoutes>(&raw)
                .map_err(|err| err.to_string())
                .and_then(|routes| validate_web_routes(routes, &[])),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(WebRoutes::default()),
            Err(err) => Err(err.to_string()),
        };
        let (routes, on_disk) = match read {
            Ok(routes) => (routes.clone(), Some(routes)),
            Err(err) => {
                tracing::error!(
                    path = %path.display(),
                    error = %err,
                    "sy-edge: the web routes file is CORRUPT or unreadable; serving no web host until SY.admin pushes the set again (within a minute)"
                );
                (WebRoutes::default(), None)
            }
        };
        if !routes.routes.is_empty() {
            tracing::info!(path = %path.display(), hosts = ?routes.hosts(), "sy-edge web hosts loaded");
        }
        Self {
            path,
            routes: RwLock::new(routes),
            on_disk: std::sync::Mutex::new(on_disk),
        }
    }

    fn snapshot(&self) -> WebRoutes {
        self.routes
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The file was read before any certificate was served: with one served, drop a web host
    /// that is the edge's main name (`validate_web_routes` refuses it in a push). The file is
    /// written again at the next push.
    fn forget_main_name(&self, certified: &[String]) {
        let Some(main) = main_certified_name(certified) else {
            return;
        };
        let mut on_disk = self.on_disk.lock().unwrap_or_else(PoisonError::into_inner);
        let mut live = self.routes.write().unwrap_or_else(PoisonError::into_inner);
        if live.routes.iter().any(|route| route.host == main) {
            live.routes.retain(|route| route.host != main);
            *on_disk = None;
            tracing::error!(
                path = %self.path.display(),
                main = %main,
                "sy-edge: the web routes file names the edge's main name; that route is dropped, the door's own routes stay on it"
            );
        }
    }

    /// Whether `host`, in `canonical_host` form, is a web host.
    fn serves(&self, host: &str) -> bool {
        self.routes
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .routes
            .iter()
            .any(|route| route.host == host)
    }

    /// Serve `routes` (already validated) from now on, and write them unless the file holds them.
    fn replace(&self, routes: WebRoutes) -> WebRoutesApplied {
        let mut on_disk = self.on_disk.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = {
            let mut live = self.routes.write().unwrap_or_else(PoisonError::into_inner);
            let changed = *live != routes;
            *live = routes.clone();
            changed
        };
        if on_disk.as_ref() == Some(&routes) {
            return WebRoutesApplied {
                changed,
                persisted: true,
            };
        }
        let written = serde_json::to_vec_pretty(&routes)
            .map_err(|err| format!("serialize web routes: {err}"))
            .and_then(|data| write_state_file(&self.path, &data));
        match written {
            Ok(()) => {
                *on_disk = Some(routes);
                WebRoutesApplied {
                    changed,
                    persisted: true,
                }
            }
            Err(err) => {
                tracing::error!(path = %self.path.display(), error = %err, "sy-edge could not keep the web hosts on disk; serving them anyway, the next push writes them again");
                *on_disk = None;
                WebRoutesApplied {
                    changed,
                    persisted: false,
                }
            }
        }
    }
}

/// A requested set of web hosts in canonical form (sorted, each host in `canonical_host` form),
/// or why it is refused: a host that is not a DNS name (a port other than 443, a wildcard, an IP
/// literal), one named twice, or the edge's main name. The upstream is io.web, the only one
/// `WebUpstream` admits.
///
/// `certified`: the DNS names of the certificate the door serves (none while it serves none),
/// from which `main_certified_name` tells the main name when it can. A web host there would leave
/// `/e/` and `/public/` without it.
fn validate_web_routes(requested: WebRoutes, certified: &[String]) -> Result<WebRoutes, String> {
    let mut routes: Vec<WebRoute> = Vec::with_capacity(requested.routes.len());
    for route in requested.routes {
        let host = canonical_host(&route.host)
            .ok()
            .filter(|host| host.port.is_none() && is_dns_host_name(&host.name))
            .map(|host| host.name)
            .ok_or_else(|| {
                format!(
                    "web host must be a DNS name (no port but 443, no wildcard, no IP literal), got '{}'",
                    route.host
                )
            })?;
        if routes.iter().any(|known| known.host == host) {
            return Err(format!("web host '{host}' is declared twice"));
        }
        routes.push(WebRoute {
            host,
            upstream: route.upstream,
        });
    }
    routes.sort_by(|left, right| left.host.cmp(&right.host));
    if let Some(main) = main_certified_name(certified) {
        if routes.iter().any(|route| route.host == main) {
            return Err(format!(
                "web host '{main}' is the edge's main name; its own routes (/e/, /public/) live there"
            ));
        }
    }
    Ok(WebRoutes { routes })
}

/// The edge's main name, when its certificate tells it: its only DNS name, or the one every other
/// name lies under (SY.admin derives each web host from it: `archi.` + the main host). `None`
/// when no certificate is served (awaiting, plaintext) or the names are unrelated.
fn main_certified_name(names: &[String]) -> Option<&str> {
    names
        .iter()
        .find(|candidate| {
            !candidate.starts_with("*.")
                && names.iter().all(|name| {
                    name == *candidate
                        || name
                            .strip_suffix(candidate.as_str())
                            .is_some_and(|label| label.ends_with('.'))
                })
        })
        .map(String::as_str)
}

/// `EDGE_WEB_ROUTES`: replace the web hosts with the full set it carries. The answer names the set
/// served from now on, `changed` and `persisted`, and the proxy's `upstream`
/// (`WebProxy::upstream_status`), so SY.admin sees a door that has its routes but no way to io.web.
fn web_routes_answer(
    tls: Option<&EdgeTls>,
    web: &WebRouteTable,
    proxy: Option<&WebProxy>,
    msg: &Message,
) -> Value {
    let outcome: Result<WebRoutes, String> = (|| {
        let tls = tls_command_edge(tls, msg, MSG_EDGE_WEB_ROUTES)?;
        let requested: WebRoutes = serde_json::from_value(msg.payload.clone())
            .map_err(|err| format!("invalid EDGE_WEB_ROUTES payload: {err}"))?;
        let certified = tls
            .served()
            .map(|served| served.leaf.names.clone())
            .unwrap_or_default();
        validate_web_routes(requested, &certified)
    })();
    match outcome {
        Ok(routes) => {
            let applied = web.replace(routes.clone());
            if applied.changed {
                tracing::info!(hosts = ?routes.hosts(), persisted = applied.persisted, trace_id = %msg.routing.trace_id, "sy-edge web hosts replaced (EDGE_WEB_ROUTES)");
            }
            json!({
                "status": "ok",
                "routes": routes.routes,
                "changed": applied.changed,
                "persisted": applied.persisted,
                "upstream": proxy.map(WebProxy::upstream_status),
            })
        }
        Err(err) => tls_command_error(MSG_EDGE_WEB_ROUTES, msg, err),
    }
}

// ---------------------------------------------------------------------------
// HTTP on the door (docs/io-web-v2.md §11.1): hyper's HTTP/1 server, whose head timer starts
// before the first byte (A-87); a stream that keeps a minimum pace (A-86); the peer's address for
// the handlers and the proxy; and every request dispatched by its host before any path route.
// ---------------------------------------------------------------------------

/// One connection's HTTP: HTTP/1 only, with no protocol sniffing, no HTTP/2 and no upgrade. Its
/// head timer starts before the first byte, so a client that sends one byte, or an h2 preface,
/// is closed at `header_read_timeout` at the latest (A-87); the same timer closes an idle
/// keep-alive connection. The stream is paced (`PacedStream`, A-86).
async fn serve_http<IO>(io: IO, peer: SocketAddr, door: Door, limits: &DoorLimits)
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let demand = BodyDemand::default();
    let stream = PacedStream::new(io, limits.pace, demand.clone());
    let service = hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
        let req = req.map(|body| Body::new(DemandBody::new(body, demand.clone())));
        door.clone().handle(req, peer)
    });
    let mut http = hyper::server::conn::http1::Builder::new();
    // 64 KiB of read and write buffer per connection (hyper's default is about 400 KiB): a head
    // over it is refused, and a peer that stops reading holds that much at most.
    http.timer(hyper_util::rt::TokioTimer::new())
        .header_read_timeout(limits.header_read_timeout)
        .max_buf_size(64 * 1024);
    if let Err(err) = http
        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
        .await
    {
        tracing::debug!(peer = %peer, error = %err, "sy-edge connection error");
    }
}

/// What the door serves: its own routes (`/e/`, `/public/`, `/healthz`), and the web hosts through
/// the proxy to io.web.
#[derive(Clone)]
struct Door {
    main: Router,
    web: Arc<WebRouteTable>,
    proxy: Arc<WebProxy>,
    /// The certificate served tells the door its main name (`host_route`), and its mode the
    /// scheme the proxy reports to io.web.
    tls: Arc<EdgeTls>,
}

impl Door {
    /// One request, by its host before any path (io-web-v2 §11.1, host-posture §4.4): only the
    /// door's own hosts reach its routes (`host_route`); a web host goes to io.web for every path
    /// (`WebProxy::forward`, §11.2), and any other host answers 404. Every answer carries the
    /// security headers (`with_security_headers`). The handlers find the peer's address in
    /// `ConnectInfo<SocketAddr>`.
    async fn handle(self, mut req: Request, peer: SocketAddr) -> Result<Response, Infallible> {
        req.extensions_mut().insert(ConnectInfo(peer));
        let host = match request_host(req.uri(), req.headers()) {
            Ok(host) => host,
            Err(reason) => {
                tracing::debug!(peer = %peer, reason, "sy-edge refused a request for its host");
                return Ok(http_error(
                    StatusCode::BAD_REQUEST,
                    "BAD_HOST",
                    "invalid host",
                ));
            }
        };
        let route = {
            let served = self.tls.served();
            let certified = served.as_ref().map(|served| served.leaf.names.as_slice());
            host_route(host.as_ref(), &self.web, certified)
        };
        let response = match (route, host) {
            (HostRoute::Door, _) => {
                hyper::service::Service::call(
                    &hyper_util::service::TowerToHyperService::new(self.main),
                    req,
                )
                .await?
            }
            // `host_route` names a web host only for a request with a host.
            (HostRoute::Web, Some(host)) => {
                let proto = if self.tls.mode == TlsMode::Plaintext {
                    "http"
                } else {
                    "https"
                };
                self.proxy.forward(req, &host.name, peer, proto).await
            }
            (HostRoute::Web, None) | (HostRoute::Unknown, _) => {
                http_error(StatusCode::NOT_FOUND, "NOT_FOUND", "not found")
            }
        };
        Ok(with_security_headers(response))
    }
}

/// Where one request goes, by its host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HostRoute {
    /// The door's own routes.
    Door,
    /// A web host: the proxy to io.web.
    Web,
    /// A host the edge does not serve: 404 (host-posture §4.4).
    Unknown,
}

/// The door's own routes answer requests with no host, for an IP literal, and for the edge's main
/// name on any port, which is never a web host. A web host is one on its own port only (443, or
/// none named): on another port it is unknown (io-web-v2 §11.1), and never reaches the door's
/// routes either. With a certificate served, any other host is unknown; with none (plaintext,
/// awaiting) the edge cannot tell its main name, and every other host keeps the door's routes.
/// `certified`: the DNS names of the certificate served. When they do not tell a main name
/// (`main_certified_name`), each of them that is not a web host is the door's.
fn host_route(
    host: Option<&RequestHost>,
    web: &WebRouteTable,
    certified: Option<&[String]>,
) -> HostRoute {
    let Some(host) = host else {
        return HostRoute::Door;
    };
    if host.is_ip_literal() {
        return HostRoute::Door;
    }
    let main = certified.and_then(main_certified_name);
    if main == Some(host.name.as_str()) {
        return HostRoute::Door;
    }
    if web.serves(&host.name) {
        return if host.port.is_none() {
            HostRoute::Web
        } else {
            HostRoute::Unknown
        };
    }
    match certified {
        None => HostRoute::Door,
        Some(_) if main.is_some() => HostRoute::Unknown,
        Some(names) if names.contains(&host.name) => HostRoute::Door,
        Some(_) => HostRoute::Unknown,
    }
}

/// Add each of `SECURITY_HEADERS` that a response lacks, so a handler's own header stays. A
/// published artifact keeps the content policy it chose (`OwnContentPolicy`).
fn with_security_headers(mut response: Response) -> Response {
    let own_policy = response.extensions().get::<OwnContentPolicy>().is_some();
    let headers = response.headers_mut();
    for (name, value) in SECURITY_HEADERS {
        if own_policy && name == header::CONTENT_SECURITY_POLICY.as_str() {
            continue;
        }
        headers
            .entry(header::HeaderName::from_static(name))
            .or_insert_with(|| HeaderValue::from_static(value));
    }
    response
}

/// A request's host in the door's form (`canonical_host`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestHost {
    /// Lowercase, one trailing dot dropped; an IPv6 literal keeps its brackets.
    name: String,
    /// `None` for 443 or no port.
    port: Option<u16>,
}

impl RequestHost {
    fn is_ip_literal(&self) -> bool {
        self.name
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok()
    }
}

/// The host one request is for: the authority of an absolute-form target when there is one,
/// otherwise its Host header, both in `canonical_host` form. `Ok(None)`: no host at all (an
/// HTTP/1.0 request, or an empty Host). `Err`: two Host headers, a Host the target's authority
/// contradicts, or one that does not parse; the door answers 400.
fn request_host(uri: &Uri, headers: &HeaderMap) -> Result<Option<RequestHost>, &'static str> {
    let mut values = headers.get_all(header::HOST).iter();
    let value = values.next();
    if values.next().is_some() {
        return Err("two Host headers");
    }
    let from_header = match value {
        None => None,
        Some(value) => {
            let text = value
                .to_str()
                .map_err(|_| "a Host that is not text")?
                .trim();
            if text.is_empty() {
                None
            } else {
                Some(canonical_host(text)?)
            }
        }
    };
    let Some(authority) = uri.authority() else {
        return Ok(from_header);
    };
    let target = canonical_host(authority.as_str())?;
    if from_header.as_ref().is_some_and(|host| *host != target) {
        return Err("a Host that contradicts the request target");
    }
    Ok(Some(target))
}

/// One authority in the door's form: the name lowercase with one trailing dot dropped, and the
/// port unless it is 443 (or empty, RFC 3986 §6.2.3).
fn canonical_host(raw: &str) -> Result<RequestHost, &'static str> {
    let authority: axum::http::uri::Authority =
        raw.parse().map_err(|_| "a host that does not parse")?;
    let text = authority.as_str();
    if text.contains('@') {
        return Err("a host with user information");
    }
    let name = authority.host().to_ascii_lowercase();
    let name = name.strip_suffix('.').unwrap_or(&name).to_string();
    if name.is_empty() {
        return Err("an empty host");
    }
    let port = match text[authority.host().len()..].strip_prefix(':') {
        None | Some("") => None,
        Some(port) => match port.parse::<u16>() {
            Ok(443) => None,
            Ok(port) => Some(port),
            Err(_) => return Err("a port that is not a number"),
        },
    };
    Ok(RequestHost { name, port })
}

/// A-86: how slowly a connection's peer may move bytes while the edge waits on it.
#[derive(Debug, Clone, Copy)]
struct PaceLimits {
    /// The longest the edge waits on a peer that moves nothing. Also the most credit a peer can
    /// hold: bytes moved early never buy a longer stall later.
    idle: Duration,
    /// Bytes per second: each `min_rate` bytes moved buy one second of waiting.
    min_rate: u64,
}

/// One direction of a connection under `PaceLimits`. Its credit drains while the edge waits on
/// the peer and refills by one second per `min_rate` bytes moved, up to `idle`: a peer that stops
/// is cut after `idle`, and one that keeps moving bytes slower than `min_rate` a little later.
/// Time the edge spends on its own work (a handler, an upstream) drains nothing.
struct Pace {
    limits: PaceLimits,
    credit: Duration,
    /// When the current wait began, or when it was last drained; `None` while not waiting.
    waiting_since: Option<Instant>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

impl Pace {
    fn new(limits: PaceLimits) -> Self {
        Self {
            limits,
            credit: limits.idle,
            waiting_since: None,
            timer: None,
        }
    }

    /// The edge waits on the peer at `now`: the credit left (none: cut the connection).
    fn wait(&mut self, now: Instant) -> Duration {
        if let Some(since) = self.waiting_since.replace(now) {
            self.credit = self
                .credit
                .saturating_sub(now.saturating_duration_since(since));
        }
        self.credit
    }

    /// The wait ended at `now`, with `bytes` moved (none for an end of file, a finished flush,
    /// or a wait the edge gave up because it is its own turn).
    fn moved(&mut self, bytes: usize, now: Instant) {
        if let Some(since) = self.waiting_since.take() {
            self.credit = self
                .credit
                .saturating_sub(now.saturating_duration_since(since));
        }
        let earned = Duration::from_nanos(
            (bytes as u64).saturating_mul(1_000_000_000) / self.limits.min_rate.max(1),
        );
        self.credit = self.credit.saturating_add(earned).min(self.limits.idle);
    }

    /// The edge waits on the peer: `Ready` with the error that ends the connection once the
    /// credit is spent, otherwise `Pending` with a timer armed for the moment it would be.
    fn poll_spent(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Error> {
        loop {
            let now = Instant::now();
            let left = self.wait(now);
            if left.is_zero() {
                return Poll::Ready(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the peer kept the connection below the door's minimum pace",
                ));
            }
            let deadline = now + left;
            let timer = self
                .timer
                .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
            timer.as_mut().reset(deadline);
            if std::future::Future::poll(timer.as_mut(), cx).is_pending() {
                return Poll::Pending;
            }
        }
    }
}

/// Whether a handler waits for request body bytes on this connection: set by its `DemandBody`,
/// read by its `PacedStream`. hyper reads the first body bytes before the handler runs, so a read
/// may go pending while nobody waits yet; it leaves its waker here, and the moment a handler starts
/// waiting the stream is polled again and paced. A body that never starts is cut like a slow one.
#[derive(Clone, Default)]
struct BodyDemand(Arc<BodyDemandState>);

#[derive(Default)]
struct BodyDemandState {
    wanted: std::sync::atomic::AtomicBool,
    reader: futures::task::AtomicWaker,
}

impl BodyDemand {
    fn set(&self, wanted: bool) {
        self.0
            .wanted
            .store(wanted, std::sync::atomic::Ordering::SeqCst);
        if wanted {
            self.0.reader.wake();
        }
    }

    fn wanted(&self) -> bool {
        self.0.wanted.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The stream's read went pending: wake it when a handler starts waiting for the body.
    fn park_reader(&self, waker: &std::task::Waker) {
        self.0.reader.register(waker);
    }
}

/// A-86: every door connection's stream. A write, flush or shutdown that waits on the peer drains
/// the write `Pace`; a read drains the read `Pace` only while a handler waits for body bytes
/// (`BodyDemand`). hyper also reads to notice an end of file while the edge works on a request,
/// and that wait is the edge's, never the peer's. A peer below the pace gets an error, which ends
/// the connection, its handler and the permits they hold: a download slot, an `/e/` slot.
struct PacedStream<IO> {
    inner: IO,
    read: Pace,
    write: Pace,
    demand: BodyDemand,
}

impl<IO> PacedStream<IO> {
    fn new(inner: IO, limits: PaceLimits, demand: BodyDemand) -> Self {
        Self {
            inner,
            read: Pace::new(limits),
            write: Pace::new(limits),
            demand,
        }
    }

    /// The outcome of a write, a flush or a shutdown: progress refills the write credit, a wait
    /// drains it.
    fn paced_write<T>(
        &mut self,
        cx: &mut Context<'_>,
        polled: Poll<std::io::Result<T>>,
        bytes: impl Fn(&T) -> usize,
    ) -> Poll<std::io::Result<T>> {
        match polled {
            Poll::Ready(Ok(value)) => {
                self.write.moved(bytes(&value), Instant::now());
                Poll::Ready(Ok(value))
            }
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => self.write.poll_spent(cx).map(Err),
        }
    }
}

impl<IO: AsyncRead + Unpin> AsyncRead for PacedStream<IO> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                this.read.moved(buf.filled().len() - before, Instant::now());
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => {
                // Parked before the check, so a handler that starts waiting right after it still
                // wakes this read.
                this.demand.park_reader(cx.waker());
                if this.demand.wanted() {
                    this.read.poll_spent(cx).map(Err)
                } else {
                    this.read.moved(0, Instant::now());
                    Poll::Pending
                }
            }
        }
    }
}

impl<IO: AsyncWrite + Unpin> AsyncWrite for PacedStream<IO> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write(cx, buf);
        this.paced_write(cx, polled, |written| *written)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_write_vectored(cx, bufs);
        this.paced_write(cx, polled, |written| *written)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_flush(cx);
        this.paced_write(cx, polled, |_| 0)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_shutdown(cx);
        this.paced_write(cx, polled, |_| 0)
    }
}

/// A request body that tells its connection's `PacedStream` when its handler waits for bytes.
struct DemandBody<B> {
    inner: B,
    demand: BodyDemand,
    /// This body set the demand, and clears it when it goes.
    wanting: bool,
}

impl<B> DemandBody<B> {
    fn new(inner: B, demand: BodyDemand) -> Self {
        Self {
            inner,
            demand,
            wanting: false,
        }
    }
}

impl<B> HttpBody for DemandBody<B>
where
    B: HttpBody + Unpin,
{
    type Data = B::Data;
    type Error = B::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        let wanting = polled.is_pending();
        if wanting != this.wanting {
            this.wanting = wanting;
            this.demand.set(wanting);
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

impl<B> Drop for DemandBody<B> {
    fn drop(&mut self) {
        if self.wanting {
            self.demand.set(false);
        }
    }
}

// ---------------------------------------------------------------------------
// The proxy to io.web (docs/io-web-v2.md §11.2), the door's third surface: a web host's requests
// go to io.web on the motherbee over the mesh's mutual TLS, and nowhere else (never a host or a
// port a request or a route names). In this stage a web host takes GET and HEAD with no body. The
// edge rebuilds the framing, strips the hop-by-hop headers both ways and sets the forwarding
// headers itself; io.web's body streams back through the door's paced stream.
// ---------------------------------------------------------------------------

/// The TCP connection to io.web and the TLS handshake on it must finish within this, together.
const IO_WEB_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// io.web's response head must arrive within this once the request is sent. The body then has no
/// deadline of its own: it streams as io.web sends it, and the door's pace bounds the client.
const IO_WEB_HEAD_TIMEOUT: Duration = Duration::from_secs(30);
/// hyper's buffer on a connection to io.web, as on the door's own connections.
const IO_WEB_MAX_BUF: usize = 64 * 1024;

/// The hop-by-hop headers (RFC 9110 §7.6.1, and the older `Keep-Alive` and `Proxy-Connection`).
/// They describe one connection, so they never cross the proxy in either direction, and neither
/// does any header a `Connection` header names (`strip_hop_by_hop`).
const HOP_BY_HOP_HEADERS: [&str; 9] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
    "proxy-authenticate",
];

/// Request headers that never reach io.web, besides the hop-by-hop ones and every
/// `X-Forwarded-*`: the framing, `Expect` and `Host`, which the edge rebuilds; the forwarding
/// headers it sets itself; the headers some servers take for the client's address or the URL; and
/// the method overrides, which would turn a GET into another method past the route's policy.
const DROPPED_REQUEST_HEADERS: [&str; 12] = [
    "content-length",
    "expect",
    "host",
    "forwarded",
    "x-request-id",
    "x-real-ip",
    "true-client-ip",
    "x-original-url",
    "x-rewrite-url",
    "x-http-method-override",
    "x-http-method",
    "x-method-override",
];

/// The door's third surface: the web hosts' requests to io.web, under the route's policy
/// (`web_policy_refusal`) and the proxy's cap.
struct WebProxy {
    /// io.web, or why the edge has no way to it; then every proxied request answers 502.
    upstream: Result<IoWebClient, String>,
    /// One permit per proxied request in flight, held to its response's last byte
    /// (`IoWebBody`); `None` when `web_max_inflight` is 0.
    inflight: Option<Arc<Semaphore>>,
    max_inflight: usize,
    /// Requests refused at the cap, logged at most once per interval.
    refused: std::sync::Mutex<RefusedConnections>,
    /// Requests io.web did not answer: the last one, for SY.admin (`upstream_status`), and the
    /// ones not logged yet.
    failures: std::sync::Mutex<ProxyFailures>,
}

/// The proxy's failures to get io.web's answer.
#[derive(Debug, Default)]
struct ProxyFailures {
    /// The last one: when (Unix seconds) and its kind (`IoWebFailure::kind`), nothing of its
    /// request.
    last: Option<(i64, &'static str)>,
    /// Logged at most once per interval, with their count and the last one's error, as the
    /// refusals at the cap are.
    unlogged: RefusedConnections,
}

impl WebProxy {
    fn new(upstream: Result<IoWebClient, String>, max_inflight: usize) -> Self {
        Self {
            upstream,
            inflight: (max_inflight > 0).then(|| Arc::new(Semaphore::new(max_inflight))),
            max_inflight,
            refused: std::sync::Mutex::new(RefusedConnections::default()),
            failures: std::sync::Mutex::new(ProxyFailures::default()),
        }
    }

    /// The upstream as the `EDGE_WEB_ROUTES` answer reports it: `state` `ready`, or `unavailable`
    /// with the boot `reason` (every web host then answers 502 until the edge restarts); and
    /// `last_failure`, the time (`at`, Unix seconds) and `kind` of the last request io.web did not
    /// answer, never anything of the request.
    fn upstream_status(&self) -> Value {
        let last = self
            .failures
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last;
        json!({
            "state": if self.upstream.is_ok() { "ready" } else { "unavailable" },
            "reason": self.upstream.as_ref().err(),
            "last_failure": last.map(|(at, kind)| json!({ "at": at, "kind": kind })),
        })
    }

    /// One request for the web host `host` (`canonical_host` form), from `peer`, which reached the
    /// door over `proto`. A request the route's policy refuses, or one over the cap, never reaches
    /// io.web. Any failure to get io.web's answer is the fixed 502 (`io_web_unavailable`), and
    /// its detail goes to the log only. One log line per request io.web answers, and the failures
    /// at most once per interval, with their count and the last one: the method, the path without
    /// its query (stage 2's exchange code rides the query, §11.3), the status, the time to the
    /// response head and the request id io.web got.
    async fn forward(
        &self,
        req: Request,
        host: &str,
        peer: SocketAddr,
        proto: &'static str,
    ) -> Response {
        if let Some(refused) = web_policy_refusal(req.method(), req.headers()) {
            tracing::debug!(
                peer = %peer,
                method = %req.method(),
                host,
                path = req.uri().path(),
                status = refused.status().as_u16(),
                "sy-edge refused a request for a web host"
            );
            return refused;
        }
        let slot = match &self.inflight {
            None => None,
            Some(inflight) => match Arc::clone(inflight).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    let refused = self
                        .refused
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .note(Instant::now(), REFUSED_CONNECTIONS_LOG_INTERVAL);
                    if let Some(count) = refused {
                        tracing::warn!(
                            peer = %peer,
                            refused = count,
                            web_max_inflight = self.max_inflight,
                            "sy-edge: the proxy to io.web is at its cap; refusing new requests"
                        );
                    }
                    return http_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "WEB_BUSY",
                        "too many requests in flight; retry shortly",
                    );
                }
            },
        };
        let started = Instant::now();
        let request_id = Uuid::new_v4().to_string();
        let method = req.method().clone();
        let path = req.uri().path().to_string();
        let outcome = match &self.upstream {
            Ok(client) => {
                let forwarding = Forwarding {
                    host,
                    client: peer.ip(),
                    proto,
                    request_id: &request_id,
                };
                client.send(io_web_request(&req, &forwarding)).await
            }
            Err(reason) => Err(IoWebFailure::NoUpstream(reason.clone())),
        };
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        match outcome {
            Ok(response) => {
                tracing::info!(
                    request_id = %request_id,
                    method = %method,
                    host,
                    path = %path,
                    status = response.status().as_u16(),
                    duration_ms,
                    peer = %peer,
                    "sy-edge proxied a request to io.web"
                );
                from_io_web(response, slot)
            }
            Err(failure) => {
                let due = {
                    let mut failures = self.failures.lock().unwrap_or_else(PoisonError::into_inner);
                    failures.last = Some((now_unix_secs(), failure.kind()));
                    failures
                        .unlogged
                        .note(Instant::now(), REFUSED_CONNECTIONS_LOG_INTERVAL)
                };
                if let Some(count) = due {
                    tracing::warn!(
                        failed = count,
                        request_id = %request_id,
                        method = %method,
                        host,
                        path = %path,
                        status = StatusCode::BAD_GATEWAY.as_u16(),
                        duration_ms,
                        peer = %peer,
                        error = %failure,
                        "sy-edge: io.web did not answer proxied requests (502): how many since the last line, and the last one"
                    );
                }
                io_web_unavailable(&request_id)
            }
        }
    }
}

/// The edge's way to io.web: the host this hive's router dials (the motherbee), on `IO_WEB_PORT`,
/// over the mesh's mutual TLS. The edge presents its hive's leaf, and verifies io.web's inside the
/// handshake with rustls' WebPki verifier over the mesh CA, under the name `IO_WEB_SERVER_NAME`
/// whatever address it dials (`MeshTlsMaterial::client_config_by_name`). One connection per
/// proxied request, closed once its response ends: nothing is pooled.
struct IoWebClient {
    /// `<motherbee host>:IO_WEB_PORT`.
    authority: String,
    tls: tokio_rustls::TlsConnector,
    /// `IO_WEB_SERVER_NAME`, never the address dialled, which the certificate does not name.
    server_name: ServerName<'static>,
    /// `IO_WEB_CONNECT_TIMEOUT`, `IO_WEB_HEAD_TIMEOUT`; shorter in the tests.
    connect_timeout: Duration,
    head_timeout: Duration,
}

impl IoWebClient {
    /// A client of io.web at `authority`, over `tls`, a client configuration that verifies the
    /// server by name (`client_config_by_name`); never the router's CA-only one.
    fn new(authority: String, tls: Arc<rustls::ClientConfig>) -> Result<Self, String> {
        let server_name = ServerName::try_from(IO_WEB_SERVER_NAME)
            .map_err(|err| format!("io.web's server name '{IO_WEB_SERVER_NAME}': {err}"))?;
        Ok(Self {
            authority,
            tls: tokio_rustls::TlsConnector::from(tls),
            server_name,
            connect_timeout: IO_WEB_CONNECT_TIMEOUT,
            head_timeout: IO_WEB_HEAD_TIMEOUT,
        })
    }

    /// This hive's client of io.web: its mesh TLS material, `<tls_root>/<hive_id>/` (`ca.crt`,
    /// `cert.crt`, `cert.key`, as add_hive distributes them), and the host of its first WAN uplink.
    /// The error says what is missing.
    fn for_hive(
        tls_root: &std::path::Path,
        hive_id: Option<&str>,
        uplink: Option<&str>,
    ) -> Result<Self, String> {
        let uplink =
            uplink.ok_or("hive.yaml has no wan.uplinks, so the motherbee's address is unknown")?;
        let host = uplink_host(uplink)
            .ok_or_else(|| format!("wan.uplinks[0].address '{uplink}' names no host"))?;
        let hive_id = hive_id
            .filter(|hive_id| is_valid_hive_id(hive_id))
            .ok_or("hive.yaml has no valid hive_id, so this hive's mesh TLS material is unknown")?;
        let dir = tls_root.join(hive_id);
        install_crypto_provider();
        let material = MeshTlsMaterial::load_from_dir(&dir)
            .map_err(|err| format!("mesh TLS material in {}: {err}", dir.display()))?;
        let tls = material
            .client_config_by_name()
            .map_err(|err| format!("mesh TLS client configuration: {err}"))?;
        Self::new(format!("{host}:{IO_WEB_PORT}"), tls)
    }

    /// Send `request` on a connection of its own, and wait for the response head. The connection
    /// closes once the response body ends, or is dropped.
    async fn send(
        &self,
        request: hyper::Request<Body>,
    ) -> Result<hyper::Response<hyper::body::Incoming>, IoWebFailure> {
        let mut sender = tokio::time::timeout(self.connect_timeout, self.dial())
            .await
            .map_err(|_| IoWebFailure::ConnectTimeout(self.connect_timeout))??;
        let response = tokio::time::timeout(self.head_timeout, sender.send_request(request))
            .await
            .map_err(|_| IoWebFailure::HeadTimeout(self.head_timeout))?
            .map_err(IoWebFailure::Head)?;
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            return Err(IoWebFailure::Switched);
        }
        Ok(response)
    }

    /// TCP, then TLS, then hyper's HTTP/1 client on top; the connection runs in its own task.
    async fn dial(&self) -> Result<hyper::client::conn::http1::SendRequest<Body>, IoWebFailure> {
        let tcp = tokio::net::TcpStream::connect(&self.authority)
            .await
            .map_err(IoWebFailure::Connect)?;
        let _ = tcp.set_nodelay(true);
        let stream = self
            .tls
            .connect(self.server_name.clone(), tcp)
            .await
            .map_err(IoWebFailure::Handshake)?;
        let (sender, connection) = hyper::client::conn::http1::Builder::new()
            .max_buf_size(IO_WEB_MAX_BUF)
            .handshake(hyper_util::rt::TokioIo::new(stream))
            .await
            .map_err(IoWebFailure::Http)?;
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(error = %error_chain(&err), "sy-edge: a connection to io.web ended with an error");
            }
        });
        Ok(sender)
    }
}

/// Why one proxied request got no answer from io.web. The log reads it; the client gets the fixed
/// 502 (`io_web_unavailable`).
#[derive(Debug)]
enum IoWebFailure {
    /// The edge has no way to io.web (`WebProxy::upstream`).
    NoUpstream(String),
    Connect(std::io::Error),
    /// The TLS handshake failed on the edge's side: io.web's certificate does not verify (another
    /// CA, another name), or the connection dropped.
    Handshake(std::io::Error),
    /// The TCP connection and the TLS handshake took longer than `connect_timeout`.
    ConnectTimeout(Duration),
    Http(hyper::Error),
    HeadTimeout(Duration),
    /// The request went out and no whole head came back. io.web's refusal of the edge's
    /// certificate lands here: in TLS 1.3 the client's handshake ends before the server checks
    /// that certificate, so the refusal (`access_denied`) is the first thing read.
    Head(hyper::Error),
    /// io.web switched protocols, which the edge never asks for.
    Switched,
}

impl IoWebFailure {
    /// The failure's name in the `EDGE_WEB_ROUTES` answer (`WebProxy::upstream_status`), without
    /// its detail.
    fn kind(&self) -> &'static str {
        match self {
            Self::NoUpstream(_) => "no_upstream",
            Self::Connect(_) => "connect",
            Self::Handshake(_) => "handshake",
            Self::ConnectTimeout(_) => "connect_timeout",
            Self::Http(_) => "http_handshake",
            Self::HeadTimeout(_) => "head_timeout",
            Self::Head(_) => "head",
            Self::Switched => "switched_protocols",
        }
    }
}

impl std::fmt::Display for IoWebFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoUpstream(reason) => write!(f, "no upstream: {reason}"),
            Self::Connect(err) => write!(f, "connect: {}", error_chain(err)),
            Self::Handshake(err) => write!(f, "TLS handshake: {}", error_chain(err)),
            Self::ConnectTimeout(limit) => {
                write!(f, "connect and TLS handshake took more than {limit:?}")
            }
            Self::Http(err) => write!(f, "HTTP/1 handshake: {}", error_chain(err)),
            Self::HeadTimeout(limit) => write!(f, "no response head within {limit:?}"),
            Self::Head(err) => write!(f, "response head: {}", error_chain(err)),
            Self::Switched => write!(f, "io.web answered 101 Switching Protocols"),
        }
    }
}

/// An error and its sources, outermost first, for a log line.
fn error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut text = err.to_string();
    let mut cause = err.source();
    while let Some(err) = cause {
        text.push_str(": ");
        text.push_str(&err.to_string());
        cause = err.source();
    }
    text
}

/// Why the route's policy refuses a request, answered before anything reaches io.web: in this
/// stage a web host takes GET and HEAD only (CONNECT and TRACE among the rest: `405` with
/// `Allow`), with no upgrade and no body (a `Content-Length` other than 0, or any
/// `Transfer-Encoding`).
fn web_policy_refusal(method: &Method, headers: &HeaderMap) -> Option<Response> {
    if !matches!(*method, Method::GET | Method::HEAD) {
        let mut refused = http_error(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "only GET and HEAD are served on this host",
        );
        refused
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return Some(refused);
    }
    if headers.contains_key(header::UPGRADE) {
        return Some(http_error(
            StatusCode::BAD_REQUEST,
            "UPGRADE_NOT_ALLOWED",
            "this host does not switch protocols",
        ));
    }
    let has_body = headers.contains_key(header::TRANSFER_ENCODING)
        || headers.get_all(header::CONTENT_LENGTH).iter().any(|value| {
            value
                .to_str()
                .ok()
                .and_then(|length| length.trim().parse::<u64>().ok())
                != Some(0)
        });
    if has_body {
        return Some(http_error(
            StatusCode::BAD_REQUEST,
            "BODY_NOT_ALLOWED",
            "a request to this host carries no body",
        ));
    }
    None
}

/// What the edge tells io.web about a request it forwards.
struct Forwarding<'a> {
    /// The web host, in `canonical_host` form.
    host: &'a str,
    /// The peer's address, as the door's accept loop saw it.
    client: std::net::IpAddr,
    /// `https`, or `http` on a plaintext door.
    proto: &'static str,
    /// Fresh for each request; the edge's log line names it too.
    request_id: &'a str,
}

/// The request io.web gets: the method and the target in origin form, the client's headers as
/// `io_web_request_headers` leaves them, and no body (`web_policy_refusal` let none through).
fn io_web_request(req: &Request, forwarding: &Forwarding<'_>) -> hyper::Request<Body> {
    let mut upstream = hyper::Request::new(Body::empty());
    *upstream.method_mut() = req.method().clone();
    *upstream.uri_mut() = req
        .uri()
        .path_and_query()
        .cloned()
        .map(Uri::from)
        .unwrap_or_else(|| Uri::from_static("/"));
    *upstream.headers_mut() = io_web_request_headers(req.headers(), forwarding);
    upstream
}

/// The client's headers as io.web gets them: the hop-by-hop ones and those `Connection` names
/// stripped first (`strip_hop_by_hop`), then `DROPPED_REQUEST_HEADERS` and every `X-Forwarded-*`;
/// then the edge's own `Host` (the web host), `X-Forwarded-For`, `X-Forwarded-Proto`,
/// `X-Forwarded-Host`, `Forwarded` (RFC 7239) and `X-Request-Id`, in place of any the client sent.
fn io_web_request_headers(inbound: &HeaderMap, forwarding: &Forwarding<'_>) -> HeaderMap {
    let mut headers = inbound.clone();
    strip_hop_by_hop(&mut headers);
    let dropped: Vec<header::HeaderName> = headers
        .keys()
        .filter(|name| {
            DROPPED_REQUEST_HEADERS.contains(&name.as_str())
                || name.as_str().starts_with("x-forwarded-")
        })
        .cloned()
        .collect();
    for name in dropped {
        headers.remove(name);
    }
    let client = forwarding.client.to_canonical();
    // RFC 7239 §6: an IPv6 node is bracketed, and then quoted.
    let node = match client {
        std::net::IpAddr::V6(v6) => format!("\"[{v6}]\""),
        std::net::IpAddr::V4(v4) => v4.to_string(),
    };
    let (host, proto) = (forwarding.host, forwarding.proto);
    let set = [
        (header::HOST, host.to_string()),
        (
            header::HeaderName::from_static("x-forwarded-for"),
            client.to_string(),
        ),
        (
            header::HeaderName::from_static("x-forwarded-proto"),
            proto.to_string(),
        ),
        (
            header::HeaderName::from_static("x-forwarded-host"),
            host.to_string(),
        ),
        (
            header::FORWARDED,
            format!("for={node};proto={proto};host={host}"),
        ),
        (
            header::HeaderName::from_static("x-request-id"),
            forwarding.request_id.to_string(),
        ),
    ];
    for (name, value) in set {
        // Every value is a host the door validated, an address, a scheme or a UUID: all valid.
        if let Ok(value) = HeaderValue::try_from(value) {
            headers.insert(name, value);
        }
    }
    headers
}

/// Remove the hop-by-hop headers (`HOP_BY_HOP_HEADERS`) and every header a `Connection` header
/// names, from a request or a response.
fn strip_hop_by_hop(headers: &mut HeaderMap) {
    let named: Vec<header::HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| header::HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in HOP_BY_HOP_HEADERS {
        headers.remove(name);
    }
}

/// io.web's headers as the door sends them: a `Content-Length` that a `Transfer-Encoding`
/// overrides goes (RFC 9112 §6.3), then the hop-by-hop headers (`strip_hop_by_hop`). hyper
/// frames the body again on the client's connection.
fn strip_io_web_response_headers(headers: &mut HeaderMap) {
    if headers.contains_key(header::TRANSFER_ENCODING) {
        headers.remove(header::CONTENT_LENGTH);
    }
    strip_hop_by_hop(headers);
}

/// io.web's response as the door sends it: its status, its headers (`strip_io_web_response_headers`)
/// and its body as it comes (`IoWebBody`). `Door::handle` adds the security headers io.web did not
/// set (`with_security_headers`).
fn from_io_web(
    response: hyper::Response<hyper::body::Incoming>,
    slot: Option<OwnedSemaphorePermit>,
) -> Response {
    let (mut parts, body) = response.into_parts();
    strip_io_web_response_headers(&mut parts.headers);
    Response::from_parts(
        parts,
        Body::new(IoWebBody {
            inner: body,
            _slot: slot,
        }),
    )
}

/// io.web's response body on its way to the client, holding the request's in-flight slot: given
/// back with the last byte, or with the connection.
struct IoWebBody {
    inner: hyper::body::Incoming,
    _slot: Option<OwnedSemaphorePermit>,
}

impl HttpBody for IoWebBody {
    type Data = Bytes;
    type Error = hyper::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, hyper::Error>>> {
        Pin::new(&mut self.get_mut().inner).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

/// The fixed answer when io.web did not answer. What failed is in the edge's log line, which the
/// request id finds.
fn io_web_unavailable(request_id: &str) -> Response {
    let mut response = error_answer(
        StatusCode::BAD_GATEWAY,
        json!({
            "ok": false,
            "error_code": "UPSTREAM_UNAVAILABLE",
            "message": "the service is unavailable; retry shortly",
            "request_id": request_id,
        }),
    );
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
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
        return built(
            Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, "GET, HEAD")
                .body(Body::empty()),
        );
    }
    if !is_lower_hex_64(&key) {
        return public_not_found();
    }
    // The table holds the SHA-256 of each key, never the key (A-82).
    let key_sha256 = sha256_hex(&key);
    let row = match state
        .public_registry
        .read()
        .ok()
        .and_then(|guard| guard.get(&key_sha256).cloned())
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
        return artifact_response(built(
            Response::builder()
                .status(StatusCode::NOT_MODIFIED)
                .header(header::ETAG, etag)
                .header(header::CACHE_CONTROL, "no-store")
                .body(Body::empty()),
        ));
    }
    // A-86: one address holds at most `public_max_inflight_per_ip` of the download slots.
    let peer_slot = match request_peer_ip(&req) {
        Some(ip) => match state.public_peer_slots.try_take(ip) {
            Some(slot) => Some(slot),
            None => {
                let refused = state
                    .refused_downloads
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .note(Instant::now(), REFUSED_CONNECTIONS_LOG_INTERVAL);
                if let Some(count) = refused {
                    tracing::warn!(
                        peer = %ip,
                        refused = count,
                        max_per_ip = state.public_peer_slots.max,
                        "sy-edge: a peer address at its download cap; refusing its new downloads"
                    );
                }
                return http_error(
                    StatusCode::TOO_MANY_REQUESTS,
                    "PUBLIC_PEER_BUSY",
                    "too many downloads from this address",
                );
            }
        },
        None => None,
    };
    let permit = match Arc::clone(&state.public_inflight).try_acquire_owned() {
        Ok(permit) => (permit, peer_slot),
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
            return built(
                Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{}", row.size))
                    .header(header::ACCEPT_RANGES, "bytes")
                    .body(Body::empty()),
            );
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
        return artifact_response(built(builder.body(Body::empty())));
    }

    let mut file = tokio::fs::File::from_std(file);
    if let Err(err) = file.seek(SeekFrom::Start(start)).await {
        tracing::warn!(publication_id = %row.publication_id, error = %err, "public artifact seek failed");
        return public_not_found();
    }
    // The download slot and the peer's slot go with the last byte, or with the connection.
    type DownloadSlots = (OwnedSemaphorePermit, Option<PeerSlot>);
    let stream = futures::stream::try_unfold(
        (file, permit, content_length),
        |(mut file, permit, remaining): (tokio::fs::File, DownloadSlots, u64)| async move {
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
    artifact_response(built(builder.body(Body::from_stream(stream))))
}

/// Marks a response whose content policy the artifact decides (`content_policy`): the door adds
/// none of its own (`with_security_headers`). A sandboxed page carries its own CSP; a PDF or an
/// image keeps none, since a `default-src 'none'` can stop a browser's viewer, and artifacts may be
/// framed.
#[derive(Debug, Clone, Copy)]
struct OwnContentPolicy;

fn artifact_response(mut response: Response) -> Response {
    response.extensions_mut().insert(OwnContentPolicy);
    response
}

/// A response the edge built itself. One it could not build (an invalid header value, say) is a
/// 500 with the security headers, never an empty 200 (A-83).
fn built(response: Result<Response, axum::http::Error>) -> Response {
    response.unwrap_or_else(|err| {
        tracing::warn!(error = %err, "sy-edge could not build a response; answering 500");
        http_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "internal error",
        )
    })
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
    let peer = request_peer(&req);

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

    // 4. Body — inline only, capped; the envelope check below is the hard gate. A body that
    //    comes slower than the door's pace ends with the connection (A-86, `PacedStream`), and
    //    one that is not whole within `body_timeout` ends here.
    let read = tokio::time::timeout(
        state.body_timeout,
        to_bytes(req.into_body(), HTTP_ENVELOPE_INLINE_LIMIT_BYTES),
    );
    let body = match read.await {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(err)) if !timed_out(&err) => {
            return http_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "BODY_TOO_LARGE",
                "request body exceeds the inline limit",
            )
        }
        Ok(Err(_)) | Err(_) => {
            tracing::debug!(ich = %ich, peer = %peer, "sy-edge: an /e/ request body came too slowly");
            return http_error(
                StatusCode::REQUEST_TIMEOUT,
                "BODY_TIMEOUT",
                "request body too slow",
            );
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
        // A-88: the sizes stay in the edge's log.
        tracing::debug!(ich = %ich, peer = %peer, error = %err, "sy-edge: an /e/ request does not fit the mesh envelope");
        return http_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "REQ_TOO_LARGE",
            "request too large",
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
            let code = rpc_error_code(&err);
            // A-88: the trace, the target and the message family stay in the edge's log; the
            // internet gets the fixed answer of the code.
            tracing::warn!(ich = %ich, peer = %peer, code, error = %err, "sy-edge: an /e/ request got no answer from its channel");
            http_error(status, code, rpc_error_message(&err))
        }
    }
}

/// The client's address, as the door's accept loop saw it (`Door::handle`), for the logs.
fn request_peer(req: &Request) -> String {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.to_string())
        .unwrap_or_else(|| "-".to_string())
}

/// The client's IP address, for the caps per address.
fn request_peer_ip(req: &Request) -> Option<std::net::IpAddr> {
    req.extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip())
}

/// Whether an I/O timeout is behind `err`: the door's pace cut the stream (`PacedStream`).
fn timed_out(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut cause = Some(err);
    while let Some(err) = cause {
        if err
            .downcast_ref::<std::io::Error>()
            .is_some_and(|err| err.kind() == std::io::ErrorKind::TimedOut)
        {
            return true;
        }
        cause = err.source();
    }
    false
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

/// The fixed message of each `rpc_error_code` (A-88): what failed, never the trace, the target or
/// the message family that `RpcError`'s text carries.
fn rpc_error_message(err: &RpcError) -> &'static str {
    match err {
        RpcError::Timeout { .. } => "the channel did not answer in time",
        RpcError::Unreachable { .. } | RpcError::TtlExceeded { .. } => "the channel is unreachable",
        RpcError::Disconnected | RpcError::Node(_) => "the channel is unavailable",
        _ => "the channel failed to answer",
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

/// The headers on every answer the edge writes itself: its errors, an unknown host's 404 and the
/// 500 of a response it could not build (A-83). Nothing is cached, sniffed, framed, indexed or
/// referred. io.web's answers get the ones io.web did not set.
const SECURITY_HEADERS: [(&str, &str); 5] = [
    ("cache-control", "no-store"),
    ("x-content-type-options", "nosniff"),
    ("referrer-policy", "no-referrer"),
    ("x-robots-tag", "noindex, nofollow, noarchive"),
    (
        "content-security-policy",
        "default-src 'none'; frame-ancestors 'none'",
    ),
];

/// One of the edge's own error answers. Its message is fixed text, never an error's detail, which
/// goes to the edge's log only (A-88).
fn http_error(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    error_answer(
        status,
        json!({ "ok": false, "error_code": code, "message": message }),
    )
}

/// An error answer of the edge's: `body` as JSON, with the security headers.
fn error_answer(status: StatusCode, body: Value) -> Response {
    let mut response = (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response();
    for (name, value) in SECURITY_HEADERS {
        response.headers_mut().insert(
            header::HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluxbee_sdk::mesh::{HiveRegistry, HiveRole, MeshCa};
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
        let ca = fluxbee_sdk::mesh::MeshCa::generate().expect("ca");
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

    /// The capability key of `sample_public_row`.
    fn sample_key() -> String {
        "ab".repeat(32)
    }

    fn sample_public_row(data: &[u8]) -> PublicArtifactRow {
        let sha256 = format!("{:x}", Sha256::digest(data));
        PublicArtifactRow {
            key_sha256: sha256_hex(&sample_key()),
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
        registry.insert(row.key_sha256.clone(), row.clone());
        persist_public_registry(&registry_path, &registry).unwrap();
        assert_eq!(
            load_public_registry(&registry_path).get(&row.key_sha256),
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
        tls_command_reply(
            tls,
            &scratch_web_routes(),
            None,
            &admin_command(msg, payload),
        )
        .await
        .expect("a TLS command")
        .1
    }

    /// A web route table on a file of its own that does not exist yet.
    fn scratch_web_routes() -> WebRouteTable {
        WebRouteTable::load(
            std::env::temp_dir()
                .join(format!("sy-edge-web-{}", Uuid::new_v4()))
                .join("web_routes.json"),
        )
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
        let door = Door {
            main: Router::new().route("/healthz", any(|| async { "ok" })),
            web: Arc::new(scratch_web_routes()),
            proxy: no_proxy(),
            tls: Arc::clone(tls),
        };
        tokio::spawn(serve_door(listener, door, Arc::clone(tls), limits));
        addr
    }

    /// A proxy with no way to io.web: every request it sends answers 502.
    fn no_proxy() -> Arc<WebProxy> {
        Arc::new(WebProxy::new(Err("no io.web in this test".to_string()), 0))
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
            DoorLimits::new(MAX_CONNECTIONS, 0, HTTP_HEADER_READ_TIMEOUT, DOOR_PACE),
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
                    &scratch_web_routes(),
                    None,
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
            Arc::new(scratch_web_routes()),
            Some(no_proxy()),
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

        // SY.admin's push of the web hosts takes the same channel.
        let routes = admin_command(
            MSG_EDGE_WEB_ROUTES,
            json!({"routes": [{"host": "archi.edge.example.com", "upstream": "io.web"}]}),
        );
        router
            .inject(routes.clone())
            .await
            .expect("web routes command");
        let reply = router
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("EDGE_WEB_ROUTES answered while the loop is busy");
        assert_eq!(
            reply.meta.msg.as_deref(),
            Some(MSG_EDGE_WEB_ROUTES_RESPONSE)
        );
        assert_eq!(reply.routing.trace_id, routes.routing.trace_id);
        assert_eq!(reply.payload["status"], "ok", "{}", reply.payload);
        // The task answers with the door's proxy, as `main` hands it over.
        assert_eq!(
            reply.payload["upstream"]["state"], "unavailable",
            "{}",
            reply.payload
        );

        // The blob command still waits for the loop, and no door command went its way.
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
        let addr = loopback_door(
            &tls,
            DoorLimits::new(1, 0, HTTP_HEADER_READ_TIMEOUT, DOOR_PACE),
        )
        .await;
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
        let addr = loopback_door(&tls, DoorLimits::new(16, 0, header_timeout, DOOR_PACE)).await;

        // The handshake completes, then nothing: hyper's head timer is already running.
        let mut silent = connect_tls(addr).await;
        let closed = closed_within(&mut silent, Duration::from_secs(5)).await;
        assert!(closed >= header_timeout.mul_f32(0.9), "{closed:?}");

        // One request, then an idle keep-alive connection: the same timer closes it.
        let mut idle = connect_tls(addr).await;
        assert_eq!(get_healthz(&mut idle).await, "ok");
        let closed = closed_within(&mut idle, Duration::from_secs(5)).await;
        assert!(closed >= header_timeout.mul_f32(0.9), "{closed:?}");
    }

    /// A-87: the head timer starts before the first byte, so a few bytes of a head never hold a
    /// connection past it (hyper-util's protocol sniffing had no timeout after the first byte).
    #[tokio::test]
    async fn one_byte_is_closed_at_the_header_timeout() {
        let header_timeout = Duration::from_millis(300);
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let addr = loopback_door(&tls, DoorLimits::new(16, 0, header_timeout, DOOR_PACE)).await;
        for start in [&b"G"[..], b"PRI"] {
            let mut stream = connect_tls(addr).await;
            tokio::io::AsyncWriteExt::write_all(&mut stream, start)
                .await
                .expect("the first bytes");
            let closed = closed_within(&mut stream, Duration::from_secs(5)).await;
            let start = String::from_utf8_lossy(start);
            assert!(closed >= header_timeout.mul_f32(0.9), "{start}: {closed:?}");
            assert!(closed < header_timeout.mul_f32(3.0), "{start}: {closed:?}");
        }
    }

    /// A-87: no HTTP/2, not even by prior knowledge. hyper's HTTP/1 parser refuses the preface at
    /// once; the head timer would close it anyway.
    #[tokio::test]
    async fn an_h2_preface_is_never_served_and_closed_by_the_header_timeout() {
        let header_timeout = Duration::from_millis(300);
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let addr = loopback_door(&tls, DoorLimits::new(16, 0, header_timeout, DOOR_PACE)).await;
        let mut stream = connect_tls(addr).await;
        // The client preface and an empty SETTINGS frame, as an h2 client opens.
        let mut preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        preface.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 0, 0]);
        tokio::io::AsyncWriteExt::write_all(&mut stream, &preface)
            .await
            .expect("the preface");
        let started = Instant::now();
        let mut answered = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            match tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await {
                Err(_) => panic!("the edge kept an h2 preface open"),
                Ok(Ok(0)) | Ok(Err(_)) => break,
                Ok(Ok(read)) => answered.extend_from_slice(&chunk[..read]),
            }
        }
        assert!(started.elapsed() < header_timeout.mul_f32(3.0));
        assert!(
            answered.is_empty() || answered.starts_with(b"HTTP/1.1 "),
            "no h2 frame comes back: {answered:?}"
        );
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
        assert_eq!(MAX_CONNECTIONS, 4096);
        assert_eq!(ACCEPT_ERROR_BACKOFF, Duration::from_millis(100));
    }

    // -----------------------------------------------------------------------
    // HTTP on the door (docs/io-web-v2.md §11.1)
    // -----------------------------------------------------------------------

    const PEER: &str = "203.0.113.7:50123";
    const WEB_HOST: &str = "archi.edge.example.com";

    /// Short limits, so the pace tests take a fraction of a second.
    fn quick_limits(idle: Duration) -> DoorLimits {
        DoorLimits::new(
            16,
            0,
            Duration::from_secs(5),
            PaceLimits {
                idle,
                min_rate: 1024,
            },
        )
    }

    /// One connection to `door` over an in-process pipe that holds `buffer` bytes each way; the
    /// server's task ends with the connection.
    fn pipe_to(
        door: Door,
        limits: DoorLimits,
        buffer: usize,
    ) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
        pipe_from(PEER, door, limits, buffer)
    }

    /// `pipe_to`, as if the connection came from `peer`.
    fn pipe_from(
        peer: &str,
        door: Door,
        limits: DoorLimits,
        buffer: usize,
    ) -> (tokio::io::DuplexStream, tokio::task::JoinHandle<()>) {
        let (client, server) = tokio::io::duplex(buffer);
        let peer: SocketAddr = peer.parse().expect("peer");
        let task = tokio::spawn(async move { serve_http(server, peer, door, &limits).await });
        (client, task)
    }

    /// One whole answer, read to the end of the connection (the requests ask `connection: close`).
    struct Answer {
        status: u16,
        head: String,
        body: Vec<u8>,
    }

    impl Answer {
        fn header(&self, name: &str) -> Option<String> {
            self.head.lines().skip(1).find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case(name)
                    .then(|| value.trim().to_string())
            })
        }

        fn json(&self) -> Value {
            serde_json::from_slice(&self.body).expect("a JSON body")
        }
    }

    async fn read_answer(stream: &mut tokio::io::DuplexStream) -> Answer {
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
            .await
            .expect("an answer in time")
            .expect("read");
        let split = raw
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .expect("a head");
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|status| status.parse().ok())
            .expect("a status");
        Answer {
            status,
            head,
            body: raw[split + 4..].to_vec(),
        }
    }

    async fn exchange(door: &Door, request: &str) -> Answer {
        let (mut client, _server) = pipe_to(
            door.clone(),
            quick_limits(Duration::from_secs(5)),
            64 * 1024,
        );
        tokio::io::AsyncWriteExt::write_all(&mut client, request.as_bytes())
            .await
            .expect("request");
        read_answer(&mut client).await
    }

    fn assert_security_headers(answer: &Answer) {
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(answer.header(name).as_deref(), Some(value), "{name}");
        }
    }

    /// A door whose own routes say which one answered, and who asked.
    fn labelled_door(web: WebRouteTable, tls: Arc<EdgeTls>) -> Door {
        Door {
            main:
                Router::new()
                    .route("/healthz", any(|| async { "ok" }))
                    .route("/e/*rest", any(|| async { "e" }))
                    .route("/public/:key", any(|| async { "public" }))
                    .route(
                        "/peer",
                        any(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move {
                            peer.to_string()
                        }),
                    ),
            web: Arc::new(web),
            proxy: no_proxy(),
            tls,
        }
    }

    fn web_routes_with(hosts: &[&str]) -> WebRouteTable {
        let table = scratch_web_routes();
        let routes = WebRoutes {
            routes: hosts
                .iter()
                .map(|host| WebRoute {
                    host: host.to_string(),
                    upstream: WebUpstream::IoWeb,
                })
                .collect(),
        };
        table.replace(validate_web_routes(routes, &[]).expect("valid routes"));
        table
    }

    #[test]
    fn a_request_has_one_canonical_host() {
        let host = |target: &str, hosts: &[&str]| {
            let uri: Uri = target.parse().expect("target");
            let mut headers = HeaderMap::new();
            for value in hosts {
                headers.append(header::HOST, HeaderValue::from_str(value).expect("value"));
            }
            request_host(&uri, &headers)
        };
        let at = |name: &str, port: Option<u16>| {
            Ok(Some(RequestHost {
                name: name.to_string(),
                port,
            }))
        };
        let some = |name: &str| at(name, None);
        // Case, one trailing dot and the port 443 go; the host stays.
        assert_eq!(host("/", &["ARCHI.Edge.Example.COM"]), some(WEB_HOST));
        assert_eq!(host("/", &["archi.edge.example.com."]), some(WEB_HOST));
        assert_eq!(host("/", &["archi.edge.example.com:443"]), some(WEB_HOST));
        assert_eq!(host("/", &["archi.edge.example.com.:443"]), some(WEB_HOST));
        assert_eq!(host("/", &["archi.edge.example.com:"]), some(WEB_HOST));
        // Only one dot goes; another port is kept.
        assert_eq!(
            host("/", &["archi.edge.example.com.."]),
            some("archi.edge.example.com.")
        );
        assert_eq!(
            host("/", &["archi.edge.example.com:8443"]),
            at(WEB_HOST, Some(8443))
        );
        assert_eq!(host("/", &["[2001:db8::1]:443"]), some("[2001:db8::1]"));
        // No host at all.
        assert_eq!(host("/", &[]), Ok(None));
        assert_eq!(host("/", &[""]), Ok(None));
        // Refused: two Hosts, a bad port, user information.
        assert!(host("/", &[WEB_HOST, "edge.example.com"]).is_err());
        assert!(host("/", &[WEB_HOST, WEB_HOST]).is_err());
        assert!(host("/", &["archi.edge.example.com:https"]).is_err());
        assert!(host("/", &["archi.edge.example.com:65536"]).is_err());
        assert!(host("/", &["user@archi.edge.example.com"]).is_err());
        // The absolute form wins, and a Host must agree with it.
        let absolute = "https://Archi.Edge.Example.com:443/healthz";
        assert_eq!(host(absolute, &[]), some(WEB_HOST));
        assert_eq!(host(absolute, &["archi.edge.example.com."]), some(WEB_HOST));
        assert!(host(absolute, &["edge.example.com"]).is_err());
        assert!(host(absolute, &["archi.edge.example.com:8443"]).is_err());
        assert_eq!(
            host("http://archi.edge.example.com:80/", &[]),
            at(WEB_HOST, Some(80))
        );
    }

    /// io-web-v2 §11.1 and host-posture §4.4: only the door's own hosts reach its routes.
    #[tokio::test]
    async fn only_the_door_s_own_hosts_reach_its_routes() {
        // The combined certificate of 0.1.84: the main name and the web host.
        let tls = serving_edge(self_signed(&[EDGE, WEB_HOST])).await;
        let door = labelled_door(web_routes_with(&[WEB_HOST]), Arc::clone(&tls));
        let request = |target: &str, host: &str| {
            format!("GET {target} HTTP/1.1\r\n{host}connection: close\r\n\r\n")
        };
        // The main name on any port, an IP literal, and no host at all keep the door's routes.
        for (target, host) in [
            ("/healthz", "host: edge.example.com\r\n"),
            ("/healthz", "host: EDGE.example.com.:443\r\n"),
            ("/healthz", "host: edge.example.com:8443\r\n"),
            ("/healthz", "host: 198.51.100.4\r\n"),
            ("/healthz", "host: [2001:db8::1]:8443\r\n"),
            ("https://edge.example.com/healthz", ""),
        ] {
            let answer = exchange(&door, &request(target, host)).await;
            assert_eq!(
                (answer.status, answer.body.as_slice()),
                (200, &b"ok"[..]),
                "{target} {host}"
            );
        }
        let no_host = exchange(&door, "GET /healthz HTTP/1.0\r\n\r\n").await;
        assert_eq!(no_host.body, b"ok");
        // The handlers see the peer.
        let peer = exchange(&door, &request("/peer", "host: edge.example.com\r\n")).await;
        assert_eq!(peer.body, PEER.as_bytes());

        // A web host, in any spelling, goes to the proxy on every path (here with no io.web: the
        // proxy's 502), never to the door's routes.
        let public = format!("/public/{}", "ab".repeat(32));
        for (target, host) in [
            ("/healthz", "host: archi.edge.example.com\r\n"),
            ("/", "host: ARCHI.edge.example.com\r\n"),
            ("/e/ich:1", "host: archi.edge.example.com.\r\n"),
            (public.as_str(), "host: archi.edge.example.com:443\r\n"),
            ("https://archi.edge.example.com/healthz", ""),
            (
                "https://archi.edge.example.com/e/ich:1",
                "host: archi.edge.example.com\r\n",
            ),
        ] {
            let answer = exchange(&door, &request(target, host)).await;
            assert_eq!(answer.status, 502, "{target} {host}");
            assert_eq!(answer.json()["error_code"], "UPSTREAM_UNAVAILABLE");
            assert_security_headers(&answer);
        }

        // A web host on another port, and any other host: a 404 with the security headers on
        // every path.
        for (target, host) in [
            ("/healthz", "host: archi.edge.example.com:8443\r\n"),
            ("/healthz", "host: archi.edge.example.com..\r\n"),
            ("/healthz", "host: other.example.org\r\n"),
            ("/e/ich:1", "host: tools.edge.example.com\r\n"),
            ("/healthz", "host: edge.example.com..\r\n"),
        ] {
            let answer = exchange(&door, &request(target, host)).await;
            assert_eq!(answer.status, 404, "{target} {host}");
            assert_eq!(answer.json()["error_code"], "NOT_FOUND");
            assert_security_headers(&answer);
        }

        // No certificate served (plaintext): the edge cannot tell its main name, and any other
        // host keeps the door's routes; a web host still never reaches them.
        let plaintext = labelled_door(web_routes_with(&[WEB_HOST]), Arc::new(EdgeTls::plaintext()));
        let other = exchange(
            &plaintext,
            &request("/healthz", "host: other.example.org\r\n"),
        )
        .await;
        assert_eq!(other.body, b"ok");
        let web = exchange(
            &plaintext,
            &request("/healthz", "host: archi.edge.example.com\r\n"),
        )
        .await;
        assert_eq!(web.status, 502);
        let web_elsewhere = exchange(
            &plaintext,
            &request("/healthz", "host: archi.edge.example.com:8443\r\n"),
        )
        .await;
        assert_eq!(web_elsewhere.status, 404);

        // Two Hosts, or a Host the target contradicts: 400.
        for (target, host) in [
            (
                "/healthz",
                "host: edge.example.com\r\nhost: archi.edge.example.com\r\n",
            ),
            (
                "https://archi.edge.example.com/healthz",
                "host: edge.example.com\r\n",
            ),
            ("/healthz", "host: archi.edge.example.com:x\r\n"),
        ] {
            let bad = exchange(&door, &request(target, host)).await;
            assert_eq!(bad.status, 400, "{target} {host}");
            assert_eq!(bad.json()["error_code"], "BAD_HOST");
            assert_security_headers(&bad);
        }
    }

    async fn push_web_routes(
        tls: Option<&EdgeTls>,
        table: &WebRouteTable,
        payload: Value,
    ) -> Value {
        let (response, reply) = tls_command_reply(
            tls,
            table,
            None,
            &admin_command(MSG_EDGE_WEB_ROUTES, payload),
        )
        .await
        .expect("a door command");
        assert_eq!(response, MSG_EDGE_WEB_ROUTES_RESPONSE);
        reply
    }

    fn routes_payload(routes: &[(&str, &str)]) -> Value {
        json!({
            "routes": routes
                .iter()
                .map(|(host, upstream)| json!({"host": host, "upstream": upstream}))
                .collect::<Vec<_>>(),
        })
    }

    #[tokio::test]
    async fn web_routes_replace_the_whole_set_and_survive_a_restart() {
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let path = std::env::temp_dir()
            .join(format!("sy-edge-web-{}", Uuid::new_v4()))
            .join("web_routes.json");
        let table = WebRouteTable::load(path.clone());

        let applied = push_web_routes(
            Some(&tls),
            &table,
            routes_payload(&[("Archi.Edge.Example.com.", "io.web")]),
        )
        .await;
        assert_eq!(applied["status"], "ok");
        assert_eq!(
            applied["routes"],
            json!([{"host": WEB_HOST, "upstream": "io.web"}])
        );
        assert_eq!(
            (&applied["changed"], &applied["persisted"]),
            (&json!(true), &json!(true))
        );
        assert!(table.serves(WEB_HOST));
        let again =
            push_web_routes(Some(&tls), &table, routes_payload(&[(WEB_HOST, "io.web")])).await;
        assert_eq!(
            again["changed"], false,
            "SY.admin's minute push of the same set"
        );

        // A restart serves the set at once.
        assert!(WebRouteTable::load(path.clone()).serves(WEB_HOST));

        // The set is replaced whole.
        let tools = "tools.edge.example.com";
        let replaced =
            push_web_routes(Some(&tls), &table, routes_payload(&[(tools, "io.web")])).await;
        assert_eq!(replaced["changed"], true);
        assert!(table.serves(tools) && !table.serves(WEB_HOST));

        // Refused, and nothing changes: another upstream, a host that is not a DNS name, a host
        // twice, an unknown field, the certificate's only name, another origin, no door.
        for payload in [
            routes_payload(&[(WEB_HOST, "io.api")]),
            routes_payload(&[(WEB_HOST, "127.0.0.1:3000")]),
            json!({"routes": [{"host": WEB_HOST}]}),
            routes_payload(&[("archi.edge.example.com:8443", "io.web")]),
            routes_payload(&[("*.edge.example.com", "io.web")]),
            routes_payload(&[("10.0.0.7", "io.web")]),
            routes_payload(&[(WEB_HOST, "io.web"), ("ARCHI.edge.example.com", "io.web")]),
            json!({"routes": [{"host": WEB_HOST, "upstream": "io.web", "port": 3000}]}),
            json!({}),
            routes_payload(&[("Edge.Example.com", "io.web")]),
            routes_payload(&[(EDGE, "io.web"), (WEB_HOST, "io.web")]),
        ] {
            let reply = push_web_routes(Some(&tls), &table, payload.clone()).await;
            assert_eq!(reply["status"], "error", "{payload}");
            assert!(table.serves(tools) && !table.serves(WEB_HOST), "{payload}");
        }
        let from_cloud = tls_command_reply(
            Some(&tls),
            &table,
            None,
            &edge_command(
                Some("IO.cloud@motherbee"),
                MSG_EDGE_WEB_ROUTES,
                routes_payload(&[]),
            ),
        )
        .await
        .expect("a door command")
        .1;
        assert_eq!(from_cloud["status"], "error");
        assert_eq!(
            push_web_routes(None, &table, routes_payload(&[])).await["status"],
            "error"
        );
        assert!(table.serves(tools));

        // With no certificate served the edge cannot tell its main name.
        let awaiting = vault_edge(&FakeSource::new(vec![refused(TlsVerdict::NotFound)]));
        awaiting.reload("boot").await.expect("a vault edge");
        let unknown = scratch_web_routes();
        assert_eq!(
            push_web_routes(
                Some(&awaiting),
                &unknown,
                routes_payload(&[(EDGE, "io.web")])
            )
            .await["status"],
            "ok"
        );

        // The empty set: no web host, on disk too.
        let cleared = push_web_routes(Some(&tls), &table, routes_payload(&[])).await;
        assert_eq!(cleared["routes"], json!([]));
        assert!(!table.serves(tools));
        assert_eq!(
            WebRouteTable::load(path.clone()).snapshot(),
            WebRoutes::default()
        );

        // A file that does not parse starts with no web host, and the next push rewrites it.
        std::fs::write(&path, b"{not json").expect("corrupt");
        let restarted = WebRouteTable::load(path.clone());
        assert_eq!(restarted.snapshot(), WebRoutes::default());
        let rewritten = push_web_routes(Some(&tls), &restarted, routes_payload(&[])).await;
        assert_eq!(
            (&rewritten["changed"], &rewritten["persisted"]),
            (&json!(false), &json!(true))
        );
        assert_eq!(
            serde_json::from_slice::<WebRoutes>(&std::fs::read(&path).expect("file")).ok(),
            Some(WebRoutes::default())
        );
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    #[test]
    fn the_edge_tells_its_main_name_from_its_certificate() {
        let names = |list: &[&str]| list.iter().map(|name| name.to_string()).collect::<Vec<_>>();
        assert_eq!(main_certified_name(&names(&[EDGE])), Some(EDGE));
        assert_eq!(main_certified_name(&names(&[WEB_HOST, EDGE])), Some(EDGE));
        assert_eq!(
            main_certified_name(&names(&["*.edge.example.com", EDGE])),
            Some(EDGE)
        );
        // Unrelated names, a name that only ends like another, a lone wildcard: it cannot tell.
        assert_eq!(
            main_certified_name(&names(&[EDGE, "other.example.org"])),
            None
        );
        assert_eq!(
            main_certified_name(&names(&[EDGE, "badedge.example.com"])),
            None
        );
        assert_eq!(main_certified_name(&names(&["*.edge.example.com"])), None);
        assert_eq!(main_certified_name(&[]), None);

        // The combined certificate of 0.1.84 (the main host and archi): the main name is still
        // refused as a web host, in any spelling.
        let set = |hosts: &[&str]| WebRoutes {
            routes: hosts
                .iter()
                .map(|host| WebRoute {
                    host: host.to_string(),
                    upstream: WebUpstream::IoWeb,
                })
                .collect(),
        };
        let combined = names(&[EDGE, WEB_HOST]);
        assert!(validate_web_routes(set(&[WEB_HOST]), &combined).is_ok());
        assert!(validate_web_routes(set(&[EDGE]), &combined).is_err());
        assert!(validate_web_routes(set(&["Edge.Example.com.:443"]), &combined).is_err());
        assert!(validate_web_routes(set(&[EDGE]), &names(&[EDGE, "other.example.org"])).is_ok());
    }

    /// The pace's arithmetic, on explicit instants.
    #[test]
    fn the_pace_drains_while_the_edge_waits_and_refills_with_bytes() {
        let limits = PaceLimits {
            idle: Duration::from_secs(10),
            min_rate: 100,
        };
        let start = Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        let mut pace = Pace::new(limits);
        assert_eq!(pace.wait(at(0)), Duration::from_secs(10));
        assert_eq!(pace.wait(at(4)), Duration::from_secs(6));
        // The wait ends with 100 bytes: one more second drained, one second earned.
        pace.moved(100, at(5));
        assert_eq!(pace.credit, Duration::from_secs(6));
        // Time the edge does not wait on the peer drains nothing.
        assert_eq!(pace.wait(at(60)), Duration::from_secs(6));
        // Bytes refill up to `idle`, never beyond.
        pace.moved(1_000_000, at(60));
        assert_eq!(pace.credit, Duration::from_secs(10));
        // A peer that moves 10 bytes a second (a tenth of the pace) loses 0.9 s a second. Each
        // second the edge waits, then checks the credit as the byte comes (its timer would fire
        // first when the credit runs out).
        let mut second = 100;
        loop {
            pace.wait(at(second));
            if pace.wait(at(second + 1)).is_zero() {
                break;
            }
            pace.moved(10, at(second + 1));
            second += 1;
        }
        assert_eq!(
            second, 110,
            "cut in its eleventh second: 10 s of credit, 0.9 s lost a second"
        );
        // One that moves 100 bytes a second never loses any.
        let mut steady = Pace::new(limits);
        for second in 0..100 {
            steady.wait(at(second));
            steady.moved(100, at(second + 1));
        }
        assert_eq!(steady.credit, Duration::from_secs(10));
    }

    /// A test state for the door's real routes, with one slot for `/e/` and one for `/public`.
    fn door_state(
        dispatcher: &Arc<RouterDispatcher>,
        public_root: PathBuf,
        timeout: Duration,
    ) -> Arc<FrontendState> {
        door_state_with(dispatcher, public_root, timeout, 1, 0, INLINE_BODY_TIMEOUT)
    }

    /// `door_state` with `downloads` download slots, `per_ip` of them for one address, and
    /// `body_timeout` for an `/e/` body.
    fn door_state_with(
        dispatcher: &Arc<RouterDispatcher>,
        public_root: PathBuf,
        timeout: Duration,
        downloads: usize,
        per_ip: usize,
        body_timeout: Duration,
    ) -> Arc<FrontendState> {
        Arc::new(FrontendState {
            dispatcher: Arc::clone(dispatcher),
            sender: dispatcher.sender_snapshot(),
            registry: Arc::new(RwLock::new(HashMap::new())),
            public_registry: Arc::new(RwLock::new(HashMap::new())),
            blob_public_root: public_root,
            self_ilk: "ilk:00000000-0000-0000-0000-000000000000".to_string(),
            ttl: 16,
            timeout,
            inflight: Arc::new(Semaphore::new(1)),
            public_inflight: Arc::new(Semaphore::new(downloads)),
            public_peer_slots: PeerSlots::new(per_ip),
            refused_downloads: std::sync::Mutex::new(RefusedConnections::default()),
            body_timeout,
        })
    }

    /// Open the public channel `ich` to `IO.test@motherbee`.
    fn open_test_channel(state: &FrontendState, ich: &str) {
        let (ich, entry) = row_to_entry(
            serde_json::from_value(json!({
                "ich": ich,
                "owner_l2_name": "IO.test@motherbee",
                "inbound_family": "user",
                "auth_mode": "public",
            }))
            .expect("row"),
        );
        state.registry.write().expect("registry").insert(ich, entry);
    }

    /// The door's real routes, served in plaintext: every host but a web host reaches them.
    fn real_door(state: &Arc<FrontendState>) -> Door {
        Door {
            main: main_routes(Arc::clone(state)),
            web: Arc::new(scratch_web_routes()),
            proxy: no_proxy(),
            tls: Arc::new(EdgeTls::plaintext()),
        }
    }

    /// `bytes` published as a download under `sample_key`, in a fresh public root.
    fn published_download(bytes: &[u8]) -> (PathBuf, PublicArtifactRow) {
        let root = std::env::temp_dir().join(format!("sy-edge-public-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("public root");
        let row = PublicArtifactRow {
            content_type: "application/octet-stream".to_string(),
            presentation: "attachment".to_string(),
            content_policy: "download-v1".to_string(),
            ..sample_public_row(bytes)
        };
        std::fs::write(root.join(&row.public_name), bytes).expect("artifact");
        (root, row)
    }

    async fn ended_within(server: tokio::task::JoinHandle<()>, limit: Duration) -> Duration {
        let started = Instant::now();
        tokio::time::timeout(limit, server)
            .await
            .expect("the connection ended in time")
            .expect("server task");
        started.elapsed()
    }

    /// A-86: a reader that stops reading a download loses it, and its download slot, after the
    /// pace's idle; a reader that keeps up gets every byte.
    #[tokio::test]
    async fn a_stalled_reader_loses_its_download_slot() {
        let (dispatcher, _router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let data: Vec<u8> = (0..1_048_576_u32).map(|i| (i % 251) as u8).collect();
        let (root, row) = published_download(&data);
        let state = door_state(&dispatcher, root.clone(), Duration::from_secs(5));
        state
            .public_registry
            .write()
            .expect("public registry")
            .insert(row.key_sha256.clone(), row);
        let request = format!(
            "GET /public/{} HTTP/1.1\r\nhost: edge.example.com\r\nconnection: close\r\n\r\n",
            sample_key()
        );
        let idle = Duration::from_millis(300);

        let (mut stalled, server) = pipe_to(real_door(&state), quick_limits(idle), 8 * 1024);
        tokio::io::AsyncWriteExt::write_all(&mut stalled, request.as_bytes())
            .await
            .expect("request");
        // It never reads.
        let ended = ended_within(server, Duration::from_secs(5)).await;
        assert!(ended >= idle.mul_f32(0.9), "{ended:?}");
        assert_eq!(
            state.public_inflight.available_permits(),
            1,
            "the slot is back"
        );

        let (mut reader, _server) = pipe_to(real_door(&state), quick_limits(idle), 8 * 1024);
        tokio::io::AsyncWriteExt::write_all(&mut reader, request.as_bytes())
            .await
            .expect("request");
        let download = read_answer(&mut reader).await;
        assert_eq!(download.status, 200);
        assert!(download.body == data, "every byte, in order");
        drop(stalled);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A-86: a body that trickles in slower than the pace ends with its connection, and its
    /// `/e/` slot comes back; nothing reaches the mesh.
    #[tokio::test]
    async fn a_slow_body_writer_is_cut_and_its_slot_released() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let state = door_state(&dispatcher, std::env::temp_dir(), Duration::from_secs(5));
        open_test_channel(&state, "ich:slow");
        let idle = Duration::from_millis(300);
        let (client, server) = pipe_to(real_door(&state), quick_limits(idle), 64 * 1024);
        let (mut read_half, mut write_half) = tokio::io::split(client);
        tokio::io::AsyncWriteExt::write_all(
            &mut write_half,
            b"POST /e/ich:slow HTTP/1.1\r\nhost: edge.example.com\r\ncontent-type: text/plain\r\ncontent-length: 100\r\nconnection: close\r\n\r\n0123456789",
        )
        .await
        .expect("head");
        // Then one byte every 100 ms: 10 bytes a second against a pace of 1024.
        let trickle = tokio::spawn(async move {
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(100)).await;
                if tokio::io::AsyncWriteExt::write_all(&mut write_half, b"x")
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let started = Instant::now();
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), read_half.read_to_end(&mut raw))
            .await
            .expect("an answer in time")
            .expect("read");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let text = String::from_utf8_lossy(&raw);
        assert!(text.starts_with("HTTP/1.1 408 "), "{text}");
        assert!(text.contains("\"error_code\":\"BODY_TIMEOUT\""), "{text}");
        ended_within(server, Duration::from_secs(5)).await;
        assert_eq!(
            state.inflight.available_permits(),
            1,
            "the /e/ slot is back"
        );
        assert!(
            router
                .next_outgoing_within(Duration::from_millis(50))
                .await
                .is_none(),
            "nothing was forwarded"
        );
        trickle.abort();
    }

    /// A-86: the edge's own wait (here its channel's handler, slower than the pace's idle) is
    /// never charged to the peer.
    #[tokio::test]
    async fn the_edge_s_own_wait_is_never_charged_to_the_peer() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let state = door_state(&dispatcher, std::env::temp_dir(), Duration::from_secs(5));
        open_test_channel(&state, "ich:t");
        let idle = Duration::from_millis(200);
        let (mut client, _server) = pipe_to(real_door(&state), quick_limits(idle), 64 * 1024);
        tokio::io::AsyncWriteExt::write_all(
            &mut client,
            b"POST /e/ich:t HTTP/1.1\r\nhost: edge.example.com\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"q\":\"hey\"}",
        )
        .await
        .expect("request");
        let forwarded = router
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("the request reaches the mesh");
        assert_eq!(forwarded.payload, json!({"q": "hey"}));
        tokio::time::sleep(idle * 3).await;
        router
            .inject(Message {
                routing: Routing {
                    src: "io-test-uuid".to_string(),
                    src_l2_name: Some("IO.test@motherbee".to_string()),
                    dst: Destination::Unicast(forwarded.routing.src.clone()),
                    ttl: 16,
                    trace_id: forwarded.routing.trace_id.clone(),
                },
                meta: Meta {
                    msg_type: "user".to_string(),
                    ..Meta::default()
                },
                payload: json!({"answer": 42}),
            })
            .await
            .expect("reply");
        let answer = read_answer(&mut client).await;
        assert_eq!(answer.status, 200);
        assert_eq!(answer.json(), json!({"answer": 42}));
    }

    /// A-88: an `/e/` error answers the fixed body of its code; the trace, the target and the
    /// message family stay in the edge's log.
    #[tokio::test]
    async fn an_e_error_answers_a_fixed_body() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let state = door_state(
            &dispatcher,
            std::env::temp_dir(),
            Duration::from_millis(200),
        );
        open_test_channel(&state, "ich:t");
        let door = real_door(&state);
        let timed_out = tokio::spawn({
            let door = door.clone();
            async move {
                exchange(
                    &door,
                    "POST /e/ich:t HTTP/1.1\r\nhost: edge.example.com\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await
            }
        });
        let forwarded = router
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("the request reaches the mesh");
        let answer = timed_out.await.expect("exchange");
        assert_eq!(answer.status, 504);
        assert_eq!(
            answer.json(),
            json!({"ok": false, "error_code": "HANDLER_TIMEOUT", "message": "the channel did not answer in time"})
        );
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert!(!body.contains(&forwarded.routing.trace_id), "{body}");
        assert!(!body.contains("IO.test"), "{body}");
        assert_security_headers(&answer);

        // A body that fits the inline limit but not the mesh envelope once escaped.
        let quotes = "\"".repeat(40_000);
        let too_large = exchange(
            &door,
            &format!(
                "POST /e/ich:t HTTP/1.1\r\nhost: edge.example.com\r\ncontent-type: text/plain\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{quotes}",
                quotes.len()
            ),
        )
        .await;
        assert_eq!(too_large.status, 413);
        assert_eq!(
            too_large.json(),
            json!({"ok": false, "error_code": "REQ_TOO_LARGE", "message": "request too large"})
        );

        // Every relay error has a fixed message that names nothing internal.
        let timeout = RpcError::Timeout {
            trace_id: "trace-7".to_string(),
            target: "IO.test@motherbee".to_string(),
            request_msg: "user".to_string(),
            response_msg: "*".to_string(),
            timeout_ms: 200,
        };
        for err in [timeout, RpcError::Disconnected] {
            let message = rpc_error_message(&err);
            assert!(!message.contains("trace-7") && !message.contains("IO.test"));
        }
    }

    /// A-83: a response the edge cannot build is a 500 with the security headers, never an empty
    /// 200.
    #[tokio::test]
    async fn an_unbuildable_response_is_a_500_with_the_security_headers() {
        let response = built(
            Response::builder()
                .header("x-broken", "one\ntwo")
                .body(Body::empty()),
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        for (name, value) in SECURITY_HEADERS {
            assert_eq!(
                response.headers().get(name).and_then(|v| v.to_str().ok()),
                Some(value),
                "{name}"
            );
        }
        let body = to_bytes(response.into_body(), 1024).await.expect("body");
        assert_eq!(
            serde_json::from_slice::<Value>(&body).expect("json")["error_code"],
            "INTERNAL_ERROR"
        );
    }

    /// A-82: a `publications.json` written before 0.1.84 holds the keys themselves. The load keys
    /// the table by their SHA-256, rewrites the file without them, and the next load converts
    /// nothing.
    #[test]
    fn publications_json_is_converted_once_to_key_hashes() {
        let root = std::env::temp_dir().join(format!("sy-edge-a82-{}", Uuid::new_v4()));
        let path = root.join("publications.json");
        std::fs::create_dir_all(&root).expect("root");
        let live_key = "c1".repeat(32);
        let expired_key = "e0".repeat(32);
        let row = sample_public_row(b"<!doctype html>a82");
        let stored = |key: &str, expires_at: u64| {
            let mut value = serde_json::to_value(&row).expect("row");
            let fields = value.as_object_mut().expect("object");
            fields.remove("key_sha256");
            fields.insert("key".to_string(), json!(key));
            fields.insert(
                "publication_id".to_string(),
                json!(format!("pub:{}", Uuid::new_v4())),
            );
            fields.insert("expires_at".to_string(), json!(expires_at));
            value
        };
        let old = json!({"publications": [stored(&live_key, u64::MAX), stored(&expired_key, 1)]});
        std::fs::write(&path, serde_json::to_vec_pretty(&old).expect("json")).expect("old file");

        let loaded = load_public_registry(&path);
        assert_eq!(loaded.len(), 1);
        let live = loaded
            .get(&sha256_hex(&live_key))
            .expect("keyed by the hash");
        assert_eq!(live.key_sha256, sha256_hex(&live_key));
        let on_disk = std::fs::read_to_string(&path).expect("rewritten");
        assert!(
            !on_disk.contains(&live_key) && !on_disk.contains(&expired_key),
            "{on_disk}"
        );
        assert!(!on_disk.contains("\"key\""), "{on_disk}");
        assert!(on_disk.contains(&sha256_hex(&live_key)));

        assert_eq!(
            load_public_registry(&path),
            loaded,
            "the second load converts nothing"
        );
        assert_eq!(std::fs::read_to_string(&path).expect("file"), on_disk);
        let _ = std::fs::remove_dir_all(root);
    }

    /// A-82: `EDGE_PUBLISH_BLOB` still carries the key; the edge keeps its hash, serves the key,
    /// and the hash it keeps is no capability.
    #[tokio::test]
    async fn a_public_artifact_is_kept_and_served_by_the_hash_of_its_key() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let data = b"<!doctype html><p>a82</p>".to_vec();
        let (root, sample) = published_download(&data);
        let registry_path = root.join("state/publications.json");
        let state = door_state(&dispatcher, root.clone(), Duration::from_secs(5));
        let key = "d7".repeat(32);
        let mut payload = serde_json::to_value(&sample).expect("row");
        let fields = payload.as_object_mut().expect("object");
        fields.remove("key_sha256");
        fields.insert("key".to_string(), json!(key));
        apply_publish_blob(
            &dispatcher.sender_snapshot(),
            &state.public_registry,
            &registry_path,
            &root,
            &admin_command(MSG_EDGE_PUBLISH_BLOB, payload),
            16,
            Duration::from_secs(2),
        )
        .await;
        let reply = router
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("a reply");
        assert_eq!(reply.payload["status"], "ok", "{}", reply.payload);
        assert_eq!(reply.payload["url"], format!("/public/{key}"));
        assert!(state
            .public_registry
            .read()
            .expect("registry")
            .contains_key(&sha256_hex(&key)));
        let on_disk = std::fs::read_to_string(&registry_path).expect("publications.json");
        assert!(!on_disk.contains(&key) && on_disk.contains(&sha256_hex(&key)));

        let door = real_door(&state);
        let get = |key: String| {
            format!(
                "GET /public/{key} HTTP/1.1\r\nhost: edge.example.com\r\nconnection: close\r\n\r\n"
            )
        };
        let served = exchange(&door, &get(key.clone())).await;
        assert_eq!((served.status, served.body.clone()), (200, data));
        assert_eq!(exchange(&door, &get(sha256_hex(&key))).await.status, 404);
        let _ = std::fs::remove_dir_all(root);
    }

    // -----------------------------------------------------------------------
    // The 0.1.84 review's fixes (E1-E7)
    // -----------------------------------------------------------------------

    /// Read one answer from the read half of a split pipe, to its end.
    async fn read_split_answer(
        stream: &mut tokio::io::ReadHalf<tokio::io::DuplexStream>,
    ) -> String {
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
            .await
            .expect("an answer in time")
            .expect("read");
        String::from_utf8_lossy(&raw).to_string()
    }

    /// E1: hyper reads a body's first bytes before the handler waits for them. A head whose body
    /// never comes is still paced: the connection ends, and its `/e/` slot comes back.
    #[tokio::test]
    async fn a_body_that_never_starts_is_cut_and_its_slot_released() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let state = door_state(&dispatcher, std::env::temp_dir(), Duration::from_secs(5));
        open_test_channel(&state, "ich:silent");
        let idle = Duration::from_millis(300);
        for head in [
            "POST /e/ich:silent HTTP/1.1\r\nhost: edge.example.com\r\ncontent-length: 10\r\n\r\n",
            "POST /e/ich:silent HTTP/1.1\r\nhost: edge.example.com\r\ntransfer-encoding: chunked\r\n\r\n",
        ] {
            let (client, server) = pipe_to(real_door(&state), quick_limits(idle), 64 * 1024);
            let (mut read_half, mut write_half) = tokio::io::split(client);
            tokio::io::AsyncWriteExt::write_all(&mut write_half, head.as_bytes())
                .await
                .expect("head");
            let started = Instant::now();
            let answer = read_split_answer(&mut read_half).await;
            assert!(started.elapsed() < idle * 5, "{:?}", started.elapsed());
            assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
            assert!(answer.contains("\"error_code\":\"BODY_TIMEOUT\""), "{answer}");
            ended_within(server, Duration::from_secs(5)).await;
            assert_eq!(state.inflight.available_permits(), 1, "the /e/ slot is back");
            drop(write_half);
        }
        assert!(
            router
                .next_outgoing_within(Duration::from_millis(50))
                .await
                .is_none(),
            "nothing was forwarded"
        );
    }

    /// E1: a chunked body that keeps the pace but never ends is cut at `body_timeout`, so it
    /// cannot hold an `/e/` slot for minutes.
    #[tokio::test]
    async fn a_body_must_be_whole_within_its_deadline() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let body_timeout = Duration::from_millis(600);
        let state = door_state_with(
            &dispatcher,
            std::env::temp_dir(),
            Duration::from_secs(5),
            1,
            0,
            body_timeout,
        );
        open_test_channel(&state, "ich:chunked");
        // A pace the trickle keeps: 200 bytes every 50 ms is 4 KB/s.
        let (client, server) = pipe_to(
            real_door(&state),
            quick_limits(Duration::from_secs(5)),
            64 * 1024,
        );
        let (mut read_half, mut write_half) = tokio::io::split(client);
        tokio::io::AsyncWriteExt::write_all(
            &mut write_half,
            b"POST /e/ich:chunked HTTP/1.1\r\nhost: edge.example.com\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .expect("head");
        let started = Instant::now();
        let trickle = tokio::spawn(async move {
            let chunk = format!("c8\r\n{}\r\n", "x".repeat(200));
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if tokio::io::AsyncWriteExt::write_all(&mut write_half, chunk.as_bytes())
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });
        let answer = read_split_answer(&mut read_half).await;
        let elapsed = started.elapsed();
        assert!(answer.starts_with("HTTP/1.1 408 "), "{answer}");
        assert!(elapsed >= body_timeout.mul_f32(0.9), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
        ended_within(server, Duration::from_secs(5)).await;
        assert_eq!(
            state.inflight.available_permits(),
            1,
            "the /e/ slot is back"
        );
        assert!(router
            .next_outgoing_within(Duration::from_millis(50))
            .await
            .is_none());
        trickle.abort();
    }

    #[test]
    fn peer_slots_count_an_address_its_ipv6_64_and_give_slots_back() {
        let ip = |text: &str| text.parse::<std::net::IpAddr>().expect("ip");
        let slots = PeerSlots::new(2);
        let first = slots.try_take(ip("198.51.100.4")).expect("first");
        let _second = slots
            .try_take(ip("::ffff:198.51.100.4"))
            .expect("the same IPv4");
        assert!(slots.try_take(ip("198.51.100.4")).is_none(), "at the cap");
        assert!(
            slots.try_take(ip("198.51.100.5")).is_some(),
            "another address"
        );
        drop(first);
        assert_eq!(slots.held_by(ip("198.51.100.4")), 1);
        assert!(
            slots.try_take(ip("198.51.100.4")).is_some(),
            "the slot came back"
        );

        // One IPv6 subscriber holds a /64: its addresses count together.
        let _a = slots.try_take(ip("2001:db8:1:2::1")).expect("a");
        let _b = slots.try_take(ip("2001:db8:1:2:ffff::9")).expect("b");
        assert!(slots.try_take(ip("2001:db8:1:2::77")).is_none());
        assert!(
            slots.try_take(ip("2001:db8:1:3::1")).is_some(),
            "another /64"
        );
        assert_eq!(slots.held_by(ip("2001:db8:1:2::")), 2);

        // 0: no cap, and nothing counted.
        let open = PeerSlots::new(0);
        let held: Vec<_> = (0..1000)
            .map(|_| open.try_take(ip("198.51.100.4")).expect("no cap"))
            .collect();
        assert_eq!(held.len(), 1000);
        assert_eq!(open.held_by(ip("198.51.100.4")), 0);
    }

    /// E2: one address holds at most its share of the door's connections, counted at accept,
    /// before the handshake.
    #[tokio::test]
    async fn one_address_holds_at_most_its_share_of_the_connections() {
        let tls = serving_edge(self_signed(&[EDGE])).await;
        let limits = DoorLimits::new(16, 2, HTTP_HEADER_READ_TIMEOUT, DOOR_PACE);
        let per_ip = Arc::clone(&limits.peer_connections);
        let addr = loopback_door(&tls, limits).await;
        let mut first = connect_tls(addr).await;
        let mut second = connect_tls(addr).await;
        assert_eq!(get_healthz(&mut first).await, "ok");
        assert_eq!(get_healthz(&mut second).await, "ok");
        assert!(
            try_connect_tls(addr).await.is_err(),
            "a third connection from the same address is closed before its handshake"
        );
        assert_eq!(per_ip.held_by("127.0.0.1".parse().expect("ip")), 2);
        drop(first);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(mut third) = try_connect_tls(addr).await {
                assert_eq!(get_healthz(&mut third).await, "ok");
                break;
            }
            assert!(Instant::now() < deadline, "the slot came back");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(get_healthz(&mut second).await, "ok");
    }

    /// E2: one address holds at most `public_max_inflight_per_ip` downloads; another still gets
    /// a slot.
    #[tokio::test]
    async fn one_address_holds_at_most_its_share_of_the_downloads() {
        let (dispatcher, _router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let data: Vec<u8> = (0..1_048_576_u32).map(|i| (i % 251) as u8).collect();
        let (root, row) = published_download(&data);
        let state = door_state_with(
            &dispatcher,
            root.clone(),
            Duration::from_secs(5),
            4,
            1,
            INLINE_BODY_TIMEOUT,
        );
        state
            .public_registry
            .write()
            .expect("public registry")
            .insert(row.key_sha256.clone(), row);
        let request = format!(
            "GET /public/{} HTTP/1.1\r\nhost: edge.example.com\r\nconnection: close\r\n\r\n",
            sample_key()
        );
        let limits = quick_limits(Duration::from_secs(5));

        // A download that does not read holds the address's only slot.
        let (mut holding, server) = pipe_to(real_door(&state), limits.clone(), 8 * 1024);
        tokio::io::AsyncWriteExt::write_all(&mut holding, request.as_bytes())
            .await
            .expect("request");
        let deadline = Instant::now() + Duration::from_secs(5);
        while state
            .public_peer_slots
            .held_by(PEER.parse::<SocketAddr>().expect("peer").ip())
            == 0
        {
            assert!(Instant::now() < deadline, "the first download started");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let refused = exchange(&real_door(&state), &request).await;
        assert_eq!(refused.status, 429);
        assert_eq!(refused.json()["error_code"], "PUBLIC_PEER_BUSY");
        assert_security_headers(&refused);

        let (mut other, _other_server) =
            pipe_from("198.51.100.9:40000", real_door(&state), limits, 64 * 1024);
        tokio::io::AsyncWriteExt::write_all(&mut other, request.as_bytes())
            .await
            .expect("request");
        let served = read_answer(&mut other).await;
        assert_eq!(served.status, 200, "another address still downloads");
        assert!(served.body == data);

        drop(holding);
        ended_within(server, Duration::from_secs(5)).await;
        assert_eq!(
            state
                .public_peer_slots
                .held_by(PEER.parse::<SocketAddr>().expect("peer").ip()),
            0
        );
        assert_eq!(state.public_inflight.available_permits(), 4);
        let _ = std::fs::remove_dir_all(root);
    }

    /// E3, over a real TCP pair (and the door's accept loop): a reader that stops reading loses
    /// its download, and its slot, after the pace's idle.
    #[tokio::test]
    async fn a_stalled_reader_over_tcp_loses_its_download() {
        let (dispatcher, _router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        // More than the kernel's buffers hold on either side (Linux and macOS grow a send buffer
        // to 4 MiB at most; a receive buffer grows only as its reader reads).
        let data: Vec<u8> = (0..8 * 1_048_576_u32).map(|i| (i % 251) as u8).collect();
        let (root, row) = published_download(&data);
        let state = door_state(&dispatcher, root.clone(), Duration::from_secs(5));
        state
            .public_registry
            .write()
            .expect("public registry")
            .insert(row.key_sha256.clone(), row);
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let idle = Duration::from_millis(500);
        tokio::spawn(serve_door(
            listener,
            real_door(&state),
            Arc::new(EdgeTls::plaintext()),
            quick_limits(idle),
        ));
        let mut stalled = tokio::net::TcpStream::connect(addr).await.expect("connect");
        tokio::io::AsyncWriteExt::write_all(
            &mut stalled,
            format!(
                "GET /public/{} HTTP/1.1\r\nhost: edge.example.com\r\n\r\n",
                sample_key()
            )
            .as_bytes(),
        )
        .await
        .expect("request");
        // It never reads: the slot comes back once the pace runs out.
        let started = Instant::now();
        while state.public_inflight.available_permits() == 1 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "the download started"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        while state.public_inflight.available_permits() == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the stalled download kept its slot"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            started.elapsed() >= idle.mul_f32(0.9),
            "{:?}",
            started.elapsed()
        );
        drop(stalled);
        let _ = std::fs::remove_dir_all(root);
    }

    /// E3: the door's sockets report themselves writable by what was delivered.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[tokio::test]
    async fn accepted_sockets_track_delivery() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _client = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let (accepted, _) = listener.accept().await.expect("accept");
        track_delivery(&accepted);
        assert_eq!(
            socket2::SockRef::from(&accepted)
                .tcp_notsent_lowat()
                .expect("TCP_NOTSENT_LOWAT"),
            16 * 1024
        );
    }

    /// E4: a web route file read before the certificate cannot take the main name: the route is
    /// dropped once a certificate is served, and the main name keeps the door's routes anyway.
    #[test]
    fn a_web_route_file_never_takes_the_main_name() {
        let path = std::env::temp_dir()
            .join(format!("sy-edge-web-{}", Uuid::new_v4()))
            .join("web_routes.json");
        let both = json!({"routes": [
            {"host": EDGE, "upstream": "io.web"},
            {"host": WEB_HOST, "upstream": "io.web"},
        ]});
        std::fs::create_dir_all(path.parent().expect("dir")).expect("dir");
        std::fs::write(&path, serde_json::to_vec(&both).expect("json")).expect("file");
        let table = WebRouteTable::load(path.clone());
        assert!(
            table.serves(EDGE),
            "nothing tells the main name before a certificate"
        );
        let certified = [EDGE.to_string(), WEB_HOST.to_string()];
        let main = RequestHost {
            name: EDGE.to_string(),
            port: None,
        };
        assert_eq!(
            host_route(Some(&main), &table, Some(certified.as_slice())),
            HostRoute::Door,
            "the main name is never a web host"
        );

        table.forget_main_name(&certified);
        assert!(!table.serves(EDGE) && table.serves(WEB_HOST));
        // The next push writes the file again, even with the same set.
        let applied = table.replace(
            validate_web_routes(
                serde_json::from_value(
                    json!({"routes": [{"host": WEB_HOST, "upstream": "io.web"}]}),
                )
                .expect("routes"),
                &certified,
            )
            .expect("valid"),
        );
        assert_eq!(
            applied,
            WebRoutesApplied {
                changed: false,
                persisted: true
            }
        );
        assert!(!WebRouteTable::load(path.clone()).serves(EDGE));
        let _ = std::fs::remove_dir_all(path.parent().expect("dir"));
    }

    /// E5: hyper's buffer is 64 KiB: a head over it is refused.
    #[tokio::test]
    async fn a_head_over_64_kib_is_refused() {
        let door = labelled_door(scratch_web_routes(), Arc::new(EdgeTls::plaintext()));
        let (client, server) = pipe_to(door, quick_limits(Duration::from_secs(5)), 16 * 1024);
        let (mut read_half, mut write_half) = tokio::io::split(client);
        let writer = tokio::spawn(async move {
            let head = format!(
                "GET /healthz HTTP/1.1\r\nhost: edge.example.com\r\nx-padding: {}\r\n\r\n",
                "p".repeat(70 * 1024)
            );
            let _ = tokio::io::AsyncWriteExt::write_all(&mut write_half, head.as_bytes()).await;
        });
        let answer = read_split_answer(&mut read_half).await;
        assert!(answer.starts_with("HTTP/1.1 431 "), "{answer}");
        ended_within(server, Duration::from_secs(5)).await;
        writer.abort();
    }

    /// E6: the reaper writes the table it holds the lock of.
    #[test]
    fn the_reaper_drops_expired_rows_and_writes_the_table() {
        let root = std::env::temp_dir().join(format!("sy-edge-reaper-{}", Uuid::new_v4()));
        let path = root.join("publications.json");
        let live = sample_public_row(b"live");
        let expired = PublicArtifactRow {
            key_sha256: sha256_hex(&"e1".repeat(32)),
            publication_id: format!("pub:{}", Uuid::new_v4()),
            expires_at: 1_000,
            ..sample_public_row(b"expired")
        };
        let registry = RwLock::new(HashMap::from([
            (live.key_sha256.clone(), live.clone()),
            (expired.key_sha256.clone(), expired),
        ]));
        assert_eq!(reap_public_registry(&registry, &path, 2_000), 1);
        assert_eq!(
            load_public_registry(&path),
            HashMap::from([(live.key_sha256.clone(), live)])
        );
        let written = std::fs::read(&path).expect("file");
        assert_eq!(reap_public_registry(&registry, &path, 2_000), 0);
        assert_eq!(
            std::fs::read(&path).expect("file"),
            written,
            "nothing to write"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    /// E7: every answer of the door carries the security headers; a published artifact keeps the
    /// content policy it chose.
    #[tokio::test]
    async fn every_answer_of_the_door_carries_the_security_headers() {
        let (dispatcher, mut router) = fluxbee_sdk::RouterDispatcherTestHarness::new(
            "SY.edge@ingress1",
            build_sy_edge_rpc_profile().expect("profile"),
        );
        let page = b"<!doctype html><p>e7</p>".to_vec();
        let root = std::env::temp_dir().join(format!("sy-edge-e7-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("root");
        let html = sample_public_row(&page);
        std::fs::write(root.join(&html.public_name), &page).expect("page");
        let download_key = "dd".repeat(32);
        let download = PublicArtifactRow {
            key_sha256: sha256_hex(&download_key),
            publication_id: format!("pub:{}", Uuid::new_v4()),
            content_type: "application/octet-stream".to_string(),
            presentation: "attachment".to_string(),
            content_policy: "download-v1".to_string(),
            ..sample_public_row(&page)
        };
        let state = door_state_with(
            &dispatcher,
            root.clone(),
            Duration::from_secs(5),
            4,
            0,
            INLINE_BODY_TIMEOUT,
        );
        {
            let mut public = state.public_registry.write().expect("public registry");
            public.insert(html.key_sha256.clone(), html.clone());
            public.insert(download.key_sha256.clone(), download);
        }
        open_test_channel(&state, "ich:t");
        let (ich, mut fanout) = row_to_entry(
            serde_json::from_value(json!({
                "ich": "ich:fan",
                "owner_l2_name": "",
                "inbound_family": "user",
                "auth_mode": "public",
                "fanout_family": "IO.wapp.*@motherbee",
            }))
            .expect("row"),
        );
        fanout.verify_token = Some("tok".to_string());
        state
            .registry
            .write()
            .expect("registry")
            .insert(ich, fanout);
        let door = real_door(&state);
        let get = |target: &str, extra: &str| {
            format!("GET {target} HTTP/1.1\r\nhost: edge.example.com\r\n{extra}connection: close\r\n\r\n")
        };
        let csp = |answer: &Answer| answer.header("content-security-policy");
        let door_csp = Some(SECURITY_HEADERS[4].1.to_string());

        // The door's own answers: every header.
        let page_key = sample_key();
        let mut answers = vec![
            ("/healthz", 200, exchange(&door, &get("/healthz", "")).await),
            ("unknown path", 404, exchange(&door, &get("/nope", "")).await),
            ("bad path", 400, exchange(&door, &get("/public/%FF", "")).await),
            (
                "unknown channel",
                404,
                exchange(&door, &get("/e/ich:none", "")).await,
            ),
            (
                "fanout challenge",
                200,
                exchange(
                    &door,
                    &get(
                        "/e/ich:fan?hub.mode=subscribe&hub.verify_token=tok&hub.challenge=7",
                        "",
                    ),
                )
                .await,
            ),
            (
                "POST /public",
                405,
                exchange(
                    &door,
                    &format!(
                        "POST /public/{page_key} HTTP/1.1\r\nhost: edge.example.com\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                    ),
                )
                .await,
            ),
            (
                "bad range",
                416,
                exchange(&door, &get(&format!("/public/{page_key}"), "range: bytes=999999-\r\n"))
                    .await,
            ),
            (
                "unknown artifact",
                404,
                exchange(&door, &get(&format!("/public/{}", "0".repeat(64)), "")).await,
            ),
        ];
        let relayed = tokio::spawn({
            let door = door.clone();
            async move {
                exchange(
                    &door,
                    "POST /e/ich:t HTTP/1.1\r\nhost: edge.example.com\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}",
                )
                .await
            }
        });
        let forwarded = router
            .next_outgoing_within(Duration::from_secs(5))
            .await
            .expect("the request reaches the mesh");
        router
            .inject(Message {
                routing: Routing {
                    src: "io-test-uuid".to_string(),
                    src_l2_name: Some("IO.test@motherbee".to_string()),
                    dst: Destination::Unicast(forwarded.routing.src.clone()),
                    ttl: 16,
                    trace_id: forwarded.routing.trace_id.clone(),
                },
                meta: Meta {
                    msg_type: "user".to_string(),
                    ..Meta::default()
                },
                payload: json!({"ok": true}),
            })
            .await
            .expect("reply");
        answers.push(("relayed answer", 200, relayed.await.expect("exchange")));
        for (case, status, answer) in &answers {
            assert_eq!(answer.status, *status, "{case}");
            assert_security_headers(answer);
        }
        assert_eq!(answers[4].2.body, b"7", "the challenge is still echoed");

        // A published artifact: the same headers, and the content policy it chose.
        let served_page = exchange(&door, &get(&format!("/public/{page_key}"), "")).await;
        assert_eq!(served_page.status, 200);
        assert!(csp(&served_page).is_some_and(|policy| policy.starts_with("sandbox allow-scripts")));
        let served_download = exchange(&door, &get(&format!("/public/{download_key}"), "")).await;
        assert_eq!(served_download.status, 200);
        assert_eq!(
            csp(&served_download),
            None,
            "a download keeps no policy of the door's"
        );
        let not_modified = exchange(
            &door,
            &get(
                &format!("/public/{page_key}"),
                &format!("if-none-match: \"{}\"\r\n", html.sha256),
            ),
        )
        .await;
        assert_eq!(not_modified.status, 304);
        assert_eq!(csp(&not_modified), None);
        for answer in [&served_page, &served_download, &not_modified] {
            for (name, value) in SECURITY_HEADERS.iter().take(4) {
                assert_eq!(answer.header(name).as_deref(), Some(*value), "{name}");
            }
        }
        assert_ne!(csp(&served_page), door_csp);
        let _ = std::fs::remove_dir_all(root);
    }

    // -----------------------------------------------------------------------
    // The proxy to io.web (docs/io-web-v2.md §11.2)
    // -----------------------------------------------------------------------

    /// io.web's page in these tests, and the content policy it sets on it.
    const IO_WEB_PAGE: &str = "<!doctype html><p>io.web</p>";
    const IO_WEB_CSP: &str = "default-src 'self'; frame-ancestors 'none'";

    /// `hive`'s mesh material: a leaf `ca` issues, with `ca`'s certificate as the roots.
    fn mesh_material(ca: &MeshCa, hive: &str) -> MeshTlsMaterial {
        let leaf = ca.issue_leaf(hive).expect("leaf");
        MeshTlsMaterial::from_pem(&leaf.cert_pem, &leaf.key_pem, ca.ca_cert_pem())
            .expect("material")
    }

    /// A hive registry in a fresh directory, as SY.orchestrator writes it: `ingress1` a connected
    /// ingress, `worker1` a connected worker.
    fn mesh_registry() -> HiveRegistry {
        let root = std::env::temp_dir().join(format!("sy-edge-hives-{}", Uuid::new_v4()));
        for (hive, role) in [("ingress1", "ingress"), ("worker1", "worker")] {
            std::fs::create_dir_all(root.join(hive)).expect("hive dir");
            std::fs::write(
                root.join(hive).join("info.yaml"),
                format!("hive_id: {hive}\nstatus: connected\nrole: {role}\njoin:\n  phase: done\n"),
            )
            .expect("info.yaml");
        }
        HiveRegistry::new(root)
    }

    /// io.web's TLS in these tests, as io.web's own: `leaf`'s certificate, and a client admitted
    /// only when the registry calls its hive a connected ingress.
    fn io_web_tls(ca: &MeshCa, leaf: &str, registry: &HiveRegistry) -> Arc<rustls::ServerConfig> {
        mesh_material(ca, leaf)
            .server_config_for_roles(registry.clone(), &[HiveRole::Ingress])
            .expect("server config")
    }

    /// The edge's client of io.web at `authority`, presenting `hive`'s leaf.
    fn io_web_client(ca: &MeshCa, hive: &str, authority: &str) -> IoWebClient {
        let tls = mesh_material(ca, hive)
            .client_config_by_name()
            .expect("client config");
        IoWebClient::new(authority.to_string(), tls).expect("client")
    }

    /// What a test io.web saw of one request.
    #[derive(Debug)]
    struct SeenByIoWeb {
        /// The hive of the client certificate the edge presented.
        client_hive: Option<String>,
        method: Method,
        target: String,
        headers: HeaderMap,
    }

    /// What a test io.web does with a request.
    #[derive(Clone, Copy)]
    enum IoWebBehavior {
        /// Answers `IO_WEB_PAGE`, with hop-by-hop headers of its own and its own content policy.
        Page,
        /// Takes the request and never answers.
        Silent,
        /// Takes the request and closes the connection.
        HangUp,
    }

    /// A test io.web on a loopback port, under `tls`: its address, and for each connection the
    /// request it saw, or why its handshake failed.
    async fn test_io_web(
        tls: Arc<rustls::ServerConfig>,
        behavior: IoWebBehavior,
    ) -> (
        String,
        tokio::sync::mpsc::UnboundedReceiver<Result<SeenByIoWeb, String>>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let authority = listener.local_addr().expect("addr").to_string();
        let (seen_tx, seen_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let tls = Arc::clone(&tls);
                let seen = seen_tx.clone();
                tokio::spawn(async move {
                    let stream = match tokio_rustls::TlsAcceptor::from(tls).accept(tcp).await {
                        Ok(stream) => stream,
                        Err(err) => {
                            let _ = seen.send(Err(err.to_string()));
                            return;
                        }
                    };
                    let client_hive = stream
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|chain| chain.first())
                        .and_then(fluxbee_sdk::mesh::peer_hive_from_cert);
                    let service = hyper::service::service_fn(
                        move |req: hyper::Request<hyper::body::Incoming>| {
                            let _ = seen.send(Ok(SeenByIoWeb {
                                client_hive: client_hive.clone(),
                                method: req.method().clone(),
                                target: req.uri().to_string(),
                                headers: req.headers().clone(),
                            }));
                            async move {
                                match behavior {
                                    IoWebBehavior::Page => Ok(hyper::Response::builder()
                                        .header("content-type", "text/html; charset=utf-8")
                                        .header("content-security-policy", IO_WEB_CSP)
                                        .header("connection", "x-io-web-hop")
                                        .header("x-io-web-hop", "1")
                                        .header("keep-alive", "timeout=5")
                                        .header("x-io-web", "page")
                                        .body(Body::from(IO_WEB_PAGE))
                                        .expect("page")),
                                    IoWebBehavior::Silent => {
                                        std::future::pending::<
                                            Result<hyper::Response<Body>, std::io::Error>,
                                        >()
                                        .await
                                    }
                                    IoWebBehavior::HangUp => {
                                        Err(std::io::Error::other("io.web hangs up"))
                                    }
                                }
                            }
                        },
                    );
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        (authority, seen_rx)
    }

    /// A door that serves `WEB_HOST` through `proxy`, with the combined certificate of 0.1.84.
    async fn proxied_door(proxy: WebProxy) -> Door {
        Door {
            main: Router::new().route("/healthz", any(|| async { "ok" })),
            web: Arc::new(web_routes_with(&[WEB_HOST])),
            proxy: Arc::new(proxy),
            tls: serving_edge(self_signed(&[EDGE, WEB_HOST])).await,
        }
    }

    /// A request for `WEB_HOST`, with `extra` header lines and `body`.
    fn web_request(method: &str, target: &str, extra: &str, body: &str) -> String {
        format!("{method} {target} HTTP/1.1\r\nhost: {WEB_HOST}\r\n{extra}connection: close\r\n\r\n{body}")
    }

    fn header_map(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                header::HeaderName::from_bytes(name.as_bytes()).expect("name"),
                HeaderValue::from_str(value).expect("value"),
            );
        }
        headers
    }

    fn sorted_names(headers: &HeaderMap) -> Vec<String> {
        let mut names: Vec<String> = headers.keys().map(|name| name.to_string()).collect();
        names.sort();
        names
    }

    /// The rustls error behind `err`, through its sources and the I/O errors that carry one.
    fn rustls_error_in(err: &(dyn std::error::Error + 'static)) -> Option<rustls::Error> {
        let mut cause = Some(err);
        while let Some(err) = cause {
            if let Some(found) = err.downcast_ref::<rustls::Error>() {
                return Some(found.clone());
            }
            if let Some(found) = err
                .downcast_ref::<std::io::Error>()
                .and_then(std::io::Error::get_ref)
                .and_then(|inner| inner.downcast_ref::<rustls::Error>())
            {
                return Some(found.clone());
            }
            cause = err.source();
        }
        None
    }

    /// io.web's refusal of the edge's certificate, as the edge reads it after its request: the
    /// `access_denied` alert. io.web closes the connection after the alert, and the request the
    /// edge wrote meanwhile turns that close into a reset; on a loaded machine the reset can reach
    /// the edge's write before the alert reaches its read.
    fn refused_by_io_web(err: &(dyn std::error::Error + 'static)) -> bool {
        if let Some(found) = rustls_error_in(err) {
            return found == rustls::Error::AlertReceived(rustls::AlertDescription::AccessDenied);
        }
        let mut cause = Some(err);
        while let Some(err) = cause {
            if let Some(io) = err.downcast_ref::<std::io::Error>() {
                return matches!(
                    io.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                );
            }
            cause = err.source();
        }
        false
    }

    /// Wait until `proxy` holds no request in flight.
    async fn all_slots_back(proxy: &WebProxy) {
        let inflight = proxy.inflight.as_ref().expect("a cap");
        let deadline = Instant::now() + Duration::from_secs(5);
        while inflight.available_permits() < proxy.max_inflight {
            assert!(Instant::now() < deadline, "a slot was never given back");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// io-web-v2 §11.2: the headers io.web gets. The hop-by-hop ones and those `Connection` names
    /// go first, then the framing, `Expect`, `Host`, the trust and forwarding headers the client
    /// sent and the method overrides; the edge's own take their place.
    #[test]
    fn the_edge_rebuilds_the_headers_io_web_gets() {
        let inbound = header_map(&[
            ("host", "ARCHI.edge.example.com:443"),
            ("connection", "keep-alive, X-Custom-Hop"),
            ("connection", "close"),
            ("x-custom-hop", "1"),
            ("keep-alive", "timeout=5"),
            ("proxy-connection", "keep-alive"),
            ("te", "trailers"),
            ("trailer", "x-checksum"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "websocket"),
            ("proxy-authorization", "Basic Zm9vOmJhcg=="),
            ("proxy-authenticate", "Basic"),
            ("content-length", "0"),
            ("expect", "100-continue"),
            ("forwarded", "for=198.51.100.66;proto=http"),
            ("x-forwarded-for", "198.51.100.66"),
            ("x-forwarded-proto", "http"),
            ("x-forwarded-host", "evil.example.org"),
            ("x-forwarded-port", "80"),
            ("x-forwarded-prefix", "/admin"),
            ("x-request-id", "chosen-by-the-client"),
            ("x-real-ip", "198.51.100.66"),
            ("true-client-ip", "198.51.100.66"),
            ("x-original-url", "/admin"),
            ("x-rewrite-url", "/admin"),
            ("x-http-method-override", "DELETE"),
            ("x-http-method", "DELETE"),
            ("x-method-override", "DELETE"),
            ("cookie", "a=b"),
            ("authorization", "Bearer t"),
            ("accept", "text/html"),
            ("user-agent", "edge-test"),
            ("origin", "https://archi.edge.example.com"),
        ]);
        let request_id = Uuid::new_v4().to_string();
        let forwarding = |client: &str| Forwarding {
            host: WEB_HOST,
            client: client.parse().expect("ip"),
            proto: "https",
            request_id: &request_id,
        };
        let headers = io_web_request_headers(&inbound, &forwarding("203.0.113.7"));
        assert_eq!(
            sorted_names(&headers),
            [
                "accept",
                "authorization",
                "cookie",
                "forwarded",
                "host",
                "origin",
                "user-agent",
                "x-forwarded-for",
                "x-forwarded-host",
                "x-forwarded-proto",
                "x-request-id",
            ]
        );
        let value = |headers: &HeaderMap, name: &str| {
            let values: Vec<&str> = headers
                .get_all(name)
                .iter()
                .map(|value| value.to_str().expect("text"))
                .collect();
            assert_eq!(values.len(), 1, "{name}: {values:?}");
            values[0].to_string()
        };
        assert_eq!(value(&headers, "host"), WEB_HOST);
        assert_eq!(value(&headers, "x-forwarded-for"), "203.0.113.7");
        assert_eq!(value(&headers, "x-forwarded-proto"), "https");
        assert_eq!(value(&headers, "x-forwarded-host"), WEB_HOST);
        assert_eq!(
            value(&headers, "forwarded"),
            format!("for=203.0.113.7;proto=https;host={WEB_HOST}")
        );
        assert_eq!(value(&headers, "x-request-id"), request_id);
        assert_eq!(value(&headers, "cookie"), "a=b");

        // An IPv6 peer: bare in X-Forwarded-For, bracketed and quoted in Forwarded; an
        // IPv4-mapped one as its IPv4.
        let v6 = io_web_request_headers(&inbound, &forwarding("2001:db8::7"));
        assert_eq!(value(&v6, "x-forwarded-for"), "2001:db8::7");
        assert_eq!(
            value(&v6, "forwarded"),
            format!("for=\"[2001:db8::7]\";proto=https;host={WEB_HOST}")
        );
        let mapped = io_web_request_headers(&inbound, &forwarding("::ffff:203.0.113.7"));
        assert_eq!(value(&mapped, "x-forwarded-for"), "203.0.113.7");
        assert!(value(&mapped, "forwarded").starts_with("for=203.0.113.7;"));
    }

    /// io-web-v2 §11.2: io.web's hop-by-hop headers, and those its `Connection` names, never reach
    /// the client; a `Transfer-Encoding` takes the `Content-Length` it overrides with it.
    #[test]
    fn io_web_s_hop_by_hop_headers_never_reach_the_client() {
        let mut headers = header_map(&[
            ("content-type", "text/html; charset=utf-8"),
            ("content-length", "27"),
            ("content-security-policy", IO_WEB_CSP),
            ("set-cookie", "__Host-s=1; Path=/; Secure; HttpOnly"),
            ("connection", "keep-alive, X-IO-Web-Hop"),
            ("x-io-web-hop", "1"),
            ("keep-alive", "timeout=5"),
            ("upgrade", "h2c"),
            ("proxy-authenticate", "Basic"),
            ("trailer", "x-checksum"),
        ]);
        strip_io_web_response_headers(&mut headers);
        assert_eq!(
            sorted_names(&headers),
            [
                "content-length",
                "content-security-policy",
                "content-type",
                "set-cookie"
            ]
        );
        let mut chunked = header_map(&[
            ("transfer-encoding", "chunked"),
            ("content-length", "27"),
            ("content-type", "text/html"),
        ]);
        strip_io_web_response_headers(&mut chunked);
        assert_eq!(sorted_names(&chunked), ["content-type"]);
    }

    /// io-web-v2 §11.2: a web host's request reaches io.web over the mesh's mutual TLS. io.web
    /// sees the ingress's certificate, the target, the edge's own forwarding headers and none of
    /// the client's hop-by-hop or trust headers; the client gets io.web's answer without io.web's
    /// hop-by-hop headers, with io.web's content policy and the door's other security headers. A
    /// leaf of another hive, from the same CA, never passes for io.web.
    #[tokio::test]
    async fn a_web_host_reaches_io_web_over_the_mesh_with_the_edge_s_forwarding_headers() {
        install_crypto_provider();
        let ca = MeshCa::generate().expect("ca");
        let registry = mesh_registry();
        let (authority, mut seen) =
            test_io_web(io_web_tls(&ca, "motherbee", &registry), IoWebBehavior::Page).await;
        let door = proxied_door(WebProxy::new(
            Ok(io_web_client(&ca, "ingress1", &authority)),
            4,
        ))
        .await;

        let client_headers = concat!(
            "x-forwarded-for: 198.51.100.66\r\n",
            "x-forwarded-host: evil.example.org\r\n",
            "x-forwarded-proto: http\r\n",
            "forwarded: for=198.51.100.66\r\n",
            "x-real-ip: 198.51.100.66\r\n",
            "true-client-ip: 198.51.100.66\r\n",
            "x-original-url: /admin\r\n",
            "x-rewrite-url: /admin\r\n",
            "x-http-method-override: POST\r\n",
            "x-request-id: chosen-by-the-client\r\n",
            "connection: x-hop\r\n",
            "x-hop: 1\r\n",
            "keep-alive: timeout=600\r\n",
            "te: trailers\r\n",
            "proxy-authorization: Basic Zm9vOmJhcg==\r\n",
            "expect: 100-continue\r\n",
            "content-length: 0\r\n",
            "cookie: a=b\r\n",
            "user-agent: edge-test\r\n",
        );
        let answer = exchange(
            &door,
            &web_request("GET", "/page?code=secret", client_headers, ""),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "{}",
            String::from_utf8_lossy(&answer.body)
        );
        assert_eq!(answer.body, IO_WEB_PAGE.as_bytes());

        let request = seen
            .recv()
            .await
            .expect("io.web saw the request")
            .expect("io.web's handshake");
        assert_eq!(
            request.client_hive.as_deref(),
            Some("ingress1"),
            "the edge presented the ingress's leaf"
        );
        assert_eq!(request.method, Method::GET);
        assert_eq!(request.target, "/page?code=secret");
        let values = |name: &str| -> Vec<String> {
            request
                .headers
                .get_all(name)
                .iter()
                .map(|value| value.to_str().expect("text").to_string())
                .collect()
        };
        assert_eq!(values("host"), [WEB_HOST]);
        assert_eq!(values("x-forwarded-for"), ["203.0.113.7"]);
        assert_eq!(values("x-forwarded-proto"), ["https"]);
        assert_eq!(values("x-forwarded-host"), [WEB_HOST]);
        assert_eq!(
            values("forwarded"),
            [format!("for=203.0.113.7;proto=https;host={WEB_HOST}")]
        );
        let request_id = values("x-request-id");
        assert!(
            request_id.len() == 1 && Uuid::parse_str(&request_id[0]).is_ok(),
            "a fresh request id: {request_id:?}"
        );
        assert_eq!(values("cookie"), ["a=b"]);
        assert_eq!(values("user-agent"), ["edge-test"]);
        for gone in [
            "x-real-ip",
            "true-client-ip",
            "x-original-url",
            "x-rewrite-url",
            "x-http-method-override",
            "connection",
            "x-hop",
            "keep-alive",
            "te",
            "proxy-authorization",
            "expect",
            "content-length",
            "transfer-encoding",
        ] {
            assert!(values(gone).is_empty(), "{gone} reached io.web");
        }

        // io.web's answer: its own content policy wins; the door adds what io.web did not set;
        // io.web's hop-by-hop headers, and the one its Connection names, stay behind.
        assert_eq!(
            answer.header("content-security-policy").as_deref(),
            Some(IO_WEB_CSP)
        );
        assert_eq!(answer.header("cache-control").as_deref(), Some("no-store"));
        assert_eq!(
            answer.header("x-content-type-options").as_deref(),
            Some("nosniff")
        );
        assert_eq!(answer.header("x-io-web").as_deref(), Some("page"));
        assert_eq!(answer.header("x-io-web-hop"), None);
        assert_eq!(answer.header("keep-alive"), None);
        all_slots_back(&door.proxy).await;

        // HEAD: io.web's length, no body. An absolute-form target goes in origin form.
        let head = exchange(&door, &web_request("HEAD", "/page", "", "")).await;
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());
        assert_eq!(
            head.header("content-length"),
            Some(IO_WEB_PAGE.len().to_string())
        );
        let absolute = exchange(
            &door,
            &web_request("GET", "https://archi.edge.example.com/abs?q=1", "", ""),
        )
        .await;
        assert_eq!(absolute.status, 200);
        let head_seen = seen.recv().await.expect("HEAD").expect("handshake");
        assert_eq!(
            (head_seen.method, head_seen.target.as_str()),
            (Method::HEAD, "/page")
        );
        let absolute_seen = seen.recv().await.expect("GET").expect("handshake");
        assert_eq!(absolute_seen.target, "/abs?q=1");

        // A plaintext door tells io.web so.
        let plaintext = Door {
            tls: Arc::new(EdgeTls::plaintext()),
            ..door.clone()
        };
        assert_eq!(
            exchange(&plaintext, &web_request("GET", "/", "", ""))
                .await
                .status,
            200
        );
        let plain_seen = seen.recv().await.expect("GET").expect("handshake");
        assert_eq!(
            plain_seen
                .headers
                .get("x-forwarded-proto")
                .and_then(|value| value.to_str().ok()),
            Some("http")
        );
        all_slots_back(&door.proxy).await;

        // A server presenting worker1's leaf, from the same CA, is not io.web: the edge's
        // handshake refuses its name, and the client gets the fixed 502.
        let (impostor, mut impostor_seen) =
            test_io_web(io_web_tls(&ca, "worker1", &registry), IoWebBehavior::Page).await;
        let client = io_web_client(&ca, "ingress1", &impostor);
        let refused = client
            .send(io_web_request(
                &Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
                &Forwarding {
                    host: WEB_HOST,
                    client: "203.0.113.7".parse().expect("ip"),
                    proto: "https",
                    request_id: "r",
                },
            ))
            .await
            .expect_err("refused");
        assert!(
            matches!(
                &refused,
                IoWebFailure::Handshake(err) if matches!(
                    rustls_error_in(err),
                    Some(rustls::Error::InvalidCertificate(
                        rustls::CertificateError::NotValidForName
                            | rustls::CertificateError::NotValidForNameContext { .. }
                    ))
                )
            ),
            "{refused}"
        );
        let impostor_door = proxied_door(WebProxy::new(Ok(client), 4)).await;
        let answer = exchange(&impostor_door, &web_request("GET", "/", "", "")).await;
        assert_eq!(answer.status, 502);
        assert_eq!(answer.json()["error_code"], "UPSTREAM_UNAVAILABLE");
        while let Ok(seen) = impostor_seen.try_recv() {
            assert!(seen.is_err(), "the impostor never got a request: {seen:?}");
        }
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// io-web-v2 §11.2: a web host takes GET and HEAD only, with no body and no upgrade; the edge
    /// refuses anything else, and io.web never sees it.
    #[tokio::test]
    async fn the_edge_refuses_other_methods_bodies_and_upgrades_before_io_web() {
        install_crypto_provider();
        let ca = MeshCa::generate().expect("ca");
        let registry = mesh_registry();
        let (authority, mut seen) =
            test_io_web(io_web_tls(&ca, "motherbee", &registry), IoWebBehavior::Page).await;
        let door = proxied_door(WebProxy::new(
            Ok(io_web_client(&ca, "ingress1", &authority)),
            4,
        ))
        .await;

        for request in [
            web_request("POST", "/", "content-length: 3\r\n", "a=1"),
            web_request("POST", "/", "content-length: 0\r\n", ""),
            web_request("PUT", "/", "content-length: 0\r\n", ""),
            web_request("DELETE", "/", "", ""),
            web_request("PATCH", "/", "content-length: 0\r\n", ""),
            web_request("OPTIONS", "/", "", ""),
            web_request("TRACE", "/", "", ""),
            format!("CONNECT {WEB_HOST}:443 HTTP/1.1\r\nhost: {WEB_HOST}:443\r\nconnection: close\r\n\r\n"),
        ] {
            let answer = exchange(&door, &request).await;
            assert_eq!(answer.status, 405, "{request}");
            assert_eq!(answer.header("allow").as_deref(), Some("GET, HEAD"));
            assert_eq!(answer.json()["error_code"], "METHOD_NOT_ALLOWED");
            assert_security_headers(&answer);
        }
        for (request, code) in [
            (
                web_request("GET", "/", "content-length: 3\r\n", "abc"),
                "BODY_NOT_ALLOWED",
            ),
            (
                web_request("GET", "/", "transfer-encoding: chunked\r\n", "0\r\n\r\n"),
                "BODY_NOT_ALLOWED",
            ),
            (
                web_request(
                    "GET",
                    "/socket",
                    "connection: upgrade\r\nupgrade: websocket\r\nsec-websocket-version: 13\r\nsec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n",
                    "",
                ),
                "UPGRADE_NOT_ALLOWED",
            ),
        ] {
            let answer = exchange(&door, &request).await;
            assert_eq!(answer.status, 400, "{request}");
            assert_eq!(answer.json()["error_code"], code, "{request}");
            assert_security_headers(&answer);
        }
        // A HEAD answer has no body to read the code from.
        let head = exchange(
            &door,
            &web_request("HEAD", "/", "content-length: 1\r\n", "x"),
        )
        .await;
        assert_eq!(head.status, 400);
        assert_security_headers(&head);
        assert!(seen.try_recv().is_err(), "nothing reached io.web");

        // A GET with an empty body does.
        let answer = exchange(&door, &web_request("GET", "/", "content-length: 0\r\n", "")).await;
        assert_eq!(answer.status, 200);
        assert!(seen.recv().await.expect("seen").is_ok());
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// io-web-v2 §11.2: whatever keeps io.web's answer from the edge, the client gets the same
    /// fixed 502 with the request id, and the detail stays in the edge's log. io.web's refusal of
    /// the edge's certificate arrives after the edge's connect: TLS 1.3 ends the client's handshake
    /// before the server checks its certificate. The proxy's upstream, as the `EDGE_WEB_ROUTES`
    /// answer reports it, keeps the last failure's time and kind, and the failures are logged at
    /// most once per interval, with their count.
    #[tokio::test]
    async fn every_failure_to_reach_io_web_is_the_fixed_502() {
        install_crypto_provider();
        let ca = MeshCa::generate().expect("ca");
        let registry = mesh_registry();
        let motherbee = || io_web_tls(&ca, "motherbee", &registry);
        // Nothing listens on the port.
        let closed = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind")
            .local_addr()
            .expect("addr")
            .to_string();
        let (served, _) = test_io_web(motherbee(), IoWebBehavior::Page).await;
        let (silent, _) = test_io_web(motherbee(), IoWebBehavior::Silent).await;
        let (hangs_up, _) = test_io_web(motherbee(), IoWebBehavior::HangUp).await;
        // A port whose connections the kernel takes and nothing ever reads: the TLS handshake
        // never ends.
        let mute = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let mute_addr = mute.local_addr().expect("addr").to_string();

        type Expected = fn(&IoWebFailure) -> bool;
        let cases: [(&str, &str, &str, Expected, &str); 5] = [
            (
                "connect refused",
                &closed,
                "ingress1",
                |failure| matches!(failure, IoWebFailure::Connect(_)),
                "connect",
            ),
            // The registry calls worker1 a worker: io.web refuses its certificate, once the edge's
            // side of the handshake is over.
            (
                "refused after connect",
                &served,
                "worker1",
                |failure| matches!(failure, IoWebFailure::Head(err) if refused_by_io_web(err)),
                "head",
            ),
            (
                "no head in time",
                &silent,
                "ingress1",
                |failure| matches!(failure, IoWebFailure::HeadTimeout(_)),
                "head_timeout",
            ),
            (
                "closed before the head",
                &hangs_up,
                "ingress1",
                |failure| matches!(failure, IoWebFailure::Head(_)),
                "head",
            ),
            (
                "no handshake in time",
                &mute_addr,
                "ingress1",
                |failure| matches!(failure, IoWebFailure::ConnectTimeout(_)),
                "connect_timeout",
            ),
        ];
        let probe = || {
            io_web_request(
                &Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("request"),
                &Forwarding {
                    host: WEB_HOST,
                    client: "203.0.113.7".parse().expect("ip"),
                    proto: "https",
                    request_id: "probe",
                },
            )
        };
        for (case, authority, hive, expected, kind) in cases {
            let mut client = io_web_client(&ca, hive, authority);
            client.connect_timeout = Duration::from_millis(300);
            client.head_timeout = Duration::from_millis(300);
            let failure = client
                .send(probe())
                .await
                .err()
                .unwrap_or_else(|| panic!("{case}: io.web answered"));
            assert!(expected(&failure), "{case}: {failure}");
            assert_eq!(failure.kind(), kind, "{case}");

            let door = proxied_door(WebProxy::new(Ok(client), 2)).await;
            assert_eq!(
                door.proxy.upstream_status(),
                json!({"state": "ready", "reason": null, "last_failure": null}),
                "{case}"
            );
            let before = now_unix_secs();
            let answer = exchange(&door, &web_request("GET", "/page?code=x", "", "")).await;
            assert_unavailable(case, &answer);
            let upstream = door.proxy.upstream_status();
            assert_eq!(upstream["state"], "ready", "{case}");
            assert_eq!(upstream["last_failure"]["kind"], kind, "{case}: {upstream}");
            let at = upstream["last_failure"]["at"].as_i64().expect("at");
            assert!(
                (before..=now_unix_secs()).contains(&at),
                "{case}: {upstream}"
            );
            // Its time and kind only: nothing of the request (its path, its query, the peer).
            let text = upstream.to_string();
            for request in ["/page", "code=", "203.0.113.7", WEB_HOST] {
                assert!(!text.contains(request), "{case}: {text}");
            }
            all_slots_back(&door.proxy).await;
        }

        // No upstream at all (no mesh material, no uplink): the same 502, never plaintext. The
        // answer says why, and the failures of one burst are one line: the first at once, the
        // others counted for the next.
        let door = proxied_door(WebProxy::new(Err("no material".to_string()), 2)).await;
        for _ in 0..3 {
            let answer = exchange(&door, &web_request("GET", "/", "", "")).await;
            assert_unavailable("no upstream", &answer);
        }
        let upstream = door.proxy.upstream_status();
        assert_eq!(upstream["state"], "unavailable");
        assert_eq!(upstream["reason"], "no material");
        assert_eq!(upstream["last_failure"]["kind"], "no_upstream");
        assert_eq!(
            door.proxy
                .failures
                .lock()
                .expect("failures")
                .unlogged
                .unlogged,
            2,
            "logged once, two counted for the next line"
        );
        // The answer to EDGE_WEB_ROUTES carries it.
        let routes = scratch_web_routes();
        let (_, reply) = tls_command_reply(
            Some(&*door.tls),
            &routes,
            Some(&*door.proxy),
            &admin_command(
                MSG_EDGE_WEB_ROUTES,
                json!({"routes": [{"host": WEB_HOST, "upstream": "io.web"}]}),
            ),
        )
        .await
        .expect("a door command");
        assert_eq!(reply["status"], "ok", "{reply}");
        assert_eq!(reply["upstream"], door.proxy.upstream_status());
        drop(mute);
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// The fixed 502: a request id in the body and in `X-Request-Id`, and nothing internal.
    fn assert_unavailable(case: &str, answer: &Answer) {
        assert_eq!(answer.status, 502, "{case}");
        let body = answer.json();
        assert_eq!(body["error_code"], "UPSTREAM_UNAVAILABLE", "{case}");
        assert_eq!(
            body["message"], "the service is unavailable; retry shortly",
            "{case}"
        );
        let request_id = body["request_id"].as_str().expect("a request id");
        assert!(Uuid::parse_str(request_id).is_ok(), "{case}: {request_id}");
        assert_eq!(
            answer.header("x-request-id").as_deref(),
            Some(request_id),
            "{case}"
        );
        let text = String::from_utf8_lossy(&answer.body).to_ascii_lowercase();
        for internal in [
            "127.0.0.1",
            "motherbee",
            "worker1",
            "access",
            "tls",
            "timed",
            "code=",
        ] {
            assert!(!text.contains(internal), "{case}: {text}");
        }
        assert_security_headers(answer);
    }

    /// io-web-v2 §11.2: past `web_max_inflight` the proxy answers a quick 503 and io.web never
    /// sees the request; a slot comes back when its request ends. 0 is no cap.
    #[tokio::test]
    async fn past_its_cap_the_proxy_answers_a_quick_503() {
        install_crypto_provider();
        let ca = MeshCa::generate().expect("ca");
        let registry = mesh_registry();
        let (silent, mut seen) = test_io_web(
            io_web_tls(&ca, "motherbee", &registry),
            IoWebBehavior::Silent,
        )
        .await;
        let mut client = io_web_client(&ca, "ingress1", &silent);
        client.head_timeout = Duration::from_millis(800);
        let door = proxied_door(WebProxy::new(Ok(client), 1)).await;

        let held = tokio::spawn({
            let door = door.clone();
            async move { exchange(&door, &web_request("GET", "/slow", "", "")).await }
        });
        seen.recv()
            .await
            .expect("io.web holds the first request")
            .expect("handshake");
        let started = Instant::now();
        let busy = exchange(&door, &web_request("GET", "/next", "", "")).await;
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(busy.status, 503);
        assert_eq!(busy.json()["error_code"], "WEB_BUSY");
        assert_security_headers(&busy);
        assert!(
            seen.try_recv().is_err(),
            "the refused request never reached io.web"
        );

        assert_eq!(held.await.expect("exchange").status, 502);
        all_slots_back(&door.proxy).await;
        assert!(WebProxy::new(Err("none".to_string()), 0).inflight.is_none());
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// io-web-v2 §11.2: the proxy's upstream comes from hive.yaml: this hive's mesh TLS material
    /// under its id, and its first uplink's host on `IO_WEB_PORT`, verified as
    /// `IO_WEB_SERVER_NAME`. A `wan:` of another shape leaves the proxy without an upstream, never
    /// the door without its section; what is missing is named, and nothing falls back.
    #[test]
    fn the_proxy_s_upstream_comes_from_hive_yaml() {
        let dir = std::env::temp_dir().join(format!("sy-edge-hive-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("hive.yaml"),
            concat!(
                "hive_id: ingress1\n",
                "role: ingress\n",
                "wan:\n",
                "  gateway_name: RT.gateway\n",
                "  mtls: required\n",
                "  uplinks:\n",
                "    - address: \"10.10.10.10:9000\"\n",
                "blob:\n",
                "  enabled: false\n",
                "edge:\n",
                "  listen: \"0.0.0.0:443\"\n",
                "  web_max_inflight: 32\n",
            ),
        )
        .expect("hive.yaml");
        let hive = load_hive_edge(&dir);
        assert_eq!(hive.hive_id.as_deref(), Some("ingress1"));
        assert_eq!(
            first_uplink_address(hive.wan.as_ref()).as_deref(),
            Some("10.10.10.10:9000")
        );
        assert_eq!(
            hive.edge.as_ref().and_then(|edge| edge.web_max_inflight),
            Some(32)
        );
        for wan in [
            "wan: 7\n",
            "wan:\n  uplinks: []\n",
            "wan:\n  uplinks:\n    - port: 9000\n",
            "",
        ] {
            std::fs::write(
                dir.join("hive.yaml"),
                format!(
                    "hive_id: ingress1\nrole: ingress\n{wan}edge:\n  listen: \"0.0.0.0:443\"\n"
                ),
            )
            .expect("hive.yaml");
            let hive = load_hive_edge(&dir);
            assert_eq!(first_uplink_address(hive.wan.as_ref()), None, "{wan}");
            assert!(hive.edge.is_some(), "{wan}");
        }

        for (address, host) in [
            ("10.10.10.10:9000", Some("10.10.10.10")),
            (" 10.10.10.10:9000 ", Some("10.10.10.10")),
            ("[fd00::10]:9000", Some("[fd00::10]")),
            ("motherbee.lan:9000", Some("motherbee.lan")),
            ("10.10.10.10", Some("10.10.10.10")),
            ("", None),
            (":9000", None),
            ("user@10.10.10.10:9000", None),
            ("not a host", None),
        ] {
            assert_eq!(uplink_host(address).as_deref(), host, "{address}");
        }

        install_crypto_provider();
        let ca = MeshCa::generate().expect("ca");
        let leaf = ca.issue_leaf("ingress1").expect("leaf");
        let root = dir.join("tls");
        std::fs::create_dir_all(root.join("ingress1")).expect("tls dir");
        std::fs::write(root.join("ingress1").join("ca.crt"), ca.ca_cert_pem()).expect("ca.crt");
        std::fs::write(root.join("ingress1").join("cert.crt"), &leaf.cert_pem).expect("cert.crt");
        std::fs::write(root.join("ingress1").join("cert.key"), &leaf.key_pem).expect("cert.key");
        let client = IoWebClient::for_hive(&root, Some("ingress1"), Some("10.10.10.10:9000"))
            .expect("a client");
        assert_eq!(client.authority, format!("10.10.10.10:{IO_WEB_PORT}"));
        assert_eq!(
            client.server_name,
            ServerName::try_from(IO_WEB_SERVER_NAME).expect("name")
        );
        let v6 = IoWebClient::for_hive(&root, Some("ingress1"), Some("[fd00::10]:9000"))
            .expect("a client");
        assert_eq!(v6.authority, format!("[fd00::10]:{IO_WEB_PORT}"));
        for (hive_id, uplink, missing) in [
            (Some("ingress1"), None, "wan.uplinks"),
            (Some("ingress1"), Some(":9000"), "names no host"),
            (None, Some("10.10.10.10:9000"), "hive_id"),
            (Some("../ingress1"), Some("10.10.10.10:9000"), "hive_id"),
            (
                Some("ingress2"),
                Some("10.10.10.10:9000"),
                "mesh TLS material",
            ),
        ] {
            let err = IoWebClient::for_hive(&root, hive_id, uplink)
                .err()
                .expect("refused");
            assert!(err.contains(missing), "{err}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
