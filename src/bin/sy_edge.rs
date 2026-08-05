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
    MSG_VAULT_SECRET_CHANGED, SYSTEM_KIND, VaultSecretChangedPayload, VaultSecretOp,
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

    // What TLS material we ended up actually serving. Declared out here, not inside the
    // frontend block, because the message loop below is what consults it: a role with no
    // public door still receives vault broadcasts and must NOT react to them.
    let mut live_tls = LiveTlsMaterial::NotFromVault;

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
        // Set when the failure is a transient "vault not up yet", so the fail-closed
        // message below does not blame the operator for a cold boot.
        let mut tls_failure_is_transient = false;
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
                Ok((cfg, version, diagnosis, fingerprint)) => {
                    log_tls_chain_diagnosis("vault", diagnosis);
                    live_tls = LiveTlsMaterial::FromVault {
                        version,
                        fingerprint: Some(fingerprint),
                    };
                    Some(Arc::new(cfg))
                }
                Err(err) => {
                    tls_failure_is_transient = err.is_transient();
                    if tls_failure_is_transient {
                        tracing::warn!(
                            error = %err.detail(),
                            "sy-edge: vault not reachable yet; systemd retries in ~5s (normal on cold boot)"
                        );
                    } else {
                        tracing::error!(error = %err.detail(), "sy-edge: TLS cert fetch from vault failed");
                    }
                    None
                }
            }
        } else if let (Some(cert), Some(key)) = (config.tls_cert.clone(), config.tls_key.clone()) {
            match load_tls_config(&cert, &key) {
                Ok((cfg, diagnosis)) => {
                    log_tls_chain_diagnosis("disk", diagnosis);
                    Some(Arc::new(cfg))
                }
                Err(err) => {
                    tracing::error!(error = %err, "sy-edge: TLS cert load from disk failed");
                    None
                }
            }
        } else {
            None
        };
        if tls_requested && tls.is_none() {
            if tls_failure_is_transient {
                // Same fail-closed exit, but do not send the operator hunting for a broken
                // secret: the material simply is not reachable yet.
                tracing::warn!(
                    "sy-edge: TLS material not available yet; refusing to bind the public listener \
                     in plaintext (fail-closed). systemd restarts in ~5s — expected during a cold \
                     boot, no operator action needed."
                );
            } else {
                tracing::error!(
                    "sy-edge: TLS requested but no valid cert loaded; refusing to bind the public \
                     listener in plaintext (fail-closed). Fix the vault secret / cert files and restart."
                );
            }
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
                        // TLS material is NOT covered by resolve_secrets: it is read exactly once
                        // at boot into an immutable `Arc<rustls::ServerConfig>` already moved into
                        // the running listener, so a rotated cert stayed invisible until someone
                        // restarted the unit by hand (observed in prod: vault at version 2 with the
                        // full chain while the edge still served the chainless version 1).
                        // React with exit(0) and let systemd restart us — the same contract
                        // sy.identity and sy.storage use for boot-only vault material
                        // (Model D' VA-J'-13). Hot-swapping via a rustls cert resolver would avoid
                        // the brief public-listener gap and is the better end state; it is tracked
                        // as PB-1 in lab/logbook/PENDING-BUGS.md.
                        //
                        // Matching is BY KEY here, deliberately diverging from the
                        // `(resource_type, tenant_id, ilk)` guidance on VaultSecretChangedPayload:
                        // that guidance is for consumers who resolve a secret by interest, whereas
                        // the edge is configured with one explicit `tls_vault_key` and fetches by
                        // that exact key. Key equality is precisely the condition that invalidates
                        // what we loaded.
                        let is_bootstrap = msg.meta.action.as_deref()
                            == Some(fluxbee_sdk::protocol::VAULT_BOOTSTRAP_ACTION);
                        match tls_secret_change_action(
                            &msg.payload,
                            config.tls_vault_key.as_deref(),
                            live_tls.clone(),
                            is_bootstrap,
                        ) {
                            TlsSecretChange::ReloadRequired { key, op, version } => {
                                // Prove the NEW material loads BEFORE surrendering the listener.
                                // Exiting first and discovering the problem on the way back up
                                // would turn one bad `vault_put` into a public HTTPS outage —
                                // the same asymmetry DeletedKeepServing already encodes.
                                //
                                // RETRIED on a transient failure. A single blip on the link to
                                // the vault would otherwise abandon a legitimate rotation
                                // outright: nothing re-fires this decision, so the edge would
                                // keep serving the old cert until it expired. The material
                                // itself is almost certainly fine — it is the fetch that failed.
                                let mut probe = Err(TlsFetchFailure::VaultUnavailable(
                                    "not attempted".to_string(),
                                ));
                                for attempt in 1..=TLS_RELOAD_PROBE_ATTEMPTS {
                                    probe = fetch_tls_config_from_vault(
                                        Arc::clone(&dispatcher),
                                        self_ilk.clone(),
                                        self_name.clone(),
                                        config.vault_hive.clone(),
                                        &key,
                                    )
                                    .await
                                    .map(|(_, _, diagnosis, _)| diagnosis);
                                    // Only a transient failure is worth retrying: bad material
                                    // will be just as bad next time.
                                    match &probe {
                                        Err(failure) if failure.is_transient() => {
                                            if attempt < TLS_RELOAD_PROBE_ATTEMPTS {
                                                tracing::warn!(
                                                    key = %key,
                                                    attempt = attempt,
                                                    detail = %failure.detail(),
                                                    "sy-edge: TLS reload probe could not reach the vault; retrying"
                                                );
                                                tokio::time::sleep(
                                                    std::time::Duration::from_secs(
                                                        TLS_RELOAD_PROBE_BACKOFF_SECS,
                                                    ),
                                                )
                                                .await;
                                            }
                                        }
                                        _ => break,
                                    }
                                }
                                match tls_reload_verdict(probe) {
                                    TlsReloadVerdict::RestartToLoad => {
                                        tracing::warn!(
                                            key = %key,
                                            op = %op,
                                            version = version,
                                            "sy-edge: TLS cert changed in vault; exiting(0) for systemd restart to load it (cannot hot-swap a live listener)"
                                        );
                                        // Flush before the process goes away.
                                        tokio::time::sleep(std::time::Duration::from_millis(250))
                                            .await;
                                        std::process::exit(0);
                                    }
                                    TlsReloadVerdict::KeepServing { detail, transient } => {
                                        tracing::error!(
                                            key = %key,
                                            op = %op,
                                            version = version,
                                            transient = transient,
                                            detail = %detail,
                                            "sy-edge: REFUSING to restart for the new TLS material — still serving the previously loaded cert. Fix the secret and re-put it (vault_rollback restores the last good version)."
                                        );
                                    }
                                }
                            }
                            TlsSecretChange::BootstrapAlreadyLoaded {
                                key,
                                version,
                                live_fingerprint,
                            } => {
                                // CONFIRM before suppressing. A vault rollback sets
                                // `current_version = previous_version` and the next put re-mints
                                // that same number, so a matching version can denote DIFFERENT
                                // material — and suppressing on it would leave the edge serving
                                // a cert the operator already replaced.
                                match fetch_tls_config_from_vault(
                                    Arc::clone(&dispatcher),
                                    self_ilk.clone(),
                                    self_name.clone(),
                                    config.vault_hive.clone(),
                                    &key,
                                )
                                .await
                                {
                                    Ok((_, _, _, fingerprint))
                                        if live_fingerprint.as_deref() == Some(&fingerprint) =>
                                    {
                                        tracing::info!(
                                            key = %key,
                                            version = version,
                                            "sy-edge: sy-vault re-announced at its own boot the material already live; not restarting"
                                        );
                                    }
                                    Ok(_) => {
                                        tracing::warn!(
                                            key = %key,
                                            version = version,
                                            "sy-edge: bootstrap announced the live version number but DIFFERENT material (a vault rollback reuses numbers); restarting to load it"
                                        );
                                        tokio::time::sleep(std::time::Duration::from_millis(250))
                                            .await;
                                        std::process::exit(0);
                                    }
                                    Err(failure) => {
                                        // Cannot confirm: keep serving. A bootstrap re-announce
                                        // is not urgent, and the next one will retry.
                                        tracing::warn!(
                                            key = %key,
                                            detail = %failure.detail(),
                                            "sy-edge: could not confirm the bootstrap-announced material; keeping the current cert"
                                        );
                                    }
                                }
                            }
                            TlsSecretChange::DeletedKeepServing { key } => {
                                tracing::error!(
                                    key = %key,
                                    "sy-edge: TLS cert DELETED from vault; still serving the previously loaded cert. A restart would leave this edge with no HTTPS frontend — restore the secret."
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
    #[allow(dead_code)]
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
const BEGIN_CERTIFICATE_MARKER: &[u8] = b"-----BEGIN CERTIFICATE-----";

/// How many times to re-probe the vault before giving up on a reload.
///
/// Nothing re-fires the reload decision, so abandoning it on the first transient error would
/// strand a legitimate cert rotation until the certificate expired.
const TLS_RELOAD_PROBE_ATTEMPTS: u32 = 3;
const TLS_RELOAD_PROBE_BACKOFF_SECS: u64 = 5;

/// What the PEM parse revealed about the certificate chain.
///
/// Reported instead of raised. The boot path is fail-closed under `Restart=always`, so turning a
/// truncated chain into an `Err` would take a currently-serving public HTTPS door DOWN and put it
/// in a crash loop the next time the unit restarts for any reason — a host reboot, a `.deb`
/// upgrade, an OOM. An edge up on a bad chain beats an edge that is down, so boot logs this
/// loudly and serves anyway. Only the reload path, which still holds a good listener, refuses to
/// act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TlsChainDiagnosis {
    /// `-----BEGIN CERTIFICATE-----` markers present in the PEM.
    markers: usize,
    /// Certificates the parser actually produced.
    parsed: usize,
}

impl TlsChainDiagnosis {
    /// The PEM held more certificates than the parser returned: some were dropped in silence.
    ///
    /// The real-world cause is a leaf's `END` glued to the next `BEGIN` on one line (CRLF
    /// mangling). openssl reads ZERO certs from such a file; `rustls_pemfile::certs` returns
    /// `Ok` with only the leaf and NO error, because its end-of-section test is satisfied by the
    /// glued line and the intermediate's body is then consumed outside any section. The edge
    /// serves a leaf-only chain and strict clients fail.
    fn truncated(&self) -> bool {
        self.markers != self.parsed
    }

    /// A chain with no intermediates.
    fn leaf_only(&self) -> bool {
        self.parsed == 1
    }

    /// One line an operator can act on, or `None` when the chain is healthy.
    fn complaint(&self) -> Option<String> {
        if self.truncated() {
            Some(format!(
                "TLS cert PEM is TRUNCATED: {} '-----BEGIN CERTIFICATE-----' markers but only {} \
                 certificate(s) parsed. Almost always an END/BEGIN glued on one line — openssl \
                 reads ZERO certs from such a file while rustls silently keeps just the leaf. \
                 Normalize the PEM (CRLF->LF, one marker per line) and re-put it.",
                self.markers, self.parsed
            ))
        } else if self.leaf_only() {
            Some(
                "TLS chain is a LEAF WITH NO INTERMEDIATES. Browsers may still succeed via AIA \
                 fetch, but strict clients (Go, Python ssl, Meta/Slack webhooks) WILL fail. \
                 Serve leaf + intermediates."
                    .to_string(),
            )
        } else {
            None
        }
    }
}

fn tls_config_from_pem(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<(rustls::ServerConfig, TlsChainDiagnosis), SyEdgeError> {
    // rustls 0.23 needs a process crypto provider before ServerConfig::builder()
    // (mirrors src/mesh_tls.rs). Idempotent: ignore "already installed".
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut cert_reader = cert_pem;
    let cert_chain: Vec<rustls::pki_types::CertificateDer<'static>> =
        rustls_pemfile::certs(&mut cert_reader).collect::<Result<_, _>>()?;
    if cert_chain.is_empty() {
        return Err("no certificates found in PEM cert material".into());
    }
    // SUBSTRING count, not a line-prefix count: the failure this exists to catch puts the second
    // marker in the middle of a line, so anything anchored to line starts cannot see it.
    let diagnosis = TlsChainDiagnosis {
        markers: cert_pem
            .windows(BEGIN_CERTIFICATE_MARKER.len())
            .filter(|w| *w == BEGIN_CERTIFICATE_MARKER)
            .count(),
        parsed: cert_chain.len(),
    };
    let mut key_reader = key_pem;
    let key = rustls_pemfile::private_key(&mut key_reader)?
        .ok_or("no private key found in PEM key material")?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    Ok((config, diagnosis))
}

/// Log whatever the chain diagnosis found, at boot, without refusing to serve.
fn log_tls_chain_diagnosis(source: &str, diagnosis: TlsChainDiagnosis) {
    match diagnosis.complaint() {
        Some(complaint) if diagnosis.truncated() => {
            tracing::error!(
                source = %source,
                markers = diagnosis.markers,
                parsed = diagnosis.parsed,
                "sy-edge: {complaint} SERVING ANYWAY (refusing to bind would take the public door down)."
            );
        }
        Some(complaint) => {
            tracing::warn!(source = %source, parsed = diagnosis.parsed, "sy-edge: {complaint}");
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

/// Build a rustls server config from PEM files on disk.
fn load_tls_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<(rustls::ServerConfig, TlsChainDiagnosis), SyEdgeError> {
    let cert_pem = std::fs::read(cert_path)?;
    let key_pem = std::fs::read(key_path)?;
    tls_config_from_pem(&cert_pem, &key_pem)
}

/// The TLS material this edge currently has live, for reload decisions.
///
/// `version` is what the vault reported on the BOOT fetch and must never be updated by a
/// reload probe: if a probe wrote its version back here, an edge that refused to load bad
/// material would silently claim to serve it, and the complaint would go quiet.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LiveTlsMaterial {
    /// Not serving vault-sourced TLS — no public frontend on this role, or the cert came from
    /// disk. Nothing a vault event can invalidate.
    NotFromVault,
    /// Serving vault-sourced TLS. `version` is what the vault reported on the boot fetch;
    /// `None` means it reported none, so we cannot compare and must treat every event as
    /// invalidating.
    ///
    /// `fingerprint` is a sha256 of the cert material actually loaded, and it is what the
    /// bootstrap check really compares. Version numbers alone are NOT a safe identity: a vault
    /// rollback sets `current_version = previous_version` (sy_vault.rs:1253) and the next put
    /// re-mints it, so within one vault.db a given number can denote different material at
    /// different times — and suppressing on a stale number would leave the edge serving a cert
    /// the operator already replaced.
    FromVault {
        version: Option<i64>,
        fingerprint: Option<String>,
    },
}

/// What a `VAULT_SECRET_CHANGED` broadcast means for this edge's TLS material.
#[derive(Debug, PartialEq, Eq)]
enum TlsSecretChange {
    /// New TLS material exists for our key; the running listener cannot pick it
    /// up, so the process must restart.
    ReloadRequired {
        key: String,
        op: &'static str,
        version: i64,
    },
    /// Our TLS key was deleted. Deliberately NOT a reload: restarting would fail
    /// closed with no cert and take the public HTTPS door down.
    DeletedKeepServing { key: String },
    /// The broadcast is about some other secret (or TLS-from-vault is not in use).
    Unrelated,
    /// The payload could not be parsed, so we cannot tell.
    Malformed { error: String },
    /// SY.vault restarted and re-announced, during ITS OWN boot, the exact cert version we
    /// already serve. Not a change. Reacting here recycled a perfectly healthy public listener
    /// every single time `sy-vault` bounced — an upgrade, a reboot, a manual restart — for a
    /// ~5 s gap in public HTTPS each time (PB-9).
    BootstrapAlreadyLoaded {
        key: String,
        version: i64,
        /// Of the material we currently serve. The caller re-fetches and compares before
        /// suppressing, because vault version numbers are reused across a rollback.
        live_fingerprint: Option<String>,
    },
}

/// Decide how to react to a `VAULT_SECRET_CHANGED` broadcast, given the vault key
/// this edge loaded its TLS material from (`None` when TLS is off or disk-sourced).
///
/// Split out from the message loop purely so the decision is testable — the caller
/// turns `ReloadRequired` into `exit(0)`.
fn tls_secret_change_action(
    payload: &serde_json::Value,
    tls_key: Option<&str>,
    live: LiveTlsMaterial,
    is_bootstrap: bool,
) -> TlsSecretChange {
    let Some(tls_key) = tls_key else {
        return TlsSecretChange::Unrelated;
    };
    // Stays ahead of every new check: a payload we cannot read must never be swallowed.
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
    if matches!(live, LiveTlsMaterial::NotFromVault) {
        // We hold nothing from the vault to invalidate. This also closes a second latent bounce:
        // a non-ingress edge has no `http_listen`, so it never fetches TLS at all, yet it can
        // still inherit `tls_vault_key` from the shared hive.yaml `edge:` section — and used to
        // exit(0) on a rotation while serving no door whatsoever.
        return TlsSecretChange::Unrelated;
    }
    // `!=`, never `>`: a vault rollback legitimately moves the version DOWN.
    if is_bootstrap {
        if let LiveTlsMaterial::FromVault {
            version: Some(live_version),
            fingerprint,
        } = live
        {
            if live_version == parsed.version {
                // A version match is NECESSARY but not sufficient: the caller confirms the
                // material is byte-identical before suppressing the reload.
                return TlsSecretChange::BootstrapAlreadyLoaded {
                    key: parsed.key,
                    version: parsed.version,
                    live_fingerprint: fingerprint,
                };
            }
        }
    }
    TlsSecretChange::ReloadRequired {
        key: parsed.key,
        op: parsed.op.as_str(),
        version: parsed.version,
    }
}

/// Whether a reload should actually surrender the running listener.
///
/// The reload path exits the process, so the NEW material must be proven loadable FIRST.
/// Without this, a `vault_put` of a broken chain turns a healthy edge into a crash loop with
/// the public door down — the same asymmetry `DeletedKeepServing` already encodes.
#[derive(Debug, PartialEq, Eq)]
enum TlsReloadVerdict {
    RestartToLoad,
    KeepServing { detail: String, transient: bool },
}

fn tls_reload_verdict(probe: Result<TlsChainDiagnosis, TlsFetchFailure>) -> TlsReloadVerdict {
    match probe {
        Ok(diagnosis) if diagnosis.truncated() => TlsReloadVerdict::KeepServing {
            detail: diagnosis.complaint().unwrap_or_default(),
            transient: false,
        },
        Ok(_) => TlsReloadVerdict::RestartToLoad,
        Err(failure) => TlsReloadVerdict::KeepServing {
            detail: failure.detail().to_string(),
            transient: failure.is_transient(),
        },
    }
}

/// Fetch the edge's TLS material from `SY.vault@<vault_hive>` (secret value
/// `{ "cert": "<PEM>", "key": "<PEM>" }`). Authorized by the dedicated-owner match
/// against the edge's deterministic ilk — no identity SHM needed on the edge hive.
/// Why the edge could not build its TLS material from the vault.
///
/// The distinction is the entire point. On an ingress hive there is no local `sy-vault`,
/// and systemd cannot order a cross-hive dependency — so `Restart=always` + `RestartSec=5`
/// IS the retry loop, deliberately (docs/edge-ingress-spec-v6.md §381-386). During a cold
/// boot the edge legitimately loses this race a few times before the mesh is up. Telling
/// the operator to "fix the vault secret" in those seconds is simply false: nothing is
/// broken, and the next restart resolves it. That false instruction was the only real
/// defect behind the "sy-edge crash-loops at cold boot" report.
enum TlsFetchFailure {
    /// No answer came back. Transient; systemd's restart is the retry.
    VaultUnavailable(String),
    /// The vault answered, and the material is missing or unusable. Needs a human.
    MaterialInvalid(String),
}

impl TlsFetchFailure {
    fn material(detail: impl Into<String>) -> Self {
        TlsFetchFailure::MaterialInvalid(detail.into())
    }

    fn from_vault_error(secret_key: &str, err: fluxbee_sdk::VaultError) -> Self {
        let detail = format!("vault get '{secret_key}' failed: {err}");
        match err {
            // Nothing came back: unreachable peer, or the request expired waiting.
            // Unreachable/TtlExceeded are TRANSPORT, not a verdict: the router answers them
            // SYNCHRONOUSLY (no timeout elapses), so without them here a single WAN blip during
            // a rotation exhausted zero retries and abandoned the reload for good.
            fluxbee_sdk::VaultError::Node(_)
            | fluxbee_sdk::VaultError::ActionTimeout { .. }
            | fluxbee_sdk::VaultError::Unreachable { .. }
            | fluxbee_sdk::VaultError::TtlExceeded { .. } => {
                TlsFetchFailure::VaultUnavailable(detail)
            }
            // The vault DID reach a verdict (or the answer was unusable). A restart
            // will reproduce this exactly — it needs the operator.
            _ => TlsFetchFailure::MaterialInvalid(detail),
        }
    }

    fn is_transient(&self) -> bool {
        matches!(self, TlsFetchFailure::VaultUnavailable(_))
    }

    fn detail(&self) -> &str {
        match self {
            TlsFetchFailure::VaultUnavailable(detail)
            | TlsFetchFailure::MaterialInvalid(detail) => detail,
        }
    }
}

async fn fetch_tls_config_from_vault(
    dispatcher: Arc<RouterDispatcher>,
    self_ilk: String,
    self_name: String,
    vault_hive: String,
    secret_key: &str,
) -> Result<(rustls::ServerConfig, Option<i64>, TlsChainDiagnosis, String), TlsFetchFailure> {
    let client = fluxbee_sdk::VaultClient::new(
        dispatcher,
        vault_hive,
        fluxbee_sdk::VaultCallerOwned::new(self_ilk, self_name),
    );
    let resp = client
        .get(secret_key, Duration::from_secs(15))
        .await
        .map_err(|err| TlsFetchFailure::from_vault_error(secret_key, err))?;
    let version = resp.version;
    let value = resp.value.ok_or_else(|| {
        TlsFetchFailure::material("vault returned no value for the edge tls secret")
    })?;
    let cert = value
        .get("cert")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TlsFetchFailure::material("edge tls secret is missing a 'cert' PEM field"))?;
    let key = value
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| TlsFetchFailure::material("edge tls secret is missing a 'key' PEM field"))?;
    let (config, diagnosis) = tls_config_from_pem(cert.as_bytes(), key.as_bytes())
        .map_err(|err| TlsFetchFailure::material(err.to_string()))?;
    // Identity of the MATERIAL, not of the version number the vault happens to have assigned
    // it. The number is reusable; the bytes are not.
    let fingerprint = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(cert.as_bytes());
        format!("{:x}", hasher.finalize())
    };
    Ok((config, version, diagnosis, fingerprint))
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
                let got = params.get("hub.verify_token").map(String::as_str).unwrap_or("");
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
                return http_error(StatusCode::FORBIDDEN, "FORBIDDEN", "webhook verify token mismatch");
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
    fn parse_query_params_decodes_webhook_challenge() {
        let m = parse_query_params(
            "hub.mode=subscribe&hub.verify_token=my%20token&hub.challenge=12345",
        );
        assert_eq!(m.get("hub.mode").map(String::as_str), Some("subscribe"));
        assert_eq!(m.get("hub.verify_token").map(String::as_str), Some("my token"));
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

    /// Regression: a rotated TLS cert used to be invisible to a running edge —
    /// resolve_secrets only refreshed channel secrets, so the vault could sit at
    /// version 2 with a full chain while the edge kept serving version 1. The
    /// broadcast must now demand a restart (sy.identity's Model D' VA-J'-13 contract).
    #[test]
    fn tls_cert_change_in_vault_demands_restart() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "put", 2),
                Some("edge_tls"),
                LiveTlsMaterial::FromVault { version: Some(1), fingerprint: Some("fp1".into()) },
                false,
            ),
            TlsSecretChange::ReloadRequired {
                key: "edge_tls".into(),
                op: "put",
                version: 2,
            }
        );
        for op in ["rotate", "rollback"] {
            assert!(matches!(
                tls_secret_change_action(
                    &vault_changed("edge_tls", op, 3),
                    Some("edge_tls"),
                    LiveTlsMaterial::FromVault { version: Some(1), fingerprint: Some("fp1".into()) },
                    false,
                ),
                TlsSecretChange::ReloadRequired { .. }
            ));
        }
    }

    /// A delete must NOT restart the edge: it would fail closed with no cert and
    /// turn one bad vault call into a public HTTPS outage.
    #[test]
    fn tls_cert_delete_never_restarts_the_public_door() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "delete", 0),
                Some("edge_tls"),
                LiveTlsMaterial::FromVault { version: Some(1), fingerprint: Some("fp1".into()) },
                false,
            ),
            TlsSecretChange::DeletedKeepServing {
                key: "edge_tls".into()
            }
        );
    }

    /// Other secrets rotating (postgres, slack, …) must never bounce the edge,
    /// and an edge not sourcing TLS from the vault ignores the broadcast entirely.
    #[test]
    fn unrelated_secret_changes_leave_the_edge_alone() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("pg_main", "put", 7),
                Some("edge_tls"),
                LiveTlsMaterial::FromVault { version: Some(1), fingerprint: Some("fp1".into()) },
                false,
            ),
            TlsSecretChange::Unrelated
        );
        // TLS off, or cert loaded from disk: nothing to invalidate.
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "put", 2),
                None,
                LiveTlsMaterial::NotFromVault,
                false,
            ),
            TlsSecretChange::Unrelated
        );
    }

    /// PB-9: `sy-vault` re-announces every secret it holds during ITS OWN boot. Reacting to
    /// that recycled a healthy public listener on every vault bounce — an upgrade, a reboot,
    /// a manual restart — for a ~5 s HTTPS gap each time.
    #[test]
    fn vault_bootstrap_of_the_version_we_already_serve_does_not_restart() {
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "put", 4),
                Some("edge_tls"),
                LiveTlsMaterial::FromVault {
                    version: Some(4),
                    fingerprint: Some("fp".into()),
                },
                true,
            ),
            TlsSecretChange::BootstrapAlreadyLoaded {
                key: "edge_tls".into(),
                version: 4,
                live_fingerprint: Some("fp".into()),
            }
        );
    }

    /// ...but a bootstrap announcing a DIFFERENT version is a real change we missed while
    /// down, and must still reload. Uses `!=`, never `>`: a vault rollback moves it DOWN.
    #[test]
    fn vault_bootstrap_of_another_version_still_reloads() {
        for live in [Some(3i64), Some(9i64), None] {
            assert!(
                matches!(
                    tls_secret_change_action(
                        &vault_changed("edge_tls", "put", 4),
                        Some("edge_tls"),
                        LiveTlsMaterial::FromVault {
                            version: live,
                            fingerprint: Some("fp".into()),
                        },
                        true,
                    ),
                    TlsSecretChange::ReloadRequired { .. }
                ),
                "live={live:?} must reload"
            );
        }
    }

    /// A role with no public door still receives vault broadcasts. It holds nothing from the
    /// vault, so it must never exit — it used to bounce while serving no listener at all.
    #[test]
    fn an_edge_serving_no_vault_tls_never_reacts() {
        for bootstrap in [true, false] {
            assert_eq!(
                tls_secret_change_action(
                    &vault_changed("edge_tls", "put", 2),
                    Some("edge_tls"),
                    LiveTlsMaterial::NotFromVault,
                    bootstrap,
                ),
                TlsSecretChange::Unrelated
            );
        }
        // ...but a delete is still reported as keep-serving, ahead of that check.
        assert_eq!(
            tls_secret_change_action(
                &vault_changed("edge_tls", "delete", 0),
                Some("edge_tls"),
                LiveTlsMaterial::NotFromVault,
                true,
            ),
            TlsSecretChange::DeletedKeepServing {
                key: "edge_tls".into()
            }
        );
    }

    /// PB-2, the case that started it: the operator's full-chain .crt had the leaf's END glued
    /// to the intermediate's BEGIN on one line. openssl reads ZERO certs from that file, while
    /// rustls returns Ok with just the leaf and no error — so the edge served a leaf-only chain
    /// and strict clients failed, with nothing in the logs.
    #[test]
    fn a_glued_end_begin_is_detected_as_a_truncated_chain() {
        let leaf = "-----BEGIN CERTIFICATE-----\nQUFB\n-----END CERTIFICATE-----";
        let inter = "-----BEGIN CERTIFICATE-----\nQkJC\n-----END CERTIFICATE-----\n";
        let glued = format!("{leaf}{inter}"); // no newline between END and BEGIN

        // The marker count must be a SUBSTRING count: the second marker is mid-line, so
        // anything anchored to line starts cannot see it.
        let markers = glued
            .as_bytes()
            .windows(BEGIN_CERTIFICATE_MARKER.len())
            .filter(|w| *w == BEGIN_CERTIFICATE_MARKER)
            .count();
        assert_eq!(markers, 2, "both markers must be visible in the raw bytes");

        let truncated = TlsChainDiagnosis { markers, parsed: 1 };
        assert!(truncated.truncated());
        assert!(truncated.complaint().unwrap().contains("TRUNCATED"));

        let healthy = TlsChainDiagnosis {
            markers: 3,
            parsed: 3,
        };
        assert!(!healthy.truncated() && !healthy.leaf_only());
        assert!(healthy.complaint().is_none());

        let leaf_only = TlsChainDiagnosis {
            markers: 1,
            parsed: 1,
        };
        assert!(!leaf_only.truncated() && leaf_only.leaf_only());
        assert!(leaf_only
            .complaint()
            .unwrap()
            .contains("NO INTERMEDIATES"));
    }

    /// Pins the EMPIRICAL premise the whole PB-2 design rests on, with real certificates:
    /// on a glued END/BEGIN, `rustls_pemfile` does NOT error — it returns Ok having silently
    /// kept only the leaf. (openssl, on the same bytes, reads zero certs.) If a future rustls
    /// bump starts erroring instead, this test fails and the marker gate can be reconsidered.
    #[test]
    fn rustls_silently_drops_the_intermediate_of_a_glued_chain() {
        let ca = json_router::mesh_tls::MeshCa::generate().expect("ca");
        let leaf = ca.issue_leaf("edge-test").expect("leaf");

        // Healthy: leaf + CA, properly separated.
        let clean = format!("{}{}", leaf.cert_pem, ca.ca_cert_pem());
        let (_, diagnosis) =
            tls_config_from_pem(clean.as_bytes(), leaf.key_pem.as_bytes()).expect("clean loads");
        assert_eq!(diagnosis.markers, 2);
        assert_eq!(diagnosis.parsed, 2, "a well-formed 2-cert chain parses fully");
        assert!(!diagnosis.truncated());

        // The operator's actual file: END glued to the next BEGIN, no newline between.
        let glued = format!(
            "{}{}",
            leaf.cert_pem.trim_end_matches('\n'),
            ca.ca_cert_pem()
        );
        let (_, diagnosis) = tls_config_from_pem(glued.as_bytes(), leaf.key_pem.as_bytes())
            .expect("glued STILL loads — that is the whole problem");
        assert_eq!(diagnosis.markers, 2, "both markers are in the bytes");
        assert_eq!(
            diagnosis.parsed, 1,
            "rustls kept only the leaf, with no error — the silent failure PB-2 is about"
        );
        assert!(
            diagnosis.truncated(),
            "and that is exactly what the gate must catch"
        );
    }

    /// The reload path exits the process, so it must prove the NEW material first. A bad
    /// `vault_put` must never be able to convert a healthy edge into a crash loop.
    #[test]
    fn a_reload_never_surrenders_the_listener_for_material_that_does_not_load() {
        assert_eq!(
            tls_reload_verdict(Ok(TlsChainDiagnosis {
                markers: 3,
                parsed: 3
            })),
            TlsReloadVerdict::RestartToLoad
        );
        // A leaf-only chain is a warning, not a reason to refuse: it is what the operator
        // asked for and it does serve.
        assert_eq!(
            tls_reload_verdict(Ok(TlsChainDiagnosis {
                markers: 1,
                parsed: 1
            })),
            TlsReloadVerdict::RestartToLoad
        );
        assert!(matches!(
            tls_reload_verdict(Ok(TlsChainDiagnosis {
                markers: 2,
                parsed: 1
            })),
            TlsReloadVerdict::KeepServing { .. }
        ));
        assert!(matches!(
            tls_reload_verdict(Err(TlsFetchFailure::material("bad pem"))),
            TlsReloadVerdict::KeepServing {
                transient: false,
                ..
            }
        ));
        assert!(matches!(
            tls_reload_verdict(Err(TlsFetchFailure::VaultUnavailable("down".into()))),
            TlsReloadVerdict::KeepServing {
                transient: true,
                ..
            }
        ));

        // The edge's vault is cross-hive by default, so a WAN blip makes the ROUTER answer
        // UNREACHABLE synchronously — no timeout elapses. Classifying that as bad material
        // burned zero retries and abandoned a legitimate rotation until the cert expired.
        for transport in [
            fluxbee_sdk::VaultError::Unreachable {
                reason: "GATEWAY_UNAVAILABLE".into(),
                original_dst: "SY.vault@motherbee".into(),
            },
            fluxbee_sdk::VaultError::TtlExceeded {
                original_dst: "SY.vault@motherbee".into(),
                last_hop: "ingress1".into(),
            },
        ] {
            let failure = TlsFetchFailure::from_vault_error("edge_tls", transport);
            assert!(
                failure.is_transient(),
                "transport failures must be retried, not reported as broken material"
            );
            assert!(matches!(
                tls_reload_verdict(Err(failure)),
                TlsReloadVerdict::KeepServing {
                    transient: true,
                    ..
                }
            ));
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
                LiveTlsMaterial::FromVault { version: Some(1), fingerprint: Some("fp1".into()) },
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
