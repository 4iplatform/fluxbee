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
    Destination, Message, Meta, Routing, MSG_EDGE_CLOSE_URL, MSG_EDGE_CLOSE_URL_RESPONSE,
    MSG_EDGE_OPEN_URL, MSG_EDGE_OPEN_URL_RESPONSE, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{
    build_node_config_response_message, is_node_config_get_message, is_node_config_set_message,
    parse_node_config_request, try_handle_default_node_status, NodeConfig, NodeConfigControlRequest,
    NodeSender, NodeUuidMode, OperationalRouteProfile, PendingMatcher, RouteMatch, RouteTarget,
    RouterDispatcher, RpcError, RpcRequestLabels,
};
use serde::{Deserialize, Serialize};
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
    /// Seed for the reverse-proxy table (`ich -> {...}`). Born-zero in production;
    /// live URLs then arrive one at a time via `EDGE_OPEN_URL` / `EDGE_CLOSE_URL` (§7).
    endpoints_path: Option<PathBuf>,
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
    let env_filter =
        EnvFilter::try_new(&initial_log).unwrap_or_else(|_| EnvFilter::new("info"));
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
        let row: EndpointRow = serde_json::from_value(msg.payload.clone())
            .map_err(|err| format!("invalid EDGE_OPEN_URL payload: {err}"))?;
        let (ich, entry) = row_to_entry(row);
        let mut guard = registry
            .write()
            .map_err(|_| "registry lock poisoned".to_string())?;
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
    methods: Option<Vec<String>>,
    #[allow(dead_code)]
    tenant_id: Option<String>,
}

/// Index the rows by `ICH` (the URL key). No `ilk` anywhere — the edge is outside
/// the identity frontier (I6).
/// Split one row into its `ich` key and the entry stored in the registry.
fn row_to_entry(row: EndpointRow) -> (String, EndpointEntry) {
    (
        row.ich,
        EndpointEntry {
            owner_l2_name: row.owner_l2_name,
            inbound_family: row.inbound_family,
            auth_mode: row.auth_mode,
            secret: row.secret,
            secret_ref: row.secret_ref,
            methods: row.methods,
            tenant_id: row.tenant_id,
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
            secret: None,                       // NEVER persist the secret VALUE
            secret_ref: e.secret_ref.clone(),   // persist the vault REF (just a name)
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
        .route("/e/:ich", any(invoke_root))
        .route("/e/:ich/*extra", any(invoke_extra))
        .route("/b/:ich", any(blob_stub))
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
                    None => tracing::warn!(ich = %ich, secret_ref = %secret_ref, "vault secret has no 'secret' field; channel stays closed to auth"),
                }
            }
            Err(err) => tracing::warn!(ich = %ich, secret_ref = %secret_ref, error = %err, "sy-edge could not fetch channel secret from vault (channel rejects until resolved)"),
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

async fn blob_stub(AxumPath(_hash): AxumPath<String>) -> Response {
    // §8 blob egress is a later increment.
    http_error(
        StatusCode::NOT_IMPLEMENTED,
        "BLOB_NOT_IMPLEMENTED",
        "blob egress not implemented yet",
    )
}

async fn invoke(state: Arc<FrontendState>, ich: String, extra: String, req: Request) -> Response {
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
    fn persisted_routes_round_trip_and_never_leak_the_secret() {
        let path = std::env::temp_dir().join(format!("sy-edge-persist-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut reg: HashMap<String, EndpointEntry> = HashMap::new();
        reg.insert(
            "ich:pub".into(),
            EndpointEntry { owner_l2_name: "IO.a@h".into(), inbound_family: "user".into(), auth_mode: AuthMode::Public, secret: None, secret_ref: None, methods: None, tenant_id: None },
        );
        reg.insert(
            "ich:sec".into(),
            EndpointEntry { owner_l2_name: "IO.b@h".into(), inbound_family: "user".into(), auth_mode: AuthMode::SharedSecret, secret: Some("s3cr3t".into()), secret_ref: Some("edge_channel_secret:ich:sec".into()), methods: None, tenant_id: None },
        );
        persist_registry(&path, &reg);

        // The routes survive a reload (warm-start). The secret VALUE never hit disk, but the
        // secret_ref (just a vault key name) DID — so the edge can re-fetch it at boot.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("s3cr3t"), "secret value leaked to the on-disk route table");
        assert!(raw.contains("edge_channel_secret:ich:sec"), "secret_ref must be persisted");
        let loaded = load_registry(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded["ich:pub"].owner_l2_name, "IO.a@h");
        assert_eq!(loaded["ich:sec"].auth_mode, AuthMode::SharedSecret);
        assert!(loaded["ich:sec"].secret.is_none(), "secret must reload as None (fetched from vault by ref)");
        assert_eq!(loaded["ich:sec"].secret_ref.as_deref(), Some("edge_channel_secret:ich:sec"));
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
            "ich": "ich:1",
            "owner_l2_name": "IO.cloud@motherbee",
            "inbound_family": "user",
            "auth_mode": "shared-secret",
            "secret": "s3cr3t"
        }))
        .unwrap();
        let (ich, entry) = row_to_entry(row);
        assert_eq!(ich, "ich:1");
        assert_eq!(entry.owner_l2_name, "IO.cloud@motherbee");
        assert_eq!(entry.inbound_family, "user");

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
