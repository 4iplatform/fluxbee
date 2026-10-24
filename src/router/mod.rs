pub mod stream;
pub mod system_policy;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{self, Duration};
use uuid::Uuid;

use crate::config::RouterConfig;
use crate::opa::OpaResolver;
use crate::shm::{
    copy_bytes_with_len, memory_shm_name_for_hive, now_epoch_ms, sha256_bytes_is_zero,
    sha256_bytes_to_hex, ConfigRegionReader, ConfigSnapshot, IdentityRegionReader, LsaRegionReader,
    LsaRegionWriter, LsaSnapshot, MemoryRegionReader, MemoryShmSnapshot, OpaRegionReader,
    OpaSnapshot, RemoteHiveEntry, RemoteNodeEntry, RemoteRouteEntry, RemoteTapEntry,
    RemoteVpnEntry, RouterRegionReader, RouterRegionWriter, ShmError, TapEntry, VpnAssignment,
    ACTION_DROP, ACTION_FORWARD, FLAG_ACTIVE, FLAG_DELETED, FLAG_FROZEN, FLAG_STALE,
    HEARTBEAT_STALE_MS, HIVE_FLAG_SELF, MATCH_EXACT, MATCH_GLOB, MATCH_PREFIX, OPA_STATUS_ERROR,
    OPA_STATUS_LOADING, OPA_STATUS_OK,
};
use fluxbee_sdk::mesh::{
    install_crypto_provider, peer_hive_from_cert, HiveRegistry, MeshTlsMaterial,
};
use fluxbee_sdk::protocol::{
    build_announce, build_lsa, build_router_hello, build_ttl_exceeded, build_unreachable,
    build_wan_accept, build_wan_hello, build_wan_reachability, build_wan_reject, Destination,
    LsaNode, LsaPayload, LsaRoute, LsaTap, LsaVpn, MemoryPackage, Message, Meta,
    NodeAnnouncePayload, NodeHelloPayload, OpaReloadPayload, RouterHelloPayload, WanAcceptPayload,
    WanHelloPayload, WanNegotiated, WanReachabilityEntry, WanReachabilityPayload, WanRejectPayload,
    WanTimers, MESH_PROTOCOL_VERSION, MSG_EDGE_ACME_CHALLENGE, MSG_EDGE_ACME_CHALLENGE_RESPONSE,
    MSG_EDGE_CLOSE_URL, MSG_EDGE_CLOSE_URL_RESPONSE, MSG_EDGE_LIST_URLS,
    MSG_EDGE_LIST_URLS_RESPONSE, MSG_EDGE_OPEN_URL, MSG_EDGE_OPEN_URL_RESPONSE,
    MSG_EDGE_PUBLISH_BLOB, MSG_EDGE_PUBLISH_BLOB_RESPONSE, MSG_EDGE_TLS_RELOAD,
    MSG_EDGE_TLS_RELOAD_RESPONSE, MSG_EDGE_TLS_STATUS, MSG_EDGE_TLS_STATUS_RESPONSE,
    MSG_EDGE_UNPUBLISH_BLOB, MSG_EDGE_UNPUBLISH_BLOB_RESPONSE, MSG_EDGE_WEB_ROUTES,
    MSG_EDGE_WEB_ROUTES_RESPONSE, MSG_HELLO, MSG_LSA, MSG_OPA_RELOAD, MSG_TTL_EXCEEDED,
    MSG_UNREACHABLE, MSG_VAULT_SECRET_CHANGED, MSG_WAN_REACHABILITY, MSG_WAN_REJECT, MSG_WITHDRAW,
    SCOPE_GLOBAL, SYSTEM_KIND,
};
use fluxbee_sdk::socket::connection::{read_frame, write_frame, MAX_FRAME_SIZE};
use fluxbee_sdk::{classify_routed_message, MSG_VAULT_GET, MSG_VAULT_GET_RESPONSE};

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("opa error: {0}")]
    Opa(#[from] crate::opa::OpaError),
    #[error("startup error: {0}")]
    Startup(String),
}

pub struct Router {
    cfg: RouterConfig,
    shm: Arc<Mutex<RouterRegionWriter>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    lsa_writer: Arc<Mutex<Option<LsaRegionWriter>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    tap_rules: Arc<Mutex<Vec<TapEntry>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_version: Arc<Mutex<u64>>,
    opa: Arc<Mutex<OpaResolver>>,
    opa_reader: Arc<Mutex<Option<OpaRegionReader>>>,
    memory_reader: Arc<Mutex<Option<MemoryRegionReader>>>,
    broadcast_cache: Arc<Mutex<BroadcastCache>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_state: Arc<Mutex<std::collections::HashMap<String, RemoteHiveState>>>,
    /// Hub-vouched multi-hop reachability table (Option B). Populated ONLY on spokes, from the
    /// hub's `MSG_WAN_REACHABILITY` advertisements. Kept separate from `lsa_state`/the LSA snapshot.
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    lsa_seq: Arc<Mutex<u64>>,
    /// This router's stream (docs/router-stream-v1.md, A-70): the turn copies and the nodes'
    /// appends go here, for SY.storage.
    stream: stream::StreamHandle,
    /// The stream's node, started once the router listens.
    stream_runtime: std::sync::Mutex<Option<stream::StreamRuntime>>,
}

const LSA_REJECT_HIVE_MISMATCH: &str = "hive_mismatch";
const LSA_REJECT_STALE_SEQ: &str = "stale_seq";
const LSA_REJECT_PARSE_ERROR: &str = "parse_error";
const LSA_REJECT_NOT_REGISTERED: &str = "not_registered";
/// A node an LSA lists under another hive's name: dropped, the rest of the LSA applies.
const LSA_REJECT_FOREIGN_NODE: &str = "foreign_node";
/// A node an LSA, or the hub's reachability, lists under a UUID that is already taken: by a node of
/// this hive (dropped) or by another hive's node (then no hive resolves that UUID).
const LSA_REJECT_DUPLICATE_UUID: &str = "duplicate_uuid";
/// A WAN frame whose sender is neither a node of the peer that sent it nor one that peer relays as
/// the hub: dropped.
const LSA_REJECT_FOREIGN_SOURCE: &str = "foreign_source";
impl Router {
    pub fn new(cfg: RouterConfig) -> Self {
        let shm = RouterRegionWriter::open_or_create(
            &cfg.shm_name,
            cfg.router_uuid,
            &cfg.hive_id,
            &cfg.router_l2_name,
            cfg.is_gateway,
        )
        .expect("shm init");
        let config_reader =
            ConfigRegionReader::open_read_only(&format!("/jsr-config-{}", cfg.hive_id)).ok();
        let lsa_reader = LsaRegionReader::open_read_only(&format!("/jsr-lsa-{}", cfg.hive_id)).ok();
        let lsa_writer = if cfg.is_gateway {
            LsaRegionWriter::open_or_create(
                &format!("/jsr-lsa-{}", cfg.hive_id),
                cfg.router_uuid,
                &cfg.hive_id,
            )
            .ok()
        } else {
            None
        };
        let opa = Arc::new(Mutex::new(OpaResolver::new()));
        let memory_reader = match memory_shm_name_for_hive(&cfg.hive_id) {
            Ok(name) => MemoryRegionReader::open_read_only(&name).ok(),
            Err(_) => None,
        };
        let (stream, stream_runtime) = stream::open(
            &cfg.router_l2_name,
            &cfg.hive_id,
            &cfg.hive_role,
            &cfg.state_dir,
            &cfg.config_dir,
            &cfg.node_socket_path,
        );
        Self {
            cfg,
            shm: Arc::new(Mutex::new(shm)),
            nodes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            peer_nodes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            peers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            peer_routers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            config_reader: Arc::new(Mutex::new(config_reader)),
            lsa_reader: Arc::new(Mutex::new(lsa_reader)),
            lsa_writer: Arc::new(Mutex::new(lsa_writer)),
            lsa_snapshot: Arc::new(Mutex::new(None)),
            static_routes: Arc::new(Mutex::new(Vec::new())),
            vpn_rules: Arc::new(Mutex::new(Vec::new())),
            tap_rules: Arc::new(Mutex::new(Vec::new())),
            fib: Arc::new(Mutex::new(Vec::new())),
            config_version: Arc::new(Mutex::new(0)),
            opa,
            opa_reader: Arc::new(Mutex::new(None)),
            memory_reader: Arc::new(Mutex::new(memory_reader)),
            broadcast_cache: Arc::new(Mutex::new(BroadcastCache::new())),
            wan_peers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            lsa_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            reachability: Arc::new(Mutex::new(std::collections::HashMap::new())),
            lsa_seq: Arc::new(Mutex::new(0)),
            stream,
            stream_runtime: std::sync::Mutex::new(Some(stream_runtime)),
        }
    }

    pub async fn run(&self) -> Result<(), RouterError> {
        // Fail closed: the SYSTEM authority + routing policies are baked into the binary
        // (policy/system.wasm + policy/system_route.wasm). If either cannot load/evaluate, the
        // router has no business running — there is no Rust fallback table, the Rego is the single
        // source of truth. Refuse to start rather than silently degrade.
        system_policy::ensure_system_policy_loaded().map_err(RouterError::Startup)?;

        // SAFETY: geteuid has no preconditions and cannot fail.
        let own_uid = unsafe { nix::libc::geteuid() };
        let listener = listen_local_socket(&self.cfg.node_socket_path)?;
        // The stream joins this router as a node once it listens; never awaited here, since the
        // router accepts nodes only at the end of `run` (§2.1).
        if let Some(runtime) = self
            .stream_runtime
            .lock()
            .ok()
            .and_then(|mut slot| slot.take())
        {
            std::mem::drop(tokio::spawn(stream::run_node(runtime)));
        }
        tracing::info!(
            router = %self.cfg.router_l2_name,
            socket = %self.cfg.node_socket_path.display(),
            "router listening"
        );

        let irp_path = peer_socket_path(&self.cfg.node_socket_dir, self.cfg.router_uuid);
        let irp_listener = listen_local_socket(&irp_path)?;
        tracing::info!(
            router = %self.cfg.router_l2_name,
            socket = %irp_path.display(),
            "irp listening"
        );

        let shm = Arc::clone(&self.shm);
        let opa = Arc::clone(&self.opa);
        let opa_reader = Arc::clone(&self.opa_reader);
        let hive_id = self.cfg.hive_id.clone();
        let heartbeat_interval = self.cfg.heartbeat_interval_ms;
        let config_reader = Arc::clone(&self.config_reader);
        let static_routes = Arc::clone(&self.static_routes);
        let vpn_rules = Arc::clone(&self.vpn_rules);
        let tap_rules = Arc::clone(&self.tap_rules);
        let config_version = Arc::clone(&self.config_version);
        let nodes_hb = Arc::clone(&self.nodes);
        let peer_nodes_hb = Arc::clone(&self.peer_nodes);
        let fib_hb = Arc::clone(&self.fib);
        let lsa_snapshot_hb = Arc::clone(&self.lsa_snapshot);
        let reachability_hb = Arc::clone(&self.reachability);
        tokio::spawn(async move {
            let mut ticker = time::interval(Duration::from_millis(heartbeat_interval));
            loop {
                ticker.tick().await;
                maybe_refresh_opa_from_shm(&opa_reader, &opa, &shm, &hive_id).await;
                // Routes, VPNs and taps are otherwise re-read only on frames from this router's
                // own nodes: a router with none (a pure gateway on a hive with several routers)
                // would route and advertise stale config. Version-gated: a no-op when unchanged.
                let _ = refresh_config(
                    &config_reader,
                    &static_routes,
                    &vpn_rules,
                    &tap_rules,
                    &config_version,
                    &hive_id,
                    &nodes_hb,
                    &peer_nodes_hb,
                    &shm,
                    &fib_hb,
                    &lsa_snapshot_hb,
                    &reachability_hb,
                    false,
                )
                .await;
                let (policy_version, load_status) = {
                    let opa = opa.lock().await;
                    opa.status()
                };
                let mut shm = shm.lock().await;
                shm.update_heartbeat();
                shm.update_opa_status(policy_version, load_status);
            }
        });

        let peers_pd = Arc::clone(&self.peers);
        let peer_nodes_pd = Arc::clone(&self.peer_nodes);
        let peer_routers_pd = Arc::clone(&self.peer_routers);
        let nodes_pd = Arc::clone(&self.nodes);
        let wan_peers_pd = Arc::clone(&self.wan_peers);
        let static_routes_pd = Arc::clone(&self.static_routes);
        let lsa_snapshot_pd = Arc::clone(&self.lsa_snapshot);
        let reachability_pd = Arc::clone(&self.reachability);
        let fib_pd = Arc::clone(&self.fib);
        let lsa_reader_pd = Arc::clone(&self.lsa_reader);
        let shm_pd = Arc::clone(&self.shm);
        let opa_pd = Arc::clone(&self.opa);
        let opa_reader_pd = Arc::clone(&self.opa_reader);
        let router_uuid = self.cfg.router_uuid;
        let router_name = self.cfg.router_l2_name.clone();
        let shm_name = self.cfg.shm_name.clone();
        let hive_id = self.cfg.hive_id.clone();
        let identity_frontdesk_node_name = self.cfg.identity_frontdesk_node_name.clone();
        let is_gateway = self.cfg.is_gateway;
        let peer_socket_dir = self.cfg.node_socket_dir.clone();
        let log_stream_pd = self.stream.clone();
        let memory_reader_pd = Arc::clone(&self.memory_reader);
        tokio::spawn(async move {
            peer_discovery_loop(
                router_uuid,
                &router_name,
                &shm_name,
                &hive_id,
                &identity_frontdesk_node_name,
                peer_socket_dir,
                peers_pd,
                peer_nodes_pd,
                peer_routers_pd,
                nodes_pd,
                wan_peers_pd,
                lsa_reader_pd,
                static_routes_pd,
                shm_pd,
                opa_pd,
                opa_reader_pd,
                memory_reader_pd,
                lsa_snapshot_pd,
                reachability_pd,
                fib_pd,
                log_stream_pd,
                is_gateway,
            )
            .await;
        });

        let lsa_reader = Arc::clone(&self.lsa_reader);
        let lsa_snapshot = Arc::clone(&self.lsa_snapshot);
        let nodes = Arc::clone(&self.nodes);
        let peer_nodes = Arc::clone(&self.peer_nodes);
        let static_routes = Arc::clone(&self.static_routes);
        let fib = Arc::clone(&self.fib);
        let reachability = Arc::clone(&self.reachability);
        let hive_id = self.cfg.hive_id.clone();
        tokio::spawn(async move {
            lsa_refresh_loop(
                lsa_reader,
                lsa_snapshot,
                &hive_id,
                nodes,
                peer_nodes,
                static_routes,
                fib,
                reachability,
            )
            .await;
        });

        let peers = Arc::clone(&self.peers);
        let peer_nodes = Arc::clone(&self.peer_nodes);
        let peer_routers = Arc::clone(&self.peer_routers);
        let nodes = Arc::clone(&self.nodes);
        let fib = Arc::clone(&self.fib);
        let lsa_reader = Arc::clone(&self.lsa_reader);
        let static_routes = Arc::clone(&self.static_routes);
        let shm = Arc::clone(&self.shm);
        let opa = Arc::clone(&self.opa);
        let opa_reader = Arc::clone(&self.opa_reader);
        let memory_reader = Arc::clone(&self.memory_reader);
        let hive_id = self.cfg.hive_id.clone();
        let identity_frontdesk_node_name = self.cfg.identity_frontdesk_node_name.clone();
        let wan_peers = Arc::clone(&self.wan_peers);
        let lsa_snapshot = Arc::clone(&self.lsa_snapshot);
        let reachability = Arc::clone(&self.reachability);
        let log_stream = self.stream.clone();
        let is_gateway = self.cfg.is_gateway;
        let router_uuid = self.cfg.router_uuid;
        tokio::spawn(async move {
            loop {
                let (stream, _) = match irp_listener.accept().await {
                    Ok(result) => result,
                    Err(err) => {
                        tracing::warn!("irp accept error: {err}");
                        continue;
                    }
                };
                if !local_peer_admitted(&stream, own_uid) {
                    continue;
                }
                let peers = Arc::clone(&peers);
                let peer_nodes = Arc::clone(&peer_nodes);
                let peer_routers = Arc::clone(&peer_routers);
                let nodes = Arc::clone(&nodes);
                let fib = Arc::clone(&fib);
                let static_routes = Arc::clone(&static_routes);
                let shm = Arc::clone(&shm);
                let opa = Arc::clone(&opa);
                let opa_reader = Arc::clone(&opa_reader);
                let memory_reader = Arc::clone(&memory_reader);
                let hive_id = hive_id.clone();
                let identity_frontdesk_node_name = identity_frontdesk_node_name.clone();
                let wan_peers = Arc::clone(&wan_peers);
                let lsa_reader = Arc::clone(&lsa_reader);
                let lsa_snapshot = Arc::clone(&lsa_snapshot);
                let reachability = Arc::clone(&reachability);
                let log_stream = log_stream.clone();
                let is_gateway = is_gateway;
                tokio::spawn(async move {
                    if let Err(err) = handle_peer_incoming(
                        stream,
                        router_uuid,
                        peers,
                        peer_nodes,
                        peer_routers,
                        nodes,
                        fib,
                        lsa_reader,
                        static_routes,
                        shm,
                        opa,
                        opa_reader,
                        memory_reader,
                        hive_id,
                        identity_frontdesk_node_name,
                        wan_peers,
                        lsa_snapshot,
                        reachability,
                        log_stream,
                        is_gateway,
                    )
                    .await
                    {
                        tracing::warn!("peer connection error: {err}");
                    }
                });
            }
        });

        if self.cfg.is_gateway {
            // Load WAN mTLS material when configured. On a load/build failure the
            // behaviour is mode-dependent and FAIL-CLOSED for `required`:
            // - required  -> keep mode `required` with NO material, so every WAN
            //   connection is rejected (the TLS handshake can't run). A missing
            //   cert is a loud, total outage — NOT a silent downgrade to plaintext
            //   that an attacker could force by deleting the cert files. (motherbee
            //   always has certs by boot; workers get them at add_hive, so this
            //   only bites on a genuine misconfig/attack.)
            // - permissive -> degrade to plaintext (the lenient rollout mode).
            let (wan_tls, effective_mtls_mode) = if self.cfg.wan_mtls_mode != "disabled" {
                install_crypto_provider();
                let degraded = if self.cfg.wan_mtls_mode == "required" {
                    "required".to_string() // fail-closed: no plaintext fallback
                } else {
                    "disabled".to_string()
                };
                match MeshTlsMaterial::load_from_dir(&self.cfg.tls_dir) {
                    Ok(mat) => match (mat.server_config(), mat.client_config()) {
                        (Ok(sc), Ok(cc)) => (
                            Some(WanTls {
                                acceptor: tokio_rustls::TlsAcceptor::from(sc),
                                connector: tokio_rustls::TlsConnector::from(cc),
                            }),
                            self.cfg.wan_mtls_mode.clone(),
                        ),
                        _ => {
                            tracing::error!(
                                mode = %self.cfg.wan_mtls_mode,
                                "wan.mtls set but TLS config build failed; WAN unavailable (required=fail-closed)"
                            );
                            (None, degraded)
                        }
                    },
                    Err(err) => {
                        if self.cfg.wan_mtls_mode == "required" {
                            tracing::error!(
                                error = %err,
                                dir = %self.cfg.tls_dir.display(),
                                "wan.mtls=required but no TLS material on disk; WAN FAIL-CLOSED (all peers rejected). A hive without its leaf is joined again (add_hive)."
                            );
                        } else {
                            tracing::warn!(
                                error = %err,
                                dir = %self.cfg.tls_dir.display(),
                                mode = %self.cfg.wan_mtls_mode,
                                "wan.mtls=permissive but no TLS material; WAN runs plaintext for now"
                            );
                        }
                        (None, degraded)
                    }
                }
            } else {
                (None, "disabled".to_string())
            };
            tracing::info!(mode = %effective_mtls_mode, "wan mtls mode");
            let ctx = Arc::new(WanContext {
                router_uuid: self.cfg.router_uuid,
                router_name: self.cfg.router_l2_name.clone(),
                hive_id: self.cfg.hive_id.clone(),
                identity_frontdesk_node_name: self.cfg.identity_frontdesk_node_name.clone(),
                hello_interval_ms: self.cfg.hello_interval_ms,
                dead_interval_ms: self.cfg.dead_interval_ms,
                hive_registry: (self.cfg.hive_role == "motherbee").then(HiveRegistry::motherbee),
                hive_registry_recheck: HIVE_REGISTRY_RECHECK,
                nodes: Arc::clone(&self.nodes),
                peer_nodes: Arc::clone(&self.peer_nodes),
                peer_routers: Arc::clone(&self.peer_routers),
                peers: Arc::clone(&self.peers),
                wan_peers: Arc::clone(&self.wan_peers),
                lsa_state: Arc::clone(&self.lsa_state),
                reachability: Arc::clone(&self.reachability),
                lsa_writer: Arc::clone(&self.lsa_writer),
                lsa_snapshot: Arc::clone(&self.lsa_snapshot),
                static_routes: Arc::clone(&self.static_routes),
                vpn_rules: Arc::clone(&self.vpn_rules),
                tap_rules: Arc::clone(&self.tap_rules),
                fib: Arc::clone(&self.fib),
                broadcast_cache: Arc::clone(&self.broadcast_cache),
                lsa_seq: Arc::clone(&self.lsa_seq),
                log_stream: self.stream.clone(),
                memory_reader: Arc::clone(&self.memory_reader),
                opa: Arc::clone(&self.opa),
                wan_session_epochs: Arc::new(Mutex::new(std::collections::HashMap::new())),
                lsa_reject_counters: Arc::new(Mutex::new(std::collections::HashMap::new())),
                wan_mtls_mode: effective_mtls_mode,
                wan_tls,
            });
            if let Some(listen) = self.cfg.wan_listen.clone() {
                let ctx_listen = Arc::clone(&ctx);
                tokio::spawn(async move {
                    wan_listen_loop(listen, ctx_listen).await;
                });
            }
            for uplink in self.cfg.wan_uplinks.clone() {
                let ctx_uplink = Arc::clone(&ctx);
                tokio::spawn(async move {
                    wan_connect_loop(uplink, ctx_uplink).await;
                });
            }
            let ctx_broadcast = Arc::clone(&ctx);
            tokio::spawn(async move {
                lsa_broadcast_loop(ctx_broadcast).await;
            });
            let ctx_stale = Arc::clone(&ctx);
            tokio::spawn(async move {
                lsa_stale_loop(ctx_stale).await;
            });
        }

        loop {
            let (stream, _) = listener.accept().await?;
            if !local_peer_admitted(&stream, own_uid) {
                continue;
            }
            tracing::info!("incoming connection accepted");
            let shm = Arc::clone(&self.shm);
            let router_name = self.cfg.router_l2_name.clone();
            let router_uuid = self.cfg.router_uuid;
            let hive_id = self.cfg.hive_id.clone();
            let nodes = Arc::clone(&self.nodes);
            let peer_nodes = Arc::clone(&self.peer_nodes);
            let peer_routers = Arc::clone(&self.peer_routers);
            let peers = Arc::clone(&self.peers);
            let wan_peers = Arc::clone(&self.wan_peers);
            let config_reader = Arc::clone(&self.config_reader);
            let lsa_reader = Arc::clone(&self.lsa_reader);
            let lsa_snapshot = Arc::clone(&self.lsa_snapshot);
            let reachability = Arc::clone(&self.reachability);
            let static_routes = Arc::clone(&self.static_routes);
            let vpn_rules = Arc::clone(&self.vpn_rules);
            let tap_rules = Arc::clone(&self.tap_rules);
            let fib = Arc::clone(&self.fib);
            let config_version = Arc::clone(&self.config_version);
            let opa = Arc::clone(&self.opa);
            let opa_reader = Arc::clone(&self.opa_reader);
            let broadcast_cache = Arc::clone(&self.broadcast_cache);
            let lsa_seq = Arc::clone(&self.lsa_seq);
            let memory_reader = Arc::clone(&self.memory_reader);
            let log_stream = self.stream.clone();
            let is_gateway = self.cfg.is_gateway;
            let identity_frontdesk_node_name = self.cfg.identity_frontdesk_node_name.clone();
            tokio::spawn(async move {
                if let Err(err) = handle_node(
                    stream,
                    shm,
                    nodes,
                    peer_nodes,
                    peer_routers,
                    peers,
                    wan_peers,
                    config_reader,
                    lsa_reader,
                    lsa_snapshot,
                    reachability,
                    static_routes,
                    vpn_rules,
                    tap_rules,
                    fib,
                    config_version,
                    opa,
                    opa_reader,
                    broadcast_cache,
                    lsa_seq,
                    memory_reader,
                    log_stream,
                    router_uuid,
                    &router_name,
                    &hive_id,
                    &identity_frontdesk_node_name,
                    is_gateway,
                )
                .await
                {
                    tracing::warn!("node connection error: {err}");
                }
            });
        }
    }
}

async fn handle_node(
    stream: UnixStream,
    shm: Arc<Mutex<RouterRegionWriter>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    tap_rules: Arc<Mutex<Vec<TapEntry>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_version: Arc<Mutex<u64>>,
    opa: Arc<Mutex<OpaResolver>>,
    opa_reader: Arc<Mutex<Option<OpaRegionReader>>>,
    broadcast_cache: Arc<Mutex<BroadcastCache>>,
    lsa_seq: Arc<Mutex<u64>>,
    memory_reader: Arc<Mutex<Option<MemoryRegionReader>>>,
    log_stream: stream::StreamHandle,
    router_uuid: Uuid,
    router_name: &str,
    hive_id: &str,
    identity_frontdesk_node_name: &str,
    is_gateway: bool,
) -> Result<(), RouterError> {
    // Who opened this socket, for the `RT.` names only the router's own process may take.
    let peer_pid = stream.peer_cred().ok().and_then(|cred| cred.pid());
    let (mut reader, writer) = stream.into_split();
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let link_peer = Arc::new(std::sync::OnceLock::<String>::new());
    let writer_task = tokio::spawn(write_link_frames(
        writer,
        rx,
        "node",
        Arc::clone(&link_peer),
    ));
    let frame = read_frame(&mut reader)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing HELLO"))?;

    let msg: Message = serde_json::from_slice(&frame)?;
    if !is_system_kind(&msg.meta.msg_type) || msg.meta.msg.as_deref() != Some(MSG_HELLO) {
        return Err(RouterError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected HELLO",
        )));
    }
    tracing::info!(src = %msg.routing.src, "hello received");
    let payload: NodeHelloPayload = serde_json::from_value(msg.payload)?;
    let src_uuid = Uuid::parse_str(&msg.routing.src)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid routing.src"))?;
    let node_uuid = Uuid::parse_str(&payload.uuid)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid payload.uuid"))?;
    if node_uuid != src_uuid {
        return Err(RouterError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "uuid mismatch",
        )));
    }

    let node_name = normalize_name(&payload.name, hive_id);
    if !rt_name_allowed(&node_name, peer_pid, std::process::id()) {
        tracing::warn!(
            node = %node_name,
            peer_pid = ?peer_pid,
            "HELLO rejected: RT.* names belong to the router's own process"
        );
        writer_task.abort();
        return Err(RouterError::Io(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "RT.* names are reserved for the router's own process",
        )));
    }
    let _ = link_peer.set(node_name.clone());
    tracing::info!(node = %node_name, "loading config snapshot");
    let snapshot = refresh_config(
        &config_reader,
        &static_routes,
        &vpn_rules,
        &tap_rules,
        &config_version,
        hive_id,
        &nodes,
        &peer_nodes,
        &shm,
        &fib,
        &lsa_snapshot,
        &reachability,
        false,
    )
    .await;
    {
        let lsa_reader = Arc::clone(&lsa_reader);
        let lsa_snapshot = Arc::clone(&lsa_snapshot);
        let nodes = Arc::clone(&nodes);
        let peer_nodes = Arc::clone(&peer_nodes);
        let static_routes = Arc::clone(&static_routes);
        let fib = Arc::clone(&fib);
        let reachability = Arc::clone(&reachability);
        let hive_id = hive_id.to_string();
        tokio::spawn(async move {
            let _ = refresh_lsa(
                &lsa_reader,
                &lsa_snapshot,
                &hive_id,
                &nodes,
                &peer_nodes,
                &static_routes,
                &fib,
                &reachability,
            )
            .await;
        });
    }
    let vpn_id = assign_vpn(&node_name, snapshot.as_ref());
    tracing::info!(node = %node_name, vpn_id = vpn_id, "vpn assigned");
    let connected_at = now_epoch_ms();
    let handle = NodeHandle {
        name: node_name.clone(),
        vpn_id,
        sender: tx.clone(),
        connected_at,
    };
    if let Err(holder) = claim_node_uuid(&nodes, node_uuid, handle, DUPLICATE_HELLO_GRACE).await {
        tracing::warn!(
            node = %node_name,
            uuid = %node_uuid,
            connected_as = %holder,
            "HELLO rejected: this node UUID already has a live connection"
        );
        writer_task.abort();
        return Err(RouterError::Io(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("node uuid {node_uuid} already connected as {holder}"),
        )));
    }
    {
        let mut shm = shm.lock().await;
        if let Err(err) = shm.register_node(node_uuid, &node_name, vpn_id, connected_at) {
            drop(shm);
            let mut nodes = nodes.lock().await;
            if nodes
                .get(&node_uuid)
                .is_some_and(|handle| handle.connected_at == connected_at)
            {
                nodes.remove(&node_uuid);
            }
            writer_task.abort();
            return Err(io::Error::other(err).into());
        }
    }
    tracing::info!(node = %node_uuid, "node registered in shm");
    rebuild_fib(
        &fib,
        &nodes,
        &peer_nodes,
        &static_routes,
        &lsa_snapshot,
        &reachability,
    )
    .await;
    if is_gateway {
        let _ = broadcast_lsa_direct(
            router_uuid,
            router_name,
            hive_id,
            &nodes,
            &peer_nodes,
            &static_routes,
            &vpn_rules,
            &tap_rules,
            &wan_peers,
            &lsa_seq,
        )
        .await;
    }

    tracing::info!(node = %node_uuid, name = %node_name, "node registered");
    // The name this socket registered with: the authenticated origin of everything it sends.
    let conn_node_name = node_name.clone();
    let announce = build_announce(
        &router_uuid.to_string(),
        &msg.routing.src,
        &msg.routing.trace_id,
        NodeAnnouncePayload {
            uuid: payload.uuid,
            name: node_name,
            status: "registered".to_string(),
            vpn_id,
            router_name: router_name.to_string(),
        },
    );
    let data = serde_json::to_vec(&announce)?;
    if tx.send(data).is_err() {
        tracing::warn!("failed to enqueue ANNOUNCE");
    } else {
        tracing::info!("announce queued");
    }

    // The connection ends only on EOF, WITHDRAW or a read error, and every end runs the cleanup
    // below (A-44): an end that skipped it left the node registered with its writer alive, so a
    // node that was still running stayed connected while nobody read its frames, and its next
    // HELLO found the UUID taken. A message that cannot be handled is logged and skipped.
    let exit: Result<(), RouterError> = loop {
        let frame = match read_frame(&mut reader).await {
            Ok(Some(f)) => f,
            Ok(None) => {
                tracing::info!(node = %node_uuid, "router: node closed connection (EOF)");
                break Ok(());
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::InvalidData {
                    tracing::error!(node = %node_uuid, error = %err, "FRAME_TOO_LARGE — node sent oversized frame; disconnecting");
                }
                break Err(err.into());
            }
        };
        {
            if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
                if !frame_is_from_connection(&msg, node_uuid) {
                    tracing::warn!(
                        node = %conn_node_name,
                        claimed_src = %msg.routing.src,
                        "router dropped a message whose routing.src is not the sending node"
                    );
                    continue;
                }
                // A node's append to this router's stream: the router consumes it and routes it
                // to no node.
                if is_system_kind(&msg.meta.msg_type)
                    && msg.meta.msg.as_deref() == Some(fluxbee_sdk::stream::MSG_STREAM_APPEND)
                {
                    take_stream_append(&log_stream, &msg, &conn_node_name, hive_id);
                    continue;
                }
                tracing::info!(
                    src = %msg.routing.src,
                    dst = ?msg.routing.dst,
                    msg_type = %msg.meta.msg_type,
                    msg = ?msg.meta.msg,
                    "message received"
                );
                if should_publish_turn(&msg) {
                    if let Some(copy) = turn_copy(&msg, &conn_node_name) {
                        log_stream.append_turn(&conn_node_name, copy);
                    }
                }
                // A local agent's send, for its cognition (§6.2): what it receives is copied at
                // delivery.
                if is_agent_node(&conn_node_name) && is_communication_turn(&msg) {
                    if let Some(copy) = cognition_copy(&msg, &conn_node_name) {
                        log_stream.append_cognition_turn(&conn_node_name, copy);
                    }
                }
                if is_system_kind(&msg.meta.msg_type) {
                    if msg.meta.msg.as_deref() == Some(MSG_WITHDRAW) {
                        break Ok(());
                    }
                    if msg.meta.msg.as_deref() == Some(MSG_OPA_RELOAD) {
                        // SY.opa.rules installed a policy: load it now, and tell the other routers
                        // of this hive. No node consumes OPA_RELOAD, so it goes to none of them.
                        match serde_json::from_value::<OpaReloadPayload>(msg.payload.clone()) {
                            Ok(payload) => {
                                apply_opa_reload(&opa, &opa_reader, &shm, hive_id, &payload).await;
                                if msg.routing.ttl >= 2 {
                                    match broadcast_to_peers(&peers, &msg).await {
                                        Ok(()) => tracing::info!("opa reload forwarded to peers"),
                                        Err(err) => tracing::warn!(
                                            error = %err,
                                            "opa reload could not be forwarded to peers"
                                        ),
                                    }
                                }
                            }
                            Err(err) => tracing::warn!(
                                node = %conn_node_name,
                                error = %err,
                                "router ignored an OPA_RELOAD with an invalid payload"
                            ),
                        }
                        continue;
                    }
                }
                let snapshot = refresh_config(
                    &config_reader,
                    &static_routes,
                    &vpn_rules,
                    &tap_rules,
                    &config_version,
                    hive_id,
                    &nodes,
                    &peer_nodes,
                    &shm,
                    &fib,
                    &lsa_snapshot,
                    &reachability,
                    false,
                )
                .await;
                let _ = refresh_lsa(
                    &lsa_reader,
                    &lsa_snapshot,
                    hive_id,
                    &nodes,
                    &peer_nodes,
                    &static_routes,
                    &fib,
                    &reachability,
                )
                .await;
                if let Err(err) = handle_message(
                    &msg,
                    &nodes,
                    &fib,
                    &peer_nodes,
                    &peer_routers,
                    &peers,
                    &wan_peers,
                    &opa,
                    &broadcast_cache,
                    &memory_reader,
                    &log_stream,
                    hive_id,
                    identity_frontdesk_node_name,
                    router_uuid,
                    is_gateway,
                    &lsa_snapshot,
                    &reachability,
                    snapshot.as_ref(),
                )
                .await
                {
                    tracing::warn!(
                        node = %conn_node_name,
                        trace_id = %msg.routing.trace_id,
                        error = %err,
                        "router could not handle a message from this node"
                    );
                }
            } else {
                tracing::warn!("received invalid message frame");
            }
        }
    };

    {
        let mut nodes = nodes.lock().await;
        if let Some(handle) = nodes.get(&node_uuid) {
            if handle.connected_at == connected_at {
                nodes.remove(&node_uuid);
                let mut shm = shm.lock().await;
                let _ = shm.unregister_node(node_uuid);
            } else {
                tracing::info!(node = %node_uuid, "skipping stale cleanup; node already reconnected");
            }
        }
    }
    rebuild_fib(
        &fib,
        &nodes,
        &peer_nodes,
        &static_routes,
        &lsa_snapshot,
        &reachability,
    )
    .await;
    if is_gateway {
        let _ = broadcast_lsa_direct(
            router_uuid,
            router_name,
            hive_id,
            &nodes,
            &peer_nodes,
            &static_routes,
            &vpn_rules,
            &tap_rules,
            &wan_peers,
            &lsa_seq,
        )
        .await;
    }
    writer_task.abort();
    tracing::info!(node = %node_uuid, "node disconnected");
    exit
}

/// How long a HELLO waits for an earlier connection with its UUID to go before it is refused
/// (A-44). A node that restarts or reconnects, and every Go node's second HELLO at startup, can
/// arrive before the router has processed the close of the connection it replaces.
const DUPLICATE_HELLO_GRACE: Duration = Duration::from_secs(1);
const DUPLICATE_HELLO_POLL: Duration = Duration::from_millis(50);

/// Registers `handle` as the connection of `uuid`, one live connection per node UUID. While
/// another connection holds the UUID it waits up to `grace` for it to go; Err carries the name
/// that connection registered with. A second live process with a node's UUID is refused instead
/// of taking its route.
async fn claim_node_uuid(
    nodes: &Mutex<std::collections::HashMap<Uuid, NodeHandle>>,
    uuid: Uuid,
    handle: NodeHandle,
    grace: Duration,
) -> Result<(), String> {
    let deadline = time::Instant::now() + grace;
    loop {
        let mut guard = nodes.lock().await;
        match guard.get(&uuid).map(|holder| holder.name.clone()) {
            None => {
                guard.insert(uuid, handle);
                return Ok(());
            }
            Some(holder) => {
                drop(guard);
                if time::Instant::now() >= deadline {
                    return Err(holder);
                }
                time::sleep(DUPLICATE_HELLO_POLL).await;
            }
        }
    }
}

/// A node speaks only as itself. `handle_message` takes the sender's authoritative L2 name from
/// `routing.src`, and the delivery gate decides protected SYSTEM actions on that name, so a frame
/// read from a node's socket must carry that node's own UUID. UUIDs travel in every message: a node
/// that could set another's would send as it (as the Admin on the motherbee, as the orchestrator on
/// a worker — seen live on 8.x, 2026-09-30).
fn frame_is_from_connection(msg: &Message, node_uuid: Uuid) -> bool {
    Uuid::parse_str(&msg.routing.src).ok() == Some(node_uuid)
}

async fn handle_message(
    msg: &Message,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    opa: &Arc<Mutex<OpaResolver>>,
    broadcast_cache: &Arc<Mutex<BroadcastCache>>,
    memory_reader: &Arc<Mutex<Option<MemoryRegionReader>>>,
    log_stream: &stream::StreamHandle,
    hive_id: &str,
    identity_frontdesk_node_name: &str,
    router_uuid: Uuid,
    is_gateway: bool,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: &Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    snapshot: Option<&ConfigSnapshot>,
) -> Result<(), RouterError> {
    let mut msg = msg.clone();
    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    let mut senders: Vec<LocalDelivery> = Vec::new();
    let src_handle = {
        let nodes_guard = nodes.lock().await;
        nodes_guard.get(&src_uuid).cloned()
    };
    let Some(src_handle) = src_handle else {
        return Ok(());
    };

    // L2-LOOKUP-5 / L2-LOOKUP-7: stamp the authoritative L2 name, overwriting
    // anything the sender may have set.
    msg.routing.src_l2_name = Some(src_handle.name.clone());

    if msg.routing.ttl == 0 {
        send_ttl_exceeded_to(&msg, &src_handle.sender, router_uuid)?;
        return Ok(());
    }

    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            if let Ok(dst_uuid) = Uuid::parse_str(dst) {
                let dst_handle = {
                    let nodes_guard = nodes.lock().await;
                    nodes_guard.get(&dst_uuid).cloned()
                };
                if let Some(dst_handle) = dst_handle {
                    if vpn_allows_between(
                        &msg.meta,
                        &src_handle.name,
                        src_handle.vpn_id,
                        &dst_handle.name,
                        dst_handle.vpn_id,
                    ) {
                        senders.push(LocalDelivery::to(&dst_handle.name, dst_handle.sender));
                    } else {
                        send_unreachable_to(&msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
                    }
                } else {
                    let peer = {
                        let peer_guard = peer_nodes.lock().await;
                        peer_guard.get(&dst_uuid).cloned()
                    };
                    if let Some(peer_node) = peer {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_handle.name,
                            src_handle.vpn_id,
                            &peer_node.name,
                            peer_node.vpn_id,
                        ) {
                            if msg.routing.ttl <= 1 {
                                send_ttl_exceeded_to(&msg, &src_handle.sender, router_uuid)?;
                                return Ok(());
                            }
                            if send_to_peer_router(peers, peer_node.router_uuid, &msg).await? {
                                return Ok(());
                            }
                            send_unreachable_to(
                                &msg,
                                &src_handle.sender,
                                router_uuid,
                                "PEER_UNAVAILABLE",
                            )?;
                        } else {
                            send_unreachable_to(
                                &msg,
                                &src_handle.sender,
                                router_uuid,
                                "VPN_BLOCKED",
                            )?;
                        }
                    } else {
                        let remote = {
                            let snapshot = lsa_snapshot.lock().await;
                            find_remote_node(&snapshot, dst_uuid)
                        };
                        // Option B: fall back to hub-vouched reachability (forwards via next_hop_hive).
                        let remote = match remote {
                            Some(info) => Some(info),
                            None => find_reachable_node(&*reachability.lock().await, dst_uuid),
                        };
                        if let Some(remote) = remote {
                            if vpn_allows_between(
                                &msg.meta,
                                &src_handle.name,
                                src_handle.vpn_id,
                                &remote.name,
                                remote.vpn_id,
                            ) {
                                forward_to_hive(
                                    &remote.next_hop_hive,
                                    &msg,
                                    is_gateway,
                                    peer_routers,
                                    peers,
                                    wan_peers,
                                    router_uuid,
                                    &src_handle.sender,
                                )
                                .await?;
                            } else {
                                send_unreachable_to(
                                    &msg,
                                    &src_handle.sender,
                                    router_uuid,
                                    "VPN_BLOCKED",
                                )?;
                            }
                        } else {
                            send_unreachable_to(
                                &msg,
                                &src_handle.sender,
                                router_uuid,
                                "NODE_NOT_FOUND",
                            )?;
                        }
                    }
                }
            } else {
                let nodes_guard = nodes.lock().await;
                let route = resolve_by_name(dst, &src_handle, &nodes_guard, fib, &msg.meta).await?;
                match route {
                    ResolvedRoute::Drop => {}
                    ResolvedRoute::Unreachable(reason) => {
                        send_unreachable_to(&msg, &src_handle.sender, router_uuid, reason)?;
                    }
                    ResolvedRoute::Deliver(dst_uuid) => {
                        if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                            if vpn_allows_between(
                                &msg.meta,
                                &src_handle.name,
                                src_handle.vpn_id,
                                &dst_handle.name,
                                dst_handle.vpn_id,
                            ) {
                                senders.push(LocalDelivery::to(
                                    &dst_handle.name,
                                    dst_handle.sender.clone(),
                                ));
                            } else {
                                send_unreachable_to(
                                    &msg,
                                    &src_handle.sender,
                                    router_uuid,
                                    "VPN_BLOCKED",
                                )?;
                            }
                        } else {
                            send_unreachable_to(
                                &msg,
                                &src_handle.sender,
                                router_uuid,
                                "NODE_NOT_FOUND",
                            )?;
                        }
                    }
                    ResolvedRoute::ForwardRouter(peer_uuid) => {
                        if msg.routing.ttl <= 1 {
                            send_ttl_exceeded_to(&msg, &src_handle.sender, router_uuid)?;
                            return Ok(());
                        }
                        if send_to_peer_router(peers, peer_uuid, &msg).await? {
                            return Ok(());
                        }
                        send_unreachable_to(
                            &msg,
                            &src_handle.sender,
                            router_uuid,
                            "PEER_UNAVAILABLE",
                        )?;
                    }
                    ResolvedRoute::ForwardHive(hive_id) => {
                        forward_to_hive(
                            &hive_id,
                            &msg,
                            is_gateway,
                            peer_routers,
                            peers,
                            wan_peers,
                            router_uuid,
                            &src_handle.sender,
                        )
                        .await?;
                    }
                }
            }
        }
        Destination::Broadcast => {
            tracing::info!(
                src = %src_handle.name,
                msg = msg.meta.msg.as_deref().unwrap_or(""),
                msg_type = %msg.meta.msg_type,
                trace_id = %msg.routing.trace_id,
                ttl = msg.routing.ttl,
                "router received broadcast"
            );
            let trace_id = match Uuid::parse_str(&msg.routing.trace_id) {
                Ok(value) => value,
                Err(_) => {
                    tracing::warn!(
                        trace_id = %msg.routing.trace_id,
                        "broadcast dropped: invalid trace_id (must be UUID)"
                    );
                    return Ok(());
                }
            };
            {
                let mut cache = broadcast_cache.lock().await;
                if !cache.check_and_add(trace_id) {
                    tracing::warn!(trace_id = %msg.routing.trace_id, "broadcast duplicate dropped by cache");
                    return Ok(());
                }
            }
            let target = msg.meta.target.as_deref();
            let nodes_guard = nodes.lock().await;
            let mut considered = 0usize;
            let mut skipped_self = 0usize;
            let mut skipped_vpn = 0usize;
            let mut skipped_target = 0usize;
            for (uuid, handle) in nodes_guard.iter() {
                considered += 1;
                if *uuid == src_uuid {
                    skipped_self += 1;
                    continue;
                }
                if !vpn_allows_broadcast_between(
                    &msg.meta,
                    &src_handle.name,
                    src_handle.vpn_id,
                    &handle.name,
                    handle.vpn_id,
                ) {
                    skipped_vpn += 1;
                    continue;
                }
                if let Some(pattern) = target {
                    if !pattern_match(pattern, &handle.name) {
                        skipped_target += 1;
                        continue;
                    }
                }
                tracing::info!(
                    src = %src_handle.name,
                    msg = msg.meta.msg.as_deref().unwrap_or(""),
                    dst = %handle.name,
                    trace_id = %msg.routing.trace_id,
                    "router enqueueing broadcast for delivery"
                );
                senders.push(LocalDelivery::to(&handle.name, handle.sender.clone()));
            }
            tracing::info!(
                src = %src_handle.name,
                msg = msg.meta.msg.as_deref().unwrap_or(""),
                trace_id = %msg.routing.trace_id,
                registered_nodes = considered,
                delivered_to = senders.len(),
                skipped_self = skipped_self,
                skipped_vpn = skipped_vpn,
                skipped_target = skipped_target,
                "router broadcast fanout summary"
            );
            if msg.routing.ttl >= 2 {
                broadcast_to_peers(peers, &msg).await?;
            }
            if msg.routing.ttl >= 3 {
                forward_broadcast_to_wan(
                    &msg,
                    is_gateway,
                    peer_routers,
                    peers,
                    wan_peers,
                    router_uuid,
                    &src_handle.sender,
                )
                .await?;
            }
        }
        Destination::Resolve => {
            let resolved_target = match resolve_target_with_identity(
                opa,
                hive_id,
                identity_frontdesk_node_name,
                &msg,
            )
            .await
            {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("opa resolve failed: {err}");
                    send_unreachable_to(&msg, &src_handle.sender, router_uuid, "OPA_ERROR")?;
                    return Ok(());
                }
            };
            let Some(target) = resolved_target.as_deref() else {
                send_unreachable_to(&msg, &src_handle.sender, router_uuid, "OPA_NO_TARGET")?;
                return Ok(());
            };
            let nodes_guard = nodes.lock().await;
            let route = resolve_by_name(target, &src_handle, &nodes_guard, fib, &msg.meta).await?;
            match route {
                ResolvedRoute::Drop => {}
                ResolvedRoute::Unreachable(reason) => {
                    send_unreachable_to(&msg, &src_handle.sender, router_uuid, reason)?;
                }
                ResolvedRoute::Deliver(dst_uuid) => {
                    if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_handle.name,
                            src_handle.vpn_id,
                            &dst_handle.name,
                            dst_handle.vpn_id,
                        ) {
                            senders.push(LocalDelivery::to(
                                &dst_handle.name,
                                dst_handle.sender.clone(),
                            ));
                        } else {
                            send_unreachable_to(
                                &msg,
                                &src_handle.sender,
                                router_uuid,
                                "VPN_BLOCKED",
                            )?;
                        }
                    } else {
                        send_unreachable_to(
                            &msg,
                            &src_handle.sender,
                            router_uuid,
                            "NODE_NOT_FOUND",
                        )?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if msg.routing.ttl <= 1 {
                        send_ttl_exceeded_to(&msg, &src_handle.sender, router_uuid)?;
                        return Ok(());
                    }
                    if send_to_peer_router(peers, peer_uuid, &msg).await? {
                        return Ok(());
                    }
                    send_unreachable_to(&msg, &src_handle.sender, router_uuid, "PEER_UNAVAILABLE")?;
                }
                ResolvedRoute::ForwardHive(hive_id) => {
                    forward_to_hive(
                        &hive_id,
                        &msg,
                        is_gateway,
                        peer_routers,
                        peers,
                        wan_peers,
                        router_uuid,
                        &src_handle.sender,
                    )
                    .await?;
                }
            }
        }
    }

    deliver_locally(senders, &msg, memory_reader, hive_id, false, log_stream).await?;

    // ROUTER-TAP-3: tap fanout. After primary unicast delivery is done,
    // scan installed taps and enqueue fire-and-forget secondary copies to
    // each matched tap target. Skipped when:
    //   - the inbound message is already a tap copy (via_tap=true) — single
    //     hop tap semantics, no cascading
    //   - destination was not Unicast (broadcasts/resolves use different
    //     fanout semantics already)
    //   - no config snapshot available (router booted without config SHM)
    // Tap failures never affect the primary delivery; they are logged at
    // warn and dropped.
    if !msg.meta.via_tap {
        if let Destination::Unicast(dst_label) = &msg.routing.dst {
            if let Some(snap) = snapshot {
                if !snap.taps.is_empty() {
                    fanout_taps_for_unicast(
                        &msg,
                        dst_label,
                        snap,
                        &src_handle,
                        nodes,
                        fib,
                        peers,
                        peer_routers,
                        peer_nodes,
                        wan_peers,
                        lsa_snapshot,
                        memory_reader,
                        hive_id,
                        router_uuid,
                        is_gateway,
                    )
                    .await;
                }
            }
        }
    }
    Ok(())
}

/// Fire-and-forget tap fanout for one inbound unicast message. For every
/// installed `TapEntry` where (match_src, match_dst) equals the message's
/// `(src_l2_name, dst label)`, build a copy with `meta.via_tap = true` and
/// dispatch it through the normal name-based resolution path. Another hive's
/// taps, from the LSA, count only when that hive is the message's source or
/// destination and the target is one of its nodes (`lsa_tap_applies`). A copy
/// never carries a protected SYSTEM action (`prepare_local_delivery`). Failures
/// (target unknown, VPN mismatch, remote unreachable) increment per-tap
/// log lines and drop the copy — they NEVER bubble up to the primary path.
///
/// `dst_label` is the literal value from `Destination::Unicast`. It may be
/// either a UUID string or an L2 name. Tap match is by L2 name; if the
/// label was a UUID we resolve back to a node name from the local registry
/// so taps installed by L2 name still match.
async fn fanout_taps_for_unicast(
    msg: &Message,
    dst_label: &str,
    snap: &ConfigSnapshot,
    src_handle: &NodeHandle,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    memory_reader: &Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: &str,
    router_uuid: Uuid,
    is_gateway: bool,
) {
    let src_name = msg.routing.src_l2_name.as_deref().unwrap_or("");
    if src_name.is_empty() {
        return;
    }
    // If dst_label is a UUID, resolve back to the L2 name of the target node
    // so tap matching (which is by L2 name) still works. Resolve it the SAME
    // way the rest of the router does (handle_message/wan/peer): local `nodes`,
    // then `peer_nodes`, then the cross-hive LSA snapshot via `find_remote_node`.
    // Without the LSA fallback a tap whose match_dst lives on another hive never
    // matches a UUID-addressed message (e.g. replies, which target the source
    // node's UUID via build_system_reply), so cross-hive response taps silently
    // never fired.
    let dst_name_owned: String = if let Ok(dst_uuid) = Uuid::parse_str(dst_label) {
        let local = {
            let nodes_guard = nodes.lock().await;
            nodes_guard.get(&dst_uuid).map(|handle| handle.name.clone())
        };
        let peered = if local.is_none() {
            let peer_guard = peer_nodes.lock().await;
            peer_guard.get(&dst_uuid).map(|p| p.name.clone())
        } else {
            None
        };
        match local.or(peered) {
            Some(name) => name,
            None => {
                let snapshot = lsa_snapshot.lock().await;
                find_remote_node(&snapshot, dst_uuid)
                    .map(|info| info.name)
                    .unwrap_or_default()
            }
        }
    } else {
        dst_label.to_string()
    };
    if dst_name_owned.is_empty() {
        return;
    }

    let mut tap_targets = Vec::new();
    for tap in &snap.taps {
        if tap.enabled == 0 {
            continue;
        }
        let tap_src = bytes_to_string(&tap.match_src, tap.match_src_len as usize);
        let tap_dst = bytes_to_string(&tap.match_dst, tap.match_dst_len as usize);
        if tap_src != src_name || tap_dst != dst_name_owned {
            continue;
        }
        let tap_target = bytes_to_string(&tap.target, tap.target_len as usize);
        if tap_target.is_empty() {
            continue;
        }
        tap_targets.push(tap_target.to_string());
    }
    if let Some(snapshot) = lsa_snapshot.lock().await.as_ref() {
        for tap in &snapshot.taps {
            if tap.enabled == 0 {
                continue;
            }
            if tap.flags != 0 && (tap.flags & FLAG_ACTIVE == 0) {
                continue;
            }
            let tap_src = bytes_to_string(&tap.match_src, tap.match_src_len as usize);
            let tap_dst = bytes_to_string(&tap.match_dst, tap.match_dst_len as usize);
            if tap_src != src_name || tap_dst != dst_name_owned {
                continue;
            }
            if !lsa_tap_applies(snapshot, tap, src_name, &dst_name_owned) {
                continue;
            }
            let tap_target = bytes_to_string(&tap.target, tap.target_len as usize);
            if tap_target.is_empty() {
                continue;
            }
            tap_targets.push(tap_target.to_string());
        }
    }
    tap_targets.sort();
    tap_targets.dedup();
    tracing::debug!(
        src = %src_name,
        dst = %dst_name_owned,
        tap_match_count = tap_targets.len(),
        "router tap fanout: evaluating"
    );

    // Iterate installed/global taps; for each match emit one copy.
    for tap_target in tap_targets {
        let mut copy = msg.clone();
        copy.meta.via_tap = true;
        copy.routing.dst = Destination::Unicast(tap_target.clone());
        // Mint a fresh trace_id so observer traces can be correlated to
        // this tap copy specifically without colliding with the primary.
        copy.routing.trace_id = Uuid::new_v4().to_string();

        let nodes_guard = nodes.lock().await;
        let resolved =
            match resolve_by_name(&tap_target, src_handle, &nodes_guard, fib, &copy.meta).await {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!(
                        tap_src = %src_name,
                        tap_dst = %dst_name_owned,
                        tap_target = %tap_target,
                        error = %err,
                        "router tap copy: name resolution failed, dropping copy"
                    );
                    continue;
                }
            };
        match resolved {
            ResolvedRoute::Drop => {
                tracing::warn!(
                    tap_src = %src_name,
                    tap_dst = %dst_name_owned,
                    tap_target = %tap_target,
                    "router tap copy: resolver returned Drop, copy discarded"
                );
            }
            ResolvedRoute::Unreachable(reason) => {
                tracing::warn!(
                    tap_src = %src_name,
                    tap_dst = %dst_name_owned,
                    tap_target = %tap_target,
                    reason = reason,
                    "router tap copy: target unreachable, copy dropped"
                );
            }
            ResolvedRoute::Deliver(target_uuid) => {
                let target_snapshot = nodes_guard.get(&target_uuid).cloned();
                drop(nodes_guard);
                let Some(target_handle) = target_snapshot else {
                    tracing::warn!(
                        tap_target = %tap_target,
                        target_uuid = %target_uuid,
                        "router tap copy: deliver target not in local nodes, dropped"
                    );
                    continue;
                };
                if !vpn_allows_between(
                    &copy.meta,
                    &src_handle.name,
                    src_handle.vpn_id,
                    &target_handle.name,
                    target_handle.vpn_id,
                ) {
                    tracing::warn!(
                        tap_src = %src_name,
                        tap_dst = %dst_name_owned,
                        tap_target = %tap_target,
                        "router tap copy: VPN mismatch with target, dropped"
                    );
                    continue;
                }
                let data = match serialize_for_local_delivery(
                    memory_reader,
                    hive_id,
                    &copy,
                    false,
                    &target_handle.name,
                )
                .await
                {
                    Ok(Some(data)) => data,
                    Ok(None) => continue, // suppressed: protected SYSTEM from unauthorized origin
                    Err(err) => {
                        tracing::warn!(
                            tap_target = %tap_target,
                            error = %err,
                            "router tap copy: serialization failed, dropped"
                        );
                        continue;
                    }
                };
                let _ = target_handle.sender.send(data);
                tracing::info!(
                    tap_src = %src_name,
                    tap_dst = %dst_name_owned,
                    tap_target = %tap_target,
                    copy_trace_id = %copy.routing.trace_id,
                    "router tap copy delivered (local)"
                );
            }
            ResolvedRoute::ForwardRouter(peer_uuid) => {
                drop(nodes_guard);
                if copy.routing.ttl <= 1 {
                    tracing::warn!(
                        tap_target = %tap_target,
                        "router tap copy: TTL exhausted before peer router forward, dropped"
                    );
                    continue;
                }
                let _ = send_to_peer_router(peers, peer_uuid, &copy).await;
                tracing::info!(
                    tap_src = %src_name,
                    tap_dst = %dst_name_owned,
                    tap_target = %tap_target,
                    copy_trace_id = %copy.routing.trace_id,
                    "router tap copy forwarded (peer router)"
                );
            }
            ResolvedRoute::ForwardHive(remote_hive) => {
                drop(nodes_guard);
                let _ = forward_to_hive(
                    &remote_hive,
                    &copy,
                    is_gateway,
                    peer_routers,
                    peers,
                    wan_peers,
                    router_uuid,
                    &src_handle.sender,
                )
                .await;
                tracing::info!(
                    tap_src = %src_name,
                    tap_dst = %dst_name_owned,
                    tap_target = %tap_target,
                    remote_hive = %remote_hive,
                    copy_trace_id = %copy.routing.trace_id,
                    "router tap copy forwarded (remote hive)"
                );
            }
        }
    }
}

fn canonical_thread_id_from_meta(meta: &Meta) -> Option<String> {
    meta.thread_id
        .as_deref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn send_unreachable_to(
    msg: &Message,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    router_uuid: Uuid,
    reason: &str,
) -> Result<(), RouterError> {
    let original_dst = match &msg.routing.dst {
        Destination::Unicast(value) => value.clone(),
        Destination::Broadcast => "broadcast".to_string(),
        Destination::Resolve => "null".to_string(),
    };
    let err = build_unreachable(
        &router_uuid.to_string(),
        &msg.routing.src,
        &msg.routing.trace_id,
        &original_dst,
        reason,
    );
    let _ = sender.send(serde_json::to_vec(&err)?);
    Ok(())
}

fn send_ttl_exceeded_to(
    msg: &Message,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    router_uuid: Uuid,
) -> Result<(), RouterError> {
    let original_dst = match &msg.routing.dst {
        Destination::Unicast(value) => value.clone(),
        Destination::Broadcast => "broadcast".to_string(),
        Destination::Resolve => "null".to_string(),
    };
    let err = build_ttl_exceeded(
        &router_uuid.to_string(),
        &msg.routing.src,
        &msg.routing.trace_id,
        &original_dst,
        &router_name(router_uuid),
    );
    let _ = sender.send(serde_json::to_vec(&err)?);
    Ok(())
}

async fn send_to_peer_router(
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_uuid: Uuid,
    msg: &Message,
) -> Result<bool, RouterError> {
    let peer = {
        let peers_guard = peers.lock().await;
        peers_guard.get(&peer_uuid).cloned()
    };
    let Some(peer) = peer else {
        return Ok(false);
    };
    let mut forward = msg.clone();
    if forward.routing.ttl > 0 {
        forward.routing.ttl = forward.routing.ttl.saturating_sub(1);
    }
    let data = serde_json::to_vec(&forward)?;
    let _ = peer.sender.send(data);
    Ok(true)
}

async fn send_to_wan_peer(
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    hive_id: &str,
    msg: &Message,
) -> Result<bool, RouterError> {
    let peer = {
        let peers_guard = wan_peers.lock().await;
        peers_guard.get(hive_id).cloned()
    };
    let Some(peer) = peer else {
        return Ok(false);
    };
    let mut forward = msg.clone();
    if forward.routing.ttl > 0 {
        forward.routing.ttl = forward.routing.ttl.saturating_sub(1);
    }
    let data = serde_json::to_vec(&forward)?;
    let _ = peer.sender.send(data);
    Ok(true)
}

async fn broadcast_to_peers(
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    msg: &Message,
) -> Result<(), RouterError> {
    let peers_guard = peers.lock().await;
    if peers_guard.is_empty() {
        return Ok(());
    }
    let mut forward = msg.clone();
    if forward.routing.ttl > 0 {
        forward.routing.ttl = forward.routing.ttl.saturating_sub(1);
    }
    let data = serde_json::to_vec(&forward)?;
    for peer in peers_guard.values() {
        let _ = peer.sender.send(data.clone());
    }
    Ok(())
}

async fn broadcast_to_wan(
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    msg: &Message,
) -> Result<(), RouterError> {
    let peers_guard = wan_peers.lock().await;
    if peers_guard.is_empty() {
        return Ok(());
    }
    let mut forward = msg.clone();
    if forward.routing.ttl > 0 {
        forward.routing.ttl = forward.routing.ttl.saturating_sub(1);
    }
    let data = serde_json::to_vec(&forward)?;
    for peer in peers_guard.values() {
        let _ = peer.sender.send(data.clone());
    }
    Ok(())
}

async fn forward_to_hive(
    hive_id: &str,
    msg: &Message,
    is_gateway: bool,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    router_uuid: Uuid,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> Result<(), RouterError> {
    if msg.routing.ttl <= 1 {
        send_ttl_exceeded_to(msg, sender, router_uuid)?;
        return Ok(());
    }
    if is_gateway {
        if send_to_wan_peer(wan_peers, hive_id, msg).await? {
            return Ok(());
        }
        send_unreachable_to(msg, sender, router_uuid, "WAN_UNAVAILABLE")?;
        return Ok(());
    }
    let gateway_uuid = {
        let guard = peer_routers.lock().await;
        guard
            .values()
            .find(|peer| peer.is_gateway)
            .map(|peer| peer.uuid)
    };
    let Some(gateway_uuid) = gateway_uuid else {
        send_unreachable_to(msg, sender, router_uuid, "GATEWAY_UNAVAILABLE")?;
        return Ok(());
    };
    if send_to_peer_router(peers, gateway_uuid, msg).await? {
        return Ok(());
    }
    send_unreachable_to(msg, sender, router_uuid, "GATEWAY_UNAVAILABLE")?;
    Ok(())
}

async fn forward_broadcast_to_wan(
    msg: &Message,
    is_gateway: bool,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    router_uuid: Uuid,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> Result<(), RouterError> {
    if msg.routing.ttl <= 2 {
        return Ok(());
    }
    if is_gateway {
        broadcast_to_wan(wan_peers, msg).await?;
        return Ok(());
    }
    let gateway_uuid = {
        let guard = peer_routers.lock().await;
        guard
            .values()
            .find(|peer| peer.is_gateway)
            .map(|peer| peer.uuid)
    };
    let Some(gateway_uuid) = gateway_uuid else {
        send_unreachable_to(msg, sender, router_uuid, "GATEWAY_UNAVAILABLE")?;
        return Ok(());
    };
    let _ = send_to_peer_router(peers, gateway_uuid, msg).await?;
    Ok(())
}

fn match_kind_label(kind: u8) -> &'static str {
    match kind {
        MATCH_EXACT => "EXACT",
        MATCH_PREFIX => "PREFIX",
        MATCH_GLOB => "GLOB",
        _ => "PREFIX",
    }
}

fn match_kind_from_label(label: &str) -> Option<u8> {
    match label.to_ascii_uppercase().as_str() {
        "EXACT" => Some(MATCH_EXACT),
        "PREFIX" => Some(MATCH_PREFIX),
        "GLOB" => Some(MATCH_GLOB),
        _ => None,
    }
}

fn action_label(action: u8) -> &'static str {
    match action {
        ACTION_DROP => "DROP",
        _ => "FORWARD",
    }
}

fn action_from_label(label: &str) -> Option<u8> {
    match label.to_ascii_uppercase().as_str() {
        "DROP" => Some(ACTION_DROP),
        "FORWARD" => Some(ACTION_FORWARD),
        _ => None,
    }
}

fn tap_mode_label(mode: u8) -> &'static str {
    match mode {
        0 => "best_effort",
        _ => "best_effort",
    }
}

fn tap_mode_from_label(label: &str) -> Option<u8> {
    match label.trim().to_ascii_lowercase().as_str() {
        "" | "best_effort" => Some(0),
        _ => None,
    }
}

async fn build_local_lsa_payload(
    router_uuid: Uuid,
    router_name: &str,
    hive_id: &str,
    seq: u64,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: &Arc<Mutex<Vec<VpnAssignment>>>,
    tap_rules: &Arc<Mutex<Vec<TapEntry>>>,
) -> LsaPayload {
    let mut node_map: HashMap<Uuid, LsaNode> = HashMap::new();
    {
        let nodes_guard = nodes.lock().await;
        for (uuid, handle) in nodes_guard.iter() {
            node_map.insert(
                *uuid,
                LsaNode {
                    uuid: uuid.to_string(),
                    name: handle.name.clone(),
                    vpn_id: handle.vpn_id,
                },
            );
        }
    }
    {
        let peer_guard = peer_nodes.lock().await;
        for (uuid, node) in peer_guard.iter() {
            node_map.insert(
                *uuid,
                LsaNode {
                    uuid: uuid.to_string(),
                    name: node.name.clone(),
                    vpn_id: node.vpn_id,
                },
            );
        }
    }
    let nodes: Vec<LsaNode> = node_map.into_values().collect();
    let routes_snapshot = {
        let routes_guard = static_routes.lock().await;
        routes_guard.clone()
    };
    let routes: Vec<LsaRoute> = routes_snapshot
        .into_iter()
        .map(|route| LsaRoute {
            prefix: route.pattern,
            match_kind: match_kind_label(route.match_kind).to_string(),
            action: action_label(route.action).to_string(),
            next_hop_hive: route.next_hop_hive,
            metric: route.metric,
        })
        .collect();
    let vpn_snapshot = {
        let vpn_guard = vpn_rules.lock().await;
        vpn_guard.clone()
    };
    let mut vpns = Vec::new();
    for vpn in vpn_snapshot {
        if vpn.pattern_len == 0 {
            continue;
        }
        if vpn.flags != 0 && (vpn.flags & FLAG_ACTIVE == 0) {
            continue;
        }
        vpns.push(LsaVpn {
            pattern: bytes_to_string(&vpn.pattern, vpn.pattern_len as usize).to_string(),
            match_kind: match_kind_label(vpn.match_kind).to_string(),
            vpn_id: vpn.vpn_id,
        });
    }
    let tap_snapshot = {
        let tap_guard = tap_rules.lock().await;
        tap_guard.clone()
    };
    let mut taps = Vec::new();
    for tap in tap_snapshot {
        if tap.enabled == 0
            || tap.match_src_len == 0
            || tap.match_dst_len == 0
            || tap.target_len == 0
        {
            continue;
        }
        if tap.flags != 0 && (tap.flags & FLAG_ACTIVE == 0) {
            continue;
        }
        taps.push(LsaTap {
            match_src: bytes_to_string(&tap.match_src, tap.match_src_len as usize).to_string(),
            match_dst: bytes_to_string(&tap.match_dst, tap.match_dst_len as usize).to_string(),
            target: bytes_to_string(&tap.target, tap.target_len as usize).to_string(),
            mode: tap_mode_label(tap.mode).to_string(),
            enabled: tap.enabled != 0,
        });
    }
    LsaPayload {
        hive: hive_id.to_string(),
        router_id: router_uuid.to_string(),
        router_name: router_name.to_string(),
        seq,
        timestamp: now_epoch_ms().to_string(),
        nodes,
        routes,
        vpns,
        taps,
    }
}

async fn broadcast_lsa(ctx: &WanContext, target_hive: Option<&str>) -> Result<(), RouterError> {
    let peers: Vec<(String, WanPeer)> = {
        let guard = ctx.wan_peers.lock().await;
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    if peers.is_empty() {
        return Ok(());
    }
    let mut seq_guard = ctx.lsa_seq.lock().await;
    *seq_guard = seq_guard.saturating_add(1);
    let seq = *seq_guard;
    drop(seq_guard);
    let payload = build_local_lsa_payload(
        ctx.router_uuid,
        &ctx.router_name,
        &ctx.hive_id,
        seq,
        &ctx.nodes,
        &ctx.peer_nodes,
        &ctx.static_routes,
        &ctx.vpn_rules,
        &ctx.tap_rules,
    )
    .await;
    for (hive, peer) in peers.iter() {
        if let Some(target) = target_hive {
            if target != hive {
                continue;
            }
        }
        let msg = build_lsa(
            &ctx.router_uuid.to_string(),
            &peer.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            payload.clone(),
        );
        let data = serde_json::to_vec(&msg)?;
        let _ = peer.sender.send(data);
    }
    Ok(())
}

/// Option B: the hub advertises which spoke nodes are reachable THROUGH it, so a spoke can resolve
/// and forward toward a non-adjacent hive. Sent to each WAN peer with SPLIT-HORIZON (a peer is
/// never told its own nodes). A leaf with fewer than 2 WAN peers naturally sends nothing. Authored
/// in THIS router's own authenticated bucket (`origin_hive == this hive`), so it never violates the
/// EDGE-01 LSA bucket binding; the receiver additionally gates on the voucher being the primary hub.
async fn broadcast_reachability(ctx: &WanContext) -> Result<(), RouterError> {
    let peers: Vec<(String, WanPeer)> = {
        let guard = ctx.wan_peers.lock().await;
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    if peers.len() < 2 {
        return Ok(()); // only a hub (>=2 peers) can relay between spokes
    }
    let (state, contested) = {
        let guard = ctx.lsa_state.lock().await;
        // A UUID that two hives list resolves to neither here (`find_remote_node`); it is not
        // vouched for either, so no spoke routes it to one of them.
        let mut listed_by: HashMap<Uuid, usize> = HashMap::new();
        for st in guard.values() {
            let uuids: HashSet<Uuid> = st
                .nodes
                .iter()
                .filter_map(|node| Uuid::parse_str(&node.uuid).ok())
                .collect();
            for uuid in uuids {
                *listed_by.entry(uuid).or_default() += 1;
            }
        }
        let contested: HashSet<Uuid> = listed_by
            .into_iter()
            .filter(|(_, hives)| *hives > 1)
            .map(|(uuid, _)| uuid)
            .collect();
        let state: Vec<(String, Vec<LsaNode>)> = guard
            .iter()
            .filter(|(_, st)| st.flags & FLAG_STALE == 0)
            .map(|(hive, st)| (hive.clone(), st.nodes.clone()))
            .collect();
        (state, contested)
    };
    let seq = now_epoch_ms();
    for (target_hive, peer) in peers.iter() {
        let mut entries: Vec<WanReachabilityEntry> = Vec::new();
        for (origin_hive, nodes) in state.iter() {
            if origin_hive == target_hive {
                continue; // split-horizon: the target learns its own nodes via its own LSA
            }
            for node in nodes {
                if Uuid::parse_str(&node.uuid).is_ok_and(|uuid| contested.contains(&uuid)) {
                    continue;
                }
                entries.push(WanReachabilityEntry {
                    uuid: node.uuid.clone(),
                    name: node.name.clone(),
                    hive_id: origin_hive.clone(),
                    vpn_id: node.vpn_id,
                });
            }
        }
        if entries.is_empty() {
            continue;
        }
        let payload = WanReachabilityPayload {
            origin_hive: ctx.hive_id.clone(),
            router_id: ctx.router_uuid.to_string(),
            router_name: ctx.router_name.clone(),
            seq,
            timestamp: seq.to_string(),
            entries,
        };
        let msg = build_wan_reachability(
            &ctx.router_uuid.to_string(),
            &peer.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            payload,
        );
        if let Ok(data) = serde_json::to_vec(&msg) {
            let _ = peer.sender.send(data);
        }
    }
    Ok(())
}

/// Ingest a hub-authored `MSG_WAN_REACHABILITY` (Option B). Gated: only the primary hub may vouch
/// (`system_policy::wan_reachability_voucher_allowed`) and only for its own authenticated bucket
/// (`payload.origin_hive == peer_hive`, EDGE-01-style). Replaces this voucher's entry set in the
/// reachability table and rebuilds the FIB so the new routes take effect immediately.
async fn apply_wan_reachability(ctx: &WanContext, peer_hive: &str, msg: &Message) {
    if !system_policy::wan_reachability_voucher_allowed(peer_hive) {
        tracing::warn!(peer_hive = %peer_hive, "rejected WAN_REACHABILITY: voucher is not the authorized hub");
        return;
    }
    let payload: WanReachabilityPayload = match serde_json::from_value(msg.payload.clone()) {
        Ok(payload) => payload,
        Err(err) => {
            tracing::warn!(peer_hive = %peer_hive, error = %err, "WAN_REACHABILITY parse error");
            return;
        }
    };
    if payload.origin_hive != peer_hive {
        tracing::warn!(
            payload_hive = %payload.origin_hive,
            peer_hive = %peer_hive,
            "WAN_REACHABILITY bucket mismatch; rejected"
        );
        return;
    }
    // A vouched UUID that is already taken, by a node of this hive or one this router learns from an
    // LSA, or that the advertisement lists twice, is dropped: a vouch never takes a node's traffic.
    let mut taken = local_node_uuids(&ctx.nodes, &ctx.peer_nodes).await;
    taken.extend(lsa_listed_uuids(&*ctx.lsa_snapshot.lock().await));
    let mut listed: HashMap<Uuid, usize> = HashMap::new();
    for entry in &payload.entries {
        if let Ok(uuid) = Uuid::parse_str(&entry.uuid) {
            *listed.entry(uuid).or_default() += 1;
        }
    }
    let mut duplicate_uuids = Vec::new();
    let now = now_epoch_ms();
    {
        let mut table = ctx.reachability.lock().await;
        // Replace the full set vouched by this hub (removed nodes disappear on the next beacon).
        table.retain(|_, entry| entry.next_hop_hive != peer_hive);
        for entry in payload.entries {
            let Ok(uuid) = Uuid::parse_str(&entry.uuid) else {
                continue;
            };
            // Never accept a reachability entry for THIS hive (self) or for the voucher's own hive
            // (that would be a normal directly-authenticated LSA node, not a transitive vouch).
            if entry.hive_id == ctx.hive_id || entry.hive_id == peer_hive || entry.name.is_empty() {
                continue;
            }
            if taken.contains(&uuid) || listed.get(&uuid).is_some_and(|count| *count > 1) {
                duplicate_uuids.push(entry.name);
                continue;
            }
            table.insert(
                uuid,
                ReachabilityEntry {
                    name: entry.name,
                    origin_hive: entry.hive_id,
                    next_hop_hive: peer_hive.to_string(),
                    vpn_id: entry.vpn_id,
                    last_updated: now,
                },
            );
        }
    }
    if !duplicate_uuids.is_empty() {
        let reject_count =
            count_lsa_rejects(ctx, LSA_REJECT_DUPLICATE_UUID, duplicate_uuids.len() as u64).await;
        tracing::warn!(
            reason = LSA_REJECT_DUPLICATE_UUID,
            peer_hive = %peer_hive,
            nodes = ?duplicate_uuids,
            reject_count = reject_count,
            "WAN_REACHABILITY entries dropped: their UUID is already taken"
        );
    }
    rebuild_fib(
        &ctx.fib,
        &ctx.nodes,
        &ctx.peer_nodes,
        &ctx.static_routes,
        &ctx.lsa_snapshot,
        &ctx.reachability,
    )
    .await;
    tracing::debug!(peer_hive = %peer_hive, "WAN_REACHABILITY applied");
}

/// Every node UUID the LSA region lists, under any hive, this one and a stale one included.
fn lsa_listed_uuids(snapshot: &Option<LsaSnapshot>) -> HashSet<Uuid> {
    snapshot
        .iter()
        .flat_map(|snapshot| snapshot.nodes.iter())
        .filter(|node| node.name_len != 0)
        .map(|node| Uuid::from_bytes(node.uuid))
        .collect()
}

async fn broadcast_lsa_direct(
    router_uuid: Uuid,
    router_name: &str,
    hive_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: &Arc<Mutex<Vec<VpnAssignment>>>,
    tap_rules: &Arc<Mutex<Vec<TapEntry>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_seq: &Arc<Mutex<u64>>,
) -> Result<(), RouterError> {
    let peers: Vec<(String, WanPeer)> = {
        let guard = wan_peers.lock().await;
        guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    };
    if peers.is_empty() {
        return Ok(());
    }
    let mut seq_guard = lsa_seq.lock().await;
    *seq_guard = seq_guard.saturating_add(1);
    let seq = *seq_guard;
    drop(seq_guard);
    let payload = build_local_lsa_payload(
        router_uuid,
        router_name,
        hive_id,
        seq,
        nodes,
        peer_nodes,
        static_routes,
        vpn_rules,
        tap_rules,
    )
    .await;
    for (_hive, peer) in peers {
        let msg = build_lsa(
            &router_uuid.to_string(),
            &peer.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            payload.clone(),
        );
        let data = serde_json::to_vec(&msg)?;
        let _ = peer.sender.send(data);
    }
    Ok(())
}

#[derive(Debug)]
enum LsaApplyResult {
    /// Applied, without the nodes it listed under another hive's name. `duplicate_uuids` names the
    /// nodes it listed under a UUID already taken (`apply_lsa_payload`).
    Applied {
        foreign_nodes: Vec<String>,
        duplicate_uuids: Vec<String>,
    },
    Rejected {
        reason: &'static str,
        payload_hive: String,
        seq: u64,
        last_seq: u64,
        session_epoch: u64,
    },
}

async fn bump_lsa_reject_counter(ctx: &WanContext, reason: &'static str) -> u64 {
    count_lsa_rejects(ctx, reason, 1).await
}

async fn count_lsa_rejects(ctx: &WanContext, reason: &'static str, count: u64) -> u64 {
    let mut guard = ctx.lsa_reject_counters.lock().await;
    let value = guard.entry(reason.to_string()).or_insert(0);
    *value = value.saturating_add(count);
    *value
}

/// The UUIDs of this hive's nodes: this router's and its siblings'.
async fn local_node_uuids(
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
) -> HashSet<Uuid> {
    let mut uuids: HashSet<Uuid> = nodes.lock().await.keys().copied().collect();
    uuids.extend(peer_nodes.lock().await.keys().copied());
    uuids
}

async fn apply_lsa_payload(
    lsa_state: &Arc<Mutex<std::collections::HashMap<String, RemoteHiveState>>>,
    local_uuids: &HashSet<Uuid>,
    expected_hive: &str,
    peer_router_uuid: Uuid,
    peer_router_name: &str,
    session_epoch: u64,
    payload: LsaPayload,
) -> LsaApplyResult {
    if payload.hive != expected_hive {
        return LsaApplyResult::Rejected {
            reason: LSA_REJECT_HIVE_MISMATCH,
            payload_hive: payload.hive,
            seq: payload.seq,
            last_seq: 0,
            session_epoch,
        };
    }

    let mut state_guard = lsa_state.lock().await;
    // The UUIDs the other hives' LSAs list, a stale one's included.
    let listed_elsewhere: HashSet<Uuid> = state_guard
        .iter()
        .filter(|(hive, _)| hive.as_str() != expected_hive)
        .flat_map(|(_, state)| state.nodes.iter())
        .filter_map(|node| Uuid::parse_str(&node.uuid).ok())
        .collect();
    let entry = state_guard.entry(expected_hive.to_string()).or_default();
    if entry.session_epoch != session_epoch {
        entry.session_epoch = session_epoch;
        entry.last_seq = 0;
    }
    if payload.seq <= entry.last_seq {
        return LsaApplyResult::Rejected {
            reason: LSA_REJECT_STALE_SEQ,
            payload_hive: payload.hive,
            seq: payload.seq,
            last_seq: entry.last_seq,
            session_epoch: entry.session_epoch,
        };
    }
    let router_uuid = if payload.router_id.is_empty() {
        peer_router_uuid
    } else {
        match Uuid::parse_str(&payload.router_id) {
            Ok(router_uuid) => router_uuid,
            Err(_) => {
                return LsaApplyResult::Rejected {
                    reason: LSA_REJECT_PARSE_ERROR,
                    payload_hive: payload.hive,
                    seq: payload.seq,
                    last_seq: entry.last_seq,
                    session_epoch: entry.session_epoch,
                };
            }
        }
    };
    entry.last_seq = payload.seq;
    entry.last_updated = now_epoch_ms();
    entry.flags &= !FLAG_STALE;
    entry.router_uuid = *router_uuid.as_bytes();
    entry.router_name = if payload.router_name.is_empty() {
        peer_router_name.to_string()
    } else {
        payload.router_name.clone()
    };
    // A hive advertises only its own nodes. A node it names for another hive (`SY.admin@motherbee`
    // in a spoke's LSA) would draw the traffic routed to that name to the advertising hive
    // (docs/control-path-hardening-v1.md §5). Routes and taps are kept as they come and limited
    // where they apply (`lsa_route_applies`, `lsa_tap_applies`).
    //
    // Nor may it take a UUID that is a node's already: the traffic addressed to that UUID, replies
    // above all, would go to it. One of this hive's nodes: dropped. One another hive lists: kept,
    // so both claims stay in the region and `find_remote_node` resolves that UUID to neither hive.
    // Dropping the later claim would give the UUID to whichever hive listed it first, which a hive
    // can arrange: right after this router restarts, or while the owner's LSA is stale.
    let mut nodes = Vec::new();
    let mut foreign_nodes = Vec::new();
    let mut duplicate_uuids = Vec::new();
    for node in payload.nodes {
        if !node_is_on_hive(&node.name, expected_hive) {
            foreign_nodes.push(node.name);
            continue;
        }
        let uuid = Uuid::parse_str(&node.uuid).ok();
        if uuid.is_some_and(|uuid| local_uuids.contains(&uuid)) {
            duplicate_uuids.push(node.name);
            continue;
        }
        if uuid.is_some_and(|uuid| listed_elsewhere.contains(&uuid)) {
            duplicate_uuids.push(node.name.clone());
        }
        nodes.push(node);
    }
    entry.nodes = nodes;
    entry.routes = payload.routes;
    entry.vpns = payload.vpns;
    entry.taps = payload.taps;
    LsaApplyResult::Applied {
        foreign_nodes,
        duplicate_uuids,
    }
}

async fn write_lsa_state(ctx: &WanContext) {
    let self_seq = {
        let guard = ctx.lsa_seq.lock().await;
        *guard
    };
    let self_payload = build_local_lsa_payload(
        ctx.router_uuid,
        &ctx.router_name,
        &ctx.hive_id,
        self_seq,
        &ctx.nodes,
        &ctx.peer_nodes,
        &ctx.static_routes,
        &ctx.vpn_rules,
        &ctx.tap_rules,
    )
    .await;
    let self_last_updated = self_payload
        .timestamp
        .parse::<u64>()
        .unwrap_or_else(|_| now_epoch_ms());
    let self_state = RemoteHiveState {
        session_epoch: 0,
        last_seq: self_payload.seq,
        last_updated: self_last_updated,
        flags: FLAG_ACTIVE | HIVE_FLAG_SELF,
        router_uuid: *ctx.router_uuid.as_bytes(),
        router_name: ctx.router_name.clone(),
        nodes: self_payload.nodes,
        routes: self_payload.routes,
        vpns: self_payload.vpns,
        taps: self_payload.taps,
    };

    let state_guard = ctx.lsa_state.lock().await;
    let mut hives: Vec<(String, RemoteHiveState)> = state_guard
        .iter()
        .filter(|(name, _)| name.as_str() != ctx.hive_id)
        .map(|(name, state)| (name.clone(), state.clone()))
        .collect();
    hives.sort_by(|a, b| a.0.cmp(&b.0));
    drop(state_guard);
    hives.insert(0, (ctx.hive_id.clone(), self_state));

    let mut writer_guard = ctx.lsa_writer.lock().await;
    let Some(writer) = writer_guard.as_mut() else {
        return;
    };

    let mut hive_entries: Vec<RemoteHiveEntry> = Vec::new();
    let mut node_entries: Vec<RemoteNodeEntry> = Vec::new();
    let mut route_entries: Vec<RemoteRouteEntry> = Vec::new();
    let mut vpn_entries: Vec<RemoteVpnEntry> = Vec::new();
    let mut tap_entries: Vec<RemoteTapEntry> = Vec::new();

    for (index, (hive_id, state)) in hives.iter().enumerate() {
        let mut hive_entry = RemoteHiveEntry {
            hive_id: [0u8; 64],
            hive_id_len: 0,
            router_uuid: state.router_uuid,
            router_name: [0u8; 64],
            router_name_len: 0,
            last_lsa_seq: state.last_seq,
            last_updated: state.last_updated,
            flags: state.flags,
            node_count: state.nodes.len() as u32,
            route_count: state.routes.len() as u32,
            vpn_count: state.vpns.len() as u32,
            tap_count: state.taps.len() as u32,
        };
        hive_entry.hive_id_len = copy_bytes_with_len(&mut hive_entry.hive_id, hive_id) as u16;
        hive_entry.router_name_len =
            copy_bytes_with_len(&mut hive_entry.router_name, &state.router_name) as u16;
        hive_entries.push(hive_entry);

        for node in state.nodes.iter() {
            let Ok(uuid) = Uuid::parse_str(&node.uuid) else {
                continue;
            };
            let mut entry = RemoteNodeEntry {
                uuid: *uuid.as_bytes(),
                name: [0u8; 256],
                name_len: 0,
                vpn_id: node.vpn_id,
                hive_index: index as u16,
                flags: FLAG_ACTIVE,
                _reserved: [0u8; 6],
            };
            entry.name_len = copy_bytes_with_len(&mut entry.name, &node.name) as u16;
            node_entries.push(entry);
        }

        for route in state.routes.iter() {
            let Some(match_kind) = match_kind_from_label(&route.match_kind) else {
                continue;
            };
            let Some(action) = action_from_label(&route.action) else {
                continue;
            };
            let mut entry = RemoteRouteEntry {
                prefix: [0u8; 256],
                prefix_len: 0,
                match_kind,
                action,
                next_hop_hive: [0u8; 32],
                next_hop_hive_len: 0,
                _pad: [0u8; 3],
                metric: route.metric,
                priority: 0,
                flags: FLAG_ACTIVE,
                hive_index: index as u16,
                _reserved: [0u8; 14],
            };
            entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, &route.prefix) as u16;
            entry.next_hop_hive_len =
                copy_bytes_with_len(&mut entry.next_hop_hive, &route.next_hop_hive) as u8;
            route_entries.push(entry);
        }

        for vpn in state.vpns.iter() {
            let Some(match_kind) = match_kind_from_label(&vpn.match_kind) else {
                continue;
            };
            let mut entry = RemoteVpnEntry {
                pattern: [0u8; 256],
                pattern_len: 0,
                match_kind,
                _pad0: 0,
                vpn_id: vpn.vpn_id,
                priority: 0,
                flags: FLAG_ACTIVE,
                hive_index: index as u16,
                _reserved: [0u8; 18],
            };
            entry.pattern_len = copy_bytes_with_len(&mut entry.pattern, &vpn.pattern) as u16;
            vpn_entries.push(entry);
        }

        for tap in state.taps.iter() {
            let Some(mode) = tap_mode_from_label(&tap.mode) else {
                continue;
            };
            let mut entry = RemoteTapEntry {
                match_src: [0u8; 256],
                match_src_len: 0,
                _pad0: [0u8; 6],
                match_dst: [0u8; 256],
                match_dst_len: 0,
                _pad1: [0u8; 6],
                target: [0u8; 256],
                target_len: 0,
                _pad2: [0u8; 6],
                mode,
                enabled: if tap.enabled { 1 } else { 0 },
                flags: FLAG_ACTIVE,
                hive_index: index as u16,
                _reserved: [0u8; 38],
            };
            entry.match_src_len = copy_bytes_with_len(&mut entry.match_src, &tap.match_src) as u16;
            entry.match_dst_len = copy_bytes_with_len(&mut entry.match_dst, &tap.match_dst) as u16;
            entry.target_len = copy_bytes_with_len(&mut entry.target, &tap.target) as u16;
            tap_entries.push(entry);
        }
    }

    if let Err(err) = writer.write_snapshot(
        &hive_entries,
        &node_entries,
        &route_entries,
        &vpn_entries,
        &tap_entries,
    ) {
        tracing::warn!("lsa snapshot write failed: {err}");
        return;
    }
    if let Some(snapshot) = writer.read_snapshot() {
        let mut guard = ctx.lsa_snapshot.lock().await;
        *guard = Some(snapshot);
    }
}

async fn lsa_refresh_loop(
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    hive_id: &str,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
) {
    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;
        let _ = refresh_lsa(
            &lsa_reader,
            &lsa_snapshot,
            hive_id,
            &nodes,
            &peer_nodes,
            &static_routes,
            &fib,
            &reachability,
        )
        .await;
    }
}

async fn lsa_broadcast_loop(ctx: Arc<WanContext>) {
    let mut ticker = time::interval(Duration::from_millis(ctx.hello_interval_ms));
    write_lsa_state(&ctx).await;
    loop {
        ticker.tick().await;
        write_lsa_state(&ctx).await;
        let _ = broadcast_lsa(&ctx, None).await;
        // Option B: the hub re-advertises transitive reachability alongside its LSA beacon.
        let _ = broadcast_reachability(&ctx).await;
    }
}

async fn lsa_stale_loop(ctx: Arc<WanContext>) {
    let mut ticker = time::interval(Duration::from_millis(
        ctx.dead_interval_ms.saturating_div(2).max(1_000),
    ));
    loop {
        ticker.tick().await;
        let now = now_epoch_ms();
        let mut changed = false;
        {
            let mut guard = ctx.lsa_state.lock().await;
            for state in guard.values_mut() {
                if now.saturating_sub(state.last_updated) > ctx.dead_interval_ms {
                    if state.flags & FLAG_STALE == 0 {
                        state.flags |= FLAG_STALE;
                        changed = true;
                    }
                } else if state.flags & FLAG_STALE != 0 {
                    state.flags &= !FLAG_STALE;
                    changed = true;
                }
            }
        }
        if changed {
            write_lsa_state(&ctx).await;
        }
        // Option B: expire hub-vouched reachability entries the hub stopped refreshing, then
        // rebuild the FIB if any were dropped so dead multi-hop routes don't linger.
        let reachability_dropped = {
            let mut table = ctx.reachability.lock().await;
            let before = table.len();
            table.retain(|_, entry| now.saturating_sub(entry.last_updated) <= ctx.dead_interval_ms);
            before != table.len()
        };
        if reachability_dropped {
            rebuild_fib(
                &ctx.fib,
                &ctx.nodes,
                &ctx.peer_nodes,
                &ctx.static_routes,
                &ctx.lsa_snapshot,
                &ctx.reachability,
            )
            .await;
        }
    }
}

async fn wan_listen_loop(listen_addr: String, ctx: Arc<WanContext>) {
    let listener = match TcpListener::bind(&listen_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::warn!("wan listen failed: {err}");
            return;
        }
    };
    tracing::info!(addr = %listen_addr, "wan listening");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(result) => result,
            Err(err) => {
                tracing::warn!("wan accept error: {err}");
                continue;
            }
        };
        let ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            if let Err(err) = accept_wan_connection(stream, ctx).await {
                tracing::warn!("wan connection error: {err}");
            }
        });
    }
}

/// Server side: terminate TLS (or not) per the effective mTLS mode, then hand the
/// stream + the TLS-verified peer hive to `handle_wan_connection`.
async fn accept_wan_connection(stream: TcpStream, ctx: Arc<WanContext>) -> Result<(), RouterError> {
    match ctx.wan_mtls_mode.as_str() {
        "required" => {
            let (tls, hive) = tls_accept(stream, &ctx).await?;
            handle_wan_connection(tls, hive, ctx, false, None).await
        }
        "permissive" => {
            // Detect TLS vs plaintext without consuming: a TLS record starts with
            // 0x16 (handshake). Lets old (plaintext) and new (TLS) peers coexist
            // during rollout. Peek can return 0 before the peer has sent its first
            // byte (a slow/half-open peer) — poll briefly so a slow TLS peer is not
            // misdetected as plaintext, with a bounded timeout to avoid a slowloris
            // hold. Permissive is a TEMPORARY rollout bridge: every accepted
            // plaintext peer is an UNAUTHENTICATED connection (no mTLS) and is
            // logged at WARN so the operator knows the rollout is incomplete and
            // can flip to `required` (the only fully-enforced state).
            let mut probe = [0u8; 1];
            let mut n = 0usize;
            let deadline = time::Instant::now() + Duration::from_secs(5);
            while n == 0 && time::Instant::now() < deadline {
                match time::timeout(Duration::from_millis(500), stream.peek(&mut probe)).await {
                    Ok(Ok(read)) => n = read,
                    Ok(Err(err)) => return Err(err.into()),
                    Err(_) => {} // peek timed out; retry until the deadline
                }
                if n == 0 {
                    time::sleep(Duration::from_millis(50)).await;
                }
            }
            if n >= 1 && probe[0] == 0x16 {
                let (tls, hive) = tls_accept(stream, &ctx).await?;
                handle_wan_connection(tls, hive, ctx, false, None).await
            } else {
                tracing::warn!(
                    "wan.mtls=permissive: accepting an UNAUTHENTICATED plaintext peer (rollout incomplete; flip to 'required' once all peers present certs)"
                );
                handle_wan_connection(stream, None, ctx, false, None).await
            }
        }
        _ => handle_wan_connection(stream, None, ctx, false, None).await,
    }
}

/// Accept a TLS connection and extract the client's authenticated hive_id from
/// its (already CA-verified) certificate.
async fn tls_accept(
    stream: TcpStream,
    ctx: &Arc<WanContext>,
) -> Result<(tokio_rustls::server::TlsStream<TcpStream>, Option<String>), RouterError> {
    let acceptor = ctx
        .wan_tls
        .as_ref()
        .ok_or_else(|| RouterError::Startup("wan.mtls active but no TLS material".to_string()))?
        .acceptor
        .clone();
    let tls = acceptor.accept(stream).await?;
    let hive = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .and_then(peer_hive_from_cert);
    // A CA-signed cert with no extractable hive_id can't be bound to the HELLO,
    // so it is not trustable as a peer identity — reject (defense-in-depth; mesh
    // leaves always carry CN+SAN, so this never fires for a legit peer).
    if hive.is_none() {
        return Err(RouterError::Startup(
            "wan peer certificate has no extractable hive_id".to_string(),
        ));
    }
    Ok((tls, hive))
}

/// Client side: initiate TLS (or not) per the effective mTLS mode, then hand the
/// stream + the TLS-verified server hive to `handle_wan_connection`. The client
/// uses TLS whenever the mode is not "disabled" (its effective mode is already
/// "disabled" if no TLS material loaded). Rollout order is server-first: a new
/// (TLS) client only appears after its uplink is at least `permissive`.
async fn connect_wan_connection(
    stream: TcpStream,
    ctx: &Arc<WanContext>,
    established: &AtomicBool,
) -> Result<(), RouterError> {
    if ctx.wan_mtls_mode == "disabled" {
        return handle_wan_connection(stream, None, Arc::clone(ctx), true, Some(established)).await;
    }
    let connector = ctx
        .wan_tls
        .as_ref()
        .ok_or_else(|| RouterError::Startup("wan.mtls active but no TLS material".to_string()))?
        .connector
        .clone();
    // The CA-only verifier ignores the server name (identity is bound below by
    // comparing the server's cert hive_id to its HELLO); use a fixed placeholder.
    let server_name = rustls::pki_types::ServerName::try_from("mesh").expect("static name");
    let tls = connector.connect(server_name, stream).await?;
    let hive = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .and_then(peer_hive_from_cert);
    if hive.is_none() {
        return Err(RouterError::Startup(
            "wan server certificate has no extractable hive_id".to_string(),
        ));
    }
    handle_wan_connection(tls, hive, Arc::clone(ctx), true, Some(established)).await
}

/// When an uplink dials again: 5 s after a link that got past WAN_ACCEPT, however it ended; after
/// an attempt that did not (no connection, a TLS or handshake error, a WAN_REJECT), 5 s doubling
/// up to 60 s.
struct UplinkRetry {
    failures: u64,
    next_delay_secs: u64,
}

impl UplinkRetry {
    const BASE_SECS: u64 = 5;
    const MAX_SECS: u64 = 60;

    fn new() -> Self {
        Self {
            failures: 0,
            next_delay_secs: Self::BASE_SECS,
        }
    }

    /// Records how an attempt ended and returns the wait before the next one.
    fn after_attempt(&mut self, established: bool) -> Duration {
        if established {
            self.failures = 0;
            self.next_delay_secs = Self::BASE_SECS;
            return Duration::from_secs(Self::BASE_SECS);
        }
        self.failures = self.failures.saturating_add(1);
        let delay_secs = self.next_delay_secs;
        self.next_delay_secs = delay_secs.saturating_mul(2).min(Self::MAX_SECS);
        Duration::from_secs(delay_secs)
    }
}

async fn wan_connect_loop(address: String, ctx: Arc<WanContext>) {
    const WAN_WARN_EVERY_ATTEMPTS: u64 = 12;

    let mut retry = UplinkRetry::new();
    loop {
        let established = AtomicBool::new(false);
        let connect_error = match TcpStream::connect(&address).await {
            Ok(stream) => {
                if retry.failures > 0 {
                    tracing::info!(
                        addr = %address,
                        failures = retry.failures,
                        "wan reconnect succeeded"
                    );
                } else {
                    tracing::info!(addr = %address, "wan connected");
                }
                if let Err(err) = connect_wan_connection(stream, &ctx, &established).await {
                    tracing::warn!("wan uplink error: {err}");
                }
                None
            }
            Err(err) => Some(err),
        };
        let delay = retry.after_attempt(established.load(Ordering::Relaxed));
        if let Some(err) = connect_error {
            let failures = retry.failures;
            if failures == 1 || failures.is_multiple_of(WAN_WARN_EVERY_ATTEMPTS) {
                tracing::warn!(
                    addr = %address,
                    failures,
                    retry_in_secs = delay.as_secs(),
                    "wan connect failed: {err}"
                );
            } else {
                tracing::debug!(
                    addr = %address,
                    failures,
                    retry_in_secs = delay.as_secs(),
                    "wan connect failed: {err}"
                );
            }
        }
        time::sleep(delay).await;
    }
}

const WAN_REJECT_HIVE_NOT_REGISTERED: &str = "HIVE_NOT_REGISTERED";
/// How often an admitted peer's session checks that its hive is still registered.
const HIVE_REGISTRY_RECHECK: Duration = Duration::from_secs(10);
/// How long a rejected peer's link stays open for the reject to be written.
const WAN_REJECT_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether a peer hive may hold a WAN session here: with a registry (the motherbee's, A-48), only a
/// hive it lists, a join in flight included, since the join needs the link; the `WAN_REJECT`
/// reason otherwise. No other hive keeps a registry.
fn wan_peer_admission(registry: Option<&HiveRegistry>, hive_id: &str) -> Result<(), &'static str> {
    match registry {
        Some(registry) if !registry.lists(hive_id) => Err(WAN_REJECT_HIVE_NOT_REGISTERED),
        _ => Ok(()),
    }
}

/// Completes once the registry stops listing `hive_id`, checked every `period`. Without a registry
/// it never completes.
async fn hive_unregistered(registry: Option<&HiveRegistry>, hive_id: &str, period: Duration) {
    match registry {
        Some(registry) => {
            registry
                .until_not(hive_id, period, HiveRegistry::lists)
                .await
        }
        None => std::future::pending().await,
    }
}

/// One WAN link, from the HELLO exchange to its end. `established` is set once the handshake got
/// past WAN_ACCEPT, so an uplink can tell a link that ended from one that never came up.
async fn handle_wan_connection<S>(
    stream: S,
    verified_peer_hive: Option<String>,
    ctx: Arc<WanContext>,
    initiator: bool,
    established: Option<&AtomicBool>,
) -> Result<(), RouterError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, writer) = tokio::io::split(stream);
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let link_peer = Arc::new(std::sync::OnceLock::<String>::new());
    let writer_task = tokio::spawn(write_link_frames(writer, rx, "wan", Arc::clone(&link_peer)));

    let hello_payload = WanHelloPayload {
        protocol: MESH_PROTOCOL_VERSION.to_string(),
        router_id: ctx.router_uuid.to_string(),
        router_name: ctx.router_name.clone(),
        hive_id: ctx.hive_id.clone(),
        capabilities: vec!["lsa".to_string(), "forwarding".to_string()],
        timers: WanTimers {
            hello_interval_ms: ctx.hello_interval_ms,
            dead_interval_ms: ctx.dead_interval_ms,
        },
    };
    if initiator {
        let hello = build_wan_hello(
            &ctx.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            hello_payload.clone(),
        );
        let _ = tx.send(serde_json::to_vec(&hello)?);
    }

    let frame = read_frame(&mut reader)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing HELLO"))?;
    let msg: Message = serde_json::from_slice(&frame)?;
    if initiator
        && is_system_kind(&msg.meta.msg_type)
        && msg.meta.msg.as_deref() == Some(MSG_WAN_REJECT)
    {
        // The uplink loop logs the reason; the link never got past WAN_ACCEPT, so the loop backs
        // off (`UplinkRetry`).
        writer_task.abort();
        let reject: WanRejectPayload = serde_json::from_value(msg.payload)?;
        return Err(io::Error::new(
            io::ErrorKind::ConnectionRefused,
            format!(
                "wan peer rejected this hive: {}: {}",
                reject.reason, reject.message
            ),
        )
        .into());
    }
    if !is_system_kind(&msg.meta.msg_type) || msg.meta.msg.as_deref() != Some(MSG_HELLO) {
        writer_task.abort();
        return Ok(());
    }
    let peer_hello: WanHelloPayload = serde_json::from_value(msg.payload)?;

    // mTLS: bind the cryptographically-verified peer identity (its cert hive_id)
    // to the hive_id it claims in the HELLO. The TLS layer already proved the peer
    // holds a CA-signed cert; this stops a valid mesh member from claiming a
    // DIFFERENT hive_id in the protocol layer (cross-hive impersonation).
    if let Some(ref verified) = verified_peer_hive {
        if verified != &peer_hello.hive_id {
            tracing::warn!(
                claimed_hive = %peer_hello.hive_id,
                cert_hive = %verified,
                "wan peer rejected: HELLO hive_id does not match its TLS certificate"
            );
            writer_task.abort();
            return Ok(());
        }
    }
    let _ = link_peer.set(peer_hello.hive_id.clone());

    // A-48: the motherbee admits a hive, by the id just bound to its certificate, only while its
    // registry lists it. Its own uplinks, if any, are not checked.
    let registry = if initiator {
        None
    } else {
        ctx.hive_registry.as_ref()
    };
    if let Err(reason) = wan_peer_admission(registry, &peer_hello.hive_id) {
        let reject_count = bump_lsa_reject_counter(ctx.as_ref(), LSA_REJECT_NOT_REGISTERED).await;
        let reject = build_wan_reject(
            &ctx.router_uuid.to_string(),
            &peer_hello.router_id,
            &Uuid::new_v4().to_string(),
            WanRejectPayload {
                reason: reason.to_string(),
                message: format!("Hive '{}' is not registered", peer_hello.hive_id),
            },
        );
        let _ = tx.send(serde_json::to_vec(&reject)?);
        tracing::warn!(
            peer_hive = %peer_hello.hive_id,
            peer_router_id = %peer_hello.router_id,
            reason = %reason,
            reject_count = reject_count,
            "wan peer rejected: hive not registered"
        );
        // The writer sends the reject and stops once its channel closes.
        drop(tx);
        let writer_abort = writer_task.abort_handle();
        if time::timeout(WAN_REJECT_FLUSH_TIMEOUT, writer_task)
            .await
            .is_err()
        {
            writer_abort.abort();
        }
        return Ok(());
    }

    if !initiator {
        let hello = build_wan_hello(
            &ctx.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            hello_payload,
        );
        let _ = tx.send(serde_json::to_vec(&hello)?);
    }

    // U-4a: the HELLO has always carried a protocol version and nobody ever looked at it — the
    // field existed and was dead, which is worse than absent because it reads like a guarantee.
    // REPORT ONLY. Rejecting the peer would be self-fencing: the cross-hive SYSTEM_UPDATE rides
    // this mesh and needs LSA visibility of its target, so cutting off an outdated hive severs
    // the very channel that could update it. Hash gates, version reports.
    if peer_hello.protocol != MESH_PROTOCOL_VERSION {
        tracing::warn!(
            peer_hive = %peer_hello.hive_id,
            peer_protocol = %peer_hello.protocol,
            local_protocol = %MESH_PROTOCOL_VERSION,
            "MESH_PROTOCOL_SKEW: peer advertises a different mesh protocol; accepting the session \
             anyway (a version difference is telemetry, not a gate). Bring the fleet to one build."
        );
    }

    let peer_uuid = Uuid::parse_str(&peer_hello.router_id)?;
    let session_epoch = {
        let mut guard = ctx.wan_session_epochs.lock().await;
        let next = guard
            .get(&peer_hello.hive_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        guard.insert(peer_hello.hive_id.clone(), next);
        next
    };
    let accept = build_wan_accept(
        &ctx.router_uuid.to_string(),
        &peer_hello.router_id,
        &Uuid::new_v4().to_string(),
        WanAcceptPayload {
            peer_router_id: peer_hello.router_id.clone(),
            negotiated: WanNegotiated {
                protocol: MESH_PROTOCOL_VERSION.to_string(),
                hello_interval_ms: ctx.hello_interval_ms,
                dead_interval_ms: ctx.dead_interval_ms,
            },
        },
    );
    if let Some(established) = established {
        established.store(true, Ordering::Relaxed);
    }
    let _ = tx.send(serde_json::to_vec(&accept)?);

    {
        let mut guard = ctx.wan_peers.lock().await;
        guard.insert(
            peer_hello.hive_id.clone(),
            WanPeer {
                sender: tx.clone(),
                router_uuid: peer_uuid,
            },
        );
    }

    let _ = broadcast_lsa(&ctx, Some(&peer_hello.hive_id)).await;

    // A hive removed while its link is up is cut off within one registry recheck.
    let unregistered = hive_unregistered(registry, &peer_hello.hive_id, ctx.hive_registry_recheck);
    tokio::pin!(unregistered);
    loop {
        let frame = tokio::select! {
            frame = read_frame(&mut reader) => frame?,
            () = &mut unregistered => {
                tracing::warn!(
                    peer_hive = %peer_hello.hive_id,
                    session_epoch = session_epoch,
                    reason = %WAN_REJECT_HIVE_NOT_REGISTERED,
                    "wan peer session closed: hive no longer registered"
                );
                break;
            }
        };
        match frame {
            Some(frame) => match serde_json::from_slice::<Message>(&frame) {
                Ok(msg) => {
                    handle_wan_message(
                        &msg,
                        &tx,
                        &ctx,
                        &peer_hello.hive_id,
                        peer_uuid,
                        &peer_hello.router_name,
                        session_epoch,
                    )
                    .await?;
                }
                Err(err) => {
                    let reject_count =
                        bump_lsa_reject_counter(ctx.as_ref(), LSA_REJECT_PARSE_ERROR).await;
                    tracing::warn!(
                        peer_hive = %peer_hello.hive_id,
                        session_epoch = session_epoch,
                        reject_count = reject_count,
                        error = %err,
                        "wan frame parse error"
                    );
                }
            },
            None => break,
        }
    }
    {
        let mut guard = ctx.wan_peers.lock().await;
        guard.remove(&peer_hello.hive_id);
    }
    writer_task.abort();
    Ok(())
}

async fn handle_wan_message(
    msg: &Message,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    ctx: &WanContext,
    peer_hive: &str,
    peer_router_uuid: Uuid,
    peer_router_name: &str,
    session_epoch: u64,
) -> Result<(), RouterError> {
    let msg = msg.clone();
    if is_system_kind(&msg.meta.msg_type) {
        if msg.meta.msg.as_deref() == Some(MSG_LSA) {
            let payload: LsaPayload = match serde_json::from_value(msg.payload.clone()) {
                Ok(payload) => payload,
                Err(err) => {
                    let reject_count = bump_lsa_reject_counter(ctx, LSA_REJECT_PARSE_ERROR).await;
                    tracing::warn!(
                        peer_hive = %peer_hive,
                        session_epoch = session_epoch,
                        reject_count = reject_count,
                        error = %err,
                        "lsa payload parse error"
                    );
                    return Ok(());
                }
            };
            let local_uuids = local_node_uuids(&ctx.nodes, &ctx.peer_nodes).await;
            match apply_lsa_payload(
                &ctx.lsa_state,
                &local_uuids,
                peer_hive,
                peer_router_uuid,
                peer_router_name,
                session_epoch,
                payload,
            )
            .await
            {
                LsaApplyResult::Applied {
                    foreign_nodes,
                    duplicate_uuids,
                } => {
                    if !foreign_nodes.is_empty() {
                        let reject_count = count_lsa_rejects(
                            ctx,
                            LSA_REJECT_FOREIGN_NODE,
                            foreign_nodes.len() as u64,
                        )
                        .await;
                        tracing::warn!(
                            reason = LSA_REJECT_FOREIGN_NODE,
                            peer_hive = %peer_hive,
                            nodes = ?foreign_nodes,
                            session_epoch = session_epoch,
                            reject_count = reject_count,
                            "lsa nodes dropped: a hive advertises only nodes named <role>@<itself>"
                        );
                    }
                    if !duplicate_uuids.is_empty() {
                        let reject_count = count_lsa_rejects(
                            ctx,
                            LSA_REJECT_DUPLICATE_UUID,
                            duplicate_uuids.len() as u64,
                        )
                        .await;
                        tracing::warn!(
                            reason = LSA_REJECT_DUPLICATE_UUID,
                            peer_hive = %peer_hive,
                            nodes = ?duplicate_uuids,
                            session_epoch = session_epoch,
                            reject_count = reject_count,
                            "lsa nodes with a UUID already taken: one of this hive's nodes is dropped, one another hive lists resolves to neither hive"
                        );
                    }
                    write_lsa_state(ctx).await;
                }
                LsaApplyResult::Rejected {
                    reason,
                    payload_hive,
                    seq,
                    last_seq,
                    session_epoch,
                } => {
                    let reject_count = bump_lsa_reject_counter(ctx, reason).await;
                    tracing::warn!(
                        reason = reason,
                        peer_hive = %peer_hive,
                        payload_hive = %payload_hive,
                        seq = seq,
                        last_seq = last_seq,
                        session_epoch = session_epoch,
                        reject_count = reject_count,
                        "lsa payload rejected"
                    );
                }
            }
            return Ok(());
        }
        if msg.meta.msg.as_deref() == Some(MSG_WAN_REACHABILITY) {
            apply_wan_reachability(ctx, peer_hive, &msg).await;
            return Ok(());
        }
        if let Some(kind) = msg.meta.msg.as_deref() {
            if kind == MSG_UNREACHABLE || kind == MSG_TTL_EXCEEDED {
                if let Destination::Unicast(dst) = &msg.routing.dst {
                    if let Ok(dst_uuid) = Uuid::parse_str(dst) {
                        let dst_handle = {
                            let nodes_guard = ctx.nodes.lock().await;
                            nodes_guard.get(&dst_uuid).cloned()
                        };
                        if let Some(dst_handle) = dst_handle {
                            let data = serde_json::to_vec(&msg)?;
                            let _ = dst_handle.sender.send(data);
                        }
                    }
                }
                return Ok(());
            }
        }
    }

    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    // EDGE-01: the sender is bound to this session. Its UUID resolves only in the LSA bucket of the
    // peer the frame came from, which apply_lsa_payload lets a peer fill with its own nodes only,
    // named `role@<its cert-bound hive>`. Option B (multi-hop): a frame the hub relays carries the
    // ORIGIN spoke's src, which is not in that bucket; it resolves among the nodes the hub vouches
    // for, and only when this peer is the vouching hub, so the terminal gate ADMITS it for DATA
    // delivery (the EDGE-H4 failure). That src_info is `via_hub`, denied SYSTEM authority downstream
    // in serialize_for_local_delivery. Any other sender is dropped: until 0.1.83 the UUID resolved
    // in every bucket, so a spoke could send as another spoke's node by its UUID
    // (docs/control-path-hardening-v1.md §5).
    let Some(src_info) =
        wan_frame_source(&ctx.lsa_snapshot, &ctx.reachability, src_uuid, peer_hive).await
    else {
        let reject_count = bump_lsa_reject_counter(ctx, LSA_REJECT_FOREIGN_SOURCE).await;
        let now = now_epoch_ms();
        let last = FOREIGN_SOURCE_LAST_LOG_MS.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= FOREIGN_SOURCE_LOG_INTERVAL_MS {
            FOREIGN_SOURCE_LAST_LOG_MS.store(now, Ordering::Relaxed);
            tracing::warn!(
                reason = LSA_REJECT_FOREIGN_SOURCE,
                peer_hive = %peer_hive,
                src = %src_uuid,
                src_l2_name = ?msg.routing.src_l2_name,
                reject_count = reject_count,
                "router dropped a WAN frame: its sender is neither a node of the peer that sent it nor one that peer relays as the hub"
            );
        }
        return Ok(());
    };
    let src_via_hub = src_info.via_hub;

    // The name is `role@<the bucket hive>`: the peer's own, or the origin the hub vouches for. A
    // node name the LSA filter let through already ends that way, so this rewrite is a guard.
    let canonical_src_name = canonical_wan_src_name(&src_info.name, &src_info.hive_id);
    if canonical_src_name != src_info.name {
        tracing::warn!(
            resolved_name = %src_info.name,
            bucket_hive = %src_info.hive_id,
            canonical = %canonical_src_name,
            "wan src name suffix rebound to authenticated bucket hive"
        );
    }
    let src_info = RemoteNodeInfo {
        name: canonical_src_name,
        ..src_info
    };

    // F4/F15: stamp the router-authoritative cross-hive L2 name (resolved from the
    // LSA snapshot) over whatever the WAN frame carried, BEFORE the origin gate in
    // serialize_for_local_delivery. Unlike the local (:915) and peer (:3413) paths
    // the WAN handler never stamped src_l2_name, so a remote peer could otherwise
    // spoof it (e.g. claim SY.orchestrator@<hive>) to pass the protected-action gate.
    // The name is now canonicalized to the authenticated bucket hive (EDGE-01 above).
    let mut msg = msg.clone();
    msg.routing.src_l2_name = Some(src_info.name.clone());

    if msg.routing.ttl == 0 {
        send_ttl_exceeded_to(&msg, sender, ctx.router_uuid)?;
        return Ok(());
    }

    let mut senders: Vec<LocalDelivery> = Vec::new();
    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            if let Ok(dst_uuid) = Uuid::parse_str(dst) {
                let dst_handle = {
                    let nodes_guard = ctx.nodes.lock().await;
                    nodes_guard.get(&dst_uuid).cloned()
                };
                if let Some(dst_handle) = dst_handle {
                    if vpn_allows_between(
                        &msg.meta,
                        &src_info.name,
                        src_info.vpn_id,
                        &dst_handle.name,
                        dst_handle.vpn_id,
                    ) {
                        senders.push(LocalDelivery::to(&dst_handle.name, dst_handle.sender));
                    } else {
                        send_unreachable_to(&msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                    }
                } else {
                    let peer = {
                        let peer_guard = ctx.peer_nodes.lock().await;
                        peer_guard.get(&dst_uuid).cloned()
                    };
                    if let Some(peer_node) = peer {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_info.name,
                            src_info.vpn_id,
                            &peer_node.name,
                            peer_node.vpn_id,
                        ) {
                            if msg.routing.ttl <= 1 {
                                send_ttl_exceeded_to(&msg, sender, ctx.router_uuid)?;
                                return Ok(());
                            }
                            if send_to_peer_router(&ctx.peers, peer_node.router_uuid, &msg).await? {
                                return Ok(());
                            }
                            send_unreachable_to(&msg, sender, ctx.router_uuid, "PEER_UNAVAILABLE")?;
                        } else {
                            send_unreachable_to(&msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        let remote = {
                            let snapshot = ctx.lsa_snapshot.lock().await;
                            find_remote_node(&snapshot, dst_uuid)
                        };
                        let remote = match remote {
                            Some(info) => Some(info),
                            None => find_reachable_node(&*ctx.reachability.lock().await, dst_uuid),
                        };
                        if let Some(remote) = remote {
                            if vpn_allows_between(
                                &msg.meta,
                                &src_info.name,
                                src_info.vpn_id,
                                &remote.name,
                                remote.vpn_id,
                            ) {
                                forward_to_hive(
                                    &remote.next_hop_hive,
                                    &msg,
                                    true,
                                    &ctx.peer_routers,
                                    &ctx.peers,
                                    &ctx.wan_peers,
                                    ctx.router_uuid,
                                    sender,
                                )
                                .await?;
                            } else {
                                send_unreachable_to(&msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                            }
                        } else {
                            send_unreachable_to(&msg, sender, ctx.router_uuid, "NODE_NOT_FOUND")?;
                        }
                    }
                }
            } else {
                let nodes_guard = ctx.nodes.lock().await;
                let route = resolve_by_name(
                    dst,
                    &NodeHandle {
                        name: src_info.name.clone(),
                        vpn_id: src_info.vpn_id,
                        sender: mpsc::unbounded_channel().0,
                        connected_at: 0,
                    },
                    &nodes_guard,
                    &ctx.fib,
                    &msg.meta,
                )
                .await?;
                match route {
                    ResolvedRoute::Drop => {}
                    ResolvedRoute::Unreachable(reason) => {
                        send_unreachable_to(&msg, sender, ctx.router_uuid, reason)?;
                    }
                    ResolvedRoute::Deliver(dst_uuid) => {
                        if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                            if vpn_allows_between(
                                &msg.meta,
                                &src_info.name,
                                src_info.vpn_id,
                                &dst_handle.name,
                                dst_handle.vpn_id,
                            ) {
                                senders.push(LocalDelivery::to(
                                    &dst_handle.name,
                                    dst_handle.sender.clone(),
                                ));
                            } else {
                                send_unreachable_to(&msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                            }
                        } else {
                            send_unreachable_to(&msg, sender, ctx.router_uuid, "NODE_NOT_FOUND")?;
                        }
                    }
                    ResolvedRoute::ForwardRouter(peer_uuid) => {
                        if msg.routing.ttl <= 1 {
                            send_ttl_exceeded_to(&msg, sender, ctx.router_uuid)?;
                            return Ok(());
                        }
                        if send_to_peer_router(&ctx.peers, peer_uuid, &msg).await? {
                            return Ok(());
                        }
                        send_unreachable_to(&msg, sender, ctx.router_uuid, "PEER_UNAVAILABLE")?;
                    }
                    ResolvedRoute::ForwardHive(hive_id) => {
                        forward_to_hive(
                            &hive_id,
                            &msg,
                            true,
                            &ctx.peer_routers,
                            &ctx.peers,
                            &ctx.wan_peers,
                            ctx.router_uuid,
                            sender,
                        )
                        .await?;
                    }
                }
            }
        }
        Destination::Broadcast => {
            let trace_id = match Uuid::parse_str(&msg.routing.trace_id) {
                Ok(value) => value,
                Err(_) => return Ok(()),
            };
            {
                let mut cache = ctx.broadcast_cache.lock().await;
                if !cache.check_and_add(trace_id) {
                    return Ok(());
                }
            }
            let target = msg.meta.target.as_deref();
            let nodes_guard = ctx.nodes.lock().await;
            for (uuid, handle) in nodes_guard.iter() {
                if *uuid == src_uuid {
                    continue;
                }
                if !vpn_allows_broadcast_between(
                    &msg.meta,
                    &src_info.name,
                    src_info.vpn_id,
                    &handle.name,
                    handle.vpn_id,
                ) {
                    continue;
                }
                if let Some(pattern) = target {
                    if !pattern_match(pattern, &handle.name) {
                        continue;
                    }
                }
                senders.push(LocalDelivery::to(&handle.name, handle.sender.clone()));
            }
            if msg.routing.ttl >= 2 {
                broadcast_to_peers(&ctx.peers, &msg).await?;
            }
            if msg.routing.ttl >= 3 {
                broadcast_to_wan(&ctx.wan_peers, &msg).await?;
            }
        }
        Destination::Resolve => {
            let resolved_target = match resolve_target_with_identity(
                &ctx.opa,
                &ctx.hive_id,
                &ctx.identity_frontdesk_node_name,
                &msg,
            )
            .await
            {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("opa resolve failed: {err}");
                    send_unreachable_to(&msg, sender, ctx.router_uuid, "OPA_ERROR")?;
                    return Ok(());
                }
            };
            let Some(target) = resolved_target.as_deref() else {
                send_unreachable_to(&msg, sender, ctx.router_uuid, "OPA_NO_TARGET")?;
                return Ok(());
            };
            let nodes_guard = ctx.nodes.lock().await;
            let route = resolve_by_name(
                target,
                &NodeHandle {
                    name: src_info.name.clone(),
                    vpn_id: src_info.vpn_id,
                    sender: mpsc::unbounded_channel().0,
                    connected_at: 0,
                },
                &nodes_guard,
                &ctx.fib,
                &msg.meta,
            )
            .await?;
            match route {
                ResolvedRoute::Drop => {}
                ResolvedRoute::Unreachable(reason) => {
                    send_unreachable_to(&msg, sender, ctx.router_uuid, reason)?;
                }
                ResolvedRoute::Deliver(dst_uuid) => {
                    if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_info.name,
                            src_info.vpn_id,
                            &dst_handle.name,
                            dst_handle.vpn_id,
                        ) {
                            senders.push(LocalDelivery::to(
                                &dst_handle.name,
                                dst_handle.sender.clone(),
                            ));
                        } else {
                            send_unreachable_to(&msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        send_unreachable_to(&msg, sender, ctx.router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if msg.routing.ttl <= 1 {
                        send_ttl_exceeded_to(&msg, sender, ctx.router_uuid)?;
                        return Ok(());
                    }
                    if send_to_peer_router(&ctx.peers, peer_uuid, &msg).await? {
                        return Ok(());
                    }
                    send_unreachable_to(&msg, sender, ctx.router_uuid, "PEER_UNAVAILABLE")?;
                }
                ResolvedRoute::ForwardHive(hive_id) => {
                    forward_to_hive(
                        &hive_id,
                        &msg,
                        true,
                        &ctx.peer_routers,
                        &ctx.peers,
                        &ctx.wan_peers,
                        ctx.router_uuid,
                        sender,
                    )
                    .await?;
                }
            }
        }
    }

    deliver_locally(
        senders,
        &msg,
        &ctx.memory_reader,
        &ctx.hive_id,
        src_via_hub,
        &ctx.log_stream,
    )
    .await?;
    Ok(())
}

fn peer_socket_path(socket_dir: &Path, router_uuid: Uuid) -> PathBuf {
    let name = format!("irp-{}.sock", router_uuid.simple());
    socket_dir.join(name)
}

async fn peer_discovery_loop(
    self_uuid: Uuid,
    self_router_name: &str,
    self_shm_name: &str,
    hive_id: &str,
    identity_frontdesk_node_name: &str,
    socket_dir: PathBuf,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    opa: Arc<Mutex<OpaResolver>>,
    opa_reader: Arc<Mutex<Option<OpaRegionReader>>>,
    memory_reader: Arc<Mutex<Option<MemoryRegionReader>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    log_stream: stream::StreamHandle,
    is_gateway: bool,
) {
    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        let regions = discover_peer_regions(self_uuid, self_shm_name, hive_id);
        let mut updated_nodes: HashMap<Uuid, PeerNode> = HashMap::new();
        let mut peer_uuids: Vec<Uuid> = Vec::new();
        let mut updated_routers: HashMap<Uuid, PeerRouter> = HashMap::new();
        for region in regions {
            peer_uuids.push(region.router_uuid);
            updated_routers.insert(
                region.router_uuid,
                PeerRouter {
                    uuid: region.router_uuid,
                    is_gateway: region.is_gateway,
                },
            );
            for node in region.nodes {
                updated_nodes.insert(node.uuid, node);
            }
        }
        {
            let mut peer_guard = peer_nodes.lock().await;
            *peer_guard = updated_nodes;
        }
        {
            let mut peer_guard = peer_routers.lock().await;
            *peer_guard = updated_routers;
        }
        rebuild_fib(
            &fib,
            &nodes,
            &peer_nodes,
            &static_routes,
            &lsa_snapshot,
            &reachability,
        )
        .await;

        for peer_uuid in peer_uuids {
            if should_initiate_peer(self_uuid, peer_uuid) {
                let already_connected = {
                    let peers_guard = peers.lock().await;
                    peers_guard.contains_key(&peer_uuid)
                };
                if !already_connected {
                    let peers = Arc::clone(&peers);
                    let peer_nodes = Arc::clone(&peer_nodes);
                    let peer_routers = Arc::clone(&peer_routers);
                    let nodes = Arc::clone(&nodes);
                    let wan_peers = Arc::clone(&wan_peers);
                    let fib = Arc::clone(&fib);
                    let lsa_reader = Arc::clone(&lsa_reader);
                    let static_routes = Arc::clone(&static_routes);
                    let shm = Arc::clone(&shm);
                    let opa = Arc::clone(&opa);
                    let opa_reader = Arc::clone(&opa_reader);
                    let memory_reader = Arc::clone(&memory_reader);
                    let hive_id = hive_id.to_string();
                    let self_router_name = self_router_name.to_string();
                    let self_shm_name = self_shm_name.to_string();
                    let lsa_snapshot = Arc::clone(&lsa_snapshot);
                    let reachability = Arc::clone(&reachability);
                    let log_stream = log_stream.clone();
                    let socket_dir = socket_dir.clone();
                    let identity_frontdesk_node_name = identity_frontdesk_node_name.to_string();
                    tokio::spawn(async move {
                        let _ = connect_to_peer(
                            peer_uuid,
                            self_uuid,
                            &self_router_name,
                            &self_shm_name,
                            &socket_dir,
                            peers,
                            peer_nodes,
                            peer_routers,
                            nodes,
                            wan_peers,
                            lsa_reader,
                            static_routes,
                            shm,
                            opa,
                            opa_reader,
                            memory_reader,
                            &hive_id,
                            &identity_frontdesk_node_name,
                            lsa_snapshot,
                            reachability,
                            fib,
                            log_stream,
                            is_gateway,
                        )
                        .await;
                    });
                }
            }
        }
        ticker.tick().await;
    }
}

fn should_initiate_peer(self_uuid: Uuid, peer_uuid: Uuid) -> bool {
    self_uuid.as_u128() < peer_uuid.as_u128()
}

/// At most one log line per interval about router regions another user could have written.
const UNTRUSTED_REGION_LOG_INTERVAL_MS: u64 = 30_000;
static UNTRUSTED_REGION_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// The sibling routers of this hive, from their regions in `/dev/shm`. A region counts only when
/// this router's user owns it and nobody else may write it: the sockets take only that user
/// (`local_peer_admitted`), and a region anyone made would otherwise put its nodes in this router's
/// FIB (docs/control-path-hardening-v1.md §5).
fn discover_peer_regions(self_uuid: Uuid, self_shm_name: &str, hive_id: &str) -> Vec<PeerRegion> {
    let mut regions = Vec::new();
    let Ok(entries) = fs::read_dir("/dev/shm") else {
        return regions;
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    let own_uid = unsafe { nix::libc::geteuid() };
    for entry in entries.flatten() {
        let name_os = entry.file_name();
        let name = name_os.to_string_lossy();
        if !name.starts_with("jsr-") {
            continue;
        }
        if name.starts_with("jsr-config-") || name.starts_with("jsr-lsa-") {
            continue;
        }
        let shm_name = format!("/{}", name);
        if shm_name == self_shm_name {
            continue;
        }
        let reader = match RouterRegionReader::open_read_only_owned_by(&shm_name, own_uid) {
            Ok(reader) => reader,
            Err(ShmError::Untrusted { uid, mode }) => {
                let now = now_epoch_ms();
                let last = UNTRUSTED_REGION_LAST_LOG_MS.load(Ordering::Relaxed);
                if now.saturating_sub(last) >= UNTRUSTED_REGION_LOG_INTERVAL_MS {
                    UNTRUSTED_REGION_LAST_LOG_MS.store(now, Ordering::Relaxed);
                    tracing::warn!(
                        region = %shm_name,
                        owner_uid = uid,
                        mode = %format_args!("{mode:o}"),
                        router_uid = own_uid,
                        "router skipped a shared-memory region another user owns or may write"
                    );
                }
                continue;
            }
            Err(_) => continue,
        };
        let Some(snapshot) = reader.read_snapshot() else {
            continue;
        };
        if snapshot.header.hive_id != hive_id {
            continue;
        }
        if snapshot.header.router_uuid == self_uuid {
            continue;
        }
        let now = now_epoch_ms();
        if now.saturating_sub(snapshot.header.heartbeat) > HEARTBEAT_STALE_MS {
            continue;
        }
        let mut peer_nodes = Vec::new();
        for node in snapshot.nodes {
            if node.name_len == 0 {
                continue;
            }
            if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
                continue;
            }
            let name = bytes_to_string(&node.name, node.name_len as usize).to_string();
            let uuid = Uuid::from_bytes(node.uuid);
            peer_nodes.push(PeerNode {
                uuid,
                name,
                vpn_id: node.vpn_id,
                router_uuid: snapshot.header.router_uuid,
            });
        }
        regions.push(PeerRegion {
            router_uuid: snapshot.header.router_uuid,
            is_gateway: snapshot.header.is_gateway,
            nodes: peer_nodes,
        });
    }
    regions
}

async fn connect_to_peer(
    peer_uuid: Uuid,
    self_uuid: Uuid,
    self_router_name: &str,
    self_shm_name: &str,
    socket_dir: &Path,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    opa: Arc<Mutex<OpaResolver>>,
    opa_reader: Arc<Mutex<Option<OpaRegionReader>>>,
    memory_reader: Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: &str,
    identity_frontdesk_node_name: &str,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    log_stream: stream::StreamHandle,
    is_gateway: bool,
) -> Result<(), RouterError> {
    let socket_path = peer_socket_path(socket_dir, peer_uuid);
    let stream = UnixStream::connect(&socket_path).await?;
    let (mut reader, writer) = stream.into_split();
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut peers_guard = peers.lock().await;
        if peers_guard.contains_key(&peer_uuid) {
            return Ok(());
        }
        peers_guard.insert(peer_uuid, PeerHandle { sender: tx.clone() });
    }
    let link_peer = Arc::new(std::sync::OnceLock::from(peer_uuid.to_string()));
    let writer_task = tokio::spawn(write_link_frames(writer, rx, "irp", link_peer));

    let hello = build_router_hello(
        &self_uuid.to_string(),
        &Uuid::new_v4().to_string(),
        RouterHelloPayload {
            router_id: self_uuid.to_string(),
            router_name: self_router_name.to_string(),
            shm_name: self_shm_name.to_string(),
            version: "1.0".to_string(),
        },
    );
    let _ = tx.send(serde_json::to_vec(&hello)?);

    while let Some(frame) = read_frame(&mut reader).await? {
        if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
            handle_peer_message(
                &msg,
                &peer_uuid,
                &peers,
                &peer_nodes,
                &peer_routers,
                &nodes,
                &wan_peers,
                &lsa_reader,
                &static_routes,
                &shm,
                &opa,
                &opa_reader,
                &memory_reader,
                hive_id,
                identity_frontdesk_node_name,
                &fib,
                self_uuid,
                is_gateway,
                &lsa_snapshot,
                &reachability,
                &log_stream,
            )
            .await?;
        }
    }
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.remove(&peer_uuid);
    }
    writer_task.abort();
    Ok(())
}

async fn handle_peer_incoming(
    stream: UnixStream,
    router_uuid: Uuid,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    opa: Arc<Mutex<OpaResolver>>,
    opa_reader: Arc<Mutex<Option<OpaRegionReader>>>,
    memory_reader: Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: String,
    identity_frontdesk_node_name: String,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    log_stream: stream::StreamHandle,
    is_gateway: bool,
) -> Result<(), RouterError> {
    let (mut reader, writer) = stream.into_split();
    let frame = read_frame(&mut reader)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing HELLO"))?;
    let msg: Message = serde_json::from_slice(&frame)?;
    if !is_system_kind(&msg.meta.msg_type) || msg.meta.msg.as_deref() != Some(MSG_HELLO) {
        return Ok(());
    }
    let payload: RouterHelloPayload = serde_json::from_value(msg.payload)?;
    let peer_uuid = Uuid::parse_str(&payload.router_id)?;
    {
        let peers_guard = peers.lock().await;
        if peers_guard.contains_key(&peer_uuid) {
            return Ok(());
        }
    }
    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.insert(peer_uuid, PeerHandle { sender: tx.clone() });
    }
    let link_peer = Arc::new(std::sync::OnceLock::from(peer_uuid.to_string()));
    let writer_task = tokio::spawn(write_link_frames(writer, rx, "irp", link_peer));

    while let Some(frame) = read_frame(&mut reader).await? {
        if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
            handle_peer_message(
                &msg,
                &peer_uuid,
                &peers,
                &peer_nodes,
                &peer_routers,
                &nodes,
                &wan_peers,
                &lsa_reader,
                &static_routes,
                &shm,
                &opa,
                &opa_reader,
                &memory_reader,
                &hive_id,
                &identity_frontdesk_node_name,
                &fib,
                router_uuid,
                is_gateway,
                &lsa_snapshot,
                &reachability,
                &log_stream,
            )
            .await?;
        }
    }
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.remove(&peer_uuid);
    }
    writer_task.abort();
    Ok(())
}

/// Frames skipped for being over the socket's size limit, process-wide, across every link. Each
/// warn line about them carries the total.
static OVERSIZE_FRAMES_SKIPPED: AtomicU64 = AtomicU64::new(0);
/// At most one log line per link and interval about the frames it skipped.
const OVERSIZE_FRAME_LOG_INTERVAL: Duration = Duration::from_secs(30);

/// Writes the frames queued for one link (a node, a WAN peer, a sibling router) until the link
/// ends. A frame over the socket's 128 KB limit is skipped and counted, and the link goes on: until
/// 0.1.74 the writer stopped while the link stayed registered, so every later frame to it was lost
/// for good (A-70, D11 of docs/router-stream-v1.md). Any other write error ends the link.
async fn write_link_frames<W>(
    mut writer: W,
    mut rx: mpsc::UnboundedReceiver<Vec<u8>>,
    link: &'static str,
    peer: Arc<std::sync::OnceLock<String>>,
) where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut skipped_unlogged = 0u64;
    let mut last_logged: Option<Instant> = None;
    while let Some(frame) = rx.recv().await {
        if frame.len() > MAX_FRAME_SIZE {
            let skipped_total = OVERSIZE_FRAMES_SKIPPED.fetch_add(1, Ordering::Relaxed) + 1;
            skipped_unlogged += 1;
            if last_logged.is_none_or(|at| at.elapsed() >= OVERSIZE_FRAME_LOG_INTERVAL) {
                tracing::warn!(
                    link,
                    peer = peer.get().map(String::as_str).unwrap_or("-"),
                    frame_len = frame.len(),
                    max = MAX_FRAME_SIZE,
                    skipped = skipped_unlogged,
                    skipped_total,
                    "router skipped a frame over the size limit; the link goes on"
                );
                skipped_unlogged = 0;
                last_logged = Some(Instant::now());
            }
            continue;
        }
        if let Err(err) = write_frame(&mut writer, &frame).await {
            tracing::warn!(
                link,
                peer = peer.get().map(String::as_str).unwrap_or("-"),
                error = %err,
                frame_len = frame.len(),
                "router: link write error; closing the link"
            );
            break;
        }
    }
}

/// `RT.` names are the router's own (operator, 2026-10-16, D12 of docs/router-stream-v1.md): a
/// HELLO with one is accepted only from the router's own process. Anything else could pose as a
/// router's stream and feed records to SY.storage, or take a stream's name first and lock it out.
fn rt_name_allowed(node_name: &str, peer_pid: Option<i32>, own_pid: u32) -> bool {
    let local = node_name.split('@').next().unwrap_or(node_name).trim();
    if !local
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("RT."))
    {
        return true;
    }
    peer_pid.is_some_and(|pid| u32::try_from(pid).is_ok_and(|pid| pid == own_pid))
}

/// At most one log line per interval about frames from the gateway whose sender this router does
/// not know.
const UNKNOWN_REMOTE_SOURCE_LOG_INTERVAL_MS: u64 = 30_000;
static UNKNOWN_REMOTE_SOURCE_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// A frame a sibling router hands over IRP comes from one of its own nodes or, when that router is
/// the hive's gateway, from a node of another hive that the gateway took in from the WAN. Until
/// 0.1.74 only the first was accepted, so a message from another hive to a node on a non-gateway
/// router was dropped (A-70).
///
/// The sender must be in this router's LSA, a directly learned node: the gateway's reachability
/// plane (hub-vouched nodes) lives only on the gateway, so a node relayed spoke to hub to spoke
/// cannot reach a non-gateway router (known limit). Its name is derived here, canonical to its
/// hive like the gateway's WAN ingress does (EDGE-01), not taken from the frame.
async fn remote_source_from_gateway(
    msg: &Message,
    peer_uuid: &Uuid,
    src_uuid: Uuid,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
) -> Option<PeerNode> {
    let from_gateway = peer_routers
        .lock()
        .await
        .get(peer_uuid)
        .is_some_and(|router| router.is_gateway);
    if !from_gateway {
        return None;
    }
    let remote = {
        let snapshot = lsa_snapshot.lock().await;
        find_remote_node(&snapshot, src_uuid)
    }?;
    let name = canonical_wan_src_name(&remote.name, &remote.hive_id);
    if let Some(stamped) = msg.routing.src_l2_name.as_deref() {
        if stamped != name {
            tracing::warn!(
                src = %src_uuid,
                stamped,
                canonical = %name,
                "router: the gateway handed over a frame whose name differs from the LSA's; using the LSA's"
            );
        }
    }
    Some(PeerNode {
        uuid: src_uuid,
        name,
        vpn_id: remote.vpn_id,
        router_uuid: *peer_uuid,
    })
}

async fn handle_peer_message(
    msg: &Message,
    peer_uuid: &Uuid,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_reader: &Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    opa: &Arc<Mutex<OpaResolver>>,
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    memory_reader: &Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: &str,
    identity_frontdesk_node_name: &str,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    router_uuid: Uuid,
    is_gateway: bool,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: &Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    log_stream: &stream::StreamHandle,
) -> Result<(), RouterError> {
    let mut msg = msg.clone();
    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    if is_system_kind(&msg.meta.msg_type) && msg.meta.msg.as_deref() == Some(MSG_OPA_RELOAD) {
        let payload: OpaReloadPayload = serde_json::from_value(msg.payload.clone())?;
        apply_opa_reload(opa, opa_reader, shm, hive_id, &payload).await;
        tracing::info!("opa reload applied (peer)");
        return Ok(());
    }
    let src_node = {
        let peer_guard = peer_nodes.lock().await;
        peer_guard.get(&src_uuid).cloned()
    };
    let src_node = match src_node {
        Some(node) => Some(node),
        None => {
            let mut found =
                remote_source_from_gateway(&msg, peer_uuid, src_uuid, peer_routers, lsa_snapshot)
                    .await;
            let from_gateway = peer_routers
                .lock()
                .await
                .get(peer_uuid)
                .is_some_and(|router| router.is_gateway);
            if found.is_none() && from_gateway {
                // A node the gateway just learned may not be in this router's snapshot yet.
                let _ = refresh_lsa(
                    lsa_reader,
                    lsa_snapshot,
                    hive_id,
                    nodes,
                    peer_nodes,
                    static_routes,
                    fib,
                    reachability,
                )
                .await;
                found = remote_source_from_gateway(
                    &msg,
                    peer_uuid,
                    src_uuid,
                    peer_routers,
                    lsa_snapshot,
                )
                .await;
                if found.is_none() {
                    let now = now_epoch_ms();
                    let last = UNKNOWN_REMOTE_SOURCE_LAST_LOG_MS.load(Ordering::Relaxed);
                    if now.saturating_sub(last) >= UNKNOWN_REMOTE_SOURCE_LOG_INTERVAL_MS {
                        UNKNOWN_REMOTE_SOURCE_LAST_LOG_MS.store(now, Ordering::Relaxed);
                        tracing::warn!(
                            src = %src_uuid,
                            src_l2_name = ?msg.routing.src_l2_name,
                            "router dropped a frame from the gateway: its sender is not in this router's LSA"
                        );
                    }
                }
            }
            found
        }
    };
    let Some(src_node) = src_node else {
        if is_system_kind(&msg.meta.msg_type) {
            if let Some(msg_kind) = msg.meta.msg.as_deref() {
                if msg_kind == MSG_UNREACHABLE || msg_kind == MSG_TTL_EXCEEDED {
                    if let Destination::Unicast(dst) = &msg.routing.dst {
                        if let Ok(dst_uuid) = Uuid::parse_str(dst) {
                            let dst_handle = {
                                let nodes_guard = nodes.lock().await;
                                nodes_guard.get(&dst_uuid).cloned()
                            };
                            if let Some(dst_handle) = dst_handle {
                                let data = serde_json::to_vec(&msg)?;
                                let _ = dst_handle.sender.send(data);
                            }
                        }
                    }
                }
            }
        }
        return Ok(());
    };

    // L2-LOOKUP-5 / L2-LOOKUP-7: stamp the authoritative L2 name resolved from
    // peer_nodes, overwriting anything the forwarding router may have set.
    msg.routing.src_l2_name = Some(src_node.name.clone());

    let _ = refresh_lsa(
        lsa_reader,
        lsa_snapshot,
        hive_id,
        nodes,
        peer_nodes,
        static_routes,
        fib,
        reachability,
    )
    .await;
    let mut senders: Vec<LocalDelivery> = Vec::new();
    if msg.routing.ttl == 0 {
        if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
            send_ttl_exceeded_to(&msg, &peer.sender, router_uuid)?;
        }
        return Ok(());
    }

    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            if let Ok(dst_uuid) = Uuid::parse_str(dst) {
                let dst_handle = {
                    let nodes_guard = nodes.lock().await;
                    nodes_guard.get(&dst_uuid).cloned()
                };
                if let Some(dst_handle) = dst_handle {
                    if vpn_allows_between(
                        &msg.meta,
                        &src_node.name,
                        src_node.vpn_id,
                        &dst_handle.name,
                        dst_handle.vpn_id,
                    ) {
                        senders.push(LocalDelivery::to(&dst_handle.name, dst_handle.sender));
                    } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(&msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                    }
                } else {
                    let remote = {
                        let snapshot = lsa_snapshot.lock().await;
                        find_remote_node(&snapshot, dst_uuid)
                    };
                    let remote = match remote {
                        Some(info) => Some(info),
                        None => find_reachable_node(&*reachability.lock().await, dst_uuid),
                    };
                    if let Some(remote) = remote {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_node.name,
                            src_node.vpn_id,
                            &remote.name,
                            remote.vpn_id,
                        ) {
                            if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                                forward_to_hive(
                                    &remote.next_hop_hive,
                                    &msg,
                                    is_gateway,
                                    peer_routers,
                                    peers,
                                    wan_peers,
                                    router_uuid,
                                    &peer.sender,
                                )
                                .await?;
                            }
                        } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            send_unreachable_to(&msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(&msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
            } else {
                let nodes_guard = nodes.lock().await;
                let route = resolve_by_name(
                    dst,
                    &NodeHandle {
                        name: src_node.name.clone(),
                        vpn_id: src_node.vpn_id,
                        sender: mpsc::unbounded_channel().0,
                        connected_at: 0,
                    },
                    &nodes_guard,
                    fib,
                    &msg.meta,
                )
                .await?;
                match route {
                    ResolvedRoute::Drop => {}
                    ResolvedRoute::Unreachable(reason) => {
                        if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            send_unreachable_to(&msg, &peer.sender, router_uuid, reason)?;
                        }
                    }
                    ResolvedRoute::Deliver(dst_uuid) => {
                        if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                            if vpn_allows_between(
                                &msg.meta,
                                &src_node.name,
                                src_node.vpn_id,
                                &dst_handle.name,
                                dst_handle.vpn_id,
                            ) {
                                senders.push(LocalDelivery::to(
                                    &dst_handle.name,
                                    dst_handle.sender.clone(),
                                ));
                            } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                                send_unreachable_to(
                                    &msg,
                                    &peer.sender,
                                    router_uuid,
                                    "VPN_BLOCKED",
                                )?;
                            }
                        } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            send_unreachable_to(&msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
                        }
                    }
                    ResolvedRoute::ForwardRouter(peer_uuid) => {
                        if let Some(peer) = peers.lock().await.get(&peer_uuid).cloned() {
                            if msg.routing.ttl <= 1 {
                                send_ttl_exceeded_to(&msg, &peer.sender, router_uuid)?;
                                return Ok(());
                            }
                        }
                        if send_to_peer_router(peers, peer_uuid, &msg).await? {
                            return Ok(());
                        }
                        if let Some(peer) = peers.lock().await.get(&peer_uuid).cloned() {
                            send_unreachable_to(
                                &msg,
                                &peer.sender,
                                router_uuid,
                                "PEER_UNAVAILABLE",
                            )?;
                        }
                    }
                    ResolvedRoute::ForwardHive(hive_id) => {
                        if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            forward_to_hive(
                                &hive_id,
                                &msg,
                                is_gateway,
                                peer_routers,
                                peers,
                                wan_peers,
                                router_uuid,
                                &peer.sender,
                            )
                            .await?;
                        }
                    }
                }
            }
        }
        Destination::Broadcast => {
            let target = msg.meta.target.as_deref();
            let nodes_guard = nodes.lock().await;
            for (uuid, handle) in nodes_guard.iter() {
                if *uuid == src_uuid {
                    continue;
                }
                if !vpn_allows_broadcast_between(
                    &msg.meta,
                    &src_node.name,
                    src_node.vpn_id,
                    &handle.name,
                    handle.vpn_id,
                ) {
                    continue;
                }
                if let Some(pattern) = target {
                    if !pattern_match(pattern, &handle.name) {
                        continue;
                    }
                }
                senders.push(LocalDelivery::to(&handle.name, handle.sender.clone()));
            }
            if msg.routing.ttl >= 3 {
                if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    forward_broadcast_to_wan(
                        &msg,
                        is_gateway,
                        peer_routers,
                        peers,
                        wan_peers,
                        router_uuid,
                        &peer.sender,
                    )
                    .await?;
                }
            }
        }
        Destination::Resolve => {
            let resolved_target = match resolve_target_with_identity(
                opa,
                hive_id,
                identity_frontdesk_node_name,
                &msg,
            )
            .await
            {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("opa resolve failed: {err}");
                    if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(&msg, &peer.sender, router_uuid, "OPA_ERROR")?;
                    }
                    return Ok(());
                }
            };
            let Some(target) = resolved_target.as_deref() else {
                if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    send_unreachable_to(&msg, &peer.sender, router_uuid, "OPA_NO_TARGET")?;
                }
                return Ok(());
            };
            let nodes_guard = nodes.lock().await;
            let route = resolve_by_name(
                target,
                &NodeHandle {
                    name: src_node.name.clone(),
                    vpn_id: src_node.vpn_id,
                    sender: mpsc::unbounded_channel().0,
                    connected_at: 0,
                },
                &nodes_guard,
                fib,
                &msg.meta,
            )
            .await?;
            match route {
                ResolvedRoute::Drop => {}
                ResolvedRoute::Unreachable(reason) => {
                    if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(&msg, &peer.sender, router_uuid, reason)?;
                    }
                }
                ResolvedRoute::Deliver(dst_uuid) => {
                    if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_node.name,
                            src_node.vpn_id,
                            &dst_handle.name,
                            dst_handle.vpn_id,
                        ) {
                            senders.push(LocalDelivery::to(
                                &dst_handle.name,
                                dst_handle.sender.clone(),
                            ));
                        } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            send_unreachable_to(&msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(&msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if let Some(peer) = peers.lock().await.get(&peer_uuid).cloned() {
                        if msg.routing.ttl <= 1 {
                            send_ttl_exceeded_to(&msg, &peer.sender, router_uuid)?;
                            return Ok(());
                        }
                    }
                    if send_to_peer_router(peers, peer_uuid, &msg).await? {
                        return Ok(());
                    }
                    if let Some(peer) = peers.lock().await.get(&peer_uuid).cloned() {
                        send_unreachable_to(&msg, &peer.sender, router_uuid, "PEER_UNAVAILABLE")?;
                    }
                }
                ResolvedRoute::ForwardHive(hive_id) => {
                    if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        forward_to_hive(
                            &hive_id,
                            &msg,
                            is_gateway,
                            peer_routers,
                            peers,
                            wan_peers,
                            router_uuid,
                            &peer.sender,
                        )
                        .await?;
                    }
                }
            }
        }
    }

    deliver_locally(senders, &msg, memory_reader, hive_id, false, log_stream).await?;
    Ok(())
}

fn router_name(router_uuid: Uuid) -> String {
    format!("router:{}", router_uuid)
}

fn normalize_name(name: &str, hive_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, hive_id)
    }
}

/// EDGE-01: canonicalize a WAN-resolved src name to `role@bucket_hive`, where `bucket_hive`
/// is the mTLS-authenticated LSA bucket the src UUID resolved in: the bucket of the peer that sent
/// the frame (`wan_frame_source`), which `apply_lsa_payload` fills only from an LSA whose `hive`
/// is that cert-bound peer_hive, or the origin the hub vouches for. The role is the substring
/// before the first `@` (role labels like `SY.admin`/`IO.cloud` never contain `@`); the
/// self-asserted `@hive` suffix carried inside the LSA node name is discarded. A peer
/// authenticated as hive `P` can therefore only assert an origin identity of the form `role@P`
/// and cannot forge a specific-hive identity (e.g. `SY.admin@motherbee`) unless it genuinely is
/// that hive.
fn canonical_wan_src_name(name: &str, bucket_hive: &str) -> String {
    let role = name.split_once('@').map(|(r, _)| r).unwrap_or(name).trim();
    format!("{}@{}", role, bucket_hive)
}

struct RemoteNodeInfo {
    name: String,
    vpn_id: u32,
    /// Origin hive of the node (where it is locally attached). Used for the canonical `role@hive`
    /// name binding and the SYSTEM-authority decision.
    hive_id: String,
    /// Hive to forward toward to reach this node. Equals `hive_id` for a directly-learned node;
    /// for a hub-vouched (`via_hub`) node it is the vouching hub (a direct WAN peer).
    next_hop_hive: String,
    /// True when this node was learned transitively from the hub's reachability advertisement
    /// (not a directly mTLS-authenticated LSA). Admitted for DATA delivery, denied SYSTEM authority.
    via_hub: bool,
}

/// One hub-vouched reachable node in a spoke's `reachability` table (Option B,
/// edge-multihop-reachability-spec-v1). Never merged into the identity-bearing LSA snapshot.
#[derive(Clone, Debug)]
struct ReachabilityEntry {
    name: String,
    /// The node's origin hive (the spoke it is locally attached to).
    origin_hive: String,
    /// The vouching hub to forward toward (a direct WAN peer of this router).
    next_hop_hive: String,
    vpn_id: u32,
    /// Local receive time (ms); entries expire when no advertisement refreshes them.
    last_updated: u64,
}

/// At most one log line per interval about WAN frames whose sender is not bound to their session.
const FOREIGN_SOURCE_LOG_INTERVAL_MS: u64 = 30_000;
static FOREIGN_SOURCE_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// At most one log line per interval about UUIDs that several hives list.
const CONTESTED_UUID_LOG_INTERVAL_MS: u64 = 30_000;
static CONTESTED_UUID_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// The sender of a frame that came in from the WAN peer `peer_hive`, by its UUID: a node of that
/// peer's own LSA bucket, or a node the hub vouches for when that peer is the vouching hub. A peer
/// speaks for its own nodes and, as the hub, for the ones it relays; any other UUID, another hive's
/// node above all, gives None (docs/control-path-hardening-v1.md §5).
async fn wan_frame_source(
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: &Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    src_uuid: Uuid,
    peer_hive: &str,
) -> Option<RemoteNodeInfo> {
    let own = {
        let snapshot = lsa_snapshot.lock().await;
        find_node_of_hive(&snapshot, src_uuid, peer_hive)
    };
    if own.is_some() {
        return own;
    }
    let reachability = reachability.lock().await;
    find_reachable_node(&reachability, src_uuid)
        .filter(|relayed| relayed.next_hop_hive == peer_hive)
}

/// A node of `hive`'s own LSA bucket, by its UUID: `find_remote_node` limited to one hive, which
/// another hive listing the same UUID does not change.
fn find_node_of_hive(
    snapshot: &Option<LsaSnapshot>,
    uuid: Uuid,
    hive: &str,
) -> Option<RemoteNodeInfo> {
    let snapshot = snapshot.as_ref()?;
    let hive_index = snapshot.hives.iter().position(|entry| {
        entry.hive_id_len != 0
            && entry.flags & (HIVE_FLAG_SELF | FLAG_STALE) == 0
            && bytes_to_string(&entry.hive_id, entry.hive_id_len as usize) == hive
    })?;
    let node = snapshot.nodes.iter().find(|node| {
        usize::from(node.hive_index) == hive_index
            && node.name_len != 0
            && node.flags & (FLAG_DELETED | FLAG_STALE) == 0
            && Uuid::from_bytes(node.uuid) == uuid
    })?;
    Some(RemoteNodeInfo {
        name: bytes_to_string(&node.name, node.name_len as usize).to_string(),
        vpn_id: node.vpn_id,
        hive_id: hive.to_string(),
        next_hop_hive: hive.to_string(),
        via_hub: false,
    })
}

/// Whether more than one hive of the snapshot lists `uuid`, this hive and a stale one included.
fn uuid_listed_by_several_hives(snapshot: &LsaSnapshot, uuid: Uuid) -> bool {
    let mut listed_by = snapshot
        .nodes
        .iter()
        .filter(|node| node.name_len != 0 && Uuid::from_bytes(node.uuid) == uuid)
        .map(|node| node.hive_index);
    let Some(first) = listed_by.next() else {
        return false;
    };
    listed_by.any(|hive_index| hive_index != first)
}

fn find_remote_node(snapshot: &Option<LsaSnapshot>, uuid: Uuid) -> Option<RemoteNodeInfo> {
    let snapshot = snapshot.as_ref()?;
    // A UUID that two hives list is neither's: a hive could otherwise take the traffic addressed to
    // another hive's node, replies above all, by listing its UUID, and the region's order (hives by
    // name) would pick the first. Refused, a contested UUID is at worst unreachable, never
    // redirected (docs/control-path-hardening-v1.md §5).
    if uuid_listed_by_several_hives(snapshot, uuid) {
        let now = now_epoch_ms();
        let last = CONTESTED_UUID_LAST_LOG_MS.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= CONTESTED_UUID_LOG_INTERVAL_MS {
            CONTESTED_UUID_LAST_LOG_MS.store(now, Ordering::Relaxed);
            tracing::warn!(
                uuid = %uuid,
                "router refused a UUID that several hives list: no hive receives what is addressed to it"
            );
        }
        return None;
    }
    let mut hives: HashMap<u16, String> = HashMap::new();
    for (idx, hive) in snapshot.hives.iter().enumerate() {
        if hive.hive_id_len == 0 {
            continue;
        }
        if hive.flags & HIVE_FLAG_SELF != 0 {
            continue;
        }
        if hive.flags & FLAG_STALE != 0 {
            continue;
        }
        let name = bytes_to_string(&hive.hive_id, hive.hive_id_len as usize).to_string();
        hives.insert(idx as u16, name);
    }
    for node in snapshot.nodes.iter() {
        if node.name_len == 0 {
            continue;
        }
        if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
            continue;
        }
        let node_uuid = Uuid::from_bytes(node.uuid);
        if node_uuid != uuid {
            continue;
        }
        let hive_id = hives.get(&node.hive_index)?.to_string();
        let name = bytes_to_string(&node.name, node.name_len as usize).to_string();
        return Some(RemoteNodeInfo {
            name,
            vpn_id: node.vpn_id,
            next_hop_hive: hive_id.clone(),
            hive_id,
            via_hub: false,
        });
    }
    None
}

/// Resolve a node UUID against the hub-vouched reachability table (Option B). Returns a
/// `RemoteNodeInfo` flagged `via_hub` whose `next_hop_hive` is the vouching hub (a direct WAN
/// peer) — the forwarding target — while `hive_id` is the node's ORIGIN hive, used only for the
/// canonical name binding. A `via_hub` result is admitted for DATA delivery but denied SYSTEM
/// authority (see `serialize_for_local_delivery`).
fn find_reachable_node(
    reachability: &std::collections::HashMap<Uuid, ReachabilityEntry>,
    uuid: Uuid,
) -> Option<RemoteNodeInfo> {
    reachability.get(&uuid).map(|entry| RemoteNodeInfo {
        name: entry.name.clone(),
        vpn_id: entry.vpn_id,
        hive_id: entry.origin_hive.clone(),
        next_hop_hive: entry.next_hop_hive.clone(),
        via_hub: true,
    })
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn set_socket_mode(path: &Path, mode: u32) -> io::Result<()> {
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(mode & 0o777);
    fs::set_permissions(path, perms)
}

/// Binds one of this router's Unix sockets, the nodes' or the IRP one, with mode 0600: only the
/// router's own user connects (docs/control-path-hardening-v1.md §5). Every accepted connection is
/// checked as well (`local_peer_admitted`), which covers the moment between bind and chmod.
fn listen_local_socket(path: &Path) -> io::Result<UnixListener> {
    ensure_parent_dir(path)?;
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    set_socket_mode(path, 0o600)?;
    Ok(listener)
}

/// Whether a connection accepted on one of this router's sockets comes from the router's own user,
/// `own_uid`, by the peer's credentials (`SO_PEERCRED`). That is root in production: every unit,
/// managed node and Go node connects as root. Another user, or a peer whose credentials cannot be
/// read, is refused with one log line.
fn local_peer_admitted(stream: &UnixStream, own_uid: u32) -> bool {
    let peer_uid = stream.peer_cred().ok().map(|cred| cred.uid());
    if peer_uid == Some(own_uid) {
        return true;
    }
    tracing::warn!(
        peer_uid = ?peer_uid,
        router_uid = own_uid,
        "router refused a local connection: its peer is not the router's own user"
    );
    false
}

struct NodeHandle {
    name: String,
    vpn_id: u32,
    sender: mpsc::UnboundedSender<Vec<u8>>,
    connected_at: u64,
}

impl Clone for NodeHandle {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            vpn_id: self.vpn_id,
            sender: self.sender.clone(),
            connected_at: self.connected_at,
        }
    }
}

#[derive(Clone, Debug)]
struct PeerNode {
    uuid: Uuid,
    name: String,
    vpn_id: u32,
    router_uuid: Uuid,
}

#[derive(Clone, Debug)]
struct PeerRouter {
    uuid: Uuid,
    is_gateway: bool,
}

#[derive(Clone)]
struct PeerHandle {
    sender: mpsc::UnboundedSender<Vec<u8>>,
}

#[derive(Clone)]
struct WanPeer {
    sender: mpsc::UnboundedSender<Vec<u8>>,
    router_uuid: Uuid,
}

#[derive(Clone, Debug, Default)]
struct RemoteHiveState {
    session_epoch: u64,
    last_seq: u64,
    last_updated: u64,
    flags: u16,
    router_uuid: [u8; 16],
    router_name: String,
    nodes: Vec<LsaNode>,
    routes: Vec<LsaRoute>,
    vpns: Vec<LsaVpn>,
    taps: Vec<LsaTap>,
}

struct WanContext {
    router_uuid: Uuid,
    router_name: String,
    hive_id: String,
    identity_frontdesk_node_name: String,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
    /// The hive registry the motherbee admits WAN peers against (A-48, its loose reading:
    /// `HiveRegistry::lists`); `None` on every other hive.
    hive_registry: Option<HiveRegistry>,
    /// How often an admitted peer's session checks that its hive is still listed.
    hive_registry_recheck: Duration,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_state: Arc<Mutex<std::collections::HashMap<String, RemoteHiveState>>>,
    reachability: Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    lsa_writer: Arc<Mutex<Option<LsaRegionWriter>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    tap_rules: Arc<Mutex<Vec<TapEntry>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    broadcast_cache: Arc<Mutex<BroadcastCache>>,
    lsa_seq: Arc<Mutex<u64>>,
    log_stream: stream::StreamHandle,
    memory_reader: Arc<Mutex<Option<MemoryRegionReader>>>,
    opa: Arc<Mutex<OpaResolver>>,
    wan_session_epochs: Arc<Mutex<std::collections::HashMap<String, u64>>>,
    lsa_reject_counters: Arc<Mutex<std::collections::HashMap<String, u64>>>,
    /// Effective WAN mTLS mode: "disabled" | "permissive" | "required". Forced to
    /// "disabled" when `wan_tls` is None (certs missing) so a node without certs
    /// never bricks the mesh mid-rollout.
    wan_mtls_mode: String,
    /// Loaded TLS material (acceptor/connector). None = plaintext WAN.
    wan_tls: Option<WanTls>,
}

struct WanTls {
    acceptor: tokio_rustls::TlsAcceptor,
    connector: tokio_rustls::TlsConnector,
}

#[derive(Clone, Debug)]
struct StaticRoute {
    pattern: String,
    match_kind: u8,
    action: u8,
    next_hop_hive: String,
    priority: u16,
    metric: u32,
    installed_at: u64,
}

const ADMIN_DISTANCE_LOCAL: u8 = 0;
const ADMIN_DISTANCE_STATIC: u8 = 1;
const ADMIN_DISTANCE_LSA: u8 = 2;
/// Hub-vouched multi-hop reachability (Option B). Strictly LESS preferred than a directly-learned
/// LSA node for the same name, so a direct route always wins when both exist.
const ADMIN_DISTANCE_REACHABILITY: u8 = 3;

#[derive(Clone, Debug)]
enum FibSource {
    LocalNode,
    PeerNode,
    StaticRoute,
    LsaNode,
    /// A route of another hive's LSA. It applies only where `lsa_route_applies` says.
    LsaRoute {
        advertised_by: String,
    },
    /// Hub-vouched multi-hop reachability (Option B): forwards toward the vouching hub next-hop.
    ReachableNode,
}

#[derive(Clone, Debug)]
enum FibNextHop {
    Local(Uuid),
    Router(Uuid),
    Hive(String),
}

#[derive(Clone, Debug)]
struct FibEntry {
    pattern: String,
    match_kind: u8,
    vpn_id: u32,
    source: FibSource,
    next_hop: Option<FibNextHop>,
    admin_distance: u8,
    priority: u16,
    metric: u32,
    installed_at: u64,
    action: u8,
}

enum ResolvedRoute {
    Drop,
    Unreachable(&'static str),
    Deliver(Uuid),
    ForwardRouter(Uuid),
    ForwardHive(String),
}

struct PeerRegion {
    router_uuid: Uuid,
    is_gateway: bool,
    nodes: Vec<PeerNode>,
}

struct BroadcastCache {
    seen: HashMap<Uuid, u64>,
    ttl_ms: u64,
}

impl BroadcastCache {
    fn new() -> Self {
        Self {
            seen: HashMap::new(),
            ttl_ms: 30_000,
        }
    }

    fn check_and_add(&mut self, trace_id: Uuid) -> bool {
        let now = now_epoch_ms();
        self.seen
            .retain(|_, ts| now.saturating_sub(*ts) <= self.ttl_ms);
        if self.seen.contains_key(&trace_id) {
            return false;
        }
        self.seen.insert(trace_id, now);
        true
    }
}

async fn refresh_config(
    config_reader: &Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: &Arc<Mutex<Vec<VpnAssignment>>>,
    tap_rules: &Arc<Mutex<Vec<TapEntry>>>,
    config_version: &Arc<Mutex<u64>>,
    hive_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: &Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
    force: bool,
) -> Option<ConfigSnapshot> {
    let mut last_snapshot = None;
    for attempt in 0..2 {
        let snapshot = read_config_snapshot_for_routing(
            config_reader,
            hive_id,
            HEARTBEAT_STALE_MS,
            force && attempt == 0,
        )
        .await?;

        let mut version_guard = config_version.lock().await;
        if force && attempt == 0 && snapshot.header.config_version == *version_guard {
            drop(version_guard);
            let mut reader = config_reader.lock().await;
            *reader = None;
            if let Ok(new_reader) =
                ConfigRegionReader::open_read_only(&format!("/jsr-config-{}", hive_id))
            {
                *reader = Some(new_reader);
            }
            continue;
        }

        if snapshot.header.config_version == *version_guard {
            last_snapshot = Some(snapshot);
            break;
        }

        if force || snapshot.header.config_version != *version_guard {
            let mut routes_guard = static_routes.lock().await;
            *routes_guard = snapshot
                .routes
                .iter()
                // Skip FLAG_FROZEN entries: they are system-owned authority
                // markers published for observability only (see FLAG_FROZEN); they
                // must never fold into the forwarding FIB.
                .filter(|route| {
                    (route.flags == 0 || (route.flags & FLAG_ACTIVE != 0))
                        && route.flags & FLAG_FROZEN == 0
                })
                .map(|route| StaticRoute {
                    pattern: bytes_to_string(&route.prefix, route.prefix_len as usize).to_string(),
                    match_kind: route.match_kind,
                    action: route.action,
                    next_hop_hive: bytes_to_string(
                        &route.next_hop_hive,
                        route.next_hop_hive_len as usize,
                    )
                    .to_string(),
                    priority: route.priority,
                    metric: route.metric,
                    installed_at: route.installed_at,
                })
                .collect();
            routes_guard.sort_by(|a, b| (a.priority, a.metric).cmp(&(b.priority, b.metric)));
            drop(routes_guard);
            {
                let mut vpn_guard = vpn_rules.lock().await;
                *vpn_guard = snapshot.vpns.clone();
            }
            {
                let mut tap_guard = tap_rules.lock().await;
                *tap_guard = snapshot.taps.clone();
            }
            *version_guard = snapshot.header.config_version;
            tracing::info!(
                config_version = snapshot.header.config_version,
                routes = snapshot.routes.len(),
                vpns = snapshot.vpns.len(),
                taps = snapshot.taps.len(),
                "config snapshot updated"
            );
            reassign_vpns(&snapshot, nodes, shm).await;
            rebuild_fib(
                fib,
                nodes,
                peer_nodes,
                static_routes,
                lsa_snapshot,
                reachability,
            )
            .await;
        }
        last_snapshot = Some(snapshot);
        break;
    }
    last_snapshot
}

async fn read_config_snapshot_for_routing(
    config_reader: &Arc<Mutex<Option<ConfigRegionReader>>>,
    hive_id: &str,
    stale_after_ms: u64,
    force_reopen: bool,
) -> Option<ConfigSnapshot> {
    let shm_name = format!("/jsr-config-{}", hive_id);
    let now = now_epoch_ms();
    let mut guard = config_reader.lock().await;

    if force_reopen {
        *guard = None;
    }

    let needs_reopen = match guard.as_ref() {
        None => true,
        Some(reader) => match reader.read_header() {
            Some(header) => now.saturating_sub(header.heartbeat) > stale_after_ms,
            None => true,
        },
    };
    if needs_reopen {
        *guard = ConfigRegionReader::open_read_only(&shm_name).ok();
    }

    if let Some(snapshot) = guard.as_ref().and_then(|reader| reader.read_snapshot()) {
        return Some(snapshot);
    }

    *guard = ConfigRegionReader::open_read_only(&shm_name).ok();
    guard.as_ref().and_then(|reader| reader.read_snapshot())
}

async fn unload_opa_user_policy(
    opa: &Arc<Mutex<OpaResolver>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
) {
    opa.lock().await.unload();
    let mut shm_guard = shm.lock().await;
    shm_guard.update_opa_status(0, OPA_STATUS_OK);
}

async fn apply_opa_reload(
    opa: &Arc<Mutex<OpaResolver>>,
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    hive_id: &str,
    payload: &OpaReloadPayload,
) {
    let Some(snapshot) = read_opa_snapshot(opa_reader, hive_id).await else {
        tracing::warn!("opa reload ignored: opa region not available");
        return;
    };
    if payload.version != 0 && payload.version != snapshot.header.policy_version {
        tracing::warn!(
            payload_version = payload.version,
            shm_version = snapshot.header.policy_version,
            "opa reload version mismatch"
        );
    }
    if let Some(hash) = payload.hash.as_deref() {
        match parse_sha256_hash(hash) {
            Some(expected) => {
                if snapshot.header.wasm_hash != expected {
                    tracing::error!(
                        payload_hash = %hash,
                        shm_hash = %format_sha256(&snapshot.header.wasm_hash),
                        "opa reload rejected: hash mismatch"
                    );
                    return;
                }
            }
            None => {
                tracing::error!(payload_hash = %hash, "opa reload rejected: hash invalid");
                return;
            }
        }
    } else {
        tracing::error!("opa reload rejected: hash missing");
        return;
    }
    apply_opa_snapshot(opa, shm, hive_id, &snapshot).await;
}

fn parse_sha256_hash(value: &str) -> Option<[u8; 32]> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (idx, chunk) in value.as_bytes().chunks(2).enumerate() {
        let hi = (*chunk.first()? as char).to_digit(16)? as u8;
        let lo = (*chunk.get(1)? as char).to_digit(16)? as u8;
        out[idx] = (hi << 4) | lo;
    }
    Some(out)
}

fn format_sha256(bytes: &[u8; 32]) -> String {
    let mut out = String::from("sha256:");
    for byte in bytes {
        out.push_str(&format!("{:02x}", byte));
    }
    out
}

async fn maybe_refresh_opa_from_shm(
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    opa: &Arc<Mutex<OpaResolver>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    hive_id: &str,
) {
    let header = read_opa_header(opa_reader, hive_id).await;
    let Some(header) = header else {
        return;
    };
    if header.policy_version == 0 && header.wasm_size == 0 && header.status == OPA_STATUS_OK {
        // Cleared (opa_clear) -- also caught here if the OPA_RELOAD broadcast was missed.
        let holds_policy = opa.lock().await.has_policy();
        if holds_policy {
            unload_opa_user_policy(opa, shm).await;
        }
        return;
    }
    if header.policy_version == 0 || header.wasm_size == 0 {
        return;
    }
    if header.status == OPA_STATUS_LOADING {
        return;
    }
    if header.status == OPA_STATUS_ERROR {
        return;
    }
    let (current_version, _) = {
        let guard = opa.lock().await;
        guard.status()
    };
    if header.policy_version == current_version {
        return;
    }
    let Some(snapshot) = read_opa_snapshot(opa_reader, hive_id).await else {
        tracing::warn!(
            version = header.policy_version,
            "opa refresh skipped: unable to read snapshot"
        );
        return;
    };
    apply_opa_snapshot(opa, shm, hive_id, &snapshot).await;
}

async fn read_opa_header(
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    hive_id: &str,
) -> Option<crate::shm::OpaHeaderSnapshot> {
    read_opa_header_for_reload(opa_reader, hive_id, HEARTBEAT_STALE_MS).await
}

async fn read_opa_header_for_reload(
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    hive_id: &str,
    stale_after_ms: u64,
) -> Option<crate::shm::OpaHeaderSnapshot> {
    let shm_name = format!("/jsr-opa-{}", hive_id);
    let now = now_epoch_ms();
    let mut guard = opa_reader.lock().await;

    let needs_reopen = match guard.as_ref() {
        None => true,
        Some(reader) => match reader.read_header() {
            Some(header) => now.saturating_sub(header.heartbeat) > stale_after_ms,
            None => true,
        },
    };
    if needs_reopen {
        *guard = OpaRegionReader::open_read_only(&shm_name).ok();
    }

    if let Some(header) = guard.as_ref().and_then(|reader| reader.read_header()) {
        return Some(header);
    }

    *guard = OpaRegionReader::open_read_only(&shm_name).ok();
    guard.as_ref().and_then(|reader| reader.read_header())
}

async fn read_opa_snapshot(
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    hive_id: &str,
) -> Option<OpaSnapshot> {
    read_opa_snapshot_for_reload(opa_reader, hive_id, HEARTBEAT_STALE_MS).await
}

async fn read_opa_snapshot_for_reload(
    opa_reader: &Arc<Mutex<Option<OpaRegionReader>>>,
    hive_id: &str,
    stale_after_ms: u64,
) -> Option<OpaSnapshot> {
    let shm_name = format!("/jsr-opa-{}", hive_id);
    let now = now_epoch_ms();
    let mut guard = opa_reader.lock().await;

    let needs_reopen = match guard.as_ref() {
        None => true,
        Some(reader) => match reader.read_header() {
            Some(header) => now.saturating_sub(header.heartbeat) > stale_after_ms,
            None => true,
        },
    };
    if needs_reopen {
        *guard = OpaRegionReader::open_read_only(&shm_name).ok();
    }

    if let Some(snapshot) = guard.as_ref().and_then(|reader| reader.read_snapshot()) {
        return Some(snapshot);
    }

    *guard = OpaRegionReader::open_read_only(&shm_name).ok();
    guard.as_ref().and_then(|reader| reader.read_snapshot())
}

async fn apply_opa_snapshot(
    opa: &Arc<Mutex<OpaResolver>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    hive_id: &str,
    snapshot: &OpaSnapshot,
) {
    if snapshot.wasm.is_empty() {
        // Version 0 + no wasm is SY.opa.rules saying "no user policy" (opa_clear): unload what
        // this router still holds in memory. Before this, an empty region was ignored and the
        // router kept enforcing the old policy until restarted.
        if snapshot.header.policy_version == 0 {
            unload_opa_user_policy(opa, shm).await;
            return;
        }
        tracing::warn!(
            version = snapshot.header.policy_version,
            "opa reload skipped: empty policy"
        );
        return;
    }
    {
        let mut shm_guard = shm.lock().await;
        shm_guard.update_opa_status(snapshot.header.policy_version, OPA_STATUS_LOADING);
    }
    let entrypoint = if snapshot.header.entrypoint.is_empty() {
        None
    } else {
        Some(snapshot.header.entrypoint.clone())
    };
    let data_bundle_path = opa_data_bundle_path();
    let data_bundle = match load_opa_data_bundle(&data_bundle_path, hive_id) {
        Ok(Some(bundle)) => {
            tracing::info!(
                path = %data_bundle_path.display(),
                bytes = bundle.len(),
                "opa data bundle loaded from disk"
            );
            Some(bundle)
        }
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                path = %data_bundle_path.display(),
                "opa data bundle read failed: {err}; using embedded policy data"
            );
            None
        }
    };
    let result = {
        let mut opa_guard = opa.lock().await;
        opa_guard.reload(
            snapshot.header.policy_version,
            entrypoint,
            &snapshot.wasm,
            data_bundle.as_deref(),
        )
    };
    let (version, status) = {
        let opa_guard = opa.lock().await;
        opa_guard.status()
    };
    {
        let mut shm_guard = shm.lock().await;
        shm_guard.update_opa_status(version, status);
    }
    match result {
        Ok(()) => {
            tracing::info!(version = version, "opa reload applied");
        }
        Err(err) => {
            tracing::warn!(
                version = snapshot.header.policy_version,
                "opa reload failed: {err}"
            );
        }
    }
}

fn opa_data_bundle_path() -> PathBuf {
    crate::paths::storage_root_dir()
        .join("opa")
        .join("current")
        .join("data.json")
}

fn load_opa_data_bundle(path: &Path, hive_id: &str) -> Result<Option<String>, io::Error> {
    match fs::read_to_string(path) {
        Ok(content) => {
            if content.trim().is_empty() {
                Ok(None)
            } else {
                let mut root: serde_json::Value =
                    serde_json::from_str(&content).map_err(|err| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("invalid OPA data bundle JSON: {err}"),
                        )
                    })?;
                if let Ok(snapshot) = read_identity_snapshot(hive_id) {
                    inject_identity_data(&mut root, &snapshot);
                }
                Ok(Some(root.to_string()))
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn read_identity_snapshot(
    hive_id: &str,
) -> Result<crate::shm::IdentitySnapshot, crate::shm::ShmError> {
    let shm_name = format!("/jsr-identity-{}", hive_id);
    let open_started = Instant::now();
    let reader = IdentityRegionReader::open_read_only_auto(&shm_name)?;
    let header_state = reader.debug_state();
    let open_elapsed_us = open_started.elapsed().as_micros() as u64;
    let read_started = Instant::now();
    match reader.try_read_snapshot() {
        Ok(snapshot) => {
            tracing::info!(
                shm = %shm_name,
                open_elapsed_us,
                read_elapsed_us = read_started.elapsed().as_micros() as u64,
                header_seq = header_state.map(|s| s.seq),
                header_tenant_count = header_state.map(|s| s.tenant_count),
                header_ilk_count = header_state.map(|s| s.ilk_count),
                header_ich_count = header_state.map(|s| s.ich_count),
                header_mapping_count = header_state.map(|s| s.ich_mapping_count),
                header_updated_at = header_state.map(|s| s.updated_at),
                tenant_count = snapshot.header.tenant_count,
                ilk_count = snapshot.header.ilk_count,
                ich_count = snapshot.header.ich_count,
                ich_mapping_count = snapshot.header.ich_mapping_count,
                updated_at = snapshot.header.updated_at,
                "router identity snapshot read succeeded"
            );
            Ok(snapshot)
        }
        Err(err) => {
            tracing::warn!(
                shm = %shm_name,
                open_elapsed_us,
                read_elapsed_us = read_started.elapsed().as_micros() as u64,
                header_seq = header_state.map(|s| s.seq),
                header_tenant_count = header_state.map(|s| s.tenant_count),
                header_ilk_count = header_state.map(|s| s.ilk_count),
                header_ich_count = header_state.map(|s| s.ich_count),
                header_mapping_count = header_state.map(|s| s.ich_mapping_count),
                header_updated_at = header_state.map(|s| s.updated_at),
                error = %err,
                "router identity snapshot read failed"
            );
            Err(err)
        }
    }
}

/// Routing-layer resolution under the OPA-dual composition order:
///
///   system authority gate  (upstream, in `serialize_for_local_delivery`)
///     -> system route       (identity pre-resolve: a forced frontdesk target)
///       -> user route       (operator OPA policy, `resolve_target`)
///
/// The SYSTEM route takes precedence and SHORT-CIRCUITS: when identity
/// pre-resolve pins a target it returns immediately and the user OPA policy is
/// NOT consulted. Only when the system yields no target does the user layer run.
/// (Authority allow/deny is enforced earlier, at delivery; this function is the
/// routing/selection half.)
async fn resolve_target_with_identity(
    opa: &Arc<Mutex<OpaResolver>>,
    hive_id: &str,
    identity_frontdesk_node_name: &str,
    msg: &Message,
) -> Result<Option<String>, crate::opa::OpaError> {
    let resolve_started = Instant::now();
    let mut msg_for_opa = msg.clone();
    let src_ilk = get_src_ilk_from_meta(&msg.meta);
    tracing::info!(
        trace_id = %msg.routing.trace_id,
        src_ilk = ?src_ilk,
        identity_frontdesk_node_name = %identity_frontdesk_node_name,
        "router starting identity-aware resolve"
    );
    // SYSTEM route (selection), OPA-backed (policy/system.rego frontdesk_route) — short-circuits
    // before the USER OPA policy. Case A: NO src_ilk at all. Decided here, WITHOUT the identity
    // snapshot, because it needs no identity data and must still force the frontdesk even when the
    // identity SHM is unavailable (the common cause of a missing ilk is a provisioning failure with
    // identity down). The present-ilk case (B) is handled below after canonicalization.
    if src_ilk.is_none() && system_policy::route_to_frontdesk(None, false) {
        tracing::info!(
            trace_id = %msg.routing.trace_id,
            forced_target = %identity_frontdesk_node_name,
            elapsed_us = resolve_started.elapsed().as_micros() as u64,
            "identity pre-resolve forced frontdesk target (no src_ilk)"
        );
        return Ok(Some(identity_frontdesk_node_name.to_string()));
    }
    let mut dynamic_opa_data: Option<serde_json::Value> = None;
    match read_identity_snapshot(hive_id) {
        Ok(snapshot) => {
            dynamic_opa_data = Some(build_dynamic_identity_opa_data(&snapshot));
            let now_ms = now_epoch_ms();
            let registration_status = src_ilk.as_deref().and_then(|src_ilk| {
                let (_, status) = canonicalize_src_ilk_and_status(&snapshot, src_ilk, now_ms);
                status
            });
            if let Some(forced_target) = apply_identity_pre_resolve(
                &mut msg_for_opa,
                identity_frontdesk_node_name,
                &snapshot,
                now_ms,
            ) {
                tracing::info!(
                    trace_id = %msg.routing.trace_id,
                    src_ilk = ?src_ilk,
                    registration_status = ?registration_status,
                    forced_target = %forced_target,
                    elapsed_us = resolve_started.elapsed().as_micros() as u64,
                    "identity pre-resolve forced frontdesk target"
                );
                return Ok(Some(forced_target));
            }
            if src_ilk.is_some() {
                tracing::info!(
                    trace_id = %msg.routing.trace_id,
                    src_ilk = ?src_ilk,
                    registration_status = ?registration_status,
                    elapsed_us = resolve_started.elapsed().as_micros() as u64,
                    "identity pre-resolve did not force target"
                );
            }
        }
        Err(err) if src_ilk.is_some() => {
            tracing::warn!(
                trace_id = %msg.routing.trace_id,
                src_ilk = ?src_ilk,
                shm = %format!("/jsr-identity-{hive_id}"),
                error = %err,
                elapsed_us = resolve_started.elapsed().as_micros() as u64,
                "identity pre-resolve skipped: identity snapshot unavailable"
            );
        }
        Err(_) => {}
    }
    let mut guard = opa.lock().await;
    guard.resolve_target_with_data(&msg_for_opa, dynamic_opa_data.as_ref())
}

fn apply_identity_pre_resolve(
    msg: &mut Message,
    identity_frontdesk_node_name: &str,
    snapshot: &crate::shm::IdentitySnapshot,
    now_ms: u64,
) -> Option<String> {
    let src_ilk = get_src_ilk_from_meta(&msg.meta)?;
    let (canonical_src_ilk, registration_status) =
        canonicalize_src_ilk_and_status(snapshot, &src_ilk, now_ms);
    if let Some(canonical) = canonical_src_ilk {
        if canonical != src_ilk {
            set_src_ilk_in_meta(&mut msg.meta, &canonical);
        }
        // Case B: a PRESENT src_ilk. The routing DECISION lives in policy (system.rego
        // frontdesk_route); the router only substitutes the per-hive frontdesk NODE NAME here.
        // Present ilk => src_ilk_present = true, so only registration_status == "temporary" forces.
        if system_policy::route_to_frontdesk(registration_status.as_deref(), true) {
            return Some(identity_frontdesk_node_name.to_string());
        }
    }
    None
}

fn canonicalize_src_ilk_and_status(
    snapshot: &crate::shm::IdentitySnapshot,
    src_ilk: &str,
    now_ms: u64,
) -> (Option<String>, Option<String>) {
    let Some(src_ilk_bytes) = parse_prefixed_uuid_bytes(src_ilk, "ilk") else {
        return (None, None);
    };
    let mut canonical = src_ilk_bytes;
    for alias in &snapshot.ilk_aliases {
        if alias.flags & FLAG_ACTIVE == 0 {
            continue;
        }
        if alias.expires_at <= now_ms {
            continue;
        }
        if alias.old_ilk_id == src_ilk_bytes {
            canonical = alias.canonical_ilk_id;
            break;
        }
    }
    let canonical_str = format_prefixed_uuid("ilk", &canonical);
    let status = snapshot
        .ilks
        .iter()
        .find(|ilk| ilk.flags & FLAG_ACTIVE != 0 && ilk.ilk_id == canonical)
        .map(|ilk| match ilk.registration_status {
            0 => "temporary".to_string(),
            1 => "partial".to_string(),
            2 => "complete".to_string(),
            _ => "unknown".to_string(),
        });
    (Some(canonical_str), status)
}

fn parse_prefixed_uuid_bytes(value: &str, prefix: &str) -> Option<[u8; 16]> {
    let expected = format!("{prefix}:");
    let raw = value.trim().strip_prefix(&expected)?;
    let uuid = Uuid::parse_str(raw).ok()?;
    Some(*uuid.as_bytes())
}

fn get_src_ilk_from_meta(meta: &Meta) -> Option<String> {
    meta.src_ilk
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn set_src_ilk_in_meta(meta: &mut Meta, src_ilk: &str) {
    meta.src_ilk = Some(src_ilk.to_string());
}

fn inject_identity_data(root: &mut serde_json::Value, snapshot: &crate::shm::IdentitySnapshot) {
    let Some(obj) = root.as_object_mut() else {
        return;
    };

    let now_ms = now_epoch_ms();
    let mut identity_obj = serde_json::Map::new();
    for ilk in &snapshot.ilks {
        if ilk.flags & FLAG_ACTIVE == 0 {
            continue;
        }
        let ilk_id = format_prefixed_uuid("ilk", &ilk.ilk_id);
        let tenant_id = format_prefixed_uuid("tnt", &ilk.tenant_id);
        let registration_status = match ilk.registration_status {
            0 => "temporary",
            1 => "partial",
            2 => "complete",
            _ => "temporary",
        };
        let ilk_type = match ilk.ilk_type {
            0 => "human",
            1 => "agent",
            2 => "system",
            // Fail SAFE: an out-of-range byte must NOT be up-converted to the
            // privileged `system` type in the OPA input. Present it as "unknown"
            // (matching the SDK's ilk_type_from_shm) so any policy gating on
            // ilk_type == "system" denies rather than grants.
            _ => "unknown",
        };
        let handler_node = fixed_str(&ilk.handler_node);
        let payload = serde_json::json!({
            "tenant_id": tenant_id,
            "ilk_type": ilk_type,
            "registration_status": registration_status,
            "handler_node": handler_node,
            "role_hash": role_hash_value(&ilk.role_hash),
            "skill_hashes": skill_hashes_value(&ilk.skill_hashes, ilk.skill_count as usize),
            "handbook_hashes": handbook_hashes_value(&ilk.handbook_hashes, ilk.handbook_count as usize),
            "personality_hash": role_hash_value(&ilk.personality_hash)
        });
        identity_obj.insert(ilk_id, payload);
    }

    let mut alias_obj = serde_json::Map::new();
    for alias in &snapshot.ilk_aliases {
        if alias.flags & FLAG_ACTIVE == 0 {
            continue;
        }
        if alias.expires_at <= now_ms {
            continue;
        }
        let old_ilk = format_prefixed_uuid("ilk", &alias.old_ilk_id);
        let canonical_ilk = format_prefixed_uuid("ilk", &alias.canonical_ilk_id);
        alias_obj.insert(old_ilk, serde_json::Value::String(canonical_ilk));
    }

    obj.insert(
        "identity".to_string(),
        serde_json::Value::Object(identity_obj),
    );
    obj.insert(
        "identity_aliases".to_string(),
        serde_json::Value::Object(alias_obj),
    );
}

fn build_dynamic_identity_opa_data(snapshot: &crate::shm::IdentitySnapshot) -> serde_json::Value {
    let mut root = serde_json::json!({});
    inject_identity_data(&mut root, snapshot);
    root
}

fn fixed_str(buf: &[u8]) -> String {
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).trim().to_string()
}

fn role_hash_value(hash: &[u8; 32]) -> serde_json::Value {
    if sha256_bytes_is_zero(hash) {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(sha256_bytes_to_hex(hash))
    }
}

fn skill_hashes_value(hashes: &[[u8; 32]], count: usize) -> serde_json::Value {
    definition_hashes_value(hashes, count)
}

fn handbook_hashes_value(hashes: &[[u8; 32]], count: usize) -> serde_json::Value {
    definition_hashes_value(hashes, count)
}

fn definition_hashes_value(hashes: &[[u8; 32]], count: usize) -> serde_json::Value {
    let count = usize::min(count, hashes.len());
    serde_json::Value::Array(
        hashes[..count]
            .iter()
            .map(|hash| serde_json::Value::String(sha256_bytes_to_hex(hash)))
            .collect(),
    )
}

fn format_prefixed_uuid(prefix: &str, bytes: &[u8; 16]) -> String {
    format!("{}:{}", prefix, Uuid::from_bytes(*bytes))
}

async fn refresh_lsa(
    lsa_reader: &Arc<Mutex<Option<LsaRegionReader>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    hive_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    reachability: &Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
) -> Option<LsaSnapshot> {
    let snapshot = read_lsa_snapshot_for_routing(lsa_reader, hive_id, HEARTBEAT_STALE_MS).await;
    if let Some(snapshot) = snapshot.clone() {
        {
            let mut guard = lsa_snapshot.lock().await;
            *guard = Some(snapshot.clone());
        }
        rebuild_fib(
            fib,
            nodes,
            peer_nodes,
            static_routes,
            lsa_snapshot,
            reachability,
        )
        .await;
    }
    snapshot
}

async fn read_lsa_snapshot_for_routing(
    lsa_reader: &Arc<Mutex<Option<LsaRegionReader>>>,
    hive_id: &str,
    stale_after_ms: u64,
) -> Option<LsaSnapshot> {
    let shm_name = format!("/jsr-lsa-{}", hive_id);
    let now = now_epoch_ms();
    let mut guard = lsa_reader.lock().await;

    let needs_reopen = match guard.as_ref() {
        None => true,
        Some(reader) => match reader.read_header() {
            Some(header) => now.saturating_sub(header.heartbeat) > stale_after_ms,
            None => true,
        },
    };
    if needs_reopen {
        *guard = LsaRegionReader::open_read_only(&shm_name).ok();
    }

    if let Some(snapshot) = guard.as_ref().and_then(|reader| reader.read_snapshot()) {
        return Some(snapshot);
    }

    *guard = LsaRegionReader::open_read_only(&shm_name).ok();
    guard.as_ref().and_then(|reader| reader.read_snapshot())
}

fn assign_vpn(name: &str, snapshot: Option<&ConfigSnapshot>) -> u32 {
    if name.starts_with("SY.") || name.starts_with("RT.") {
        return 0;
    }
    let Some(snapshot) = snapshot else {
        return 0;
    };
    tracing::debug!(
        config_version = snapshot.header.config_version,
        vpns = snapshot.header.vpn_assignment_count,
        "vpn assignment snapshot"
    );
    if snapshot.vpns.is_empty() {
        tracing::warn!("vpn table empty in config snapshot");
    }
    let mut rules: Vec<(u16, &str, u8, u32, u16)> = Vec::new();
    for entry in &snapshot.vpns {
        let flags = entry.flags;
        if flags != 0 && flags & FLAG_ACTIVE == 0 {
            continue;
        }
        let pattern = bytes_to_string(&entry.pattern, entry.pattern_len as usize);
        tracing::debug!(
            pattern = %pattern,
            match_kind = entry.match_kind,
            vpn_id = entry.vpn_id,
            priority = entry.priority,
            "vpn rule candidate"
        );
        rules.push((
            entry.priority,
            pattern,
            entry.match_kind,
            entry.vpn_id,
            flags,
        ));
    }
    rules.sort_by_key(|rule| rule.0);
    for (_, pattern, match_kind, vpn_id, _) in rules {
        if pattern_match_kind(match_kind, pattern, name) {
            tracing::debug!(node = %name, vpn_id, "vpn matched");
            return vpn_id;
        }
    }
    0
}

async fn reassign_vpns(
    snapshot: &ConfigSnapshot,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
) {
    let mut updates: Vec<(Uuid, u32)> = Vec::new();
    {
        let nodes_guard = nodes.lock().await;
        for (uuid, handle) in nodes_guard.iter() {
            let new_vpn = assign_vpn(&handle.name, Some(snapshot));
            if new_vpn != handle.vpn_id {
                updates.push((*uuid, new_vpn));
            }
        }
    }
    tracing::info!(nodes = updates.len(), "vpn reassignment scan completed");
    if updates.is_empty() {
        return;
    }
    {
        let mut nodes_guard = nodes.lock().await;
        for (uuid, vpn_id) in updates.iter() {
            if let Some(handle) = nodes_guard.get_mut(uuid) {
                handle.vpn_id = *vpn_id;
            }
        }
    }
    {
        let mut shm = shm.lock().await;
        for (uuid, vpn_id) in updates {
            let _ = shm.update_node_vpn(uuid, vpn_id);
            tracing::info!(node = %uuid, vpn_id, "vpn updated");
        }
    }
}

async fn rebuild_fib(
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    reachability: &Arc<Mutex<std::collections::HashMap<Uuid, ReachabilityEntry>>>,
) {
    let nodes_snapshot: Vec<(Uuid, NodeHandle)> = {
        let nodes_guard = nodes.lock().await;
        nodes_guard
            .iter()
            .map(|(uuid, handle)| (*uuid, handle.clone()))
            .collect()
    };
    let peer_nodes_snapshot: Vec<(Uuid, PeerNode)> = {
        let peer_guard = peer_nodes.lock().await;
        peer_guard
            .iter()
            .map(|(uuid, node)| (*uuid, node.clone()))
            .collect()
    };
    let routes_snapshot: Vec<StaticRoute> = {
        let routes_guard = static_routes.lock().await;
        routes_guard.clone()
    };
    let lsa_snapshot = { lsa_snapshot.lock().await.clone() };
    let mut entries: Vec<FibEntry> = Vec::new();
    for (uuid, handle) in nodes_snapshot {
        entries.push(FibEntry {
            pattern: handle.name.clone(),
            match_kind: MATCH_EXACT,
            vpn_id: handle.vpn_id,
            source: FibSource::LocalNode,
            next_hop: Some(FibNextHop::Local(uuid)),
            admin_distance: ADMIN_DISTANCE_LOCAL,
            priority: 0,
            metric: 0,
            installed_at: handle.connected_at,
            action: ACTION_FORWARD,
        });
    }
    for (_uuid, node) in peer_nodes_snapshot {
        entries.push(FibEntry {
            pattern: node.name.clone(),
            match_kind: MATCH_EXACT,
            vpn_id: node.vpn_id,
            source: FibSource::PeerNode,
            next_hop: Some(FibNextHop::Router(node.router_uuid)),
            admin_distance: ADMIN_DISTANCE_LOCAL,
            priority: 0,
            metric: 0,
            installed_at: 0,
            action: ACTION_FORWARD,
        });
    }
    for route in routes_snapshot {
        let next_hop = if route.next_hop_hive.is_empty() {
            None
        } else {
            Some(FibNextHop::Hive(route.next_hop_hive.clone()))
        };
        entries.push(FibEntry {
            pattern: route.pattern,
            match_kind: route.match_kind,
            vpn_id: 0,
            source: FibSource::StaticRoute,
            next_hop,
            admin_distance: ADMIN_DISTANCE_STATIC,
            priority: route.priority,
            metric: route.metric,
            installed_at: route.installed_at,
            action: route.action,
        });
    }

    if let Some(snapshot) = lsa_snapshot {
        let mut hive_map: Vec<(String, u16, u16, u64)> = Vec::new();
        for (idx, hive) in snapshot.hives.iter().enumerate() {
            if hive.hive_id_len == 0 {
                continue;
            }
            if hive.flags & HIVE_FLAG_SELF != 0 {
                continue;
            }
            let name = bytes_to_string(&hive.hive_id, hive.hive_id_len as usize);
            if hive.flags & FLAG_STALE != 0 {
                continue;
            }
            hive_map.push((name.to_string(), idx as u16, hive.flags, hive.last_updated));
        }

        for node in snapshot.nodes.iter() {
            if node.name_len == 0 {
                continue;
            }
            if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
                continue;
            }
            let hive = hive_map
                .iter()
                .find(|(_, idx, _, _)| *idx == node.hive_index)
                .map(|(name, _, _, updated)| (name.clone(), *updated));
            let Some((hive_id, updated_at)) = hive else {
                continue;
            };
            let name = bytes_to_string(&node.name, node.name_len as usize).to_string();
            entries.push(FibEntry {
                pattern: name,
                match_kind: MATCH_EXACT,
                vpn_id: node.vpn_id,
                source: FibSource::LsaNode,
                next_hop: Some(FibNextHop::Hive(hive_id)),
                admin_distance: ADMIN_DISTANCE_LSA,
                priority: 0,
                metric: 0,
                installed_at: updated_at,
                action: ACTION_FORWARD,
            });
        }

        for route in snapshot.routes.iter() {
            if route.prefix_len == 0 {
                continue;
            }
            if route.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
                continue;
            }
            let hive = hive_map
                .iter()
                .find(|(_, idx, _, _)| *idx == route.hive_index)
                .map(|(name, _, _, updated)| (name.clone(), *updated));
            let Some((hive_id, updated_at)) = hive else {
                continue;
            };
            let prefix = bytes_to_string(&route.prefix, route.prefix_len as usize).to_string();
            let mut next_hop_hive =
                bytes_to_string(&route.next_hop_hive, route.next_hop_hive_len as usize).to_string();
            if next_hop_hive.is_empty() {
                next_hop_hive = hive_id.clone();
            }
            let next_hop = if next_hop_hive.is_empty() {
                None
            } else {
                Some(FibNextHop::Hive(next_hop_hive))
            };
            entries.push(FibEntry {
                pattern: prefix,
                match_kind: route.match_kind,
                vpn_id: 0,
                source: FibSource::LsaRoute {
                    advertised_by: hive_id,
                },
                next_hop,
                admin_distance: ADMIN_DISTANCE_LSA,
                priority: route.priority,
                metric: route.metric,
                installed_at: updated_at,
                action: route.action,
            });
        }
    }

    // Option B: hub-vouched multi-hop reachability. Each entry forwards toward its vouching hub
    // (next_hop_hive, a direct WAN peer); the hub re-resolves the unchanged dst onward. Installed
    // at ADMIN_DISTANCE_REACHABILITY so a directly-learned LSA node for the same name always wins.
    {
        let reach_guard = reachability.lock().await;
        for entry in reach_guard.values() {
            entries.push(FibEntry {
                pattern: entry.name.clone(),
                match_kind: MATCH_EXACT,
                vpn_id: entry.vpn_id,
                source: FibSource::ReachableNode,
                next_hop: Some(FibNextHop::Hive(entry.next_hop_hive.clone())),
                admin_distance: ADMIN_DISTANCE_REACHABILITY,
                priority: 0,
                metric: 0,
                installed_at: entry.last_updated,
                action: ACTION_FORWARD,
            });
        }
    }

    entries.sort_by(|a, b| {
        let sa = fib_specificity(a);
        let sb = fib_specificity(b);
        sb.cmp(&sa)
            .then_with(|| a.admin_distance.cmp(&b.admin_distance))
            .then_with(|| a.priority.cmp(&b.priority))
            .then_with(|| a.metric.cmp(&b.metric))
            .then_with(|| a.installed_at.cmp(&b.installed_at))
    });

    let mut fib_guard = fib.lock().await;
    *fib_guard = entries;
    tracing::debug!(fib_entries = fib_guard.len(), "fib rebuilt");
}

fn bytes_to_string(buf: &[u8], len: usize) -> &str {
    let len = len.min(buf.len());
    std::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn is_system_node(name: &str) -> bool {
    name.starts_with("SY.") || name.starts_with("RT.")
}

fn is_edge_node(name: &str) -> bool {
    name.split_once('@')
        .map(|(role, _)| role == "SY.edge")
        .unwrap_or(name == "SY.edge")
}

fn is_io_node(name: &str) -> bool {
    name.split_once('@')
        .map(|(role, _)| role.starts_with("IO."))
        .unwrap_or_else(|| name.starts_with("IO."))
}

fn is_vault_node(name: &str) -> bool {
    name.split_once('@')
        .map(|(role, _)| role == "SY.vault")
        .unwrap_or(name == "SY.vault")
}

fn is_global_scope(meta: &Meta) -> bool {
    matches!(meta.scope.as_deref(), Some(SCOPE_GLOBAL))
}

/// Whether a frame's `msg_type` is the SYSTEM kind — CASE-INSENSITIVE, matching how every edge/IO
/// node classifies control-plane traffic (`eq_ignore_ascii_case`, e.g. io-slack/io-api
/// is_control_plane_msg_type). The router MUST agree: a case-sensitive `== SYSTEM_KIND` let a frame
/// stamped `msg_type="SYSTEM"` slip past every SYSTEM gate here (EDGE-05 edge<->IO reject, the
/// protected-action authority gate, the edge-control gates) while the destination node still ran it
/// as control-plane — a full gate bypass by case. Use this everywhere the router decides on the
/// SYSTEM kind.
fn is_system_kind(msg_type: &str) -> bool {
    msg_type.eq_ignore_ascii_case(SYSTEM_KIND)
}

/// The commands SY.admin@motherbee sends an edge: the `edge_service_actions` of
/// `policy/system.rego` rule (1), all of them in `system_policy::PROTECTED_SYSTEM_ACTIONS`.
const EDGE_SERVICE_COMMANDS: &[&str] = &[
    MSG_EDGE_OPEN_URL,
    MSG_EDGE_CLOSE_URL,
    MSG_EDGE_LIST_URLS,
    MSG_EDGE_PUBLISH_BLOB,
    MSG_EDGE_UNPUBLISH_BLOB,
    MSG_EDGE_TLS_STATUS,
    MSG_EDGE_TLS_RELOAD,
    MSG_EDGE_ACME_CHALLENGE,
    MSG_EDGE_WEB_ROUTES,
];

/// The edge's answers to `EDGE_SERVICE_COMMANDS`, which go back to SY.admin@motherbee only.
const EDGE_SERVICE_RESPONSES: &[&str] = &[
    MSG_EDGE_OPEN_URL_RESPONSE,
    MSG_EDGE_CLOSE_URL_RESPONSE,
    MSG_EDGE_LIST_URLS_RESPONSE,
    MSG_EDGE_PUBLISH_BLOB_RESPONSE,
    MSG_EDGE_UNPUBLISH_BLOB_RESPONSE,
    MSG_EDGE_TLS_STATUS_RESPONSE,
    MSG_EDGE_TLS_RELOAD_RESPONSE,
    MSG_EDGE_ACME_CHALLENGE_RESPONSE,
    MSG_EDGE_WEB_ROUTES_RESPONSE,
];

fn edge_service_control_allowed(meta: &Meta, src_name: &str, dst_name: &str) -> bool {
    is_edge_service_control_message(meta, dst_name)
        && src_name == system_policy::EDGE_CONTROL_AUTHORITY
}

fn edge_service_control_response_allowed(meta: &Meta, src_name: &str, dst_name: &str) -> bool {
    is_edge_node(src_name)
        && dst_name == system_policy::EDGE_CONTROL_AUTHORITY
        && is_system_kind(&meta.msg_type)
        && meta
            .msg
            .as_deref()
            .is_some_and(|action| EDGE_SERVICE_RESPONSES.contains(&action))
}

fn is_edge_service_control_message(meta: &Meta, dst_name: &str) -> bool {
    is_edge_node(dst_name)
        && is_system_kind(&meta.msg_type)
        && meta
            .msg
            .as_deref()
            .is_some_and(|action| EDGE_SERVICE_COMMANDS.contains(&action))
}

fn edge_data_path_allowed(meta: &Meta, src_name: &str, dst_name: &str) -> bool {
    // EDGE-05: the edge<->IO data path carries ONLY the channel's inbound_family
    // (validate_endpoint_row_for_open rejects family=="system", and IO handlers reply in that
    // same non-system family). The edge's only legitimate SYSTEM traffic is with SY.admin
    // (control acks) and SY.vault (cert/secret) — both non-IO, gated by the sibling OR terms;
    // the router's own UNREACHABLE/TTL frames are RT.*-originated (not is_io_node). So a SYSTEM
    // frame on an edge<->IO pair is never legitimate: reject it. This bounds a compromised edge
    // (can't originate SYSTEM verbs at IO nodes) and a rogue IO (can't land a SYSTEM frame on
    // the edge, closing the RouteMatch::Any reply-shadowing risk). Owner-scoping (only
    // externalized IO owners) is Phase 2, deferred — see the EDGE-05 audit note.
    if is_system_kind(&meta.msg_type) {
        return false;
    }
    (is_edge_node(src_name) && is_io_node(dst_name))
        || (is_io_node(src_name) && is_edge_node(dst_name))
}

fn edge_vault_read_allowed(meta: &Meta, src_name: &str, dst_name: &str) -> bool {
    if !is_system_kind(&meta.msg_type) {
        return false;
    }
    match meta.msg.as_deref() {
        Some(MSG_VAULT_GET) => is_edge_node(src_name) && is_vault_node(dst_name),
        Some(MSG_VAULT_GET_RESPONSE) => is_vault_node(src_name) && is_edge_node(dst_name),
        _ => false,
    }
}

/// A-78: SY.vault's `VAULT_SECRET_CHANGED` about a secret an edge owns, from `SY.vault@<primary
/// hive>` (the router-stamped name: the only vault) to an `SY.edge`. Only `vpn_allows_between`, the
/// gate of an addressed message, admits it: SY.vault sends the edge the notices about its own
/// secrets as unicasts, while its broadcast names every stored key, its tenant and its owners, and
/// never reaches the DMZ (`vpn_allows_broadcast_between`).
fn edge_vault_notice_allowed(meta: &Meta, src_name: &str, dst_name: &str) -> bool {
    is_system_kind(&meta.msg_type)
        && meta.msg.as_deref() == Some(MSG_VAULT_SECRET_CHANGED)
        && src_name.split_once('@') == Some(("SY.vault", system_policy::PRIMARY_HIVE_ID))
        && is_edge_node(dst_name)
}

/// May `src_name` reach `dst_name` with a message addressed to it: by UUID or by name, resolved by
/// the user policy, or as a tap copy. A broadcast fan-out asks `vpn_allows_broadcast_between`.
fn vpn_allows_between(
    meta: &Meta,
    src_name: &str,
    src_vpn: u32,
    dst_name: &str,
    dst_vpn: u32,
) -> bool {
    if is_edge_node(src_name) || is_edge_node(dst_name) {
        if is_edge_service_control_message(meta, dst_name) {
            return edge_service_control_allowed(meta, src_name, dst_name);
        }
        // The control-command case already returned above via the exclusive
        // is_edge_service_control_message gate, so edge_service_control_allowed can never be true
        // here — the remaining legitimate edge traffic is a control RESPONSE, the data path, a
        // vault read, or the vault's notice about the edge's own secret.
        return edge_service_control_response_allowed(meta, src_name, dst_name)
            || edge_data_path_allowed(meta, src_name, dst_name)
            || edge_vault_read_allowed(meta, src_name, dst_name)
            || edge_vault_notice_allowed(meta, src_name, dst_name);
    }
    if is_system_kind(&meta.msg_type) && is_global_scope(meta) {
        return true;
    }
    if is_system_node(src_name) || is_system_node(dst_name) {
        return true;
    }
    src_vpn == dst_vpn
}

/// The gate for one copy of a broadcast fan-out (from a local node, a sibling router or the WAN):
/// `vpn_allows_between` without A-78's unicast, so SY.vault's broadcast never reaches an edge.
fn vpn_allows_broadcast_between(
    meta: &Meta,
    src_name: &str,
    src_vpn: u32,
    dst_name: &str,
    dst_vpn: u32,
) -> bool {
    !edge_vault_notice_allowed(meta, src_name, dst_name)
        && vpn_allows_between(meta, src_name, src_vpn, dst_name, dst_vpn)
}

fn pattern_match(pattern: &str, name: &str) -> bool {
    if pattern.contains('*') {
        return wildcard_match(pattern.as_bytes(), name.as_bytes());
    }
    pattern == name
}

fn pattern_match_kind(match_kind: u8, pattern: &str, name: &str) -> bool {
    match match_kind {
        MATCH_EXACT => pattern == name,
        MATCH_PREFIX => {
            let prefix = pattern.trim_end_matches(".*");
            name.starts_with(prefix)
        }
        MATCH_GLOB => wildcard_match(pattern.as_bytes(), name.as_bytes()),
        _ => false,
    }
}

fn fib_specificity(entry: &FibEntry) -> (usize, u8) {
    let len = match entry.match_kind {
        MATCH_EXACT => entry.pattern.len(),
        MATCH_PREFIX => entry.pattern.trim_end_matches(".*").len(),
        MATCH_GLOB => entry.pattern.chars().filter(|c| *c != '*').count(),
        _ => 0,
    };
    let rank = match entry.match_kind {
        MATCH_EXACT => 3,
        MATCH_PREFIX => 2,
        MATCH_GLOB => 1,
        _ => 0,
    };
    (len, rank)
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut pi = 0usize;
    let mut ti = 0usize;
    let mut star = None;
    let mut match_idx = 0usize;
    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            match_idx = ti;
        } else if let Some(star_idx) = star {
            pi = star_idx + 1;
            match_idx += 1;
            ti = match_idx;
        } else {
            return false;
        }
    }
    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }
    pi == pattern.len()
}

async fn resolve_by_name(
    target: &str,
    src: &NodeHandle,
    nodes: &std::collections::HashMap<Uuid, NodeHandle>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    meta: &Meta,
) -> Result<ResolvedRoute, RouterError> {
    if let Some(base) = target.strip_suffix("@*") {
        if let Some(src_hive) = extract_hive(&src.name) {
            let local_target = format!("{}@{}", base, src_hive);
            let local = resolve_by_name_inner(&local_target, src, nodes, fib, meta).await?;
            if !matches!(local, ResolvedRoute::Unreachable("NODE_NOT_FOUND")) {
                return Ok(local);
            }
        }
        return resolve_by_name_any_hive(base, src, nodes, fib, meta).await;
    }
    resolve_by_name_inner(target, src, nodes, fib, meta).await
}

async fn resolve_by_name_inner(
    target: &str,
    src: &NodeHandle,
    nodes: &std::collections::HashMap<Uuid, NodeHandle>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    meta: &Meta,
) -> Result<ResolvedRoute, RouterError> {
    let fib_guard = fib.lock().await;
    for entry in fib_guard.iter() {
        if !pattern_match_kind(entry.match_kind, &entry.pattern, target) {
            continue;
        }
        match entry.source {
            FibSource::StaticRoute => match entry.action {
                ACTION_DROP => return Ok(ResolvedRoute::Drop),
                ACTION_FORWARD => {
                    if let Some(FibNextHop::Hive(hive)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardHive(hive.clone()));
                    }
                    continue;
                }
                _ => continue,
            },
            FibSource::LsaRoute { ref advertised_by }
                if !lsa_route_applies(advertised_by, target) =>
            {
                continue
            }
            FibSource::LsaRoute { .. } => match entry.action {
                ACTION_DROP => return Ok(ResolvedRoute::Drop),
                ACTION_FORWARD => {
                    if let Some(FibNextHop::Hive(hive)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardHive(hive.clone()));
                    }
                    continue;
                }
                _ => continue,
            },
            FibSource::LocalNode => {
                let Some(FibNextHop::Local(dst_uuid)) = &entry.next_hop else {
                    continue;
                };
                let Some(dst_handle) = nodes.get(dst_uuid) else {
                    continue;
                };
                if vpn_allows_between(
                    meta,
                    &src.name,
                    src.vpn_id,
                    &dst_handle.name,
                    dst_handle.vpn_id,
                ) {
                    return Ok(ResolvedRoute::Deliver(*dst_uuid));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
            FibSource::PeerNode => {
                let Some(FibNextHop::Router(peer_uuid)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows_between(meta, &src.name, src.vpn_id, &entry.pattern, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardRouter(*peer_uuid));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
            // LsaNode = directly-learned remote hive; ReachableNode = hub-vouched multi-hop
            // (its next_hop is the vouching hub). Both forward via FibNextHop::Hive.
            FibSource::LsaNode | FibSource::ReachableNode => {
                let Some(FibNextHop::Hive(hive)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows_between(meta, &src.name, src.vpn_id, &entry.pattern, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardHive(hive.clone()));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
        }
    }
    Ok(ResolvedRoute::Unreachable("NODE_NOT_FOUND"))
}

async fn resolve_by_name_any_hive(
    base: &str,
    src: &NodeHandle,
    nodes: &std::collections::HashMap<Uuid, NodeHandle>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    meta: &Meta,
) -> Result<ResolvedRoute, RouterError> {
    let fib_guard = fib.lock().await;
    // Routes see the destination as written, `name@*`.
    let target = format!("{}@*", base);
    for entry in fib_guard.iter() {
        let matched = match entry.source {
            FibSource::LocalNode
            | FibSource::PeerNode
            | FibSource::LsaNode
            | FibSource::ReachableNode => match_any_hive(base, &entry.pattern),
            FibSource::StaticRoute | FibSource::LsaRoute { .. } => {
                pattern_match_kind(entry.match_kind, &entry.pattern, &target)
            }
        };
        if !matched {
            continue;
        }
        match entry.source {
            FibSource::StaticRoute => match entry.action {
                ACTION_DROP => return Ok(ResolvedRoute::Drop),
                ACTION_FORWARD => {
                    if let Some(FibNextHop::Hive(hive)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardHive(hive.clone()));
                    }
                    continue;
                }
                _ => continue,
            },
            FibSource::LsaRoute { ref advertised_by }
                if !lsa_route_applies(advertised_by, &target) =>
            {
                continue
            }
            FibSource::LsaRoute { .. } => match entry.action {
                ACTION_DROP => return Ok(ResolvedRoute::Drop),
                ACTION_FORWARD => {
                    if let Some(FibNextHop::Hive(hive)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardHive(hive.clone()));
                    }
                    continue;
                }
                _ => continue,
            },
            FibSource::LocalNode => {
                let Some(FibNextHop::Local(dst_uuid)) = &entry.next_hop else {
                    continue;
                };
                let Some(dst_handle) = nodes.get(dst_uuid) else {
                    continue;
                };
                if vpn_allows_between(
                    meta,
                    &src.name,
                    src.vpn_id,
                    &dst_handle.name,
                    dst_handle.vpn_id,
                ) {
                    return Ok(ResolvedRoute::Deliver(*dst_uuid));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
            FibSource::PeerNode => {
                let Some(FibNextHop::Router(peer_uuid)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows_between(meta, &src.name, src.vpn_id, &entry.pattern, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardRouter(*peer_uuid));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
            FibSource::LsaNode | FibSource::ReachableNode => {
                let Some(FibNextHop::Hive(hive)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows_between(meta, &src.name, src.vpn_id, &entry.pattern, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardHive(hive.clone()));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
        }
    }
    Ok(ResolvedRoute::Unreachable("NODE_NOT_FOUND"))
}

fn extract_hive(name: &str) -> Option<&str> {
    name.split_once('@').map(|(_, hive)| hive)
}

/// Whether `name` is a node of `hive`: `<role>@<hive>` with neither part empty, split at the first
/// `@` like `extract_hive` and the system policy.
fn node_is_on_hive(name: &str, hive: &str) -> bool {
    !hive.is_empty()
        && name
            .split_once('@')
            .is_some_and(|(role, name_hive)| !role.is_empty() && name_hive == hive)
}

/// Whether a route that hive `advertised_by` put in its LSA applies to `target`: only to a
/// destination on that hive, or to an `@*` one, so a hive can neither redirect nor drop the traffic
/// of another hive's nodes (docs/control-path-hardening-v1.md §5). Its `next_hop_hive` works as
/// before.
fn lsa_route_applies(advertised_by: &str, target: &str) -> bool {
    target.ends_with("@*") || node_is_on_hive(target, advertised_by)
}

/// Whether a tap of the LSA applies to a message from `src_name` to `dst_name`. This hive's own taps
/// always do. Another hive's apply only to a message that hive is a party to, as its source or its
/// destination, and only with a target on that hive: so a hive cannot copy traffic it does not
/// already see, nor hand a copy to another hive's node (docs/control-path-hardening-v1.md §5). A
/// tap of a hive the snapshot does not list applies to nothing.
fn lsa_tap_applies(
    snapshot: &LsaSnapshot,
    tap: &RemoteTapEntry,
    src_name: &str,
    dst_name: &str,
) -> bool {
    let Some(hive) = snapshot.hives.get(usize::from(tap.hive_index)) else {
        return false;
    };
    if hive.flags & HIVE_FLAG_SELF != 0 {
        return true;
    }
    let advertised_by = bytes_to_string(&hive.hive_id, hive.hive_id_len as usize);
    let target = bytes_to_string(&tap.target, tap.target_len as usize);
    node_is_on_hive(target, advertised_by)
        && (node_is_on_hive(src_name, advertised_by) || node_is_on_hive(dst_name, advertised_by))
}

fn match_any_hive(base: &str, full_name: &str) -> bool {
    let Some((name_base, _)) = full_name.split_once('@') else {
        return false;
    };
    if base.contains('*') {
        return wildcard_match(base.as_bytes(), name_base.as_bytes());
    }
    name_base == base
}

/// The router's copy of a turn for its stream: the message as routed, with its action class and
/// the sender's real name. The node's own `src_l2_name` is never kept:
/// the router stamps it only later, on delivery.
fn turn_copy(msg: &Message, conn_node_name: &str) -> Option<Vec<u8>> {
    let mut copy = msg.clone();
    if copy.meta.action_class.is_none() {
        copy.meta.action_class = classify_routed_message(&copy.meta.msg_type);
    }
    copy.routing.src_l2_name = Some(conn_node_name.to_string());
    serde_json::to_vec(&copy).ok()
}

/// At most one log line per interval about the appends this router refused.
const REFUSED_APPEND_LOG_INTERVAL_MS: u64 = 30_000;
static REFUSED_APPEND_LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

/// A node's `STREAM_APPEND`: appended under its subject when that node may append it (§2.2), the
/// producer being the node's connection name.
fn take_stream_append(stream: &stream::StreamHandle, msg: &Message, producer: &str, hive_id: &str) {
    let append: Option<fluxbee_sdk::stream::StreamAppend> =
        serde_json::from_value(msg.payload.clone()).ok();
    let allowed = append.as_ref().is_some_and(|append| {
        stream::subject_allowed(&append.subject, producer, hive_id, stream.role())
    });
    match (append, allowed) {
        (Some(append), true) => match serde_json::to_vec(&append.payload) {
            Ok(payload) => stream.append_from_node(&append.subject, producer, payload),
            Err(_) => stream.count_refused(),
        },
        (append, _) => {
            stream.count_refused();
            let now = now_epoch_ms();
            let last = REFUSED_APPEND_LAST_LOG_MS.load(Ordering::Relaxed);
            if now.saturating_sub(last) >= REFUSED_APPEND_LOG_INTERVAL_MS {
                REFUSED_APPEND_LAST_LOG_MS.store(now, Ordering::Relaxed);
                tracing::warn!(
                    producer,
                    subject = ?append.map(|append| append.subject),
                    "router refused a STREAM_APPEND: not a subject this node may append"
                );
            }
        }
    }
}

fn should_publish_turn(msg: &Message) -> bool {
    if is_system_kind(&msg.meta.msg_type) {
        return false;
    }
    if msg.meta.msg_type == "admin" || msg.meta.msg_type == "query_response" {
        return false;
    }
    true
}

fn should_enrich_with_memory_package(msg: &Message) -> bool {
    if is_system_kind(&msg.meta.msg_type) {
        return false;
    }
    if msg.meta.msg_type == "admin" || msg.meta.msg_type == "query_response" {
        return false;
    }
    true
}

/// A local node a message is delivered to.
struct LocalDelivery {
    node: String,
    sender: mpsc::UnboundedSender<Vec<u8>>,
}

impl LocalDelivery {
    fn to(node: &str, sender: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self {
            node: node.to_string(),
            sender,
        }
    }
}

/// Whether a node is an agent: the interlocutor that owns its communications (§6.1, D15 of
/// docs/router-stream-v1.md). Agents run as `AI.*` nodes.
fn is_agent_node(name: &str) -> bool {
    name.get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("AI."))
}

/// Whether a message is a turn of a communication: a kind that counts as a turn, with a thread,
/// neither a tap copy nor a broadcast (§6.2).
fn is_communication_turn(msg: &Message) -> bool {
    should_publish_turn(msg)
        && !msg.meta.via_tap
        && !matches!(msg.routing.dst, Destination::Broadcast)
        && canonical_thread_id_from_meta(&msg.meta).is_some()
}

/// The copy of a turn for its agent's cognition: the message as routed, with its sender's real
/// name, without any memory package (§6.2).
fn cognition_copy(msg: &Message, sender_name: &str) -> Option<Vec<u8>> {
    let mut copy = msg.clone();
    copy.routing.src_l2_name = Some(sender_name.to_string());
    copy.meta.memory_package = None;
    if copy.meta.action_class.is_none() {
        copy.meta.action_class = classify_routed_message(&copy.meta.msg_type);
    }
    serde_json::to_vec(&copy).ok()
}

/// Delivers a message to local nodes (§6.2). A system message passes the authority gate once; a
/// package the sender set is dropped for every destination. An agent destination gets the memory
/// package of its communication, and its router's copy of the turn for its cognition: with the
/// copy its agent's sends make at entry, the agent's router hands its cognition both directions.
async fn deliver_locally(
    deliveries: Vec<LocalDelivery>,
    msg: &Message,
    memory_reader: &Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: &str,
    src_via_hub: bool,
    log_stream: &stream::StreamHandle,
) -> Result<(), RouterError> {
    if deliveries.is_empty() {
        return Ok(());
    }
    let Some(prepared) = prepare_local_delivery(hive_id, msg, src_via_hub) else {
        return Ok(());
    };
    let turn = is_communication_turn(&prepared);
    let sender_name = prepared.routing.src_l2_name.clone().unwrap_or_default();
    let mut plain: Option<Vec<u8>> = None;
    let mut memory: Option<Option<MemoryShmSnapshot>> = None;
    for delivery in deliveries {
        let agent = is_agent_node(&delivery.node);
        if agent && turn {
            if let Some(copy) = cognition_copy(&prepared, &sender_name) {
                log_stream.append_cognition_turn(&delivery.node, copy);
            }
        }
        let package = if agent
            && should_enrich_with_memory_package(&prepared)
            && canonical_thread_id_from_meta(&prepared.meta).is_some()
        {
            if memory.is_none() {
                memory = Some(
                    read_memory_snapshot_for_delivery(memory_reader, hive_id, HEARTBEAT_STALE_MS)
                        .await,
                );
            }
            package_for(
                memory.as_ref().and_then(Option::as_ref),
                &prepared,
                &delivery.node,
            )
        } else {
            None
        };
        let data = match package {
            Some(package) => {
                let mut enriched = prepared.clone();
                enriched.meta.memory_package = Some(package);
                serde_json::to_vec(&enriched)?
            }
            None => match &plain {
                Some(data) => data.clone(),
                None => {
                    let data = serde_json::to_vec(&prepared)?;
                    plain = Some(data.clone());
                    data
                }
            },
        };
        let _ = delivery.sender.send(data);
    }
    Ok(())
}

/// One message to one local node (a tap copy, the tests): the same gate, stamps and package as
/// `deliver_locally`, without a copy for cognition.
async fn serialize_for_local_delivery(
    memory_reader: &Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: &str,
    msg: &Message,
    src_via_hub: bool,
    dst_node: &str,
) -> Result<Option<Vec<u8>>, RouterError> {
    let Some(mut prepared) = prepare_local_delivery(hive_id, msg, src_via_hub) else {
        return Ok(None);
    };
    if is_agent_node(dst_node)
        && should_enrich_with_memory_package(&prepared)
        && canonical_thread_id_from_meta(&prepared.meta).is_some()
    {
        let memory =
            read_memory_snapshot_for_delivery(memory_reader, hive_id, HEARTBEAT_STALE_MS).await;
        prepared.meta.memory_package = package_for(memory.as_ref(), &prepared, dst_node);
    }
    Ok(Some(serde_json::to_vec(&prepared)?))
}

/// The authority gate for a system message, and the delivery's own stamps: the action class, and
/// no memory package from the sender (nothing legitimate sets one, and a node could otherwise put
/// one in an agent's prompt, §6.2). None when the gate drops it.
fn prepare_local_delivery(hive_id: &str, msg: &Message, src_via_hub: bool) -> Option<Message> {
    if is_system_kind(&msg.meta.msg_type) {
        // OPA-dual SYSTEM (authority) layer: the baked, non-user-editable Rego policy
        // (policy/system.rego -> system.wasm), evaluated via authorize_system() — the SINGLE
        // source of truth, no Rust fallback (the router fails closed at startup if the wasm cannot
        // load). System DENY is final. (The user/OPA layer handles routing and, in future, may
        // only narrow this — never broaden it.)
        let action = msg.meta.msg.as_deref().unwrap_or_default();
        if system_policy::is_protected_system_action(action) {
            // Option B security invariant: a src learned TRANSITIVELY via the hub's reachability
            // vouch (`src_via_hub`) is admitted for DATA delivery but NEVER granted SYSTEM
            // authority — it is denied here WITHOUT consulting the allow policy at all, so a
            // compromised hub cannot fabricate cross-hive control-plane authority between spokes.
            // A tap copy (`via_tap`) is denied the same way: it keeps the original sender's name,
            // but goes to the tap's target, which that sender never addressed
            // (docs/control-path-hardening-v1.md §5). A directly-authenticated src still goes
            // through the normal authority gate.
            let authorized = !src_via_hub
                && !msg.meta.via_tap
                && system_policy::authorize_system(
                    action,
                    msg.routing.src_l2_name.as_deref(),
                    hive_id,
                );
            if !authorized {
                tracing::warn!(
                    action = action,
                    src_uuid = %msg.routing.src,
                    src_l2_name = ?msg.routing.src_l2_name,
                    src_via_hub = src_via_hub,
                    via_tap = msg.meta.via_tap,
                    "router dropped protected SYSTEM action from unauthorized origin"
                );
                return None;
            }
        }
    }
    let mut prepared = msg.clone();
    if prepared.meta.action_class.is_none() {
        prepared.meta.action_class = classify_routed_message(&prepared.meta.msg_type);
    }
    prepared.meta.memory_package = None;
    Some(prepared)
}

/// The memory package of the communication between this message's thread and the agent node it is
/// delivered to.
fn package_for(
    memory: Option<&MemoryShmSnapshot>,
    msg: &Message,
    agent_node: &str,
) -> Option<MemoryPackage> {
    let thread_id = canonical_thread_id_from_meta(&msg.meta)?;
    memory?
        .threads
        .iter()
        .find(|entry| entry.thread_id == thread_id && entry.agent_node == agent_node)
        .map(|entry| entry.package.clone())
}

async fn read_memory_snapshot_for_delivery(
    memory_reader: &Arc<Mutex<Option<MemoryRegionReader>>>,
    hive_id: &str,
    stale_after_ms: u64,
) -> Option<MemoryShmSnapshot> {
    let shm_name = memory_shm_name_for_hive(hive_id).ok()?;
    let now = now_epoch_ms();
    let mut guard = memory_reader.lock().await;

    let needs_reopen = match guard.as_ref() {
        None => true,
        Some(reader) => match reader.read_header() {
            Some(header) => now.saturating_sub(header.heartbeat) > stale_after_ms,
            None => true,
        },
    };
    if needs_reopen {
        *guard = MemoryRegionReader::open_read_only(&shm_name).ok();
    }

    if let Some(snapshot) = guard
        .as_ref()
        .and_then(|reader| reader.read_snapshot().ok())
    {
        return Some(snapshot);
    }

    *guard = MemoryRegionReader::open_read_only(&shm_name).ok();
    guard
        .as_ref()
        .and_then(|reader| reader.read_snapshot().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shm::{
        IdentityHeaderSnapshot, IdentitySnapshot, IlkAliasEntry, IlkEntry,
        IDENTITY_DEFINITION_MAX_HANDBOOKS, IDENTITY_DEFINITION_MAX_SKILLS,
    };
    use fluxbee_sdk::protocol::Routing;
    use std::collections::HashMap;

    fn lsa_payload(hive: &str, seq: u64, router_id: &str, router_name: &str) -> LsaPayload {
        LsaPayload {
            hive: hive.to_string(),
            router_id: router_id.to_string(),
            router_name: router_name.to_string(),
            seq,
            timestamp: "0".to_string(),
            nodes: Vec::new(),
            routes: Vec::new(),
            vpns: Vec::new(),
            taps: Vec::new(),
        }
    }

    fn test_tap_entry(match_src: &str, match_dst: &str, target: &str) -> TapEntry {
        let mut entry = TapEntry {
            match_src: [0u8; 256],
            match_src_len: 0,
            _pad0: [0u8; 6],
            match_dst: [0u8; 256],
            match_dst_len: 0,
            _pad1: [0u8; 6],
            target: [0u8; 256],
            target_len: 0,
            _pad2: [0u8; 6],
            mode: 0,
            enabled: 1,
            flags: FLAG_ACTIVE,
            installed_at: 1,
            _reserved: [0u8; 32],
        };
        entry.match_src_len = copy_bytes_with_len(&mut entry.match_src, match_src) as u16;
        entry.match_dst_len = copy_bytes_with_len(&mut entry.match_dst, match_dst) as u16;
        entry.target_len = copy_bytes_with_len(&mut entry.target, target) as u16;
        entry
    }

    #[tokio::test]
    async fn lsa_payload_includes_router_taps() {
        let payload = build_local_lsa_payload(
            Uuid::new_v4(),
            "RT.gateway@motherbee",
            "motherbee",
            1,
            &Arc::new(Mutex::new(HashMap::<Uuid, NodeHandle>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new())),
            &Arc::new(Mutex::new(Vec::<StaticRoute>::new())),
            &Arc::new(Mutex::new(Vec::<VpnAssignment>::new())),
            &Arc::new(Mutex::new(vec![test_tap_entry(
                "IO.api.sales@motherbee",
                "AI.sales@motherbee",
                "IO.slack.sales@motherbee",
            )])),
        )
        .await;

        assert_eq!(payload.taps.len(), 1);
        assert_eq!(payload.taps[0].match_src, "IO.api.sales@motherbee");
        assert_eq!(payload.taps[0].match_dst, "AI.sales@motherbee");
        assert_eq!(payload.taps[0].target, "IO.slack.sales@motherbee");
        assert_eq!(payload.taps[0].mode, "best_effort");
        assert!(payload.taps[0].enabled);
    }

    #[tokio::test]
    async fn lsa_rejects_hive_mismatch() {
        let lsa_state = Arc::new(Mutex::new(HashMap::<String, RemoteHiveState>::new()));
        let peer_router_uuid = Uuid::new_v4();

        let result = apply_lsa_payload(
            &lsa_state,
            &HashSet::new(),
            "worker-220",
            peer_router_uuid,
            "RT.gateway@worker-220",
            1,
            lsa_payload(
                "sandbox",
                1,
                &peer_router_uuid.to_string(),
                "RT.gateway@sandbox",
            ),
        )
        .await;

        assert!(matches!(
            result,
            LsaApplyResult::Rejected {
                reason: LSA_REJECT_HIVE_MISMATCH,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn lsa_sequence_resets_on_new_session_epoch() {
        let lsa_state = Arc::new(Mutex::new(HashMap::<String, RemoteHiveState>::new()));
        let peer_router_uuid = Uuid::new_v4();
        let peer_router_name = "RT.gateway@worker-220";
        let expected_hive = "worker-220";

        let first = apply_lsa_payload(
            &lsa_state,
            &HashSet::new(),
            expected_hive,
            peer_router_uuid,
            peer_router_name,
            1,
            lsa_payload(
                expected_hive,
                10,
                &peer_router_uuid.to_string(),
                peer_router_name,
            ),
        )
        .await;
        assert!(matches!(first, LsaApplyResult::Applied { .. }));

        let stale_same_session = apply_lsa_payload(
            &lsa_state,
            &HashSet::new(),
            expected_hive,
            peer_router_uuid,
            peer_router_name,
            1,
            lsa_payload(
                expected_hive,
                2,
                &peer_router_uuid.to_string(),
                peer_router_name,
            ),
        )
        .await;
        assert!(matches!(
            stale_same_session,
            LsaApplyResult::Rejected {
                reason: LSA_REJECT_STALE_SEQ,
                ..
            }
        ));

        let after_reconnect = apply_lsa_payload(
            &lsa_state,
            &HashSet::new(),
            expected_hive,
            peer_router_uuid,
            peer_router_name,
            2,
            lsa_payload(
                expected_hive,
                2,
                &peer_router_uuid.to_string(),
                peer_router_name,
            ),
        )
        .await;
        assert!(matches!(after_reconnect, LsaApplyResult::Applied { .. }));

        let guard = lsa_state.lock().await;
        let entry = guard.get(expected_hive).expect("missing hive state");
        assert_eq!(entry.session_epoch, 2);
        assert_eq!(entry.last_seq, 2);
    }

    #[tokio::test]
    async fn lsa_uses_peer_identity_when_payload_identity_missing() {
        let lsa_state = Arc::new(Mutex::new(HashMap::<String, RemoteHiveState>::new()));
        let peer_router_uuid = Uuid::new_v4();
        let peer_router_name = "RT.gateway@worker-220";
        let expected_hive = "worker-220";

        let result = apply_lsa_payload(
            &lsa_state,
            &HashSet::new(),
            expected_hive,
            peer_router_uuid,
            peer_router_name,
            1,
            lsa_payload(expected_hive, 1, "", ""),
        )
        .await;
        assert!(matches!(result, LsaApplyResult::Applied { .. }));

        let guard = lsa_state.lock().await;
        let entry = guard.get(expected_hive).expect("missing hive state");
        assert_eq!(entry.router_uuid, *peer_router_uuid.as_bytes());
        assert_eq!(entry.router_name, peer_router_name);
    }

    fn lsa_node(name: &str) -> LsaNode {
        LsaNode {
            uuid: Uuid::new_v4().to_string(),
            name: name.to_string(),
            vpn_id: 0,
        }
    }

    /// An LSA installs only the nodes named `<role>@<the hive that sent it>`; the others are
    /// returned to be counted (docs/control-path-hardening-v1.md §5).
    #[tokio::test]
    async fn an_lsa_installs_only_the_nodes_of_the_hive_that_sent_it() {
        let lsa_state = Arc::new(Mutex::new(HashMap::<String, RemoteHiveState>::new()));
        let peer_router_uuid = Uuid::new_v4();
        let names = [
            ("AI.sales@worker1", true),
            ("SY.orchestrator@worker1", true),
            ("SY.admin@motherbee", false),
            ("SY.vault@motherbee", false),
            ("AI.sales@worker10", false),
            ("AI.sales@worker1@motherbee", false),
            ("AI.sales@motherbee@worker1", false),
            ("SY.orchestrator", false),
            ("@worker1", false),
        ];
        let mut payload = lsa_payload(
            "worker1",
            1,
            &peer_router_uuid.to_string(),
            "RT.gateway@worker1",
        );
        payload.nodes = names.iter().map(|(name, _)| lsa_node(name)).collect();

        let result = apply_lsa_payload(
            &lsa_state,
            &HashSet::new(),
            "worker1",
            peer_router_uuid,
            "RT.gateway@worker1",
            1,
            payload,
        )
        .await;

        let LsaApplyResult::Applied { foreign_nodes, .. } = result else {
            panic!("the LSA applies: {result:?}");
        };
        let kept: Vec<&str> = names
            .iter()
            .filter(|(_, kept)| *kept)
            .map(|(name, _)| *name)
            .collect();
        let dropped: Vec<&str> = names
            .iter()
            .filter(|(_, kept)| !*kept)
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(foreign_nodes, dropped);
        let guard = lsa_state.lock().await;
        let installed: Vec<&str> = guard["worker1"]
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(installed, kept);
    }

    /// The hive `name` in an LSA snapshot, with `flags`.
    fn lsa_hive(name: &str, flags: u16) -> RemoteHiveEntry {
        let mut entry = RemoteHiveEntry {
            hive_id: [0u8; 64],
            hive_id_len: 0,
            router_uuid: [0u8; 16],
            router_name: [0u8; 64],
            router_name_len: 0,
            last_lsa_seq: 1,
            last_updated: 0,
            flags,
            node_count: 0,
            route_count: 0,
            vpn_count: 0,
            tap_count: 0,
        };
        entry.hive_id_len = copy_bytes_with_len(&mut entry.hive_id, name) as u16;
        entry
    }

    /// A route that the hive at `hive_index` of the snapshot advertised.
    fn lsa_route(
        prefix: &str,
        match_kind: u8,
        action: u8,
        next_hop_hive: &str,
        hive_index: u16,
    ) -> RemoteRouteEntry {
        let mut entry = RemoteRouteEntry {
            prefix: [0u8; 256],
            prefix_len: 0,
            match_kind,
            action,
            next_hop_hive: [0u8; 32],
            next_hop_hive_len: 0,
            _pad: [0u8; 3],
            metric: 0,
            priority: 0,
            flags: FLAG_ACTIVE,
            hive_index,
            _reserved: [0u8; 14],
        };
        entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, prefix) as u16;
        entry.next_hop_hive_len =
            copy_bytes_with_len(&mut entry.next_hop_hive, next_hop_hive) as u8;
        entry
    }

    /// A tap that the hive at `hive_index` of the snapshot advertised.
    fn lsa_tap(match_src: &str, match_dst: &str, target: &str, hive_index: u16) -> RemoteTapEntry {
        let mut entry = RemoteTapEntry {
            match_src: [0u8; 256],
            match_src_len: 0,
            _pad0: [0u8; 6],
            match_dst: [0u8; 256],
            match_dst_len: 0,
            _pad1: [0u8; 6],
            target: [0u8; 256],
            target_len: 0,
            _pad2: [0u8; 6],
            mode: 0,
            enabled: 1,
            flags: FLAG_ACTIVE,
            hive_index,
            _reserved: [0u8; 38],
        };
        entry.match_src_len = copy_bytes_with_len(&mut entry.match_src, match_src) as u16;
        entry.match_dst_len = copy_bytes_with_len(&mut entry.match_dst, match_dst) as u16;
        entry.target_len = copy_bytes_with_len(&mut entry.target, target) as u16;
        entry
    }

    /// The motherbee's LSA snapshot: itself at index 0, worker1 at index 1.
    fn motherbee_lsa(routes: Vec<RemoteRouteEntry>, taps: Vec<RemoteTapEntry>) -> LsaSnapshot {
        LsaSnapshot {
            header: crate::shm::LsaHeaderSnapshot {
                hive_count: 2,
                total_node_count: 0,
                total_route_count: routes.len() as u32,
                total_vpn_count: 0,
                total_tap_count: taps.len() as u32,
                heartbeat: 0,
            },
            hives: vec![
                lsa_hive("motherbee", FLAG_ACTIVE | HIVE_FLAG_SELF),
                lsa_hive("worker1", FLAG_ACTIVE),
            ],
            nodes: Vec::new(),
            routes,
            vpns: Vec::new(),
            taps,
        }
    }

    fn route_outcome(route: ResolvedRoute) -> String {
        match route {
            ResolvedRoute::Drop => "drop".to_string(),
            ResolvedRoute::Unreachable(reason) => format!("unreachable {reason}"),
            ResolvedRoute::Deliver(_) => "deliver".to_string(),
            ResolvedRoute::ForwardRouter(_) => "router".to_string(),
            ResolvedRoute::ForwardHive(hive) => format!("hive {hive}"),
        }
    }

    /// Another hive's routes apply only to destinations on that hive, or to `name@*`
    /// (docs/control-path-hardening-v1.md §5); its `next_hop_hive` and this hive's static routes
    /// work as before.
    #[tokio::test]
    async fn another_hives_routes_apply_only_to_its_own_nodes_and_any_hive_names() {
        let lsa_snapshot = Arc::new(Mutex::new(Some(motherbee_lsa(
            vec![
                lsa_route("*", MATCH_GLOB, ACTION_FORWARD, "", 1),
                lsa_route("SY.admin", MATCH_PREFIX, ACTION_DROP, "", 1),
                lsa_route("AI.y@worker1", MATCH_EXACT, ACTION_FORWARD, "egress1", 1),
            ],
            Vec::new(),
        ))));
        let static_routes = Arc::new(Mutex::new(vec![StaticRoute {
            pattern: "SY.vault".to_string(),
            match_kind: MATCH_PREFIX,
            action: ACTION_DROP,
            next_hop_hive: String::new(),
            priority: 100,
            metric: 0,
            installed_at: 0,
        }]));
        let fib = Arc::new(Mutex::new(Vec::<FibEntry>::new()));
        rebuild_fib(
            &fib,
            &Arc::new(Mutex::new(HashMap::<Uuid, NodeHandle>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new())),
            &static_routes,
            &lsa_snapshot,
            &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
        )
        .await;
        let (sender, _receiver) = mpsc::unbounded_channel::<Vec<u8>>();
        let src = NodeHandle {
            name: "IO.slack@motherbee".to_string(),
            vpn_id: 0,
            sender,
            connected_at: 0,
        };
        let nodes = HashMap::new();
        let meta = Meta {
            msg_type: "user".to_string(),
            ..Meta::default()
        };
        for (target, expected) in [
            // The motherbee's names, and another spoke's: worker1's routes neither take nor drop
            // them.
            ("SY.admin@motherbee", "unreachable NODE_NOT_FOUND"),
            ("AI.x@motherbee", "unreachable NODE_NOT_FOUND"),
            ("AI.x@egress1", "unreachable NODE_NOT_FOUND"),
            // worker1's names: its routes apply, next_hop_hive included.
            ("AI.x@worker1", "hive worker1"),
            ("SY.admin@worker1", "drop"),
            ("AI.y@worker1", "hive egress1"),
            // Any hive.
            ("AI.x@*", "hive worker1"),
            // This hive's static routes, on any name.
            ("SY.vault@motherbee", "drop"),
            ("SY.vault@worker1", "drop"),
        ] {
            let route = resolve_by_name(target, &src, &nodes, &fib, &meta)
                .await
                .expect("resolve");
            assert_eq!(route_outcome(route), expected, "{target}");
        }
    }

    #[test]
    fn another_hives_tap_applies_only_to_a_message_it_is_a_party_to() {
        let snapshot = motherbee_lsa(Vec::new(), Vec::new());
        let own = lsa_tap(
            "SY.admin@motherbee",
            "SY.vault@motherbee",
            "IO.x@motherbee",
            0,
        );
        let worker1s = lsa_tap(
            "SY.admin@motherbee",
            "SY.vault@motherbee",
            "IO.x@worker1",
            1,
        );
        // worker1 is a party to what it taps, but the copy would go to another hive's node.
        let to_worker2 = lsa_tap(
            "SY.admin@motherbee",
            "SY.orchestrator@worker1",
            "SY.orchestrator@worker2",
            1,
        );
        let to_motherbee = lsa_tap(
            "SY.admin@motherbee",
            "SY.orchestrator@worker1",
            "IO.x@motherbee",
            1,
        );
        let unlisted = lsa_tap("SY.admin@motherbee", "AI.x@worker1", "IO.x@worker1", 7);
        for (tap, src, dst, applies) in [
            (&own, "SY.admin@motherbee", "SY.vault@motherbee", true),
            (&worker1s, "SY.admin@motherbee", "SY.vault@motherbee", false),
            (&worker1s, "SY.admin@motherbee", "AI.x@egress1", false),
            (&worker1s, "SY.admin@motherbee", "AI.x@worker1", true),
            (&worker1s, "AI.x@worker1", "SY.vault@motherbee", true),
            (&worker1s, "SY.admin@motherbee", "AI.x@*", false),
            (
                &worker1s,
                "SY.admin@motherbee",
                "AI.x@worker1@motherbee",
                false,
            ),
            (
                &to_worker2,
                "SY.admin@motherbee",
                "SY.orchestrator@worker1",
                false,
            ),
            (
                &to_motherbee,
                "SY.admin@motherbee",
                "SY.orchestrator@worker1",
                false,
            ),
            (&unlisted, "SY.admin@motherbee", "AI.x@worker1", false),
        ] {
            assert_eq!(
                lsa_tap_applies(&snapshot, tap, src, dst),
                applies,
                "tap of hive index {} on {src} -> {dst}",
                tap.hive_index
            );
        }
    }

    /// The design's example, end to end on the motherbee's gateway: a spoke's tap on SY.admin's
    /// VAULT_PUT to the vault, both on the motherbee, copies nothing. A spoke's tap copies a message
    /// to one of its nodes, and only to one of its own nodes; the motherbee's own taps copy anything
    /// to anyone; and no copy carries a protected SYSTEM action.
    #[tokio::test]
    async fn the_tap_fanout_copies_for_another_hive_only_what_it_is_a_party_to() {
        let admin = Uuid::new_v4();
        let (admin_tx, _admin_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (vault_tx, _vault_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (audit_tx, mut audit_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        for (uuid, name, sender) in [
            (admin, "SY.admin@motherbee", admin_tx),
            (Uuid::new_v4(), "SY.vault@motherbee", vault_tx),
            (Uuid::new_v4(), "IO.audit@motherbee", audit_tx),
        ] {
            nodes.lock().await.insert(
                uuid,
                NodeHandle {
                    name: name.to_string(),
                    vpn_id: 0,
                    sender,
                    connected_at: 0,
                },
            );
        }
        let peer_nodes = Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new()));
        // worker1's link: a copy for IO.audit@worker1 goes there.
        let (worker1_tx, mut worker1_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let wan_peers = Arc::new(Mutex::new(HashMap::from([(
            "worker1".to_string(),
            WanPeer {
                sender: worker1_tx,
                router_uuid: Uuid::new_v4(),
            },
        )])));
        let src_handle = nodes.lock().await[&admin].clone();
        let config = ConfigSnapshot {
            header: crate::shm::ConfigHeaderSnapshot {
                static_route_count: 0,
                vpn_assignment_count: 0,
                config_version: 1,
                heartbeat: 0,
                tap_count: 0,
            },
            routes: Vec::new(),
            vpns: Vec::new(),
            taps: Vec::new(),
        };
        for (hive_index, action, dst, target, copied_here, copied_to_worker1) in [
            // worker1 is not a party: no copy.
            (
                1,
                "VAULT_PUT",
                "SY.vault@motherbee",
                "IO.audit@worker1",
                false,
                false,
            ),
            // worker1 is the destination's hive: a copy, to its own node.
            (
                1,
                "VAULT_PUT",
                "AI.x@worker1",
                "IO.audit@worker1",
                false,
                true,
            ),
            // ... and never to another hive's.
            (
                1,
                "VAULT_PUT",
                "AI.x@worker1",
                "IO.audit@motherbee",
                false,
                false,
            ),
            // The motherbee's own tap: any message, any target.
            (
                0,
                "VAULT_PUT",
                "SY.vault@motherbee",
                "IO.audit@motherbee",
                true,
                false,
            ),
            // A protected SYSTEM action, though its sender may send it: no copy.
            (
                0,
                "SPAWN_NODE",
                "SY.orchestrator@motherbee",
                "IO.audit@motherbee",
                false,
                false,
            ),
        ] {
            let mut lsa = motherbee_lsa(
                Vec::new(),
                vec![lsa_tap("SY.admin@motherbee", dst, target, hive_index)],
            );
            lsa.nodes
                .push(lsa_node_entry(Uuid::new_v4(), "IO.audit@worker1", 1));
            let lsa_snapshot = Arc::new(Mutex::new(Some(lsa)));
            let fib = Arc::new(Mutex::new(Vec::<FibEntry>::new()));
            rebuild_fib(
                &fib,
                &nodes,
                &peer_nodes,
                &Arc::new(Mutex::new(Vec::<StaticRoute>::new())),
                &lsa_snapshot,
                &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
            )
            .await;
            let msg = Message {
                routing: Routing {
                    src: admin.to_string(),
                    src_l2_name: Some("SY.admin@motherbee".to_string()),
                    dst: Destination::Unicast(dst.to_string()),
                    ttl: 16,
                    trace_id: Uuid::new_v4().to_string(),
                },
                meta: Meta {
                    msg_type: SYSTEM_KIND.to_string(),
                    msg: Some(action.to_string()),
                    ..Meta::default()
                },
                payload: serde_json::json!({}),
            };
            fanout_taps_for_unicast(
                &msg,
                dst,
                &config,
                &src_handle,
                &nodes,
                &fib,
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerHandle>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerRouter>::new())),
                &peer_nodes,
                &wan_peers,
                &lsa_snapshot,
                &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
                "motherbee",
                Uuid::new_v4(),
                true,
            )
            .await;
            let case = format!("tap of hive index {hive_index} on {action} to {dst}, for {target}");
            for (copy, expected) in [
                (audit_rx.try_recv().ok(), copied_here),
                (worker1_rx.try_recv().ok(), copied_to_worker1),
            ] {
                assert_eq!(copy.is_some(), expected, "{case}");
                if let Some(copy) = copy {
                    let copy: Message = serde_json::from_slice(&copy).expect("a message");
                    assert!(copy.meta.via_tap, "{case}");
                }
            }
        }
    }

    /// A node of the LSA snapshot, listed by the hive at `hive_index`.
    fn lsa_node_entry(uuid: Uuid, name: &str, hive_index: u16) -> RemoteNodeEntry {
        let mut entry = RemoteNodeEntry {
            uuid: *uuid.as_bytes(),
            name: [0u8; 256],
            name_len: 0,
            vpn_id: 0,
            hive_index,
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 6],
        };
        entry.name_len = copy_bytes_with_len(&mut entry.name, name) as u16;
        entry
    }

    /// An LSA snapshot of `hives`, listing `nodes`.
    fn lsa_of(hives: Vec<RemoteHiveEntry>, nodes: Vec<RemoteNodeEntry>) -> LsaSnapshot {
        LsaSnapshot {
            header: crate::shm::LsaHeaderSnapshot {
                hive_count: hives.len() as u32,
                total_node_count: nodes.len() as u32,
                total_route_count: 0,
                total_vpn_count: 0,
                total_tap_count: 0,
                heartbeat: 0,
            },
            hives,
            nodes,
            routes: Vec::new(),
            vpns: Vec::new(),
            taps: Vec::new(),
        }
    }

    /// R3: a tap copy never carries a protected SYSTEM action, whoever sent the original; any other
    /// copy passes (docs/control-path-hardening-v1.md §5).
    #[test]
    fn a_tap_copy_never_carries_a_protected_system_action() {
        let message = |action: &str, via_tap: bool| Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some("SY.orchestrator@motherbee".to_string()),
                dst: Destination::Unicast("SY.orchestrator@worker1".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(action.to_string()),
                via_tap,
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        };
        assert!(prepare_local_delivery("worker1", &message("SPAWN_NODE", false), false).is_some());
        assert!(prepare_local_delivery("worker1", &message("SPAWN_NODE", true), false).is_none());
        assert!(prepare_local_delivery("worker1", &message("VAULT_PUT", true), false).is_some());
    }

    /// R1: a WAN frame's sender is bound to its session: a node of the peer's own LSA bucket, or,
    /// on a spoke, a node the hub relays when that peer is the hub
    /// (docs/control-path-hardening-v1.md §5).
    #[tokio::test]
    async fn a_wan_frame_speaks_only_for_the_peer_that_sent_it() {
        let wf_rules = Uuid::new_v4();
        let ingress_node = Uuid::new_v4();
        // The motherbee's view: two spokes' buckets.
        let motherbee_lsa = Arc::new(Mutex::new(Some(lsa_of(
            vec![
                lsa_hive("motherbee", FLAG_ACTIVE | HIVE_FLAG_SELF),
                lsa_hive("ingress1", FLAG_ACTIVE),
                lsa_hive("worker1", FLAG_ACTIVE),
            ],
            vec![
                lsa_node_entry(ingress_node, "IO.x@ingress1", 1),
                lsa_node_entry(wf_rules, "SY.wf-rules@worker1", 2),
            ],
        ))));
        let no_relays = Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new()));
        for (peer, src, expected) in [
            ("worker1", wf_rules, Some("SY.wf-rules@worker1")),
            // Another spoke's node, by its UUID.
            ("ingress1", wf_rules, None),
            ("ingress1", ingress_node, Some("IO.x@ingress1")),
            ("worker1", Uuid::new_v4(), None),
        ] {
            let source = wan_frame_source(&motherbee_lsa, &no_relays, src, peer).await;
            assert_eq!(
                source.as_ref().map(|info| info.name.as_str()),
                expected,
                "{src} from {peer}"
            );
        }

        // A spoke's view: the hub's bucket, and a node the hub relays.
        let relayed = Uuid::new_v4();
        let spoke_lsa = Arc::new(Mutex::new(Some(lsa_of(
            vec![
                lsa_hive("worker2", FLAG_ACTIVE | HIVE_FLAG_SELF),
                lsa_hive("motherbee", FLAG_ACTIVE),
            ],
            Vec::new(),
        ))));
        let relays = Arc::new(Mutex::new(HashMap::from([(
            relayed,
            ReachabilityEntry {
                name: "AI.y@worker1".to_string(),
                origin_hive: "worker1".to_string(),
                next_hop_hive: "motherbee".to_string(),
                vpn_id: 0,
                last_updated: 0,
            },
        )])));
        let from_hub = wan_frame_source(&spoke_lsa, &relays, relayed, "motherbee")
            .await
            .expect("the hub relays it");
        assert!(from_hub.via_hub);
        assert_eq!(from_hub.hive_id, "worker1");
        assert!(
            wan_frame_source(&spoke_lsa, &relays, relayed, "ingress1")
                .await
                .is_none(),
            "only the hub that vouches for it relays it"
        );
    }

    /// R2: a UUID that two hives list resolves to neither, whatever their order in the region; one
    /// that a single hive lists resolves to it.
    #[test]
    fn a_uuid_that_two_hives_list_resolves_to_neither() {
        let shared = Uuid::new_v4();
        let local = Uuid::new_v4();
        let own = Uuid::new_v4();
        let snapshot = Some(lsa_of(
            vec![
                lsa_hive("motherbee", FLAG_ACTIVE | HIVE_FLAG_SELF),
                lsa_hive("ingress1", FLAG_ACTIVE),
                lsa_hive("worker1", FLAG_ACTIVE),
            ],
            vec![
                lsa_node_entry(local, "SY.vault@motherbee", 0),
                lsa_node_entry(shared, "IO.x@ingress1", 1),
                lsa_node_entry(local, "IO.y@ingress1", 1),
                lsa_node_entry(shared, "AI.sales@worker1", 2),
                lsa_node_entry(own, "AI.support@worker1", 2),
            ],
        ));
        assert!(
            find_remote_node(&snapshot, shared).is_none(),
            "ingress1 comes first by name and still does not get it"
        );
        assert!(
            find_remote_node(&snapshot, local).is_none(),
            "a node of this hive, listed by another"
        );
        let resolved = find_remote_node(&snapshot, own).expect("one hive lists it");
        assert_eq!(
            (resolved.name.as_str(), resolved.hive_id.as_str()),
            ("AI.support@worker1", "worker1")
        );
    }

    #[test]
    fn edge_does_not_inherit_blanket_system_vpn_bypass() {
        let meta = Meta {
            msg_type: "user".to_string(),
            ..Meta::default()
        };
        let global_system = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            scope: Some(SCOPE_GLOBAL.to_string()),
            ..Meta::default()
        };

        assert!(vpn_allows_between(
            &meta,
            "SY.identity@motherbee",
            1,
            "AI.worker@worker1",
            2
        ));
        assert!(!vpn_allows_between(
            &meta,
            "SY.edge@ingress1",
            1,
            "AI.worker@worker1",
            2
        ));
        assert!(!vpn_allows_between(
            &meta,
            "SY.edge@ingress1",
            1,
            "SY.identity@ingress1",
            1
        ));
        assert!(vpn_allows_between(
            &global_system,
            "SY.identity@motherbee",
            1,
            "AI.worker@worker1",
            2
        ));
        assert!(!vpn_allows_between(
            &global_system,
            "SY.edge@ingress1",
            1,
            "SY.identity@motherbee",
            2
        ));
        assert!(!vpn_allows_between(
            &global_system,
            "SY.identity@motherbee",
            2,
            "SY.edge@ingress1",
            1
        ));
        assert!(vpn_allows_between(
            &meta,
            "SY.edge@ingress1",
            1,
            "IO.cloud@motherbee",
            2
        ));
        assert!(vpn_allows_between(
            &meta,
            "IO.cloud@motherbee",
            2,
            "SY.edge@ingress1",
            1
        ));
    }

    #[test]
    fn system_kind_classification_is_case_insensitive() {
        // The router MUST agree with the nodes (eq_ignore_ascii_case). Otherwise a frame stamped
        // msg_type="SYSTEM" bypasses every SYSTEM gate here while the node still runs it as
        // control-plane (the audited HIGH gate-bypass). Guard all four casings.
        for mt in ["system", "SYSTEM", "System", "sYsTeM"] {
            assert!(is_system_kind(mt), "is_system_kind({mt}) must be true");
        }
        assert!(!is_system_kind("user"));
        assert!(!is_system_kind("admin"));

        // Regression: an UPPERCASE 'SYSTEM' frame on the edge<->IO path is still rejected (the
        // EDGE-05 guard used to only match lowercase "system" and let this through).
        for mt in ["SYSTEM", "System"] {
            let sys = Meta {
                msg_type: mt.to_string(),
                ..Meta::default()
            };
            assert!(!vpn_allows_between(
                &sys,
                "SY.edge@ingress1",
                1,
                "IO.cloud@motherbee",
                2
            ));
            assert!(!vpn_allows_between(
                &sys,
                "IO.cloud@motherbee",
                2,
                "SY.edge@ingress1",
                1
            ));
        }
    }

    #[test]
    fn edge_data_path_rejects_system_msg_type() {
        // EDGE-05: the edge<->IO data path is DATA-family only. A SYSTEM frame on an edge<->IO
        // pair is never legitimate (the edge forwards under the channel's inbound_family, which
        // validate_endpoint_row_for_open proves != "system"; IO handlers reply in that same
        // family). Reject both directions, for an arbitrary/unprotected SYSTEM verb AND a
        // protected one that is NOT an edge-service action (so it reaches the data-path term).
        let sys_bare = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            ..Meta::default()
        };
        let sys_protected = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some("SPAWN_NODE".to_string()),
            ..Meta::default()
        };
        for m in [&sys_bare, &sys_protected] {
            // compromised edge -> IO node: cannot originate SYSTEM at an IO node
            assert!(!vpn_allows_between(
                m,
                "SY.edge@ingress1",
                1,
                "IO.cloud@motherbee",
                2
            ));
            // rogue IO node -> edge: cannot land a SYSTEM frame on the edge (reply-shadowing)
            assert!(!vpn_allows_between(
                m,
                "IO.cloud@motherbee",
                2,
                "SY.edge@ingress1",
                1
            ));
        }

        // Preservation: the legitimate data-family forward + reply stay allowed.
        let user = Meta {
            msg_type: "user".to_string(),
            ..Meta::default()
        };
        assert!(vpn_allows_between(
            &user,
            "SY.edge@ingress1",
            1,
            "IO.cloud@motherbee",
            2
        ));
        assert!(vpn_allows_between(
            &user,
            "IO.cloud@motherbee",
            2,
            "SY.edge@ingress1",
            1
        ));

        // Regression: the edge's SYSTEM traffic with SY.vault and SY.admin (non-IO peers) rides
        // the sibling OR terms and is NOT touched by the edge<->IO SYSTEM guard.
        let vault_get = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_VAULT_GET.to_string()),
            ..Meta::default()
        };
        assert!(vpn_allows_between(
            &vault_get,
            "SY.edge@ingress1",
            1,
            "SY.vault@motherbee",
            1
        ));
        let open = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_EDGE_OPEN_URL.to_string()),
            ..Meta::default()
        };
        assert!(vpn_allows_between(
            &open,
            "SY.admin@motherbee",
            1,
            "SY.edge@ingress1",
            2
        ));
    }

    #[test]
    fn edge_vault_path_is_read_only_and_explicit() {
        let vault_get = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_VAULT_GET.to_string()),
            ..Meta::default()
        };
        let vault_get_response = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_VAULT_GET_RESPONSE.to_string()),
            ..Meta::default()
        };
        let vault_put = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some("VAULT_PUT".to_string()),
            ..Meta::default()
        };

        assert!(vpn_allows_between(
            &vault_get,
            "SY.edge@ingress1",
            1,
            "SY.vault@motherbee",
            2
        ));
        assert!(vpn_allows_between(
            &vault_get_response,
            "SY.vault@motherbee",
            2,
            "SY.edge@ingress1",
            1
        ));
        assert!(!vpn_allows_between(
            &vault_put,
            "SY.edge@ingress1",
            1,
            "SY.vault@motherbee",
            2
        ));
    }

    #[test]
    fn edge_control_routing_is_admin_motherbee_only() {
        let open = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_EDGE_OPEN_URL.to_string()),
            ..Meta::default()
        };
        let open_response = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_EDGE_OPEN_URL_RESPONSE.to_string()),
            ..Meta::default()
        };
        let publish_blob = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_EDGE_PUBLISH_BLOB.to_string()),
            ..Meta::default()
        };
        let publish_blob_response = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_EDGE_PUBLISH_BLOB_RESPONSE.to_string()),
            ..Meta::default()
        };

        assert!(vpn_allows_between(
            &open,
            "SY.admin@motherbee",
            1,
            "SY.edge@ingress1",
            2
        ));
        assert!(!vpn_allows_between(
            &open,
            "SY.admin@ingress1",
            2,
            "SY.edge@ingress1",
            2
        ));
        assert!(!vpn_allows_between(
            &open,
            "SY.orchestrator@motherbee",
            1,
            "SY.edge@ingress1",
            2
        ));
        assert!(vpn_allows_between(
            &open_response,
            "SY.edge@ingress1",
            2,
            "SY.admin@motherbee",
            1
        ));
        assert!(!vpn_allows_between(
            &open_response,
            "SY.edge@ingress1",
            2,
            "SY.admin@ingress1",
            2
        ));
        assert!(!vpn_allows_between(
            &open,
            "SY.edge@ingress1",
            2,
            "SY.admin@motherbee",
            1
        ));
        assert!(vpn_allows_between(
            &publish_blob,
            "SY.admin@motherbee",
            1,
            "SY.edge@ingress1",
            2
        ));
        assert!(vpn_allows_between(
            &publish_blob_response,
            "SY.edge@ingress1",
            2,
            "SY.admin@motherbee",
            1
        ));
        assert!(!vpn_allows_between(
            &publish_blob,
            "SY.orchestrator@motherbee",
            1,
            "SY.edge@ingress1",
            2
        ));
    }

    fn system_meta(msg: &str) -> Meta {
        Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg.to_string()),
            ..Meta::default()
        }
    }

    /// The certificate commands (docs/acme-edge-certificates-v1.md §5) ride the edge control gate
    /// like the URL commands: from SY.admin@motherbee only, and their answers back to it only.
    /// Without them in the gate the router answers VPN_BLOCKED before the system policy is asked.
    #[test]
    fn edge_certificate_commands_ride_the_edge_control_gate() {
        for (command, response) in [
            (MSG_EDGE_TLS_STATUS, MSG_EDGE_TLS_STATUS_RESPONSE),
            (MSG_EDGE_TLS_RELOAD, MSG_EDGE_TLS_RELOAD_RESPONSE),
            (MSG_EDGE_ACME_CHALLENGE, MSG_EDGE_ACME_CHALLENGE_RESPONSE),
        ] {
            let command = system_meta(command);
            let response = system_meta(response);
            assert!(vpn_allows_between(
                &command,
                "SY.admin@motherbee",
                0,
                "SY.edge@ingress1",
                0
            ));
            for rogue in [
                "SY.admin@ingress1",
                "SY.orchestrator@motherbee",
                "SY.vault@motherbee",
                "IO.cloud@motherbee",
                "SY.edge@ingress2",
            ] {
                assert!(
                    !vpn_allows_between(&command, rogue, 0, "SY.edge@ingress1", 0),
                    "{rogue} must not reach the edge with {:?}",
                    command.msg
                );
            }
            assert!(vpn_allows_between(
                &response,
                "SY.edge@ingress1",
                0,
                "SY.admin@motherbee",
                0
            ));
            for elsewhere in [
                "SY.admin@ingress1",
                "SY.orchestrator@motherbee",
                "IO.cloud@motherbee",
            ] {
                assert!(
                    !vpn_allows_between(&response, "SY.edge@ingress1", 0, elsewhere, 0),
                    "{:?} must not go to {elsewhere}",
                    response.msg
                );
            }
            // Neither turns around: the edge never commands the Admin, nor is it sent an answer.
            assert!(!vpn_allows_between(
                &command,
                "SY.edge@ingress1",
                0,
                "SY.admin@motherbee",
                0
            ));
            assert!(!vpn_allows_between(
                &response,
                "SY.admin@motherbee",
                0,
                "SY.edge@ingress1",
                0
            ));
        }
    }

    /// The web routes (docs/io-web-v2.md §11.1) ride the edge control gate like the other edge
    /// commands: from SY.admin@motherbee only, whatever the VPNs, and the answer back to it only.
    /// At the edge's router the delivery gate then asks the system policy, which admits the same
    /// single sender (rule (1)).
    #[test]
    fn the_web_routes_reach_the_edge_only_from_the_primary_admin() {
        let command = system_meta(MSG_EDGE_WEB_ROUTES);
        let response = system_meta(MSG_EDGE_WEB_ROUTES_RESPONSE);
        let admin = system_policy::EDGE_CONTROL_AUTHORITY;
        let edge = "SY.edge@ingress1";
        let rogues = [
            "SY.admin@ingress1",
            "SY.admin@worker1",
            "SY.orchestrator@motherbee",
            "SY.orchestrator@ingress1",
            "IO.web@motherbee",
            "IO.cloud@motherbee",
            "SY.edge@ingress1",
            "SY.edge@ingress2",
            "SY.vault@motherbee",
        ];

        assert!(vpn_allows_between(&command, admin, 0, edge, 0));
        assert!(vpn_allows_between(&command, admin, 0, edge, 7));
        for rogue in rogues {
            assert!(
                !vpn_allows_between(&command, rogue, 0, edge, 0),
                "{rogue} must not send the web routes to the edge"
            );
        }
        assert!(vpn_allows_between(&response, edge, 0, admin, 0));
        for elsewhere in [
            "SY.admin@ingress1",
            "SY.orchestrator@motherbee",
            "IO.web@motherbee",
            "SY.edge@ingress2",
        ] {
            assert!(
                !vpn_allows_between(&response, edge, 0, elsewhere, 0),
                "the edge's answer must not go to {elsewhere}"
            );
        }
        // Neither turns around: the edge never sends the routes, nor is it sent the answer.
        assert!(!vpn_allows_between(&command, edge, 0, admin, 0));
        assert!(!vpn_allows_between(&response, admin, 0, edge, 0));

        let delivery = |sender: &str| Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some(sender.to_string()),
                dst: Destination::Unicast(edge.to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: system_meta(MSG_EDGE_WEB_ROUTES),
            payload: serde_json::json!({
                "routes": [{"host": "archi.example.test", "upstream": "io.web"}]
            }),
        };
        assert!(prepare_local_delivery("ingress1", &delivery(admin), false).is_some());
        for rogue in rogues {
            assert!(
                prepare_local_delivery("ingress1", &delivery(rogue), false).is_none(),
                "the system policy must refuse the web routes from {rogue}"
            );
        }
    }

    /// The edge gate and the system policy list the same edge commands. A command missing from the
    /// gate is answered VPN_BLOCKED before the policy is asked; one missing from the protected
    /// actions reaches the edge without the policy. Every command has its answer in the gate.
    #[test]
    fn the_edge_gate_lists_every_protected_edge_command() {
        let mut gate = EDGE_SERVICE_COMMANDS.to_vec();
        gate.sort_unstable();
        let mut protected: Vec<&str> = system_policy::PROTECTED_SYSTEM_ACTIONS
            .iter()
            .copied()
            .filter(|action| action.starts_with("EDGE_"))
            .collect();
        protected.sort_unstable();
        assert_eq!(gate, protected);
        let mut responses = EDGE_SERVICE_RESPONSES.to_vec();
        responses.sort_unstable();
        let mut expected: Vec<String> = gate
            .iter()
            .map(|command| format!("{command}_RESPONSE"))
            .collect();
        expected.sort_unstable();
        assert_eq!(responses, expected);
    }

    /// A-78: SY.vault's notice reaches the edge only addressed to it, from the primary hive's
    /// vault. The broadcast, which names every stored key, its tenant and its owners, stays out of
    /// the DMZ, and so does every other SYSTEM frame.
    #[test]
    fn the_vault_notice_reaches_the_edge_only_as_a_unicast_from_the_primary_vault() {
        let notice = system_meta(MSG_VAULT_SECRET_CHANGED);
        let broadcast_notice = Meta {
            scope: Some(SCOPE_GLOBAL.to_string()),
            ..system_meta(MSG_VAULT_SECRET_CHANGED)
        };
        let vault = "SY.vault@motherbee";
        let edge = "SY.edge@ingress1";

        // The unicast passes, whatever the VPNs, and the kind is matched without case.
        assert!(vpn_allows_between(&notice, vault, 0, edge, 0));
        assert!(vpn_allows_between(&notice, vault, 0, edge, 7));
        assert!(vpn_allows_between(&broadcast_notice, vault, 0, edge, 0));
        let upper = Meta {
            msg_type: "SYSTEM".to_string(),
            ..system_meta(MSG_VAULT_SECRET_CHANGED)
        };
        assert!(vpn_allows_between(&upper, vault, 0, edge, 0));
        assert!(vpn_allows_between(
            &notice,
            vault,
            0,
            "SY.edge@motherbee",
            0
        ));

        // The broadcast does not, with or without its global scope; every other node still gets it.
        for meta in [&notice, &broadcast_notice] {
            assert!(!vpn_allows_broadcast_between(meta, vault, 0, edge, 0));
            assert!(!vpn_allows_broadcast_between(
                meta,
                vault,
                0,
                "SY.edge@motherbee",
                0
            ));
            assert!(vpn_allows_broadcast_between(
                meta,
                vault,
                0,
                "SY.storage@motherbee",
                0
            ));
        }
        assert!(vpn_allows_broadcast_between(
            &broadcast_notice,
            vault,
            0,
            "AI.chat@worker1",
            4
        ));

        // Another source: the router-stamped name must be the primary hive's SY.vault.
        for source in [
            "SY.vault@worker1",
            "SY.vault@ingress1",
            "SY.vault.relay@motherbee",
            "SY.vault",
            "SY.admin@motherbee",
            "SY.storage@motherbee",
            "IO.cloud@motherbee",
            "SY.edge@ingress2",
        ] {
            assert!(
                !vpn_allows_between(&notice, source, 0, edge, 0),
                "a notice from {source} must not reach the edge"
            );
        }

        // Another destination: only an SY.edge is admitted by this rule. The edge never sends one.
        for destination in [
            "SY.edge.relay@ingress1",
            "SY.storage@motherbee",
            "IO.cloud@motherbee",
        ] {
            assert!(
                !edge_vault_notice_allowed(&notice, vault, destination),
                "{destination} is not an edge"
            );
        }
        assert!(!vpn_allows_between(&notice, edge, 0, vault, 0));
        assert!(!vpn_allows_between(&notice, edge, 0, "SY.edge@ingress2", 0));

        // Every other frame from the vault to the edge stays blocked: other SYSTEM verbs, no verb,
        // and the notice's name on a non-SYSTEM kind.
        for other in [
            "VAULT_PUT",
            "VAULT_LIST_RESPONSE",
            "VAULT_GET_METADATA_RESPONSE",
            "NODE_STATUS_GET",
            "CONFIG_CHANGED",
            MSG_EDGE_TLS_RELOAD,
        ] {
            assert!(
                !vpn_allows_between(&system_meta(other), vault, 0, edge, 0),
                "{other} from the vault must not reach the edge"
            );
        }
        let no_verb = Meta {
            msg_type: SYSTEM_KIND.to_string(),
            ..Meta::default()
        };
        assert!(!vpn_allows_between(&no_verb, vault, 0, edge, 0));
        let user_kind = Meta {
            msg_type: "user".to_string(),
            msg: Some(MSG_VAULT_SECRET_CHANGED.to_string()),
            ..Meta::default()
        };
        assert!(!vpn_allows_between(&user_kind, vault, 0, edge, 0));
    }

    // ---- Option B: WAN multi-hop reachability (edge-multihop-reachability-spec-v1) ----

    #[test]
    fn wan_reachability_only_primary_hub_may_vouch() {
        assert!(system_policy::wan_reachability_voucher_allowed("motherbee"));
        assert!(!system_policy::wan_reachability_voucher_allowed("worker1"));
        assert!(!system_policy::wan_reachability_voucher_allowed("ingress1"));
        assert!(!system_policy::wan_reachability_voucher_allowed(""));
    }

    #[test]
    fn find_reachable_node_marks_via_hub_and_next_hop() {
        let mut table = std::collections::HashMap::new();
        let uuid = Uuid::new_v4();
        table.insert(
            uuid,
            ReachabilityEntry {
                name: "IO.api.orders@worker1".to_string(),
                origin_hive: "worker1".to_string(),
                next_hop_hive: "motherbee".to_string(),
                vpn_id: 0,
                last_updated: 0,
            },
        );
        let info = find_reachable_node(&table, uuid).expect("resolves");
        assert!(info.via_hub);
        assert_eq!(info.hive_id, "worker1"); // origin — for canonical name + authority
        assert_eq!(info.next_hop_hive, "motherbee"); // forward target (a direct WAN peer)
        assert!(find_reachable_node(&table, Uuid::new_v4()).is_none());
    }

    #[tokio::test]
    async fn via_hub_src_is_denied_system_authority_but_direct_src_is_allowed() {
        // The security invariant: a transitively hub-vouched origin, even bearing an authoritative
        // name (SY.orchestrator@motherbee is the cross-hive control plane), must NOT pass the
        // protected-SYSTEM gate. The SAME frame from a directly-authenticated src is admitted.
        let memory_reader = Arc::new(Mutex::new(None::<MemoryRegionReader>));
        let msg = Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: Some("SY.orchestrator@motherbee".to_string()),
                dst: Destination::Unicast("SY.orchestrator@worker1".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some("SPAWN_NODE".to_string()),
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        };
        let direct = serialize_for_local_delivery(
            &memory_reader,
            "worker1",
            &msg,
            false,
            "SY.orchestrator@worker1",
        )
        .await
        .unwrap();
        assert!(
            direct.is_some(),
            "directly-authenticated orchestrator is allowed"
        );
        let vouched = serialize_for_local_delivery(
            &memory_reader,
            "worker1",
            &msg,
            true,
            "SY.orchestrator@worker1",
        )
        .await
        .unwrap();
        assert!(
            vouched.is_none(),
            "hub-vouched (via_hub) origin must be denied SYSTEM authority"
        );
    }

    #[test]
    fn canonical_wan_src_name_binds_role_to_bucket_hive() {
        // suffix already matches the authenticated bucket -> unchanged
        assert_eq!(
            canonical_wan_src_name("SY.admin@motherbee", "motherbee"),
            "SY.admin@motherbee"
        );
        // forged foreign suffix -> rewritten to the authenticated bucket
        assert_eq!(
            canonical_wan_src_name("SY.admin@motherbee", "attacker"),
            "SY.admin@attacker"
        );
        // bare name (no suffix) -> gets the bucket appended
        assert_eq!(
            canonical_wan_src_name("SY.orchestrator", "worker1"),
            "SY.orchestrator@worker1"
        );
        // only the FIRST '@' delimits the role (role labels never contain '@')
        assert_eq!(
            canonical_wan_src_name("IO.cloud@a@b", "worker1"),
            "IO.cloud@worker1"
        );
    }

    #[test]
    fn edge01_wan_canonicalization_defeats_cross_hive_name_forgery() {
        use system_policy::authorize_system;
        let receiving_hive = "ingress1";

        // ATTACK: a hive authenticated as "attacker" advertises a node NAMED
        // "SY.admin@motherbee" in its OWN LSA bucket. Pre-fix the WAN handler stamped the
        // raw name and authorize_system() authorized EDGE_OPEN_URL; post-fix the name is rebound to
        // the authenticated bucket, so the forged motherbee attribution is destroyed.
        let forged_admin = canonical_wan_src_name("SY.admin@motherbee", "attacker");
        assert_eq!(forged_admin, "SY.admin@attacker");
        assert!(
            !authorize_system("EDGE_OPEN_URL", Some(&forged_admin), receiving_hive),
            "forged SY.admin@motherbee from an attacker bucket must be denied"
        );

        // Same forgery against the orchestrators' control plane: the hive label is forced to the
        // true origin bucket, and rule (3) admits a mutation only from SY.orchestrator@motherbee.
        let forged_orch = canonical_wan_src_name("SY.orchestrator@motherbee", "attacker");
        assert_eq!(forged_orch, "SY.orchestrator@attacker");
        assert!(
            !authorize_system("SPAWN_NODE", Some(&forged_orch), receiving_hive),
            "forged SY.orchestrator@motherbee from an attacker bucket must be denied"
        );

        // PRESERVATION: the legitimate admin->edge command (SY.admin resolved in the motherbee
        // bucket over the direct motherbee<->ingress session) is unchanged and still allowed.
        let legit_admin = canonical_wan_src_name("SY.admin@motherbee", "motherbee");
        assert_eq!(legit_admin, "SY.admin@motherbee");
        assert!(authorize_system(
            "EDGE_OPEN_URL",
            Some(&legit_admin),
            receiving_hive
        ));

        // PRESERVATION: the motherbee's orchestrator (resolved in its own bucket) is still
        // authorized for a protected action on a spoke; a spoke's orchestrator is not.
        let legit_orch = canonical_wan_src_name("SY.orchestrator@motherbee", "motherbee");
        assert_eq!(legit_orch, "SY.orchestrator@motherbee");
        assert!(authorize_system(
            "SPAWN_NODE",
            Some(&legit_orch),
            receiving_hive
        ));
        let spoke_orch = canonical_wan_src_name("SY.orchestrator@worker1", "worker1");
        assert!(!authorize_system(
            "SPAWN_NODE",
            Some(&spoke_orch),
            receiving_hive
        ));
    }

    #[test]
    fn inject_identity_data_exposes_identity_and_aliases() {
        let now = now_epoch_ms();
        let ilk_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let canonical_ilk_id = Uuid::new_v4();
        let old_ilk_id = Uuid::new_v4();

        let mut skill_hashes = [[0u8; 32]; IDENTITY_DEFINITION_MAX_SKILLS];
        skill_hashes[0] = [0xcd; 32];
        skill_hashes[1] = [0xef; 32];
        let mut handbook_hashes = [[0u8; 32]; IDENTITY_DEFINITION_MAX_HANDBOOKS];
        handbook_hashes[0] = [0x12; 32];
        let mut ilk = IlkEntry {
            ilk_id: *ilk_id.as_bytes(),
            ilk_type: 1,
            registration_status: 2,
            flags: FLAG_ACTIVE,
            tenant_id: *tenant_id.as_bytes(),
            display_name: [0u8; 128],
            handler_node: [0u8; 128],
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0u8; 2],
            role_hash: [0xab; 32],
            skill_hashes,
            skill_count: 2,
            handbook_count: 1,
            handbook_hashes,
            created_at: now,
            updated_at: now,
            personality_hash: [0x34; 32],
            _reserved: [0u8; 8],
        };
        copy_bytes_with_len(&mut ilk.handler_node, "SY.frontdesk.gov@sandbox");

        let alias = IlkAliasEntry {
            old_ilk_id: *old_ilk_id.as_bytes(),
            canonical_ilk_id: *canonical_ilk_id.as_bytes(),
            expires_at: now + 60_000,
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 22],
        };

        let snapshot = IdentitySnapshot {
            header: IdentityHeaderSnapshot {
                tenant_count: 0,
                ilk_count: 1,
                ich_count: 0,
                ich_mapping_count: 0,
                vocabulary_count: 0,
                ilk_alias_count: 1,
                max_ilks: 0,
                max_tenants: 0,
                max_ichs: 0,
                max_ich_mappings: 0,
                max_vocabulary: 0,
                max_ilk_aliases: 0,
                heartbeat: now,
                updated_at: now,
            },
            tenants: Vec::new(),
            ilks: vec![ilk],
            ichs: Vec::new(),
            ich_mappings: Vec::new(),
            ilk_aliases: vec![alias],
            vocabulary: Vec::new(),
        };

        let mut root = serde_json::json!({ "foo": "bar" });
        inject_identity_data(&mut root, &snapshot);

        let identity = root
            .get("identity")
            .and_then(serde_json::Value::as_object)
            .expect("missing identity map");
        let key = format!("ilk:{ilk_id}");
        let record = identity
            .get(&key)
            .and_then(serde_json::Value::as_object)
            .expect("missing ilk record");
        let expected_tenant = format!("tnt:{tenant_id}");
        assert_eq!(
            record.get("tenant_id").and_then(serde_json::Value::as_str),
            Some(expected_tenant.as_str())
        );
        assert_eq!(
            record
                .get("registration_status")
                .and_then(serde_json::Value::as_str),
            Some("complete")
        );
        assert_eq!(
            record
                .get("handler_node")
                .and_then(serde_json::Value::as_str),
            Some("SY.frontdesk.gov@sandbox")
        );
        let expected_role_hash = "ab".repeat(32);
        let expected_skill_hash_a = "cd".repeat(32);
        let expected_skill_hash_b = "ef".repeat(32);
        let expected_handbook_hash = "12".repeat(32);
        assert_eq!(
            record.get("role_hash").and_then(serde_json::Value::as_str),
            Some(expected_role_hash.as_str())
        );
        assert_eq!(
            record
                .get("skill_hashes")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                }),
            Some(vec![
                expected_skill_hash_a.as_str(),
                expected_skill_hash_b.as_str()
            ])
        );
        assert_eq!(
            record
                .get("handbook_hashes")
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                }),
            Some(vec![expected_handbook_hash.as_str()])
        );
        let expected_personality_hash = "34".repeat(32);
        assert_eq!(
            record
                .get("personality_hash")
                .and_then(serde_json::Value::as_str),
            Some(expected_personality_hash.as_str())
        );
        assert!(record.get("roles").is_none());
        assert!(record.get("capabilities").is_none());

        let aliases = root
            .get("identity_aliases")
            .and_then(serde_json::Value::as_object)
            .expect("missing aliases map");
        let old_key = format!("ilk:{old_ilk_id}");
        let expected_canonical = format!("ilk:{canonical_ilk_id}");
        assert_eq!(
            aliases.get(&old_key).and_then(serde_json::Value::as_str),
            Some(expected_canonical.as_str())
        );
    }

    #[test]
    fn canonicalize_src_ilk_and_status_resolves_alias_and_status() {
        let now = now_epoch_ms();
        let canonical_ilk_id = Uuid::new_v4();
        let old_ilk_id = Uuid::new_v4();

        let ilk = IlkEntry {
            ilk_id: *canonical_ilk_id.as_bytes(),
            ilk_type: 0,
            registration_status: 0,
            flags: FLAG_ACTIVE,
            tenant_id: [0u8; 16],
            display_name: [0u8; 128],
            handler_node: [0u8; 128],
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0u8; 2],
            role_hash: [0u8; 32],
            skill_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_SKILLS],
            skill_count: 0,
            handbook_count: 0,
            handbook_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_HANDBOOKS],
            created_at: now,
            updated_at: now,
            personality_hash: [0u8; 32],
            _reserved: [0u8; 8],
        };
        let alias = IlkAliasEntry {
            old_ilk_id: *old_ilk_id.as_bytes(),
            canonical_ilk_id: *canonical_ilk_id.as_bytes(),
            expires_at: now + 60_000,
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 22],
        };
        let snapshot = IdentitySnapshot {
            header: IdentityHeaderSnapshot {
                tenant_count: 0,
                ilk_count: 1,
                ich_count: 0,
                ich_mapping_count: 0,
                vocabulary_count: 0,
                ilk_alias_count: 1,
                max_ilks: 0,
                max_tenants: 0,
                max_ichs: 0,
                max_ich_mappings: 0,
                max_vocabulary: 0,
                max_ilk_aliases: 0,
                heartbeat: now,
                updated_at: now,
            },
            tenants: Vec::new(),
            ilks: vec![ilk],
            ichs: Vec::new(),
            ich_mappings: Vec::new(),
            ilk_aliases: vec![alias],
            vocabulary: Vec::new(),
        };

        let old_src = format!("ilk:{old_ilk_id}");
        let (canonical, status) = canonicalize_src_ilk_and_status(&snapshot, &old_src, now);
        let expected_canonical = format!("ilk:{canonical_ilk_id}");
        assert_eq!(canonical.as_deref(), Some(expected_canonical.as_str()));
        assert_eq!(status.as_deref(), Some("temporary"));
    }

    #[test]
    fn src_ilk_meta_helpers_roundtrip() {
        let mut meta = Meta {
            msg_type: "user".to_string(),
            msg: None,
            src_ilk: None,
            scope: None,
            target: None,
            action: None,
            priority: None,
            context: None,
            ..Meta::default()
        };
        assert!(get_src_ilk_from_meta(&meta).is_none());
        set_src_ilk_in_meta(&mut meta, "ilk:11111111-1111-1111-1111-111111111111");
        assert_eq!(
            meta.src_ilk.as_deref(),
            Some("ilk:11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            get_src_ilk_from_meta(&meta).as_deref(),
            Some("ilk:11111111-1111-1111-1111-111111111111")
        );
    }

    #[test]
    fn canonical_thread_id_from_meta_reads_only_canonical_top_level() {
        let canonical = canonical_thread_id_from_meta(&Meta {
            msg_type: "user".to_string(),
            thread_id: Some("thread:canonical".to_string()),
            context: Some(serde_json::json!({ "thread_id": "thread:legacy" })),
            ..Meta::default()
        });
        assert_eq!(canonical.as_deref(), Some("thread:canonical"));

        let legacy = canonical_thread_id_from_meta(&Meta {
            msg_type: "user".to_string(),
            context: Some(serde_json::json!({ "thread_id": "thread:legacy" })),
            ..Meta::default()
        });
        assert_eq!(legacy, None);
    }

    fn message_with_src_ilk(src_ilk: &str) -> Message {
        Message {
            routing: Routing {
                src: Uuid::new_v4().to_string(),
                src_l2_name: None,
                dst: Destination::Resolve,
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                msg: None,
                src_ilk: Some(src_ilk.to_string()),
                scope: None,
                target: Some("dummy.target".to_string()),
                action: None,
                priority: None,
                context: None,
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        }
    }

    #[test]
    fn apply_identity_pre_resolve_keeps_alias_canonical_during_ttl() {
        let now = now_epoch_ms();
        let canonical_ilk_id = Uuid::new_v4();
        let old_ilk_id = Uuid::new_v4();

        let canonical_ilk = IlkEntry {
            ilk_id: *canonical_ilk_id.as_bytes(),
            ilk_type: 0,
            registration_status: 2,
            flags: FLAG_ACTIVE,
            tenant_id: [0u8; 16],
            display_name: [0u8; 128],
            handler_node: [0u8; 128],
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0u8; 2],
            role_hash: [0u8; 32],
            skill_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_SKILLS],
            skill_count: 0,
            handbook_count: 0,
            handbook_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_HANDBOOKS],
            created_at: now,
            updated_at: now,
            personality_hash: [0u8; 32],
            _reserved: [0u8; 8],
        };
        let alias = IlkAliasEntry {
            old_ilk_id: *old_ilk_id.as_bytes(),
            canonical_ilk_id: *canonical_ilk_id.as_bytes(),
            expires_at: now + 60_000,
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 22],
        };
        let snapshot = IdentitySnapshot {
            header: IdentityHeaderSnapshot {
                tenant_count: 0,
                ilk_count: 1,
                ich_count: 0,
                ich_mapping_count: 0,
                vocabulary_count: 0,
                ilk_alias_count: 1,
                max_ilks: 0,
                max_tenants: 0,
                max_ichs: 0,
                max_ich_mappings: 0,
                max_vocabulary: 0,
                max_ilk_aliases: 0,
                heartbeat: now,
                updated_at: now,
            },
            tenants: Vec::new(),
            ilks: vec![canonical_ilk],
            ichs: Vec::new(),
            ich_mappings: Vec::new(),
            ilk_aliases: vec![alias],
            vocabulary: Vec::new(),
        };

        let old_src = format!("ilk:{old_ilk_id}");
        let expected_canonical = format!("ilk:{canonical_ilk_id}");
        let mut msg = message_with_src_ilk(&old_src);
        let forced_target =
            apply_identity_pre_resolve(&mut msg, "SY.frontdesk.gov@sandbox", &snapshot, now);

        assert!(forced_target.is_none());
        assert_eq!(
            get_src_ilk_from_meta(&msg.meta).as_deref(),
            Some(expected_canonical.as_str())
        );
    }

    #[test]
    fn apply_identity_pre_resolve_ignores_alias_after_ttl() {
        let now = now_epoch_ms();
        let canonical_ilk_id = Uuid::new_v4();
        let old_ilk_id = Uuid::new_v4();

        let canonical_ilk = IlkEntry {
            ilk_id: *canonical_ilk_id.as_bytes(),
            ilk_type: 0,
            registration_status: 2,
            flags: FLAG_ACTIVE,
            tenant_id: [0u8; 16],
            display_name: [0u8; 128],
            handler_node: [0u8; 128],
            ich_offset: 0,
            ich_count: 0,
            _pad0: [0u8; 2],
            role_hash: [0u8; 32],
            skill_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_SKILLS],
            skill_count: 0,
            handbook_count: 0,
            handbook_hashes: [[0u8; 32]; IDENTITY_DEFINITION_MAX_HANDBOOKS],
            created_at: now,
            updated_at: now,
            personality_hash: [0u8; 32],
            _reserved: [0u8; 8],
        };
        let alias = IlkAliasEntry {
            old_ilk_id: *old_ilk_id.as_bytes(),
            canonical_ilk_id: *canonical_ilk_id.as_bytes(),
            expires_at: now.saturating_sub(1),
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 22],
        };
        let snapshot = IdentitySnapshot {
            header: IdentityHeaderSnapshot {
                tenant_count: 0,
                ilk_count: 1,
                ich_count: 0,
                ich_mapping_count: 0,
                vocabulary_count: 0,
                ilk_alias_count: 1,
                max_ilks: 0,
                max_tenants: 0,
                max_ichs: 0,
                max_ich_mappings: 0,
                max_vocabulary: 0,
                max_ilk_aliases: 0,
                heartbeat: now,
                updated_at: now,
            },
            tenants: Vec::new(),
            ilks: vec![canonical_ilk],
            ichs: Vec::new(),
            ich_mappings: Vec::new(),
            ilk_aliases: vec![alias],
            vocabulary: Vec::new(),
        };

        let old_src = format!("ilk:{old_ilk_id}");
        let mut msg = message_with_src_ilk(&old_src);
        let forced_target =
            apply_identity_pre_resolve(&mut msg, "SY.frontdesk.gov@sandbox", &snapshot, now);

        assert!(forced_target.is_none());
        assert_eq!(
            get_src_ilk_from_meta(&msg.meta).as_deref(),
            Some(old_src.as_str())
        );
    }

    /// The stamped src_l2_name is only as good as the UUID it is looked up by: a node's socket may
    /// only carry that node's own `routing.src`. Seen live on 8.x (2026-09-30): a probe node sent a
    /// protected NODE_STATUS_GET with the Admin's UUID and SY.config.routes answered it.
    #[test]
    fn a_node_cannot_send_as_another_node() {
        let node = Uuid::new_v4();
        let mut msg = Message {
            routing: Routing {
                src: node.to_string(),
                src_l2_name: None,
                dst: Destination::Unicast("SY.config.routes@motherbee".to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        };
        assert!(frame_is_from_connection(&msg, node));
        msg.routing.src = node.to_string().to_uppercase();
        assert!(
            frame_is_from_connection(&msg, node),
            "same UUID, other spelling"
        );
        for claimed in [
            Uuid::new_v4().to_string(),
            "SY.admin@motherbee".to_string(),
            String::new(),
        ] {
            msg.routing.src = claimed.clone();
            assert!(
                !frame_is_from_connection(&msg, node),
                "{claimed:?} must be refused"
            );
        }
    }

    /// Pins the CALLER: the node loop must check the frame before anything routes it.
    #[test]
    fn the_node_loop_checks_the_sender_before_routing() {
        let src = include_str!("mod.rs");
        let start = src.find("async fn handle_node(").expect("handle_node");
        let end = start + src[start..].find("\n}\n").expect("end of handle_node");
        let body = &src[start..end];
        let check = body
            .find("frame_is_from_connection(&msg, node_uuid)")
            .expect("handle_node checks routing.src against its connection");
        let route = body.find("handle_message(").expect("handle_node routes");
        assert!(check < route, "the check must come before routing");
    }

    // L2-LOOKUP-21: Spoof test — sender-supplied src_l2_name is overwritten by the router.
    #[tokio::test]
    async fn router_overwrites_spoofed_src_l2_name() {
        let sender_uuid = Uuid::new_v4();
        let receiver_uuid = Uuid::new_v4();

        let (sender_tx, _sender_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = nodes.lock().await;
            g.insert(
                sender_uuid,
                NodeHandle {
                    name: "WF.demo@motherbee".to_string(),
                    vpn_id: 0,
                    sender: sender_tx,
                    connected_at: 0,
                },
            );
            g.insert(
                receiver_uuid,
                NodeHandle {
                    name: "SY.orchestrator@motherbee".to_string(),
                    vpn_id: 0,
                    sender: receiver_tx,
                    connected_at: 0,
                },
            );
        }

        let msg = Message {
            routing: Routing {
                src: sender_uuid.to_string(),
                src_l2_name: Some("ATTACKER@evil".to_string()),
                dst: Destination::Unicast(receiver_uuid.to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        };

        handle_message(
            &msg,
            &nodes,
            &Arc::new(Mutex::new(Vec::<FibEntry>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerRouter>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerHandle>::new())),
            &Arc::new(Mutex::new(HashMap::<String, WanPeer>::new())),
            &Arc::new(Mutex::new(OpaResolver::new())),
            &Arc::new(Mutex::new(BroadcastCache::new())),
            &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
            &stream::StreamHandle::detached(),
            "motherbee",
            "SY.frontdesk.gov@motherbee",
            Uuid::new_v4(),
            false,
            &Arc::new(Mutex::new(None)),
            &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
            None,
        )
        .await
        .unwrap();

        let delivered = receiver_rx.try_recv().expect("message should be delivered");
        let delivered_msg: Message = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(
            delivered_msg.routing.src_l2_name.as_deref(),
            Some("WF.demo@motherbee"),
            "router must overwrite spoofed src_l2_name with the authenticated NodeHandle name"
        );
    }

    /// The A-22 check driven through a real socket (panel DTAP T-1): after its HELLO, a node that
    /// sends a frame claiming another node's UUID as routing.src has it dropped; its frames with
    /// its own UUID are delivered, stamped with the name it registered.
    #[tokio::test]
    async fn a_node_frame_claiming_another_nodes_uuid_is_dropped_at_the_socket() {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let id = Uuid::new_v4().simple().to_string();
        let shm_name = format!("/rt-a22-{}", &id[..8]);
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);
        let router_uuid = Uuid::new_v4();
        let shm = Arc::new(Mutex::new(
            RouterRegionWriter::open_or_create(
                &shm_name,
                router_uuid,
                "motherbee",
                "RT.gateway@motherbee",
                false,
            )
            .expect("create test shm"),
        ));

        // The node it targets, and the node whose UUID it claims.
        let receiver_uuid = Uuid::new_v4();
        let victim_uuid = Uuid::new_v4();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (victim_tx, _victim_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = nodes.lock().await;
            for (uuid, name, sender) in [
                (receiver_uuid, "SY.timer@motherbee", receiver_tx),
                (victim_uuid, "SY.admin@motherbee", victim_tx),
            ] {
                g.insert(
                    uuid,
                    NodeHandle {
                        name: name.to_string(),
                        vpn_id: 0,
                        sender,
                        connected_at: 0,
                    },
                );
            }
        }

        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let router = tokio::spawn(handle_node(
            server,
            shm,
            Arc::clone(&nodes),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(0)),
            Arc::new(Mutex::new(OpaResolver::new())),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(BroadcastCache::new())),
            Arc::new(Mutex::new(0)),
            Arc::new(Mutex::new(None)),
            stream::StreamHandle::detached(),
            router_uuid,
            "RT.gateway@motherbee",
            "motherbee",
            "SY.frontdesk.gov@motherbee",
            false,
        ));

        let probe_uuid = Uuid::new_v4();
        let frame = |src: Uuid, msg_type: &str, msg: Option<&str>, payload: serde_json::Value| {
            serde_json::to_vec(&Message {
                routing: Routing {
                    src: src.to_string(),
                    src_l2_name: None,
                    dst: if msg == Some(MSG_HELLO) {
                        Destination::Broadcast
                    } else {
                        Destination::Unicast(receiver_uuid.to_string())
                    },
                    ttl: 16,
                    trace_id: Uuid::new_v4().to_string(),
                },
                meta: Meta {
                    msg_type: msg_type.to_string(),
                    msg: msg.map(str::to_string),
                    ..Meta::default()
                },
                payload,
            })
            .expect("frame")
        };
        let hello = frame(
            probe_uuid,
            SYSTEM_KIND,
            Some(MSG_HELLO),
            serde_json::json!({ "uuid": probe_uuid.to_string(), "name": "AI.probe", "version": "1" }),
        );
        write_frame(&mut client, &hello).await.expect("hello");
        let announce = time::timeout(Duration::from_secs(5), read_frame(&mut client))
            .await
            .expect("announce in time")
            .expect("read")
            .expect("announce");
        assert!(!announce.is_empty());

        let claimed = frame(
            victim_uuid,
            "user",
            None,
            serde_json::json!({ "n": "claimed" }),
        );
        let own = frame(probe_uuid, "user", None, serde_json::json!({ "n": "own" }));
        write_frame(&mut client, &claimed)
            .await
            .expect("claimed frame");
        write_frame(&mut client, &own).await.expect("own frame");

        let delivered = time::timeout(Duration::from_secs(5), receiver_rx.recv())
            .await
            .expect("delivery in time")
            .expect("delivered");
        let delivered: Message = serde_json::from_slice(&delivered).expect("message");
        assert_eq!(
            delivered.payload["n"], "own",
            "the claimed frame must not be routed"
        );
        assert_eq!(delivered.routing.src, probe_uuid.to_string());
        assert_eq!(
            delivered.routing.src_l2_name.as_deref(),
            Some("AI.probe@motherbee")
        );
        assert!(
            time::timeout(Duration::from_millis(200), receiver_rx.recv())
                .await
                .is_err(),
            "nothing else may arrive"
        );

        drop(client);
        let _ = time::timeout(Duration::from_secs(5), router).await;
        cleanup(&shm_name);
    }

    /// A router SHM for one test, removed when dropped.
    struct TestShm {
        name: String,
        writer: Arc<Mutex<RouterRegionWriter>>,
    }

    impl TestShm {
        fn new(prefix: &str) -> Self {
            let name = format!("/{prefix}-{}", &Uuid::new_v4().simple().to_string()[..8]);
            unlink_test_shm(&name);
            let writer = RouterRegionWriter::open_or_create(
                &name,
                Uuid::new_v4(),
                "motherbee",
                "RT.gateway@motherbee",
                false,
            )
            .expect("create test shm");
            Self {
                name,
                writer: Arc::new(Mutex::new(writer)),
            }
        }
    }

    impl Drop for TestShm {
        fn drop(&mut self) {
            unlink_test_shm(&self.name);
        }
    }

    fn unlink_test_shm(name: &str) {
        if let Ok(cstr) = std::ffi::CString::new(name) {
            let _ = nix::sys::mman::shm_unlink(cstr.as_c_str());
        }
    }

    /// `handle_node` on `server` for a motherbee router with no peers, config or policy.
    fn spawn_node_connection(
        server: UnixStream,
        shm: &TestShm,
        nodes: &Arc<Mutex<HashMap<Uuid, NodeHandle>>>,
    ) -> tokio::task::JoinHandle<Result<(), RouterError>> {
        spawn_node_connection_with_stream(server, shm, nodes, stream::StreamHandle::detached())
    }

    fn spawn_node_connection_with_stream(
        server: UnixStream,
        shm: &TestShm,
        nodes: &Arc<Mutex<HashMap<Uuid, NodeHandle>>>,
        log_stream: stream::StreamHandle,
    ) -> tokio::task::JoinHandle<Result<(), RouterError>> {
        tokio::spawn(handle_node(
            server,
            Arc::clone(&shm.writer),
            Arc::clone(nodes),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(0)),
            Arc::new(Mutex::new(OpaResolver::new())),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(BroadcastCache::new())),
            Arc::new(Mutex::new(0)),
            Arc::new(Mutex::new(None)),
            log_stream,
            Uuid::new_v4(),
            "RT.gateway@motherbee",
            "motherbee",
            "SY.frontdesk.gov@motherbee",
            false,
        ))
    }

    fn test_frame(
        src: Uuid,
        msg_type: &str,
        msg: Option<&str>,
        dst: Destination,
        payload: serde_json::Value,
    ) -> Vec<u8> {
        serde_json::to_vec(&Message {
            routing: Routing {
                src: src.to_string(),
                src_l2_name: None,
                dst,
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: msg_type.to_string(),
                msg: msg.map(str::to_string),
                ..Meta::default()
            },
            payload,
        })
        .expect("frame")
    }

    /// Connects a node as `name` with `uuid`: Some(stream) once the router announced it, None if
    /// the router closed the connection instead.
    async fn hello(
        shm: &TestShm,
        nodes: &Arc<Mutex<HashMap<Uuid, NodeHandle>>>,
        uuid: Uuid,
        name: &str,
    ) -> (
        Option<UnixStream>,
        tokio::task::JoinHandle<Result<(), RouterError>>,
    ) {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let connection = spawn_node_connection(server, shm, nodes);
        let frame = test_frame(
            uuid,
            SYSTEM_KIND,
            Some(MSG_HELLO),
            Destination::Broadcast,
            serde_json::json!({ "uuid": uuid.to_string(), "name": name, "version": "1" }),
        );
        write_frame(&mut client, &frame).await.expect("hello");
        let answer = time::timeout(Duration::from_secs(5), read_frame(&mut client))
            .await
            .expect("an answer in time");
        match answer {
            Ok(Some(_announce)) => (Some(client), connection),
            _ => (None, connection),
        }
    }

    async fn holder_name(
        nodes: &Arc<Mutex<HashMap<Uuid, NodeHandle>>>,
        uuid: Uuid,
    ) -> Option<String> {
        nodes
            .lock()
            .await
            .get(&uuid)
            .map(|handle| handle.name.clone())
    }

    /// A-70 (stage 1b): the router copies a node's message to its stream under the name it
    /// registered, takes the node's `STREAM_APPEND` for a subject it may append, refuses the rest,
    /// and routes neither append to any node.
    #[tokio::test]
    async fn the_router_copies_turns_and_takes_appends_without_routing_them() {
        let shm = TestShm::new("rt-stream");
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        let receiver_uuid = Uuid::new_v4();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        nodes.lock().await.insert(
            receiver_uuid,
            NodeHandle {
                name: "SY.timer@motherbee".to_string(),
                vpn_id: 0,
                sender: receiver_tx,
                connected_at: 0,
            },
        );
        let (log_stream, observed) = stream::StreamHandle::observed();
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        let _connection = spawn_node_connection_with_stream(server, &shm, &nodes, log_stream);
        let uuid = Uuid::new_v4();
        let frame = test_frame(
            uuid,
            SYSTEM_KIND,
            Some(MSG_HELLO),
            Destination::Broadcast,
            serde_json::json!({ "uuid": uuid.to_string(), "name": "AI.sales", "version": "1" }),
        );
        write_frame(&mut client, &frame).await.expect("hello");
        time::timeout(Duration::from_secs(5), read_frame(&mut client))
            .await
            .expect("announce in time")
            .expect("read")
            .expect("announce");

        let to_receiver = Destination::Unicast(receiver_uuid.to_string());
        let usage = test_frame(
            uuid,
            SYSTEM_KIND,
            Some(fluxbee_sdk::stream::MSG_STREAM_APPEND),
            to_receiver.clone(),
            serde_json::json!({ "subject": "storage.usage", "payload": { "tokens": 12 } }),
        );
        let forged_turn = test_frame(
            uuid,
            SYSTEM_KIND,
            Some(fluxbee_sdk::stream::MSG_STREAM_APPEND),
            to_receiver.clone(),
            serde_json::json!({ "subject": "storage.turns", "payload": { "fake": true } }),
        );
        let turn = test_frame(
            uuid,
            "user",
            None,
            to_receiver,
            serde_json::json!({ "n": "hola" }),
        );
        for frame in [&usage, &forged_turn, &turn] {
            write_frame(&mut client, frame).await.expect("frame");
        }

        let delivered = time::timeout(Duration::from_secs(5), receiver_rx.recv())
            .await
            .expect("delivery in time")
            .expect("delivered");
        let delivered: Message = serde_json::from_slice(&delivered).expect("message");
        assert_eq!(
            delivered.payload["n"], "hola",
            "only the user message is routed"
        );
        assert!(
            time::timeout(Duration::from_millis(200), receiver_rx.recv())
                .await
                .is_err(),
            "no append reaches a node"
        );

        let appends = observed.drain();
        assert_eq!(appends.len(), 2, "{appends:?}");
        assert_eq!(appends[0].0, "storage.usage");
        assert_eq!(appends[0].1, "AI.sales@motherbee");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&appends[0].2).unwrap(),
            serde_json::json!({ "tokens": 12 })
        );
        assert_eq!(appends[1].0, "storage.turns");
        assert_eq!(appends[1].1, "AI.sales@motherbee");
        let copy: Message = serde_json::from_slice(&appends[1].2).expect("the turn copy");
        assert_eq!(copy.payload["n"], "hola");
        assert_eq!(
            copy.routing.src_l2_name.as_deref(),
            Some("AI.sales@motherbee")
        );
        assert_eq!(observed.refused(), 1, "a node never appends a turn");
    }

    /// A-70 stage 2a (§6.2): the agent's router hands its cognition both directions of the agent's
    /// communication: what reaches the agent, copied at delivery (owner = that agent), and what the
    /// agent sends, copied at entry. A message without a thread, or between non-agents, is not
    /// copied; a package the sender set never reaches the agent.
    #[tokio::test]
    async fn the_agents_router_copies_both_directions_for_its_cognition() {
        let shm = TestShm::new("rt-cog");
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        let agent_uuid = Uuid::new_v4();
        let web_uuid = Uuid::new_v4();
        let (agent_tx, mut agent_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (web_tx, mut web_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        {
            let mut guard = nodes.lock().await;
            for (uuid, name, sender) in [
                (agent_uuid, "AI.sales@motherbee", agent_tx),
                (web_uuid, "IO.web@motherbee", web_tx),
            ] {
                guard.insert(
                    uuid,
                    NodeHandle {
                        name: name.to_string(),
                        vpn_id: 0,
                        sender,
                        connected_at: 0,
                    },
                );
            }
        }
        let (log_stream, observed) = stream::StreamHandle::observed();
        let threaded = |src: Uuid, dst: Uuid, thread: Option<&str>, n: u32| {
            let mut msg: Message = serde_json::from_slice(&test_frame(
                src,
                "user",
                None,
                Destination::Unicast(dst.to_string()),
                serde_json::json!({ "n": n }),
            ))
            .unwrap();
            msg.meta.thread_id = thread.map(str::to_string);
            msg.meta.memory_package = Some(fluxbee_sdk::protocol::MemoryPackage {
                package_version: 2,
                thread_id: "forged".to_string(),
                dominant_context: None,
                dominant_reason: None,
                contexts: Vec::new(),
                reasons: Vec::new(),
                memories: Vec::new(),
                episodes: Vec::new(),
                truncated: None,
            });
            serde_json::to_vec(&msg).unwrap()
        };
        let connect = |name: &'static str| {
            let shm = &shm;
            let nodes = &nodes;
            let log_stream = log_stream.clone();
            async move {
                let uuid = Uuid::new_v4();
                let (mut client, server) = UnixStream::pair().expect("socket pair");
                let _connection = spawn_node_connection_with_stream(server, shm, nodes, log_stream);
                let hello = test_frame(
                    uuid,
                    SYSTEM_KIND,
                    Some(MSG_HELLO),
                    Destination::Broadcast,
                    serde_json::json!({ "uuid": uuid.to_string(), "name": name, "version": "1" }),
                );
                write_frame(&mut client, &hello).await.expect("hello");
                time::timeout(Duration::from_secs(5), read_frame(&mut client))
                    .await
                    .expect("announce in time")
                    .expect("read")
                    .expect("announce");
                (uuid, client)
            }
        };

        // A user (through an IO node) to the agent: copied at delivery, for the agent.
        let (user_uuid, mut user) = connect("IO.wapp").await;
        write_frame(
            &mut user,
            &threaded(user_uuid, agent_uuid, Some("thread:t1"), 1),
        )
        .await
        .expect("to the agent");
        // Without a thread, and to a node that is not an agent: no cognition copy.
        write_frame(&mut user, &threaded(user_uuid, agent_uuid, None, 2))
            .await
            .expect("no thread");
        write_frame(
            &mut user,
            &threaded(user_uuid, web_uuid, Some("thread:t1"), 3),
        )
        .await
        .expect("to a non-agent");
        for _ in 0..2 {
            let got = time::timeout(Duration::from_secs(5), agent_rx.recv())
                .await
                .expect("delivery in time")
                .expect("delivered");
            let got: Message = serde_json::from_slice(&got).unwrap();
            assert!(
                got.meta.memory_package.is_none(),
                "the sender's package is dropped"
            );
        }
        let got = time::timeout(Duration::from_secs(5), web_rx.recv())
            .await
            .expect("delivery in time")
            .expect("delivered");
        let got: Message = serde_json::from_slice(&got).unwrap();
        assert!(got.meta.memory_package.is_none());

        // The agent replies: copied at entry, for the agent.
        let (bot_uuid, mut bot) = connect("AI.bot").await;
        write_frame(
            &mut bot,
            &threaded(bot_uuid, web_uuid, Some("thread:t2"), 4),
        )
        .await
        .expect("from an agent");
        time::timeout(Duration::from_secs(5), web_rx.recv())
            .await
            .expect("delivery in time")
            .expect("delivered");

        let cognition: Vec<(String, String, Message)> = observed
            .drain()
            .into_iter()
            .filter(|(subject, _, _)| subject == fluxbee_sdk::stream::SUBJECT_COGNITION_TURNS)
            .map(|(subject, producer, payload)| {
                (subject, producer, serde_json::from_slice(&payload).unwrap())
            })
            .collect();
        assert_eq!(cognition.len(), 2, "{cognition:?}");
        assert_eq!(cognition[0].1, "AI.sales@motherbee");
        assert_eq!(cognition[0].2.payload["n"], 1);
        assert_eq!(
            cognition[0].2.routing.src_l2_name.as_deref(),
            Some("IO.wapp@motherbee")
        );
        assert!(cognition[0].2.meta.memory_package.is_none());
        assert_eq!(cognition[1].1, "AI.bot@motherbee");
        assert_eq!(cognition[1].2.payload["n"], 4);
    }

    #[test]
    fn an_agent_gets_the_package_of_its_own_communication() {
        let package = |label: &str| fluxbee_sdk::protocol::MemoryPackage {
            package_version: 2,
            thread_id: "thread:t".to_string(),
            dominant_context: Some(fluxbee_sdk::protocol::MemoryContextSummary {
                context_id: format!("context:{label}"),
                label: label.to_string(),
                weight: 1.0,
            }),
            dominant_reason: None,
            contexts: Vec::new(),
            reasons: Vec::new(),
            memories: Vec::new(),
            episodes: Vec::new(),
            truncated: None,
        };
        let entry = |thread: &str, node: &str, label: &str| crate::shm::MemoryShmThreadEntry {
            thread_id: thread.to_string(),
            agent_ilk: format!("ilk:{node}"),
            agent_node: node.to_string(),
            package: package(label),
        };
        let memory = MemoryShmSnapshot {
            schema_version: 2,
            updated_at: 1,
            threads: vec![
                entry("thread:t", "AI.a@motherbee", "a"),
                entry("thread:t", "AI.b@motherbee", "b"),
                entry("thread:u", "AI.b@motherbee", "u"),
            ],
        };
        let mut msg: Message = serde_json::from_slice(&test_frame(
            Uuid::new_v4(),
            "user",
            None,
            Destination::Resolve,
            serde_json::json!({}),
        ))
        .unwrap();
        msg.meta.thread_id = Some("thread:t".to_string());
        let label = |node: &str| {
            package_for(Some(&memory), &msg, node)
                .and_then(|package| package.dominant_context)
                .map(|context| context.label)
        };
        assert_eq!(label("AI.b@motherbee").as_deref(), Some("b"));
        assert_eq!(label("AI.a@motherbee").as_deref(), Some("a"));
        assert_eq!(label("AI.c@motherbee"), None);
        assert!(is_agent_node("ai.sales@worker1") && !is_agent_node("SY.frontdesk.gov@motherbee"));
    }

    /// A-44: a second live process with a node's UUID does not take its route. Its HELLO waits
    /// the grace for the first connection to go, and is refused while it stays.
    #[tokio::test]
    async fn a_hello_with_a_uuid_that_is_still_connected_is_refused() {
        let shm = TestShm::new("rt-dup");
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        let uuid = Uuid::new_v4();
        let (first, _first_conn) = hello(&shm, &nodes, uuid, "AI.first").await;
        assert!(first.is_some(), "the first connection is announced");

        let started = time::Instant::now();
        let (second, second_conn) = hello(&shm, &nodes, uuid, "AI.second").await;
        assert!(second.is_none(), "the second connection gets no ANNOUNCE");
        assert!(
            started.elapsed() >= DUPLICATE_HELLO_GRACE,
            "it waited the grace first"
        );
        let refused = time::timeout(Duration::from_secs(5), second_conn)
            .await
            .expect("the refused connection ends")
            .expect("join");
        assert!(refused.is_err());
        assert_eq!(
            holder_name(&nodes, uuid).await.as_deref(),
            Some("AI.first@motherbee"),
            "the first connection keeps the route"
        );
        drop(first);
    }

    /// A node that reconnects right after its connection closed (a restart, the Go SDK's second
    /// HELLO) is accepted: its HELLO waits until the router cleaned up the old connection.
    #[tokio::test]
    async fn a_node_reconnecting_right_after_closing_is_accepted() {
        let shm = TestShm::new("rt-recon");
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        let uuid = Uuid::new_v4();
        let (first, first_conn) = hello(&shm, &nodes, uuid, "AI.node").await;
        drop(first.expect("announced"));
        let (second, _second_conn) = hello(&shm, &nodes, uuid, "AI.node").await;
        assert!(second.is_some(), "the reconnect is announced");
        assert!(time::timeout(Duration::from_secs(5), first_conn)
            .await
            .expect("the first connection ended")
            .expect("join")
            .is_ok());
        assert_eq!(
            holder_name(&nodes, uuid).await.as_deref(),
            Some("AI.node@motherbee")
        );
    }

    /// A-44: a connection that ends in a read error is cleaned up like one that ends in EOF.
    /// Before, the node stayed registered with its writer alive.
    #[tokio::test]
    async fn a_connection_that_ends_in_a_read_error_is_cleaned_up() {
        use tokio::io::AsyncWriteExt;

        let shm = TestShm::new("rt-rderr");
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        let uuid = Uuid::new_v4();
        let (client, connection) = hello(&shm, &nodes, uuid, "AI.node").await;
        let mut client = client.expect("announced");
        // A frame header for 100 bytes, then 10 bytes and the close: a partial frame.
        client
            .write_all(&100u32.to_be_bytes())
            .await
            .expect("header");
        client
            .write_all(&[0u8; 10])
            .await
            .expect("part of the body");
        drop(client);

        let ended = time::timeout(Duration::from_secs(5), connection)
            .await
            .expect("the connection ends")
            .expect("join");
        assert!(ended.is_err(), "a partial frame is a read error");
        assert_eq!(
            holder_name(&nodes, uuid).await,
            None,
            "the node is unregistered"
        );
        let snapshot = shm.writer.lock().await.read_snapshot().expect("snapshot");
        assert!(
            snapshot
                .nodes
                .iter()
                .all(|node| node.uuid != *uuid.as_bytes()),
            "and its SHM slot is freed"
        );
    }

    /// A message the router cannot handle is skipped; the connection stays up.
    #[tokio::test]
    async fn a_message_the_router_cannot_handle_does_not_end_the_connection() {
        let shm = TestShm::new("rt-badmsg");
        let receiver_uuid = Uuid::new_v4();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        nodes.lock().await.insert(
            receiver_uuid,
            NodeHandle {
                name: "SY.timer@motherbee".to_string(),
                vpn_id: 0,
                sender: receiver_tx,
                connected_at: 0,
            },
        );
        let uuid = Uuid::new_v4();
        let (client, connection) = hello(&shm, &nodes, uuid, "AI.node").await;
        let mut client = client.expect("announced");
        // An OPA_RELOAD whose payload is not one ended the connection before.
        let bad = test_frame(
            uuid,
            SYSTEM_KIND,
            Some(MSG_OPA_RELOAD),
            Destination::Broadcast,
            serde_json::json!({ "not": "an opa reload" }),
        );
        write_frame(&mut client, &bad).await.expect("bad frame");
        let good = test_frame(
            uuid,
            "user",
            None,
            Destination::Unicast(receiver_uuid.to_string()),
            serde_json::json!({ "n": "after" }),
        );
        write_frame(&mut client, &good).await.expect("good frame");

        let delivered = time::timeout(Duration::from_secs(5), receiver_rx.recv())
            .await
            .expect("delivery in time")
            .expect("delivered");
        let delivered: Message = serde_json::from_slice(&delivered).expect("message");
        assert_eq!(delivered.payload["n"], "after");
        assert!(!connection.is_finished(), "the connection is still up");
        drop(client);
    }

    /// The admin's OPA sync notice (panel DTAP T-8): a broadcast with meta.target
    /// `SY.opa.rules@*` reaches SY.opa.rules and no other local node; the same notice from any
    /// sender but the primary admin reaches nobody (CONFIG_CHANGED is protected).
    #[tokio::test]
    async fn the_opa_sync_notice_reaches_only_opa_rules_and_only_from_the_primary_admin() {
        for (sender_name, reaches_opa_rules) in [
            ("SY.admin@motherbee", true),
            ("SY.orchestrator@motherbee", false),
        ] {
            let sender_uuid = Uuid::new_v4();
            let (sender_tx, _sender_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
            let mut inboxes = Vec::new();
            {
                let mut g = nodes.lock().await;
                g.insert(
                    sender_uuid,
                    NodeHandle {
                        name: sender_name.to_string(),
                        vpn_id: 0,
                        sender: sender_tx,
                        connected_at: 0,
                    },
                );
                for name in [
                    "SY.opa.rules@motherbee",
                    "AI.chat@motherbee",
                    "SY.timer@motherbee",
                ] {
                    let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
                    g.insert(
                        Uuid::new_v4(),
                        NodeHandle {
                            name: name.to_string(),
                            vpn_id: 0,
                            sender: tx,
                            connected_at: 0,
                        },
                    );
                    inboxes.push((name, rx));
                }
            }
            let notice = Message {
                routing: Routing {
                    src: sender_uuid.to_string(),
                    src_l2_name: None,
                    dst: Destination::Broadcast,
                    ttl: 16,
                    trace_id: Uuid::new_v4().to_string(),
                },
                meta: Meta {
                    msg_type: SYSTEM_KIND.to_string(),
                    msg: Some("CONFIG_CHANGED".to_string()),
                    scope: Some("global".to_string()),
                    target: Some("SY.opa.rules@*".to_string()),
                    ..Meta::default()
                },
                payload: serde_json::json!({
                    "subsystem": "opa", "action": "sync", "version": 0,
                    "config": { "hash": "" }
                }),
            };
            handle_message(
                &notice,
                &nodes,
                &Arc::new(Mutex::new(Vec::<FibEntry>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerRouter>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerHandle>::new())),
                &Arc::new(Mutex::new(HashMap::<String, WanPeer>::new())),
                &Arc::new(Mutex::new(OpaResolver::new())),
                &Arc::new(Mutex::new(BroadcastCache::new())),
                &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
                &stream::StreamHandle::detached(),
                "motherbee",
                "SY.frontdesk.gov@motherbee",
                Uuid::new_v4(),
                false,
                &Arc::new(Mutex::new(None)),
                &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
                None,
            )
            .await
            .unwrap();
            for (name, mut inbox) in inboxes {
                let delivered = inbox.try_recv().is_ok();
                let want = reaches_opa_rules && name == "SY.opa.rules@motherbee";
                assert_eq!(delivered, want, "{sender_name} -> {name}");
            }
        }
    }

    /// A-78 through `handle_message`: SY.vault's broadcast reaches every node but the edge, its
    /// unicast reaches the edge, and the same unicast from another node, or another verb from the
    /// vault, does not.
    #[tokio::test]
    async fn the_vault_notice_reaches_the_edge_as_a_unicast_and_never_as_a_broadcast() {
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        let mut uuids = HashMap::new();
        let mut inboxes = HashMap::new();
        for name in [
            "SY.vault@motherbee",
            "SY.storage@motherbee",
            "SY.edge@motherbee",
            "AI.chat@motherbee",
        ] {
            let uuid = Uuid::new_v4();
            let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
            nodes.lock().await.insert(
                uuid,
                NodeHandle {
                    name: name.to_string(),
                    vpn_id: if name.starts_with("AI.") { 3 } else { 0 },
                    sender: tx,
                    connected_at: 0,
                },
            );
            uuids.insert(name, uuid);
            inboxes.insert(name, rx);
        }
        let route = |src: &str, dst: Destination, msg: &str| Message {
            routing: Routing {
                src: uuids[src].to_string(),
                src_l2_name: None,
                dst,
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(msg.to_string()),
                scope: Some(SCOPE_GLOBAL.to_string()),
                ..Meta::default()
            },
            payload: serde_json::json!({"op": "put", "key": "edge_tls"}),
        };
        let edge = Destination::Unicast(uuids["SY.edge@motherbee"].to_string());
        let cases = [
            (
                route(
                    "SY.vault@motherbee",
                    Destination::Broadcast,
                    MSG_VAULT_SECRET_CHANGED,
                ),
                vec!["SY.storage@motherbee", "AI.chat@motherbee"],
            ),
            (
                route("SY.vault@motherbee", edge.clone(), MSG_VAULT_SECRET_CHANGED),
                vec!["SY.edge@motherbee"],
            ),
            (
                route(
                    "SY.storage@motherbee",
                    edge.clone(),
                    MSG_VAULT_SECRET_CHANGED,
                ),
                vec![],
            ),
            (
                route("SY.vault@motherbee", edge, "VAULT_LIST_RESPONSE"),
                vec![],
            ),
        ];
        for (message, receivers) in cases {
            handle_message(
                &message,
                &nodes,
                &Arc::new(Mutex::new(Vec::<FibEntry>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerRouter>::new())),
                &Arc::new(Mutex::new(HashMap::<Uuid, PeerHandle>::new())),
                &Arc::new(Mutex::new(HashMap::<String, WanPeer>::new())),
                &Arc::new(Mutex::new(OpaResolver::new())),
                &Arc::new(Mutex::new(BroadcastCache::new())),
                &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
                &stream::StreamHandle::detached(),
                "motherbee",
                "SY.frontdesk.gov@motherbee",
                Uuid::new_v4(),
                false,
                &Arc::new(Mutex::new(None)),
                &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
                None,
            )
            .await
            .unwrap();
            let sender = nodes.lock().await[&Uuid::parse_str(&message.routing.src).unwrap()]
                .name
                .clone();
            for (name, inbox) in inboxes.iter_mut() {
                let mut delivered = false;
                while let Ok(frame) = inbox.try_recv() {
                    let frame: Message = serde_json::from_slice(&frame).unwrap();
                    if frame.meta.msg.as_deref() == message.meta.msg.as_deref() {
                        delivered = true;
                    }
                }
                assert_eq!(
                    delivered,
                    receivers.contains(name),
                    "{sender} {:?} {:?} -> {name}",
                    message.routing.dst,
                    message.meta.msg
                );
            }
        }
    }

    // L2-LOOKUP-22: Local end-to-end — receiver observes correct src UUID and src_l2_name.
    #[tokio::test]
    async fn local_delivery_stamps_correct_src_uuid_and_l2_name() {
        let sender_uuid = Uuid::new_v4();
        let receiver_uuid = Uuid::new_v4();

        let (sender_tx, _sender_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = nodes.lock().await;
            g.insert(
                sender_uuid,
                NodeHandle {
                    name: "IO.webchat@motherbee".to_string(),
                    vpn_id: 0,
                    sender: sender_tx,
                    connected_at: 0,
                },
            );
            g.insert(
                receiver_uuid,
                NodeHandle {
                    name: "WF.pipeline@motherbee".to_string(),
                    vpn_id: 0,
                    sender: receiver_tx,
                    connected_at: 0,
                },
            );
        }

        let msg = Message {
            routing: Routing {
                src: sender_uuid.to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(receiver_uuid.to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                ..Meta::default()
            },
            payload: serde_json::json!({"hello": "world"}),
        };

        handle_message(
            &msg,
            &nodes,
            &Arc::new(Mutex::new(Vec::<FibEntry>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerNode>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerRouter>::new())),
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerHandle>::new())),
            &Arc::new(Mutex::new(HashMap::<String, WanPeer>::new())),
            &Arc::new(Mutex::new(OpaResolver::new())),
            &Arc::new(Mutex::new(BroadcastCache::new())),
            &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
            &stream::StreamHandle::detached(),
            "motherbee",
            "SY.frontdesk.gov@motherbee",
            Uuid::new_v4(),
            false,
            &Arc::new(Mutex::new(None)),
            &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
            None,
        )
        .await
        .unwrap();

        let delivered = receiver_rx.try_recv().expect("message should be delivered");
        let delivered_msg: Message = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(
            delivered_msg.routing.src,
            sender_uuid.to_string(),
            "delivered message must carry the original sender UUID"
        );
        assert_eq!(
            delivered_msg.routing.src_l2_name.as_deref(),
            Some("IO.webchat@motherbee"),
            "delivered message must carry the L2 name stamped by the router"
        );
    }

    // L2-LOOKUP-23: Inter-router end-to-end — src_l2_name is stamped from peer_nodes,
    // preserving the real origin even if the forwarding router sent a different value.
    #[tokio::test]
    async fn inter_router_delivery_stamps_src_l2_name_from_peer_nodes() {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let id = Uuid::new_v4().simple().to_string();
        let shm_name = format!("/rt-l2-{}", &id[..8]);
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);

        let peer_sender_uuid = Uuid::new_v4();
        let peer_router_uuid = Uuid::new_v4();
        let local_receiver_uuid = Uuid::new_v4();

        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();

        // Local node that will receive the forwarded message.
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = nodes.lock().await;
            g.insert(
                local_receiver_uuid,
                NodeHandle {
                    name: "SY.orchestrator@motherbee".to_string(),
                    vpn_id: 0,
                    sender: receiver_tx,
                    connected_at: 0,
                },
            );
        }

        // peer_nodes maps the remote sender UUID to its L2 name as known by this router.
        let peer_nodes: Arc<Mutex<HashMap<Uuid, PeerNode>>> = Arc::new(Mutex::new(HashMap::new()));
        {
            let mut g = peer_nodes.lock().await;
            g.insert(
                peer_sender_uuid,
                PeerNode {
                    uuid: peer_sender_uuid,
                    name: "WF.external@worker-hive".to_string(),
                    vpn_id: 0,
                    router_uuid: peer_router_uuid,
                },
            );
        }

        // peer_uuid is registered in peers so TTL/unreachable replies have somewhere to go.
        let (peer_tx, _peer_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let peers: Arc<Mutex<HashMap<Uuid, PeerHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        peers
            .lock()
            .await
            .insert(peer_router_uuid, PeerHandle { sender: peer_tx });

        // Message arrives from the remote peer with no src_l2_name set (router will stamp it).
        let msg = Message {
            routing: Routing {
                src: peer_sender_uuid.to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(local_receiver_uuid.to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        };

        let router_uuid = Uuid::new_v4();
        let shm_writer = RouterRegionWriter::open_or_create(
            &shm_name,
            router_uuid,
            "motherbee",
            "RT.main@motherbee",
            false,
        )
        .expect("create test shm");
        let shm = Arc::new(Mutex::new(shm_writer));

        handle_peer_message(
            &msg,
            &peer_router_uuid,
            &peers,
            &peer_nodes,
            &Arc::new(Mutex::new(HashMap::<Uuid, PeerRouter>::new())),
            &nodes,
            &Arc::new(Mutex::new(HashMap::<String, WanPeer>::new())),
            &Arc::new(Mutex::new(None::<LsaRegionReader>)),
            &Arc::new(Mutex::new(Vec::<StaticRoute>::new())),
            &shm,
            &Arc::new(Mutex::new(OpaResolver::new())),
            &Arc::new(Mutex::new(None::<OpaRegionReader>)),
            &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
            "motherbee",
            "SY.frontdesk.gov@motherbee",
            &Arc::new(Mutex::new(Vec::<FibEntry>::new())),
            router_uuid,
            false,
            &Arc::new(Mutex::new(None)),
            &Arc::new(Mutex::new(HashMap::<Uuid, ReachabilityEntry>::new())),
            &stream::StreamHandle::detached(),
        )
        .await
        .unwrap();

        cleanup(&shm_name);

        let delivered = receiver_rx
            .try_recv()
            .expect("message should be delivered to local node");
        let delivered_msg: Message = serde_json::from_slice(&delivered).unwrap();
        assert_eq!(
            delivered_msg.routing.src,
            peer_sender_uuid.to_string(),
            "delivered message must preserve the original sender UUID from the remote hive"
        );
        assert_eq!(
            delivered_msg.routing.src_l2_name.as_deref(),
            Some("WF.external@worker-hive"),
            "delivered message must carry the L2 name from this router's peer_nodes, not whatever the remote router sent"
        );
    }

    #[test]
    fn rt_names_belong_to_the_routers_own_process() {
        let own = 4242u32;
        assert!(rt_name_allowed(
            "RT.gateway.stream@worker1",
            Some(4242),
            own
        ));
        assert!(rt_name_allowed(
            "rt.gateway.stream@worker1",
            Some(4242),
            own
        ));
        assert!(!rt_name_allowed(
            "RT.gateway.stream@worker1",
            Some(4243),
            own
        ));
        assert!(!rt_name_allowed("RT.x.stream@worker1", None, own));
        assert!(!rt_name_allowed("rt.edge.buffer@motherbee", Some(1), own));
        // Every other name is untouched, whoever opened the socket.
        assert!(rt_name_allowed("SY.storage@motherbee", None, own));
        assert!(rt_name_allowed("AI.sales@worker1", Some(1), own));
        assert!(rt_name_allowed("RTX.thing@worker1", Some(1), own));
    }

    #[tokio::test]
    async fn an_oversize_frame_is_skipped_and_the_link_goes_on() {
        let (writer, mut reader) = tokio::io::duplex(4 * MAX_FRAME_SIZE);
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let task = tokio::spawn(write_link_frames(
            writer,
            rx,
            "node",
            Arc::new(std::sync::OnceLock::from(
                "SY.storage@motherbee".to_string(),
            )),
        ));
        tx.send(vec![b'x'; MAX_FRAME_SIZE + 1]).unwrap();
        tx.send(b"next".to_vec()).unwrap();
        drop(tx);
        task.await.unwrap();
        let frame = read_frame(&mut reader)
            .await
            .unwrap()
            .expect("a frame after the skip");
        assert_eq!(frame, b"next".to_vec());
        assert!(
            read_frame(&mut reader).await.unwrap().is_none(),
            "nothing else was written"
        );
    }

    /// An LSA snapshot with one remote hive and one node of it.
    fn lsa_with_remote_node(hive: &str, node_uuid: Uuid, node_name: &str) -> LsaSnapshot {
        let mut hive_id = [0u8; 64];
        hive_id[..hive.len()].copy_from_slice(hive.as_bytes());
        let mut name = [0u8; 256];
        name[..node_name.len()].copy_from_slice(node_name.as_bytes());
        LsaSnapshot {
            header: crate::shm::LsaHeaderSnapshot {
                hive_count: 1,
                total_node_count: 1,
                total_route_count: 0,
                total_vpn_count: 0,
                total_tap_count: 0,
                heartbeat: 0,
            },
            hives: vec![crate::shm::RemoteHiveEntry {
                hive_id,
                hive_id_len: hive.len() as u16,
                router_uuid: [0u8; 16],
                router_name: [0u8; 64],
                router_name_len: 0,
                last_lsa_seq: 1,
                last_updated: 0,
                flags: 0,
                node_count: 1,
                route_count: 0,
                vpn_count: 0,
                tap_count: 0,
            }],
            nodes: vec![crate::shm::RemoteNodeEntry {
                uuid: *node_uuid.as_bytes(),
                name,
                name_len: node_name.len() as u16,
                vpn_id: 0,
                hive_index: 0,
                flags: 0,
                _reserved: [0u8; 6],
            }],
            routes: Vec::new(),
            vpns: Vec::new(),
            taps: Vec::new(),
        }
    }

    #[tokio::test]
    async fn a_hello_from_the_routers_own_process_may_take_an_rt_name() {
        // The plumbing the HELLO check relies on: a socket's peer is this very process.
        let (a, _b) = tokio::net::UnixStream::pair().unwrap();
        let pid = a.peer_cred().unwrap().pid();
        assert!(rt_name_allowed(
            "RT.gateway.stream@motherbee",
            pid,
            std::process::id()
        ));
    }

    /// The router's sockets are created 0600, replacing whatever was at the path, and a
    /// connection is served only when its peer is the router's own user: root in production, this
    /// test's user here (docs/control-path-hardening-v1.md §5).
    #[tokio::test]
    async fn the_routers_sockets_are_0600_and_serve_only_its_own_user() {
        let dir = std::env::temp_dir().join(format!(
            "fbsock-{}",
            &Uuid::new_v4().simple().to_string()[..8]
        ));
        let path = dir.join("router.sock");
        fs::create_dir_all(&dir).expect("socket dir");
        fs::write(&path, b"stale").expect("a stale file at the path");

        let listener = listen_local_socket(&path).expect("listen");
        let mode = fs::metadata(&path).expect("socket").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let _client = UnixStream::connect(&path).await.expect("connect");
        let (accepted, _) = listener.accept().await.expect("accept");
        // SAFETY: geteuid has no preconditions and cannot fail.
        let own_uid = unsafe { nix::libc::geteuid() };
        assert!(local_peer_admitted(&accepted, own_uid));
        assert!(
            !local_peer_admitted(&accepted, own_uid.wrapping_add(1)),
            "a peer of another user is refused"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Pins the callers: both accept loops of `Router::run` check the peer before they serve it,
    /// and both sockets are made by `listen_local_socket`.
    #[test]
    fn both_accept_loops_check_the_peer_before_serving_it() {
        let src = include_str!("mod.rs");
        let start = src
            .find("    pub async fn run(&self)")
            .expect("Router::run");
        let end = start
            + src[start..]
                .find("\n    }\n}\n")
                .expect("end of Router::run");
        let body = &src[start..end];
        assert_eq!(body.matches("listen_local_socket(").count(), 2);
        for (accept, serve) in [
            ("irp_listener.accept()", "handle_peer_incoming("),
            ("listener.accept().await?", "handle_node("),
        ] {
            let at = body.find(accept).expect(accept);
            let check = body[at..]
                .find("local_peer_admitted(&stream, own_uid)")
                .expect("the peer is checked");
            let served = body[at..].find(serve).expect(serve);
            assert!(
                check < served,
                "{accept}: the check must come before {serve}"
            );
        }
    }

    async fn deliver_from_peer(peer_is_gateway: bool) -> Option<Message> {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let id = Uuid::new_v4().simple().to_string();
        let shm_name = format!("/rt-gw-{}", &id[..8]);
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);

        let remote_sender_uuid = Uuid::new_v4();
        let gateway_uuid = Uuid::new_v4();
        let local_receiver_uuid = Uuid::new_v4();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let nodes: Arc<Mutex<HashMap<Uuid, NodeHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        nodes.lock().await.insert(
            local_receiver_uuid,
            NodeHandle {
                name: "RT.switch1.stream@worker1".to_string(),
                vpn_id: 0,
                sender: receiver_tx,
                connected_at: 0,
            },
        );
        // The sender is a node of another hive: not one of this router's siblings' nodes, but in
        // the LSA this router reads.
        let peer_nodes: Arc<Mutex<HashMap<Uuid, PeerNode>>> = Arc::new(Mutex::new(HashMap::new()));
        let lsa_snapshot = Arc::new(Mutex::new(Some(lsa_with_remote_node(
            "motherbee",
            remote_sender_uuid,
            "SY.storage@motherbee",
        ))));
        let peer_routers: Arc<Mutex<HashMap<Uuid, PeerRouter>>> =
            Arc::new(Mutex::new(HashMap::new()));
        peer_routers.lock().await.insert(
            gateway_uuid,
            PeerRouter {
                uuid: gateway_uuid,
                is_gateway: peer_is_gateway,
            },
        );
        let reachability: Arc<Mutex<HashMap<Uuid, ReachabilityEntry>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (peer_tx, _peer_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let peers: Arc<Mutex<HashMap<Uuid, PeerHandle>>> = Arc::new(Mutex::new(HashMap::new()));
        peers
            .lock()
            .await
            .insert(gateway_uuid, PeerHandle { sender: peer_tx });
        let msg = Message {
            routing: Routing {
                src: remote_sender_uuid.to_string(),
                // Whatever the frame says, the name comes from this router's LSA.
                src_l2_name: Some("SY.forged@motherbee".to_string()),
                dst: Destination::Unicast(local_receiver_uuid.to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "system".to_string(),
                msg: Some("STREAM_ACK".to_string()),
                ..Meta::default()
            },
            payload: serde_json::json!({"up_to_seq": 7}),
        };
        let router_uuid = Uuid::new_v4();
        let shm_writer = RouterRegionWriter::open_or_create(
            &shm_name,
            router_uuid,
            "worker1",
            "RT.switch1@worker1",
            false,
        )
        .expect("create test shm");
        let shm = Arc::new(Mutex::new(shm_writer));
        handle_peer_message(
            &msg,
            &gateway_uuid,
            &peers,
            &peer_nodes,
            &peer_routers,
            &nodes,
            &Arc::new(Mutex::new(HashMap::<String, WanPeer>::new())),
            &Arc::new(Mutex::new(None::<LsaRegionReader>)),
            &Arc::new(Mutex::new(Vec::<StaticRoute>::new())),
            &shm,
            &Arc::new(Mutex::new(OpaResolver::new())),
            &Arc::new(Mutex::new(None::<OpaRegionReader>)),
            &Arc::new(Mutex::new(None::<MemoryRegionReader>)),
            "worker1",
            "SY.frontdesk.gov@worker1",
            &Arc::new(Mutex::new(Vec::<FibEntry>::new())),
            router_uuid,
            false,
            &lsa_snapshot,
            &reachability,
            &stream::StreamHandle::detached(),
        )
        .await
        .unwrap();
        cleanup(&shm_name);
        receiver_rx
            .try_recv()
            .ok()
            .map(|frame| serde_json::from_slice(&frame).unwrap())
    }

    #[tokio::test]
    async fn a_message_from_another_hive_reaches_a_node_on_a_non_gateway_router() {
        let delivered = deliver_from_peer(true)
            .await
            .expect("the gateway's hand-over is delivered");
        assert_eq!(
            delivered.routing.src_l2_name.as_deref(),
            Some("SY.storage@motherbee"),
            "the name comes from this router's LSA, not from the frame"
        );
    }

    #[tokio::test]
    async fn only_the_gateway_hands_over_another_hives_nodes() {
        assert!(
            deliver_from_peer(false).await.is_none(),
            "a sibling that is not the gateway cannot speak for another hive's node"
        );
    }

    #[tokio::test]
    async fn memory_reader_reopens_after_shm_recreation_when_old_reader_is_stale() {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let hive_id = format!("memtest-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let shm_name = memory_shm_name_for_hive(&hive_id).expect("memory shm name");
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);

        let thread_id = "thread:test-reopen".to_string();
        let old_package = fluxbee_sdk::protocol::MemoryPackage {
            package_version: 2,
            thread_id: thread_id.clone(),
            dominant_context: Some(fluxbee_sdk::protocol::MemoryContextSummary {
                context_id: "context:old".to_string(),
                label: "old".to_string(),
                weight: 1.0,
            }),
            dominant_reason: None,
            contexts: Vec::new(),
            reasons: Vec::new(),
            memories: Vec::new(),
            episodes: Vec::new(),
            truncated: None,
        };
        let fresh_package = fluxbee_sdk::protocol::MemoryPackage {
            package_version: 2,
            thread_id: thread_id.clone(),
            dominant_context: Some(fluxbee_sdk::protocol::MemoryContextSummary {
                context_id: "context:fresh".to_string(),
                label: "fresh".to_string(),
                weight: 2.0,
            }),
            dominant_reason: None,
            contexts: Vec::new(),
            reasons: Vec::new(),
            memories: Vec::new(),
            episodes: Vec::new(),
            truncated: None,
        };

        let mut writer =
            crate::shm::MemoryRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
                .expect("create memory writer");
        writer
            .write_snapshot(&MemoryShmSnapshot {
                schema_version: 1,
                updated_at: 1,
                threads: vec![crate::shm::MemoryShmThreadEntry {
                    thread_id: thread_id.clone(),
                    agent_ilk: "ilk:agent".to_string(),
                    agent_node: "AI.test@sandbox".to_string(),
                    package: old_package,
                }],
            })
            .expect("write initial snapshot");

        let stale_reader =
            MemoryRegionReader::open_read_only(&shm_name).expect("open stale reader");
        let shared_reader = Arc::new(Mutex::new(Some(stale_reader)));

        cleanup(&shm_name);

        let mut replacement_writer =
            crate::shm::MemoryRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
                .expect("create replacement writer");
        replacement_writer
            .write_snapshot(&MemoryShmSnapshot {
                schema_version: 1,
                updated_at: 2,
                threads: vec![crate::shm::MemoryShmThreadEntry {
                    thread_id: thread_id.clone(),
                    agent_ilk: "ilk:agent".to_string(),
                    agent_node: "AI.test@sandbox".to_string(),
                    package: fresh_package,
                }],
            })
            .expect("write replacement snapshot");

        tokio::time::sleep(Duration::from_millis(5)).await;

        let snapshot = read_memory_snapshot_for_delivery(&shared_reader, &hive_id, 1)
            .await
            .expect("router should reopen stale memory reader");
        let entry = snapshot
            .threads
            .iter()
            .find(|entry| entry.thread_id == thread_id)
            .expect("fresh thread entry present");
        assert_eq!(
            entry
                .package
                .dominant_context
                .as_ref()
                .map(|context| context.label.as_str()),
            Some("fresh"),
            "router must reopen jsr-memory and observe the replacement snapshot"
        );

        cleanup(&shm_name);
    }

    #[tokio::test]
    async fn lsa_reader_reopens_after_shm_recreation_when_old_reader_is_stale() {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let hive_id = format!("lsatest-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let shm_name = format!("/jsr-lsa-{}", hive_id);
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);

        let make_hive_entry = |hive: &str, router_name: &str| {
            let mut entry = crate::shm::RemoteHiveEntry {
                hive_id: [0; 64],
                hive_id_len: 0,
                router_uuid: *Uuid::new_v4().as_bytes(),
                router_name: [0; 64],
                router_name_len: 0,
                last_lsa_seq: 1,
                last_updated: 1,
                flags: 0,
                node_count: 0,
                route_count: 0,
                vpn_count: 0,
                tap_count: 0,
            };
            entry.hive_id_len = copy_bytes_with_len(&mut entry.hive_id, hive) as u16;
            entry.router_name_len = copy_bytes_with_len(&mut entry.router_name, router_name) as u16;
            entry
        };

        let mut writer =
            crate::shm::LsaRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
                .expect("create lsa writer");
        writer
            .write_snapshot(
                &[make_hive_entry("old-hive", "router-old")],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("write initial lsa snapshot");

        let stale_reader = LsaRegionReader::open_read_only(&shm_name).expect("open stale reader");
        let shared_reader = Arc::new(Mutex::new(Some(stale_reader)));

        cleanup(&shm_name);

        let mut replacement_writer =
            crate::shm::LsaRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
                .expect("create replacement lsa writer");
        replacement_writer
            .write_snapshot(
                &[make_hive_entry("fresh-hive", "router-fresh")],
                &[],
                &[],
                &[],
                &[],
            )
            .expect("write replacement lsa snapshot");

        tokio::time::sleep(Duration::from_millis(5)).await;

        let snapshot = read_lsa_snapshot_for_routing(&shared_reader, &hive_id, 1)
            .await
            .expect("router should reopen stale lsa reader");
        assert_eq!(snapshot.hives.len(), 1);
        let router_name_len = snapshot.hives[0].router_name_len as usize;
        assert_eq!(
            std::str::from_utf8(&snapshot.hives[0].router_name[..router_name_len])
                .expect("utf8 router name"),
            "router-fresh",
            "router must reopen jsr-lsa and observe the replacement snapshot"
        );

        cleanup(&shm_name);
    }

    #[tokio::test]
    async fn config_reader_reopens_after_shm_recreation_when_old_reader_is_stale() {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let hive_id = format!("cfgtest-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let shm_name = format!("/jsr-config-{}", hive_id);
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);

        let make_route = |prefix: &str| {
            let mut entry = crate::shm::StaticRouteEntry {
                prefix: [0; 256],
                prefix_len: 0,
                match_kind: MATCH_PREFIX,
                action: ACTION_FORWARD,
                next_hop_hive: [0; 32],
                next_hop_hive_len: 0,
                _pad: [0; 3],
                metric: 1,
                priority: 1,
                flags: FLAG_ACTIVE,
                installed_at: 1,
                _reserved: [0; 8],
            };
            entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, prefix) as u16;
            entry.next_hop_hive_len = copy_bytes_with_len(&mut entry.next_hop_hive, "worker") as u8;
            entry
        };

        let mut writer =
            crate::shm::ConfigRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
                .expect("create config writer");
        writer
            .write_static_routes(&[make_route("old.")], true)
            .expect("write initial config snapshot");

        let stale_reader =
            ConfigRegionReader::open_read_only(&shm_name).expect("open stale config reader");
        let shared_reader = Arc::new(Mutex::new(Some(stale_reader)));

        cleanup(&shm_name);

        let mut replacement_writer =
            crate::shm::ConfigRegionWriter::open_or_create(&shm_name, Uuid::new_v4(), &hive_id)
                .expect("create replacement config writer");
        replacement_writer
            .write_static_routes(&[make_route("fresh.")], true)
            .expect("write replacement config snapshot");

        tokio::time::sleep(Duration::from_millis(5)).await;

        let snapshot = read_config_snapshot_for_routing(&shared_reader, &hive_id, 1, false)
            .await
            .expect("router should reopen stale config reader");
        assert_eq!(snapshot.routes.len(), 1);
        let prefix_len = snapshot.routes[0].prefix_len as usize;
        assert_eq!(
            std::str::from_utf8(&snapshot.routes[0].prefix[..prefix_len]).expect("utf8 prefix"),
            "fresh.",
            "router must reopen jsr-config and observe the replacement snapshot"
        );
    }

    #[tokio::test]
    async fn opa_reader_reopens_after_shm_recreation_when_old_reader_is_stale() {
        use nix::sys::mman::shm_unlink;
        use std::ffi::CString;

        let hive_id = format!("opatest-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let shm_name = format!("/jsr-opa-{}", hive_id);
        let cleanup = |name: &str| {
            if let Ok(cstr) = CString::new(name) {
                let _ = shm_unlink(cstr.as_c_str());
            }
        };
        cleanup(&shm_name);

        let old_snapshot = crate::shm::OpaSnapshot {
            header: crate::shm::OpaHeaderSnapshot {
                policy_version: 1,
                wasm_size: 3,
                wasm_hash: [1; 32],
                updated_at: 0,
                heartbeat: 0,
                status: 1,
                entrypoint: "old/main".to_string(),
                owner_uuid: Uuid::new_v4(),
                owner_pid: 1,
            },
            wasm: vec![1, 2, 3],
        };
        let fresh_snapshot = crate::shm::OpaSnapshot {
            header: crate::shm::OpaHeaderSnapshot {
                policy_version: 2,
                wasm_size: 4,
                wasm_hash: [2; 32],
                updated_at: 0,
                heartbeat: 0,
                status: 2,
                entrypoint: "fresh/main".to_string(),
                owner_uuid: Uuid::new_v4(),
                owner_pid: 1,
            },
            wasm: vec![9, 8, 7, 6],
        };

        let mut writer = crate::shm::OpaRegionWriter::open_or_create(&shm_name, Uuid::new_v4())
            .expect("create opa writer");
        writer
            .write_snapshot(&old_snapshot)
            .expect("write initial opa snapshot");

        let stale_reader = OpaRegionReader::open_read_only(&shm_name).expect("open stale reader");
        let shared_reader = Arc::new(Mutex::new(Some(stale_reader)));

        cleanup(&shm_name);

        let mut replacement_writer =
            crate::shm::OpaRegionWriter::open_or_create(&shm_name, Uuid::new_v4())
                .expect("create replacement opa writer");
        replacement_writer
            .write_snapshot(&fresh_snapshot)
            .expect("write replacement opa snapshot");

        tokio::time::sleep(Duration::from_millis(5)).await;

        let snapshot = read_opa_snapshot_for_reload(&shared_reader, &hive_id, 1)
            .await
            .expect("router should reopen stale opa reader");
        assert_eq!(
            snapshot.header.policy_version, 2,
            "router must reopen jsr-opa and observe the replacement header"
        );
        assert_eq!(
            snapshot.wasm,
            vec![9, 8, 7, 6],
            "router must reopen jsr-opa and observe the replacement wasm"
        );

        cleanup(&shm_name);
    }

    /// How often a test's WAN session rechecks the registry.
    const TEST_REGISTRY_RECHECK: Duration = Duration::from_millis(20);

    /// A hive registry in a temporary directory, listing `hives`.
    fn test_hive_registry(hives: &[&str]) -> HiveRegistry {
        let root = std::env::temp_dir().join(format!("fluxbee-hives-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create registry");
        for hive in hives {
            fs::create_dir(root.join(hive)).expect("create registry entry");
        }
        HiveRegistry::new(root)
    }

    /// The motherbee gateway's WAN context, with no nodes, peers or TLS.
    fn test_wan_context(hive_registry: Option<HiveRegistry>) -> Arc<WanContext> {
        wan_context_of("motherbee", hive_registry)
    }

    /// `hive`'s gateway WAN context, with no nodes, peers or TLS.
    fn wan_context_of(hive: &str, hive_registry: Option<HiveRegistry>) -> Arc<WanContext> {
        Arc::new(WanContext {
            router_uuid: Uuid::new_v4(),
            router_name: format!("RT.gateway@{hive}"),
            hive_id: hive.to_string(),
            identity_frontdesk_node_name: format!("SY.frontdesk.gov@{hive}"),
            hello_interval_ms: 10_000,
            dead_interval_ms: 40_000,
            hive_registry,
            hive_registry_recheck: TEST_REGISTRY_RECHECK,
            nodes: Arc::new(Mutex::new(HashMap::new())),
            peer_nodes: Arc::new(Mutex::new(HashMap::new())),
            peer_routers: Arc::new(Mutex::new(HashMap::new())),
            peers: Arc::new(Mutex::new(HashMap::new())),
            wan_peers: Arc::new(Mutex::new(HashMap::new())),
            lsa_state: Arc::new(Mutex::new(HashMap::new())),
            reachability: Arc::new(Mutex::new(HashMap::new())),
            lsa_writer: Arc::new(Mutex::new(None)),
            lsa_snapshot: Arc::new(Mutex::new(None)),
            static_routes: Arc::new(Mutex::new(Vec::new())),
            vpn_rules: Arc::new(Mutex::new(Vec::new())),
            tap_rules: Arc::new(Mutex::new(Vec::new())),
            fib: Arc::new(Mutex::new(Vec::new())),
            broadcast_cache: Arc::new(Mutex::new(BroadcastCache::new())),
            lsa_seq: Arc::new(Mutex::new(0)),
            log_stream: stream::StreamHandle::detached(),
            memory_reader: Arc::new(Mutex::new(None)),
            opa: Arc::new(Mutex::new(OpaResolver::new())),
            wan_session_epochs: Arc::new(Mutex::new(HashMap::new())),
            lsa_reject_counters: Arc::new(Mutex::new(HashMap::new())),
            wan_mtls_mode: "disabled".to_string(),
            wan_tls: None,
        })
    }

    fn wan_hello_frame(hive_id: &str) -> Vec<u8> {
        let router_id = Uuid::new_v4().to_string();
        let hello = build_wan_hello(
            &router_id,
            &Uuid::new_v4().to_string(),
            WanHelloPayload {
                protocol: MESH_PROTOCOL_VERSION.to_string(),
                router_id: router_id.clone(),
                router_name: format!("RT.gateway@{hive_id}"),
                hive_id: hive_id.to_string(),
                capabilities: vec!["lsa".to_string(), "forwarding".to_string()],
                timers: WanTimers {
                    hello_interval_ms: 10_000,
                    dead_interval_ms: 40_000,
                },
            },
        );
        serde_json::to_vec(&hello).expect("hello frame")
    }

    async fn read_system_message<R>(reader: &mut R) -> Message
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let frame = read_frame(reader)
            .await
            .expect("read frame")
            .expect("a frame");
        serde_json::from_slice(&frame).expect("a message")
    }

    /// The accepting side of a plaintext WAN link on `stream`.
    fn spawn_accepted_link(
        stream: tokio::io::DuplexStream,
        ctx: &Arc<WanContext>,
    ) -> tokio::task::JoinHandle<Result<(), RouterError>> {
        tokio::spawn(handle_wan_connection(
            stream,
            None,
            Arc::clone(ctx),
            false,
            None,
        ))
    }

    /// An uplink on `stream`, and whether it got past WAN_ACCEPT.
    fn spawn_uplink(
        stream: tokio::io::DuplexStream,
        ctx: &Arc<WanContext>,
    ) -> (
        tokio::task::JoinHandle<Result<(), RouterError>>,
        Arc<AtomicBool>,
    ) {
        let established = Arc::new(AtomicBool::new(false));
        let session = tokio::spawn({
            let ctx = Arc::clone(ctx);
            let established = Arc::clone(&established);
            async move {
                handle_wan_connection(stream, None, ctx, true, Some(established.as_ref())).await
            }
        });
        (session, established)
    }

    #[test]
    fn an_uplink_backs_off_until_a_link_gets_past_wan_accept() {
        let mut retry = UplinkRetry::new();
        // Attempts that fail, a WAN_REJECT among them: 5 s, doubling up to 60 s.
        let waits: Vec<u64> = (0..6)
            .map(|_| retry.after_attempt(false).as_secs())
            .collect();
        assert_eq!(waits, [5, 10, 20, 40, 60, 60]);
        // A link that got past WAN_ACCEPT starts the retries over, however it ended.
        assert_eq!(retry.after_attempt(true).as_secs(), 5);
        assert_eq!(retry.failures, 0);
        assert_eq!(retry.after_attempt(false).as_secs(), 5);
        assert_eq!(retry.after_attempt(false).as_secs(), 10);
    }

    #[test]
    fn the_motherbee_admits_only_the_hives_its_registry_lists() {
        let registry = test_hive_registry(&["worker1"]);
        assert_eq!(wan_peer_admission(Some(&registry), "worker1"), Ok(()));
        assert_eq!(
            wan_peer_admission(Some(&registry), "worker2"),
            Err(WAN_REJECT_HIVE_NOT_REGISTERED)
        );
        // An id that is not a plain name never reaches a directory outside the registry.
        for hive_id in ["", "..", ".", "/tmp", "worker1/..", "../worker1"] {
            assert_eq!(
                wan_peer_admission(Some(&registry), hive_id),
                Err(WAN_REJECT_HIVE_NOT_REGISTERED),
                "{hive_id:?}"
            );
        }
        // Another hive keeps no registry and checks nothing.
        assert_eq!(wan_peer_admission(None, "worker2"), Ok(()));
        let _ = fs::remove_dir_all(registry.root());
    }

    #[tokio::test]
    async fn the_registry_recheck_completes_once_the_hive_is_removed() {
        let registry = test_hive_registry(&["worker1"]);
        let unregistered = hive_unregistered(Some(&registry), "worker1", TEST_REGISTRY_RECHECK);
        tokio::pin!(unregistered);
        assert!(
            time::timeout(Duration::from_millis(100), &mut unregistered)
                .await
                .is_err(),
            "the hive is still registered"
        );
        fs::remove_dir_all(registry.root().join("worker1")).expect("remove registry entry");
        time::timeout(Duration::from_secs(2), &mut unregistered)
            .await
            .expect("the recheck notices the removal");
        // Without a registry there is nothing to recheck.
        let never = time::timeout(
            Duration::from_millis(100),
            hive_unregistered(None, "worker1", TEST_REGISTRY_RECHECK),
        );
        assert!(never.await.is_err());
        let _ = fs::remove_dir_all(registry.root());
    }

    #[tokio::test]
    async fn the_motherbee_rejects_a_hive_its_registry_does_not_list() {
        let ctx = test_wan_context(Some(test_hive_registry(&[])));
        let (server, mut client) = tokio::io::duplex(64 * 1024);
        let session = spawn_accepted_link(server, &ctx);
        write_frame(&mut client, &wan_hello_frame("worker1"))
            .await
            .expect("send hello");

        let reject = read_system_message(&mut client).await;
        assert_eq!(reject.meta.msg.as_deref(), Some(MSG_WAN_REJECT));
        let payload: WanRejectPayload =
            serde_json::from_value(reject.payload).expect("reject payload");
        assert_eq!(payload.reason, WAN_REJECT_HIVE_NOT_REGISTERED);
        assert!(
            read_frame(&mut client).await.expect("read").is_none(),
            "the link closes after the reject"
        );
        time::timeout(Duration::from_secs(2), session)
            .await
            .expect("the session ends")
            .expect("session task")
            .expect("session result");
        assert!(ctx.wan_peers.lock().await.is_empty());
        if let Some(registry) = ctx.hive_registry.as_ref() {
            let _ = fs::remove_dir_all(registry.root());
        }
    }

    #[tokio::test]
    async fn the_link_of_a_hive_removed_from_the_registry_closes() {
        let ctx = test_wan_context(Some(test_hive_registry(&["worker1"])));
        let (server, mut client) = tokio::io::duplex(64 * 1024);
        let session = spawn_accepted_link(server, &ctx);
        write_frame(&mut client, &wan_hello_frame("worker1"))
            .await
            .expect("send hello");
        assert_eq!(
            read_system_message(&mut client).await.meta.msg.as_deref(),
            Some(MSG_HELLO)
        );
        assert_eq!(
            read_system_message(&mut client).await.meta.msg.as_deref(),
            Some(fluxbee_sdk::protocol::MSG_WAN_ACCEPT)
        );
        assert!(ctx.wan_peers.lock().await.contains_key("worker1"));

        let registry_root = ctx
            .hive_registry
            .as_ref()
            .expect("registry")
            .root()
            .to_path_buf();
        fs::remove_dir_all(registry_root.join("worker1")).expect("remove registry entry");
        time::timeout(Duration::from_secs(2), session)
            .await
            .expect("the session ends once the hive is removed")
            .expect("session task")
            .expect("session result");
        assert!(ctx.wan_peers.lock().await.is_empty());
        let _ = fs::remove_dir_all(&registry_root);
    }

    #[tokio::test]
    async fn an_uplink_the_peer_rejects_fails_with_the_reason() {
        let ctx = test_wan_context(None);
        let (spoke, mut motherbee) = tokio::io::duplex(64 * 1024);
        let (session, established) = spawn_uplink(spoke, &ctx);
        let hello = read_system_message(&mut motherbee).await;
        assert_eq!(hello.meta.msg.as_deref(), Some(MSG_HELLO));
        let reject = build_wan_reject(
            &Uuid::new_v4().to_string(),
            &ctx.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            WanRejectPayload {
                reason: WAN_REJECT_HIVE_NOT_REGISTERED.to_string(),
                message: "Hive 'motherbee' is not registered".to_string(),
            },
        );
        let reject = serde_json::to_vec(&reject).expect("reject frame");
        write_frame(&mut motherbee, &reject)
            .await
            .expect("send reject");
        let err = time::timeout(Duration::from_secs(2), session)
            .await
            .expect("the session ends")
            .expect("session task")
            .expect_err("a rejected uplink is an error");
        assert!(
            err.to_string().contains(WAN_REJECT_HIVE_NOT_REGISTERED),
            "{err}"
        );
        assert!(
            !established.load(Ordering::Relaxed),
            "a rejected link never got past WAN_ACCEPT"
        );
    }

    #[tokio::test]
    async fn an_admitted_uplink_reports_that_it_got_past_wan_accept() {
        let ctx = test_wan_context(None);
        let (spoke, mut motherbee) = tokio::io::duplex(64 * 1024);
        let (session, established) = spawn_uplink(spoke, &ctx);
        let hello = read_system_message(&mut motherbee).await;
        assert_eq!(hello.meta.msg.as_deref(), Some(MSG_HELLO));
        write_frame(&mut motherbee, &wan_hello_frame("motherbee"))
            .await
            .expect("send hello");
        let accept = read_system_message(&mut motherbee).await;
        assert_eq!(
            accept.meta.msg.as_deref(),
            Some(fluxbee_sdk::protocol::MSG_WAN_ACCEPT)
        );
        drop(motherbee);
        time::timeout(Duration::from_secs(2), session)
            .await
            .expect("the session ends")
            .expect("session task")
            .expect("session result");
        assert!(established.load(Ordering::Relaxed));
    }

    /// The motherbee counts the nodes a spoke's LSA lists under another hive's name, and applies the
    /// rest of that LSA.
    #[tokio::test]
    async fn the_foreign_nodes_of_an_lsa_are_counted_and_the_rest_applies() {
        let ctx = test_wan_context(None);
        let peer_router_uuid = Uuid::new_v4();
        let mut payload = lsa_payload(
            "worker1",
            1,
            &peer_router_uuid.to_string(),
            "RT.gateway@worker1",
        );
        payload.nodes = vec![
            lsa_node("AI.sales@worker1"),
            lsa_node("SY.admin@motherbee"),
            lsa_node("SY.vault@motherbee"),
        ];
        payload.routes = vec![LsaRoute {
            prefix: "AI.".to_string(),
            match_kind: "PREFIX".to_string(),
            action: "FORWARD".to_string(),
            next_hop_hive: String::new(),
            metric: 0,
        }];
        payload.taps = vec![LsaTap {
            match_src: "AI.sales@worker1".to_string(),
            match_dst: "SY.admin@motherbee".to_string(),
            target: "IO.audit@worker1".to_string(),
            mode: "best_effort".to_string(),
            enabled: true,
        }];
        let msg = build_lsa(
            &peer_router_uuid.to_string(),
            &ctx.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            payload,
        );
        let (tx, _rx) = mpsc::unbounded_channel::<Vec<u8>>();
        handle_wan_message(
            &msg,
            &tx,
            &ctx,
            "worker1",
            peer_router_uuid,
            "RT.gateway@worker1",
            1,
        )
        .await
        .expect("the LSA is handled");

        assert_eq!(
            ctx.lsa_reject_counters
                .lock()
                .await
                .get(LSA_REJECT_FOREIGN_NODE),
            Some(&2)
        );
        let state = ctx.lsa_state.lock().await;
        let worker1 = state.get("worker1").expect("worker1's LSA applies");
        let installed: Vec<&str> = worker1
            .nodes
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(installed, ["AI.sales@worker1"]);
        assert_eq!(worker1.routes.len(), 1);
        assert_eq!(worker1.taps.len(), 1);
    }

    /// R1 end to end: on the motherbee, a frame from ingress1 carrying the UUID of a worker1 node is
    /// dropped and counted; the same frame from worker1 is delivered under that node's name.
    #[tokio::test]
    async fn the_motherbee_drops_a_spokes_frame_sent_as_another_spokes_node() {
        let ctx = test_wan_context(None);
        let wf_rules = Uuid::new_v4();
        *ctx.lsa_snapshot.lock().await = Some(lsa_of(
            vec![
                lsa_hive("motherbee", FLAG_ACTIVE | HIVE_FLAG_SELF),
                lsa_hive("ingress1", FLAG_ACTIVE),
                lsa_hive("worker1", FLAG_ACTIVE),
            ],
            vec![lsa_node_entry(wf_rules, "SY.wf-rules@worker1", 2)],
        ));
        let receiver = Uuid::new_v4();
        let (receiver_tx, mut receiver_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        ctx.nodes.lock().await.insert(
            receiver,
            NodeHandle {
                name: "SY.admin@motherbee".to_string(),
                vpn_id: 0,
                sender: receiver_tx,
                connected_at: 0,
            },
        );
        let frame = Message {
            routing: Routing {
                src: wf_rules.to_string(),
                src_l2_name: Some("SY.wf-rules@worker1".to_string()),
                dst: Destination::Unicast(receiver.to_string()),
                ttl: 16,
                trace_id: Uuid::new_v4().to_string(),
            },
            meta: Meta {
                msg_type: "user".to_string(),
                ..Meta::default()
            },
            payload: serde_json::json!({}),
        };
        let (tx, _rx) = mpsc::unbounded_channel::<Vec<u8>>();

        handle_wan_message(
            &frame,
            &tx,
            &ctx,
            "ingress1",
            Uuid::new_v4(),
            "RT.gateway@ingress1",
            1,
        )
        .await
        .expect("handled");
        assert!(
            receiver_rx.try_recv().is_err(),
            "ingress1 cannot send as worker1's node"
        );
        assert_eq!(
            ctx.lsa_reject_counters
                .lock()
                .await
                .get(LSA_REJECT_FOREIGN_SOURCE),
            Some(&1)
        );

        handle_wan_message(
            &frame,
            &tx,
            &ctx,
            "worker1",
            Uuid::new_v4(),
            "RT.gateway@worker1",
            1,
        )
        .await
        .expect("handled");
        let delivered = receiver_rx
            .try_recv()
            .expect("worker1's own node is delivered");
        let delivered: Message = serde_json::from_slice(&delivered).expect("a message");
        assert_eq!(
            delivered.routing.src_l2_name.as_deref(),
            Some("SY.wf-rules@worker1")
        );
    }

    /// The LSA message `hive` sends with `nodes`, as `(uuid, name)`.
    fn lsa_message(hive: &str, seq: u64, nodes: &[(Uuid, &str)], to: Uuid) -> Message {
        let router_id = Uuid::new_v4().to_string();
        let mut payload = lsa_payload(hive, seq, &router_id, &format!("RT.gateway@{hive}"));
        payload.nodes = nodes
            .iter()
            .map(|(uuid, name)| LsaNode {
                uuid: uuid.to_string(),
                name: name.to_string(),
                vpn_id: 0,
            })
            .collect();
        build_lsa(
            &router_id,
            &to.to_string(),
            &Uuid::new_v4().to_string(),
            payload,
        )
    }

    /// R2 end to end on the motherbee: ingress1 lists a worker1 node's UUID, and the UUID of the
    /// motherbee's vault. Whichever LSA comes first, worker1's node UUID resolves to neither hive,
    /// the vault's claim is dropped, and both claims are counted.
    #[tokio::test]
    async fn a_hive_cannot_take_another_nodes_uuid_whichever_lsa_comes_first() {
        for ingress1_first in [false, true] {
            let ctx = test_wan_context(None);
            let lsa_name = format!("/lsa-{}", &Uuid::new_v4().simple().to_string()[..8]);
            unlink_test_shm(&lsa_name);
            *ctx.lsa_writer.lock().await = Some(
                LsaRegionWriter::open_or_create(&lsa_name, ctx.router_uuid, "motherbee")
                    .expect("lsa region"),
            );
            let vault = Uuid::new_v4();
            let (vault_tx, _vault_rx) = mpsc::unbounded_channel::<Vec<u8>>();
            ctx.nodes.lock().await.insert(
                vault,
                NodeHandle {
                    name: "SY.vault@motherbee".to_string(),
                    vpn_id: 0,
                    sender: vault_tx,
                    connected_at: 0,
                },
            );
            let sales = Uuid::new_v4();
            let worker1 = lsa_message(
                "worker1",
                1,
                &[(sales, "AI.sales@worker1")],
                ctx.router_uuid,
            );
            let ingress1 = lsa_message(
                "ingress1",
                1,
                &[(sales, "IO.x@ingress1"), (vault, "IO.y@ingress1")],
                ctx.router_uuid,
            );
            let order = if ingress1_first {
                [("ingress1", &ingress1), ("worker1", &worker1)]
            } else {
                [("worker1", &worker1), ("ingress1", &ingress1)]
            };
            let (tx, _rx) = mpsc::unbounded_channel::<Vec<u8>>();
            for (hive, lsa) in order {
                handle_wan_message(
                    lsa,
                    &tx,
                    &ctx,
                    hive,
                    Uuid::new_v4(),
                    &format!("RT.gateway@{hive}"),
                    1,
                )
                .await
                .expect("handled");
            }

            let snapshot = ctx.lsa_snapshot.lock().await;
            assert!(
                find_remote_node(&snapshot, sales).is_none(),
                "ingress1 first: {ingress1_first}"
            );
            assert!(find_remote_node(&snapshot, vault).is_none());
            let ingress1_nodes: Vec<String> = ctx.lsa_state.lock().await["ingress1"]
                .nodes
                .iter()
                .map(|node| node.name.clone())
                .collect();
            assert_eq!(
                ingress1_nodes,
                ["IO.x@ingress1"],
                "the vault's UUID is dropped"
            );
            assert_eq!(
                ctx.lsa_reject_counters
                    .lock()
                    .await
                    .get(LSA_REJECT_DUPLICATE_UUID),
                Some(&2)
            );
            unlink_test_shm(&lsa_name);
        }
    }

    /// R2 on a spoke: the hub's reachability cannot vouch for a UUID that is already taken, by one of
    /// the spoke's nodes or a node it learns from an LSA, nor list one twice.
    #[tokio::test]
    async fn the_hub_cannot_vouch_for_a_uuid_already_taken() {
        let ctx = wan_context_of("worker1", None);
        let local = Uuid::new_v4();
        let hub_node = Uuid::new_v4();
        let twice = Uuid::new_v4();
        let fine = Uuid::new_v4();
        let (local_tx, _local_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        ctx.nodes.lock().await.insert(
            local,
            NodeHandle {
                name: "AI.sales@worker1".to_string(),
                vpn_id: 0,
                sender: local_tx,
                connected_at: 0,
            },
        );
        *ctx.lsa_snapshot.lock().await = Some(lsa_of(
            vec![
                lsa_hive("worker1", FLAG_ACTIVE | HIVE_FLAG_SELF),
                lsa_hive("motherbee", FLAG_ACTIVE),
            ],
            vec![lsa_node_entry(hub_node, "SY.admin@motherbee", 1)],
        ));
        let entry = |uuid: Uuid, name: &str| WanReachabilityEntry {
            uuid: uuid.to_string(),
            name: name.to_string(),
            hive_id: "worker2".to_string(),
            vpn_id: 0,
        };
        let msg = build_wan_reachability(
            &Uuid::new_v4().to_string(),
            &ctx.router_uuid.to_string(),
            &Uuid::new_v4().to_string(),
            WanReachabilityPayload {
                origin_hive: "motherbee".to_string(),
                router_id: String::new(),
                router_name: String::new(),
                seq: 1,
                timestamp: "1".to_string(),
                entries: vec![
                    entry(local, "IO.a@worker2"),
                    entry(hub_node, "IO.b@worker2"),
                    entry(twice, "IO.c@worker2"),
                    entry(twice, "IO.d@worker2"),
                    entry(fine, "IO.e@worker2"),
                ],
            },
        );

        apply_wan_reachability(&ctx, "motherbee", &msg).await;

        let vouched: Vec<String> = ctx
            .reachability
            .lock()
            .await
            .values()
            .map(|entry| entry.name.clone())
            .collect();
        assert_eq!(vouched, ["IO.e@worker2"]);
        assert_eq!(
            ctx.lsa_reject_counters
                .lock()
                .await
                .get(LSA_REJECT_DUPLICATE_UUID),
            Some(&4)
        );
    }

    /// R2 on the hub: a UUID that two spokes list is vouched for to no spoke.
    #[tokio::test]
    async fn the_hub_vouches_for_no_uuid_that_two_spokes_list() {
        let ctx = test_wan_context(None);
        let shared = Uuid::new_v4();
        let support = Uuid::new_v4();
        let lsa_nodes = |nodes: &[(Uuid, &str)]| -> Vec<LsaNode> {
            nodes
                .iter()
                .map(|(uuid, name)| LsaNode {
                    uuid: uuid.to_string(),
                    name: name.to_string(),
                    vpn_id: 0,
                })
                .collect()
        };
        {
            let mut state = ctx.lsa_state.lock().await;
            state.insert(
                "worker1".to_string(),
                RemoteHiveState {
                    nodes: lsa_nodes(&[
                        (shared, "AI.sales@worker1"),
                        (support, "AI.support@worker1"),
                    ]),
                    ..RemoteHiveState::default()
                },
            );
            state.insert(
                "ingress1".to_string(),
                RemoteHiveState {
                    nodes: lsa_nodes(&[(shared, "IO.x@ingress1")]),
                    ..RemoteHiveState::default()
                },
            );
        }
        let mut worker2_rx = None;
        for hive in ["worker1", "ingress1", "worker2"] {
            let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
            ctx.wan_peers.lock().await.insert(
                hive.to_string(),
                WanPeer {
                    sender: tx,
                    router_uuid: Uuid::new_v4(),
                },
            );
            if hive == "worker2" {
                worker2_rx = Some(rx);
            }
        }

        broadcast_reachability(&ctx).await.expect("broadcast");

        let frame = worker2_rx
            .expect("worker2's link")
            .try_recv()
            .expect("worker2 hears of the other spokes");
        let msg: Message = serde_json::from_slice(&frame).expect("a message");
        let payload: WanReachabilityPayload =
            serde_json::from_value(msg.payload).expect("a reachability payload");
        let vouched: Vec<&str> = payload
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(vouched, ["AI.support@worker1"]);
    }

    /// R4, the caller: sibling routers' regions are read only through the ownership check.
    #[test]
    fn sibling_regions_are_read_only_through_the_ownership_check() {
        let src = include_str!("mod.rs");
        let start = src
            .find("fn discover_peer_regions(")
            .expect("discover_peer_regions");
        let end = start + src[start..].find("\n}\n").expect("its end");
        let body = &src[start..end];
        assert!(body.contains("RouterRegionReader::open_read_only_owned_by(&shm_name, own_uid)"));
        assert!(!body.contains("RouterRegionReader::open_read_only(&"));
    }
}
