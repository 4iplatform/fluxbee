#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::body::to_bytes;
use axum::extract::{Path as AxumPath, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use fluxbee_sdk::protocol::{
    ConfigChangedPayload, Destination, Message, Meta, Routing, MSG_CONFIG_CHANGED, MSG_TTL_EXCEEDED,
    MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{
    try_handle_default_node_status, NodeConfig, NodeSender, NodeUuidMode, OperationalRouteProfile,
    PendingMatcher, RouteMatch, RouteTarget, RouterDispatcher, RpcError, RpcRequestLabels,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::TcpListener;
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

    pub fn ensure_http_envelope_within_limit(
        message: &Message,
    ) -> Result<usize, HttpIngressError> {
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

/// `CONFIG_CHANGED` subsystem that carries the edge's reverse-proxy table
/// (`hash -> {ilk, handler_node, inbound_family, auth}`) pushed by the core
/// authority via `NODE_CONFIG_SET` (spec §5.1). Also accepts the generic
/// `node_config` wrapper.
const ENDPOINTS_SUBSYSTEM: &str = "endpoints";
const NODE_CONFIG_SUBSYSTEM: &str = "node_config";

/// Hop-by-hop + auth headers stripped before a request crosses inward. Entry auth
/// is already gated at the door; the raw bearer token never travels inward (§3).
const STRIPPED_REQUEST_HEADERS: &[&str] = &[
    "authorization",
    "host",
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "content-length",
];

#[derive(Debug, Clone)]
struct Config {
    node_name: String,
    node_version: String,
    router_socket: PathBuf,
    uuid_persistence_dir: PathBuf,
    config_dir: PathBuf,
    ttl: u8,
    handler_timeout_ms: u64,
    /// Public HTTP frontend bind address. Present ONLY on the ingress-hive role
    /// (fail-closed role gate, `resolve_http_listen`). Absent ⇒ the node connects
    /// to the mesh but serves no public door.
    http_listen: Option<String>,
    /// Seed for the reverse-proxy table (`hash -> {...}`). Live updates then
    /// arrive over the mesh via `NODE_CONFIG_SET` (§5.1).
    endpoints_path: Option<PathBuf>,
    /// Public TLS material from disk. When both are `Some` the frontend serves HTTPS.
    tls_cert: Option<PathBuf>,
    tls_key: Option<PathBuf>,
    /// Vault secret key holding the cert (preferred over the disk paths).
    tls_vault_key: Option<String>,
    /// Target hive for `SY.vault@<hive>` (default: the mesh's motherbee).
    vault_hive: String,
}

#[tokio::main]
async fn main() -> Result<(), SyEdgeError> {
    let config = Config::load();
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,sy_edge=debug,fluxbee_sdk=info"));
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

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

    // The reverse-proxy table: seeded from disk, then hot-swapped in place when
    // the core authority pushes `NODE_CONFIG_SET` (spec §5.1). Shared (read) by
    // the frontend and (write) by the system loop below.
    let registry: Arc<RwLock<HashMap<String, EndpointEntry>>> = Arc::new(RwLock::new(
        config
            .endpoints_path
            .as_deref()
            .map(load_registry)
            .unwrap_or_default(),
    ));

    // Ingress-hive role: run the public HTTPS door. Fail-closed — absent on any
    // other role (`resolve_http_listen`).
    if let Some(listen) = config.http_listen.clone() {
        tracing::info!(
            listen = %listen,
            endpoints = registry.read().map(|r| r.len()).unwrap_or(0),
            "sy-edge starting public HTTP frontend"
        );
        let state = Arc::new(FrontendState {
            dispatcher: Arc::clone(&dispatcher),
            sender: sender.clone(),
            registry: Arc::clone(&registry),
            self_ilk: self_ilk.clone(),
            ttl: config.ttl,
            timeout: Duration::from_millis(config.handler_timeout_ms),
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
        } else {
            tokio::spawn(async move {
                if let Err(err) = run_frontend(listen, state, tls).await {
                    tracing::error!(error = %err, "sy-edge public frontend exited");
                }
            });
        }
    }

    // System channel: default node status + live endpoint-table pushes
    // (`NODE_CONFIG_SET` / `CONFIG_CHANGED`, §5.1). This loop is the process's
    // only blocking await — it keeps main() alive.
    let mut applied_version: u64 = 0;
    while let Some(msg) = system_rx.recv().await {
        if let Some(payload) = as_config_changed(&msg) {
            if payload.subsystem == ENDPOINTS_SUBSYSTEM || payload.subsystem == NODE_CONFIG_SUBSYSTEM
            {
                // Monotonic version gate (apply_mode = replace, §5.1). version 0
                // (unset) always applies.
                if payload.version != 0 && payload.version <= applied_version {
                    tracing::debug!(version = payload.version, applied = applied_version, "sy-edge ignoring stale endpoints config");
                    continue;
                }
                if let Some(rows) = extract_endpoint_rows(&payload.config) {
                    let count = rows.len();
                    let next = rows_to_registry(rows);
                    match registry.write() {
                        Ok(mut guard) => *guard = next,
                        Err(_) => {
                            tracing::error!("sy-edge registry lock poisoned; endpoint update dropped");
                            continue;
                        }
                    }
                    applied_version = payload.version;
                    tracing::info!(endpoints = count, version = payload.version, "sy-edge endpoint table replaced via NODE_CONFIG_SET");
                }
                continue;
            }
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

/// Parse a `CONFIG_CHANGED` broadcast (or `None` if this message isn't one).
fn as_config_changed(msg: &Message) -> Option<ConfigChangedPayload> {
    if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_CONFIG_CHANGED) {
        return None;
    }
    serde_json::from_value::<ConfigChangedPayload>(msg.payload.clone()).ok()
}

/// Pull the `endpoints` array out of a `CONFIG_CHANGED.config`, tolerating the
/// generic node-config wrapper (`config.patch.endpoints` / `config.config.endpoints`)
/// as well as a direct `config.endpoints`.
fn extract_endpoint_rows(config: &Value) -> Option<Vec<EndpointRow>> {
    let arr = config
        .get("endpoints")
        .or_else(|| config.get("patch").and_then(|p| p.get("endpoints")))
        .or_else(|| config.get("config").and_then(|p| p.get("endpoints")))?;
    serde_json::from_value::<Vec<EndpointRow>>(arr.clone()).ok()
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
            http_listen,
            endpoints_path: Some(PathBuf::from(endpoints_path)),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum AuthMode {
    Public,
    SharedSecret,
}

/// One row of the reverse-proxy table (seed file or `NODE_CONFIG_SET` push).
#[derive(Debug, Clone, Deserialize)]
struct EndpointRow {
    hash: String,
    /// Published target ilk (carried inward for the handler's own info/authz).
    ilk: String,
    /// The **pre-resolved** handler L2 name (Option Z): the core authority
    /// resolved `ilk -> handler_node` at publication time and cached it here, so
    /// the identity-less edge forwards by name with no request-time resolve.
    handler_node: String,
    /// The `msg_type`/subject the target speaks (Option A): the edge labels the
    /// forwarded message with exactly this family.
    inbound_family: String,
    auth_mode: AuthMode,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    methods: Option<Vec<String>>,
    #[serde(default)]
    tenant_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct EndpointsFile {
    #[serde(default)]
    endpoints: Vec<EndpointRow>,
}

#[derive(Debug, Clone)]
struct EndpointEntry {
    ilk: String,
    handler_node: String,
    inbound_family: String,
    auth_mode: AuthMode,
    secret: Option<String>,
    methods: Option<Vec<String>>,
    #[allow(dead_code)]
    tenant_id: Option<String>,
}

fn rows_to_registry(rows: Vec<EndpointRow>) -> HashMap<String, EndpointEntry> {
    let mut registry = HashMap::with_capacity(rows.len());
    for row in rows {
        registry.insert(
            row.hash,
            EndpointEntry {
                ilk: row.ilk,
                handler_node: row.handler_node,
                inbound_family: row.inbound_family,
                auth_mode: row.auth_mode,
                secret: row.secret,
                methods: row.methods,
                tenant_id: row.tenant_id,
            },
        );
    }
    registry
}

/// Load the static `hash -> endpoint` seed. A missing file is fine (empty
/// registry, every request 404s) so the frontend can boot before any publish.
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

struct FrontendState {
    dispatcher: Arc<RouterDispatcher>,
    sender: NodeSender,
    registry: Arc<RwLock<HashMap<String, EndpointEntry>>>,
    /// The edge's own deterministic ("a fuego") system ilk, stamped as `src_ilk`.
    self_ilk: String,
    ttl: u8,
    timeout: Duration,
}

async fn run_frontend(
    listen: String,
    state: Arc<FrontendState>,
    tls: Option<Arc<rustls::ServerConfig>>,
) -> Result<(), SyEdgeError> {
    let app = Router::new()
        .route("/e/:hash", any(invoke_root))
        .route("/e/:hash/*extra", any(invoke_extra))
        .route("/b/:hash", any(blob_stub))
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
fn tls_config_from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<rustls::ServerConfig, SyEdgeError> {
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

async fn invoke_root(
    State(state): State<Arc<FrontendState>>,
    AxumPath(hash): AxumPath<String>,
    req: Request,
) -> Response {
    invoke(state, hash, String::new(), req).await
}

async fn invoke_extra(
    State(state): State<Arc<FrontendState>>,
    AxumPath((hash, extra)): AxumPath<(String, String)>,
    req: Request,
) -> Response {
    invoke(state, hash, extra, req).await
}

async fn blob_stub(AxumPath(_hash): AxumPath<String>) -> Response {
    // §8 blob egress is a later increment.
    http_error(
        StatusCode::NOT_IMPLEMENTED,
        "BLOB_NOT_IMPLEMENTED",
        "blob egress not implemented yet",
    )
}

async fn invoke(state: Arc<FrontendState>, hash: String, extra: String, req: Request) -> Response {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let headers = req.headers().clone();

    // 1. Registry lookup — the hash is the first capability gate. Clone the row
    //    out of the lock so nothing is held across the inward await.
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
        match registry.get(&hash) {
            Some(entry) => entry.clone(),
            None => return http_error(StatusCode::NOT_FOUND, "NOT_FOUND", "no such endpoint"),
        }
    };

    // 2. Method allowlist (if the row constrains methods).
    if let Some(allowed) = &entry.methods {
        if !allowed.iter().any(|m| m.eq_ignore_ascii_case(method.as_str())) {
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
            let ok = entry
                .secret
                .as_deref()
                .map(|expected| bearer_matches(&headers, expected))
                .unwrap_or(false);
            if !ok {
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
            Err(_) => (
                Some(base64_encode(&body)),
                Some(HttpBodyEncoding::Base64),
            ),
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
            // Option Z: forward by the pre-resolved handler name (LSA cross-hive).
            dst: Destination::Unicast(entry.handler_node.clone()),
            ttl: state.ttl,
            trace_id: Uuid::new_v4().to_string(),
        },
        meta: Meta {
            // Option A: the target's own declared family.
            msg_type: entry.inbound_family.clone(),
            // Carried for the handler's info/authz; routing does not depend on it.
            dst_ilk: Some(entry.ilk.clone()),
            // The edge's own a-fuego ilk. Core is the first trusted identity
            // boundary and may re-derive/ignore it (spec §12).
            src_ilk: Some(state.self_ilk.clone()),
            context: Some(Value::Object(ctx)),
            ..Meta::default()
        },
        payload,
    };
    if let Err(err) = ensure_http_envelope_within_limit(&message) {
        return http_error(StatusCode::PAYLOAD_TOO_LARGE, "REQ_TOO_LARGE", &err.to_string());
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
            if reply.meta.msg_type == SYSTEM_KIND
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
            let status =
                StatusCode::from_u16(rpc_error_to_http_status(&err)).unwrap_or(StatusCode::BAD_GATEWAY);
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

fn bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer ")))
        .map(|token| token.trim() == expected)
        .unwrap_or(false)
}

fn filter_request_headers(headers: &HeaderMap) -> Vec<HttpHeader> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str().to_ascii_lowercase();
            if STRIPPED_REQUEST_HEADERS.contains(&name.as_str()) {
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
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
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
    fn registry_file_parses_z_rows() {
        let raw = r#"{"endpoints":[
            {"hash":"h1","ilk":"ilk:a","handler_node":"AI.handler@motherbee","inbound_family":"user","auth_mode":"public","methods":["POST"]},
            {"hash":"h2","ilk":"ilk:b","handler_node":"IO.api@motherbee","inbound_family":"text","auth_mode":"shared-secret","secret":"s3cr3t"}
        ]}"#;
        let parsed: EndpointsFile = serde_json::from_str(raw).expect("parse");
        assert_eq!(parsed.endpoints.len(), 2);
        assert_eq!(parsed.endpoints[0].handler_node, "AI.handler@motherbee");
        assert_eq!(parsed.endpoints[0].inbound_family, "user");
        assert_eq!(parsed.endpoints[0].auth_mode, AuthMode::Public);
        assert_eq!(parsed.endpoints[1].auth_mode, AuthMode::SharedSecret);
        assert_eq!(parsed.endpoints[1].secret.as_deref(), Some("s3cr3t"));
    }

    #[test]
    fn extract_endpoint_rows_handles_direct_and_wrapped() {
        let direct = json!({ "endpoints": [
            {"hash":"h","ilk":"ilk:x","handler_node":"AI.h@motherbee","inbound_family":"user","auth_mode":"public"}
        ]});
        assert_eq!(extract_endpoint_rows(&direct).unwrap().len(), 1);

        let wrapped = json!({ "node_name": "SY.edge@ingress1", "patch": { "endpoints": [
            {"hash":"h","ilk":"ilk:x","handler_node":"AI.h@motherbee","inbound_family":"user","auth_mode":"public"}
        ] }});
        assert_eq!(extract_endpoint_rows(&wrapped).unwrap().len(), 1);

        // A NODE_CONFIG_SET carrying no endpoints section yields None.
        let none = json!({ "node_name": "SY.edge@ingress1", "patch": { "other": 1 } });
        assert!(extract_endpoint_rows(&none).is_none());
    }

    #[test]
    fn rows_to_registry_indexes_by_hash() {
        let rows: Vec<EndpointRow> = serde_json::from_value(json!([
            {"hash":"abc","ilk":"ilk:a","handler_node":"AI.h@motherbee","inbound_family":"user","auth_mode":"public"}
        ]))
        .unwrap();
        let reg = rows_to_registry(rows);
        let entry = reg.get("abc").expect("indexed");
        assert_eq!(entry.handler_node, "AI.h@motherbee");
        assert_eq!(entry.inbound_family, "user");
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
    fn filter_request_headers_strips_auth_and_hop_by_hop() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer x".parse().unwrap());
        headers.insert("host", "edge.fluxbee.ai".parse().unwrap());
        headers.insert("connection", "keep-alive".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());
        headers.insert("x-custom", "keep-me".parse().unwrap());
        let filtered = filter_request_headers(&headers);
        let names: Vec<&str> = filtered.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"content-type"));
        assert!(names.contains(&"x-custom"));
        assert!(!names.contains(&"authorization"));
        assert!(!names.contains(&"host"));
        assert!(!names.contains(&"connection"));
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
