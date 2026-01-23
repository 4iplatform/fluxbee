use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{self, Duration};
use uuid::Uuid;

use crate::config::RouterConfig;
use crate::opa::OpaResolver;
use crate::shm::{
    copy_bytes_with_len, now_epoch_ms, ConfigRegionReader, ConfigSnapshot, LsaRegionReader,
    LsaRegionWriter, LsaSnapshot, RemoteIslandEntry, RemoteNodeEntry, RemoteRouteEntry,
    RemoteVpnEntry, RouterRegionReader, RouterRegionWriter, VpnAssignment, ACTION_DROP,
    ACTION_FORWARD, FLAG_ACTIVE, FLAG_DELETED, FLAG_STALE, HEARTBEAT_STALE_MS, MATCH_EXACT,
    MATCH_GLOB, MATCH_PREFIX,
};
use jsr_client::protocol::{
    build_announce, build_lsa, build_router_hello, build_ttl_exceeded, build_unreachable,
    build_wan_accept, build_wan_hello, build_wan_reject, Destination, LsaNode, LsaPayload,
    LsaRoute, LsaVpn, Message, Meta, NodeAnnouncePayload, NodeHelloPayload, RouterHelloPayload,
    WanAcceptPayload, WanHelloPayload, WanNegotiated, WanRejectPayload, WanTimers,
    MSG_CONFIG_CHANGED, MSG_HELLO, MSG_LSA, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_WITHDRAW,
    SCOPE_GLOBAL, SYSTEM_KIND,
};
use jsr_client::socket::connection::{read_frame, write_frame};

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
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_version: Arc<Mutex<u64>>,
    opa: Arc<Mutex<OpaResolver>>,
    broadcast_cache: Arc<Mutex<BroadcastCache>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_state: Arc<Mutex<std::collections::HashMap<String, RemoteIslandState>>>,
    lsa_seq: Arc<Mutex<u64>>,
}

impl Router {
    pub fn new(cfg: RouterConfig) -> Self {
        let shm = RouterRegionWriter::open_or_create(
            &cfg.shm_name,
            cfg.router_uuid,
            &cfg.island_id,
            &cfg.router_l2_name,
            cfg.is_gateway,
        )
        .expect("shm init");
        let config_reader = match ConfigRegionReader::open_read_only(&format!(
            "/jsr-config-{}",
            cfg.island_id
        )) {
            Ok(reader) => Some(reader),
            Err(_) => None,
        };
        let lsa_reader = match LsaRegionReader::open_read_only(&format!(
            "/jsr-lsa-{}",
            cfg.island_id
        )) {
            Ok(reader) => Some(reader),
            Err(_) => None,
        };
        let lsa_writer = if cfg.is_gateway {
            LsaRegionWriter::open_or_create(
                &format!("/jsr-lsa-{}", cfg.island_id),
                cfg.router_uuid,
                &cfg.island_id,
            )
            .ok()
        } else {
            None
        };
        let opa = Arc::new(Mutex::new(OpaResolver::new(PathBuf::from(
            "/var/lib/json-router/policy.wasm",
        ))));
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
            fib: Arc::new(Mutex::new(Vec::new())),
            config_version: Arc::new(Mutex::new(0)),
            opa,
            broadcast_cache: Arc::new(Mutex::new(BroadcastCache::new())),
            wan_peers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            lsa_state: Arc::new(Mutex::new(std::collections::HashMap::new())),
            lsa_seq: Arc::new(Mutex::new(0)),
        }
    }

    pub async fn run(&self) -> Result<(), RouterError> {
        ensure_parent_dir(&self.cfg.node_socket_path)?;
        let _ = std::fs::remove_file(&self.cfg.node_socket_path);
        let listener = UnixListener::bind(&self.cfg.node_socket_path)?;
        tracing::info!(
            router = %self.cfg.router_l2_name,
            socket = %self.cfg.node_socket_path.display(),
            "router listening"
        );

        let irp_path = peer_socket_path(self.cfg.router_uuid);
        ensure_parent_dir(&irp_path)?;
        let _ = std::fs::remove_file(&irp_path);
        let irp_listener = UnixListener::bind(&irp_path)?;
        tracing::info!(
            router = %self.cfg.router_l2_name,
            socket = %irp_path.display(),
            "irp listening"
        );

        let shm = Arc::clone(&self.shm);
        let opa = Arc::clone(&self.opa);
        let heartbeat_interval = self.cfg.heartbeat_interval_ms;
        tokio::spawn(async move {
            let mut ticker = time::interval(Duration::from_millis(heartbeat_interval));
            loop {
                ticker.tick().await;
                let (policy_version, load_status) = {
                    let mut opa = opa.lock().await;
                    match opa.refresh_status() {
                        Ok(status) => status,
                        Err(err) => {
                            tracing::warn!("opa status refresh failed: {err}");
                            opa.status()
                        }
                    }
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
        let vpn_rules_pd = Arc::clone(&self.vpn_rules);
        let lsa_snapshot_pd = Arc::clone(&self.lsa_snapshot);
        let fib_pd = Arc::clone(&self.fib);
        let config_reader_pd = Arc::clone(&self.config_reader);
        let lsa_reader_pd = Arc::clone(&self.lsa_reader);
        let config_version_pd = Arc::clone(&self.config_version);
        let shm_pd = Arc::clone(&self.shm);
        let opa_pd = Arc::clone(&self.opa);
        let router_uuid = self.cfg.router_uuid;
        let router_name = self.cfg.router_l2_name.clone();
        let shm_name = self.cfg.shm_name.clone();
        let island_id = self.cfg.island_id.clone();
        let is_gateway = self.cfg.is_gateway;
        tokio::spawn(async move {
            peer_discovery_loop(
                router_uuid,
                &router_name,
                &shm_name,
                &island_id,
                peers_pd,
                peer_nodes_pd,
                peer_routers_pd,
                nodes_pd,
                wan_peers_pd,
                config_reader_pd,
                lsa_reader_pd,
                static_routes_pd,
                vpn_rules_pd,
                config_version_pd,
                shm_pd,
                opa_pd,
                lsa_snapshot_pd,
                fib_pd,
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
        let island_id = self.cfg.island_id.clone();
        tokio::spawn(async move {
            lsa_refresh_loop(
                lsa_reader,
                lsa_snapshot,
                &island_id,
                nodes,
                peer_nodes,
                static_routes,
                fib,
            )
            .await;
        });

        let peers = Arc::clone(&self.peers);
        let peer_nodes = Arc::clone(&self.peer_nodes);
        let peer_routers = Arc::clone(&self.peer_routers);
        let nodes = Arc::clone(&self.nodes);
        let fib = Arc::clone(&self.fib);
        let config_reader = Arc::clone(&self.config_reader);
        let lsa_reader = Arc::clone(&self.lsa_reader);
        let static_routes = Arc::clone(&self.static_routes);
        let vpn_rules = Arc::clone(&self.vpn_rules);
        let config_version = Arc::clone(&self.config_version);
        let shm = Arc::clone(&self.shm);
        let opa = Arc::clone(&self.opa);
        let island_id = self.cfg.island_id.clone();
        let wan_peers = Arc::clone(&self.wan_peers);
        let lsa_snapshot = Arc::clone(&self.lsa_snapshot);
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
                let peers = Arc::clone(&peers);
                let peer_nodes = Arc::clone(&peer_nodes);
                let peer_routers = Arc::clone(&peer_routers);
                let nodes = Arc::clone(&nodes);
                let fib = Arc::clone(&fib);
                let config_reader = Arc::clone(&config_reader);
                let static_routes = Arc::clone(&static_routes);
                let vpn_rules = Arc::clone(&vpn_rules);
                let config_version = Arc::clone(&config_version);
                let shm = Arc::clone(&shm);
                let opa = Arc::clone(&opa);
                let island_id = island_id.clone();
                let wan_peers = Arc::clone(&wan_peers);
                let lsa_reader = Arc::clone(&lsa_reader);
                let lsa_snapshot = Arc::clone(&lsa_snapshot);
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
                        config_reader,
                        lsa_reader,
                        static_routes,
                        vpn_rules,
                        config_version,
                        shm,
                        opa,
                        island_id,
                        wan_peers,
                        lsa_snapshot,
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
            let ctx = Arc::new(WanContext {
                router_uuid: self.cfg.router_uuid,
                router_name: self.cfg.router_l2_name.clone(),
                island_id: self.cfg.island_id.clone(),
                hello_interval_ms: self.cfg.hello_interval_ms,
                dead_interval_ms: self.cfg.dead_interval_ms,
                authorized_islands: self.cfg.wan_authorized_islands.clone(),
                nodes: Arc::clone(&self.nodes),
                peer_nodes: Arc::clone(&self.peer_nodes),
                peer_routers: Arc::clone(&self.peer_routers),
                peers: Arc::clone(&self.peers),
                wan_peers: Arc::clone(&self.wan_peers),
                lsa_state: Arc::clone(&self.lsa_state),
                lsa_writer: Arc::clone(&self.lsa_writer),
                lsa_snapshot: Arc::clone(&self.lsa_snapshot),
                static_routes: Arc::clone(&self.static_routes),
                vpn_rules: Arc::clone(&self.vpn_rules),
                fib: Arc::clone(&self.fib),
                broadcast_cache: Arc::clone(&self.broadcast_cache),
                lsa_seq: Arc::clone(&self.lsa_seq),
                opa: Arc::clone(&self.opa),
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
            tracing::info!("incoming connection accepted");
            let shm = Arc::clone(&self.shm);
            let router_name = self.cfg.router_l2_name.clone();
            let router_uuid = self.cfg.router_uuid;
            let island_id = self.cfg.island_id.clone();
            let nodes = Arc::clone(&self.nodes);
            let peer_nodes = Arc::clone(&self.peer_nodes);
            let peer_routers = Arc::clone(&self.peer_routers);
            let peers = Arc::clone(&self.peers);
            let wan_peers = Arc::clone(&self.wan_peers);
            let config_reader = Arc::clone(&self.config_reader);
            let lsa_reader = Arc::clone(&self.lsa_reader);
            let lsa_snapshot = Arc::clone(&self.lsa_snapshot);
            let static_routes = Arc::clone(&self.static_routes);
            let vpn_rules = Arc::clone(&self.vpn_rules);
            let fib = Arc::clone(&self.fib);
            let config_version = Arc::clone(&self.config_version);
            let opa = Arc::clone(&self.opa);
            let broadcast_cache = Arc::clone(&self.broadcast_cache);
            let lsa_seq = Arc::clone(&self.lsa_seq);
            let is_gateway = self.cfg.is_gateway;
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
                    static_routes,
                    vpn_rules,
                    fib,
                    config_version,
                    opa,
                    broadcast_cache,
                    lsa_seq,
                    router_uuid,
                    &router_name,
                    &island_id,
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
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_version: Arc<Mutex<u64>>,
    opa: Arc<Mutex<OpaResolver>>,
    broadcast_cache: Arc<Mutex<BroadcastCache>>,
    lsa_seq: Arc<Mutex<u64>>,
    router_uuid: Uuid,
    router_name: &str,
    island_id: &str,
    is_gateway: bool,
) -> Result<(), RouterError> {
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(err) = write_frame(&mut writer, &frame).await {
                tracing::warn!("node write error: {err}");
                break;
            }
        }
    });
    let frame = read_frame(&mut reader)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing HELLO"))?;

    let msg: Message = serde_json::from_slice(&frame)?;
    if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_HELLO) {
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

    let node_name = normalize_name(&payload.name, island_id);
    tracing::info!(node = %node_name, "loading config snapshot");
    let snapshot = refresh_config(
        &config_reader,
        &static_routes,
        &vpn_rules,
        &config_version,
        island_id,
        &nodes,
        &peer_nodes,
        &shm,
        &fib,
        &lsa_snapshot,
        false,
    )
    .await;
    let _ = refresh_lsa(
        &lsa_reader,
        &lsa_snapshot,
        island_id,
        &nodes,
        &peer_nodes,
        &static_routes,
        &fib,
    )
    .await;
    let vpn_id = assign_vpn(&node_name, snapshot.as_ref());
    tracing::info!(node = %node_name, vpn_id = vpn_id, "vpn assigned");
    let connected_at = now_epoch_ms();
    {
        let mut shm = shm.lock().await;
        shm.register_node(node_uuid, &node_name, vpn_id, connected_at)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    }
    tracing::info!(node = %node_uuid, "node registered in shm");
    {
        let mut nodes = nodes.lock().await;
        nodes.insert(
            node_uuid,
            NodeHandle {
                name: node_name.clone(),
                vpn_id,
                sender: tx.clone(),
                connected_at,
            },
        );
    }
    rebuild_fib(&fib, &nodes, &peer_nodes, &static_routes, &lsa_snapshot).await;
    if is_gateway {
        let _ = broadcast_lsa_direct(
            router_uuid,
            island_id,
            &nodes,
            &peer_nodes,
            &static_routes,
            &vpn_rules,
            &wan_peers,
            &lsa_seq,
        )
        .await;
    }

    tracing::info!(node = %node_uuid, name = %node_name, "node registered");
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

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
                    tracing::info!(
                        src = %msg.routing.src,
                        dst = ?msg.routing.dst,
                        msg_type = %msg.meta.msg_type,
                        msg = ?msg.meta.msg,
                        "message received"
                    );
                    if msg.meta.msg_type == SYSTEM_KIND {
                        if msg.meta.msg.as_deref() == Some(MSG_WITHDRAW) {
                            break;
                        }
                        if msg.meta.msg.as_deref() == Some(MSG_CONFIG_CHANGED) {
                            let _ = refresh_config(
                                &config_reader,
                                &static_routes,
                                &vpn_rules,
                                &config_version,
                                island_id,
                                &nodes,
                                &peer_nodes,
                                &shm,
                                &fib,
                                &lsa_snapshot,
                                true,
                            )
                            .await;
                            tracing::info!("config changed applied");
                            if msg.routing.ttl >= 2 {
                                broadcast_to_peers(&peers, &msg).await?;
                                tracing::info!("config changed forwarded to peers");
                            }
                            if is_gateway {
                                let _ = broadcast_lsa_direct(
                                    router_uuid,
                                    island_id,
                                    &nodes,
                                    &peer_nodes,
                                    &static_routes,
                                    &vpn_rules,
                                    &wan_peers,
                                    &lsa_seq,
                                )
                                .await;
                            }
                            continue;
                        }
                    }
                    let snapshot = refresh_config(
                        &config_reader,
                        &static_routes,
                        &vpn_rules,
                        &config_version,
                        island_id,
                        &nodes,
                        &peer_nodes,
                        &shm,
                        &fib,
                        &lsa_snapshot,
                        false,
                    )
                    .await;
                    let _ = refresh_lsa(
                        &lsa_reader,
                        &lsa_snapshot,
                        island_id,
                        &nodes,
                        &peer_nodes,
                        &static_routes,
                        &fib,
                    )
                    .await;
                    handle_message(
                        &msg,
                        &nodes,
                        &fib,
                        &peer_nodes,
                        &peer_routers,
                        &peers,
                        &wan_peers,
                        &opa,
                        &broadcast_cache,
                        router_uuid,
                        is_gateway,
                        &lsa_snapshot,
                        snapshot.as_ref(),
                    )
                    .await?;
                } else {
                    tracing::warn!("received invalid message frame");
                }
            }
            None => break,
        }
    }

    {
        let mut shm = shm.lock().await;
        let _ = shm.unregister_node(node_uuid);
    }
    {
        let mut nodes = nodes.lock().await;
        nodes.remove(&node_uuid);
    }
    rebuild_fib(&fib, &nodes, &peer_nodes, &static_routes, &lsa_snapshot).await;
    if is_gateway {
        let _ = broadcast_lsa_direct(
            router_uuid,
            island_id,
            &nodes,
            &peer_nodes,
            &static_routes,
            &vpn_rules,
            &wan_peers,
            &lsa_seq,
        )
        .await;
    }
    writer_task.abort();
    tracing::info!(node = %node_uuid, "node disconnected");
    Ok(())
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
    router_uuid: Uuid,
    is_gateway: bool,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    _snapshot: Option<&ConfigSnapshot>,
) -> Result<(), RouterError> {
    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    let mut senders: Vec<mpsc::UnboundedSender<Vec<u8>>> = Vec::new();
    let src_handle = {
        let nodes_guard = nodes.lock().await;
        nodes_guard.get(&src_uuid).cloned()
    };
    let Some(src_handle) = src_handle else {
        return Ok(());
    };

    if msg.routing.ttl == 0 {
        send_ttl_exceeded_to(msg, &src_handle.sender, router_uuid)?;
        return Ok(());
    }

    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            let Ok(dst_uuid) = Uuid::parse_str(dst) else {
                send_unreachable_to(msg, &src_handle.sender, router_uuid, "INVALID_DST")?;
                return Ok(());
            };
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
                    senders.push(dst_handle.sender);
                } else {
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
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
                            send_ttl_exceeded_to(msg, &src_handle.sender, router_uuid)?;
                            return Ok(());
                        }
                        if send_to_peer_router(peers, peer_node.router_uuid, msg).await? {
                            return Ok(());
                        }
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "PEER_UNAVAILABLE")?;
                    } else {
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
                    }
                } else {
                    let remote = {
                        let snapshot = lsa_snapshot.lock().await;
                        find_remote_node(&snapshot, dst_uuid)
                    };
                    if let Some(remote) = remote {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_handle.name,
                            src_handle.vpn_id,
                            &remote.name,
                            remote.vpn_id,
                        ) {
                            forward_to_island(
                                &remote.island_id,
                                msg,
                                is_gateway,
                                peer_routers,
                                peers,
                                wan_peers,
                                router_uuid,
                                &src_handle.sender,
                            )
                            .await?;
                        } else {
                            send_unreachable_to(msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "NODE_NOT_FOUND")?;
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
                let mut cache = broadcast_cache.lock().await;
                if !cache.check_and_add(trace_id) {
                    tracing::debug!(trace_id = %msg.routing.trace_id, "broadcast duplicate dropped");
                    return Ok(());
                }
            }
            let target = msg.meta.target.as_deref();
            let nodes_guard = nodes.lock().await;
            for (uuid, handle) in nodes_guard.iter() {
                if *uuid == src_uuid {
                    continue;
                }
                if !vpn_allows_between(
                    &msg.meta,
                    &src_handle.name,
                    src_handle.vpn_id,
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
                senders.push(handle.sender.clone());
            }
            if msg.routing.ttl >= 2 {
                broadcast_to_peers(peers, msg).await?;
            }
            if msg.routing.ttl >= 3 {
                forward_broadcast_to_wan(
                    msg,
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
            let resolved_target = match {
                let mut opa = opa.lock().await;
                opa.resolve_target(msg)
            } {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("opa resolve failed: {err}");
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, "OPA_ERROR")?;
                    return Ok(());
                }
            };
            let Some(target) = resolved_target.as_deref() else {
                send_unreachable_to(msg, &src_handle.sender, router_uuid, "OPA_NO_TARGET")?;
                return Ok(());
            };
            let nodes_guard = nodes.lock().await;
            let route = resolve_by_name(target, &src_handle, &nodes_guard, fib, &msg.meta).await?;
            match route {
                ResolvedRoute::Drop => {}
                ResolvedRoute::Unreachable(reason) => {
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, reason)?;
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
                            senders.push(dst_handle.sender.clone());
                        } else {
                            send_unreachable_to(msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if msg.routing.ttl <= 1 {
                        send_ttl_exceeded_to(msg, &src_handle.sender, router_uuid)?;
                        return Ok(());
                    }
                    if send_to_peer_router(peers, peer_uuid, msg).await? {
                        return Ok(());
                    }
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, "PEER_UNAVAILABLE")?;
                }
                ResolvedRoute::ForwardIsland(island_id) => {
                    forward_to_island(
                        &island_id,
                        msg,
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

    if !senders.is_empty() {
        let data = serde_json::to_vec(msg)?;
        for sender in senders {
            let _ = sender.send(data.clone());
        }
    }
    Ok(())
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
    island_id: &str,
    msg: &Message,
) -> Result<bool, RouterError> {
    let peer = {
        let peers_guard = wan_peers.lock().await;
        peers_guard.get(island_id).cloned()
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

async fn forward_to_island(
    island_id: &str,
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
        if send_to_wan_peer(wan_peers, island_id, msg).await? {
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

async fn build_local_lsa_payload(
    island_id: &str,
    seq: u64,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: &Arc<Mutex<Vec<VpnAssignment>>>,
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
            next_hop_island: route.next_hop_island,
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
    LsaPayload {
        island: island_id.to_string(),
        seq,
        timestamp: now_epoch_ms().to_string(),
        nodes,
        routes,
        vpns,
    }
}

async fn broadcast_lsa(
    ctx: &WanContext,
    target_island: Option<&str>,
) -> Result<(), RouterError> {
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
        &ctx.island_id,
        seq,
        &ctx.nodes,
        &ctx.peer_nodes,
        &ctx.static_routes,
        &ctx.vpn_rules,
    )
    .await;
    for (island, peer) in peers.iter() {
        if let Some(target) = target_island {
            if target != island {
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

async fn broadcast_lsa_direct(
    router_uuid: Uuid,
    island_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: &Arc<Mutex<Vec<VpnAssignment>>>,
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
    let payload =
        build_local_lsa_payload(island_id, seq, nodes, peer_nodes, static_routes, vpn_rules)
            .await;
    for (_island, peer) in peers {
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

async fn apply_lsa_payload(
    lsa_state: &Arc<Mutex<std::collections::HashMap<String, RemoteIslandState>>>,
    payload: LsaPayload,
) -> bool {
    let mut state_guard = lsa_state.lock().await;
    let entry = state_guard
        .entry(payload.island.clone())
        .or_default();
    if payload.seq <= entry.last_seq {
        return false;
    }
    entry.last_seq = payload.seq;
    entry.last_updated = now_epoch_ms();
    entry.flags &= !FLAG_STALE;
    entry.nodes = payload.nodes;
    entry.routes = payload.routes;
    entry.vpns = payload.vpns;
    true
}

async fn write_lsa_state(
    lsa_state: &Arc<Mutex<std::collections::HashMap<String, RemoteIslandState>>>,
    lsa_writer: &Arc<Mutex<Option<LsaRegionWriter>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
) {
    let mut writer_guard = lsa_writer.lock().await;
    let Some(writer) = writer_guard.as_mut() else {
        return;
    };
    let state_guard = lsa_state.lock().await;
    let mut islands: Vec<(String, RemoteIslandState)> = state_guard
        .iter()
        .map(|(name, state)| (name.clone(), state.clone()))
        .collect();
    islands.sort_by(|a, b| a.0.cmp(&b.0));
    drop(state_guard);

    let mut island_entries: Vec<RemoteIslandEntry> = Vec::new();
    let mut node_entries: Vec<RemoteNodeEntry> = Vec::new();
    let mut route_entries: Vec<RemoteRouteEntry> = Vec::new();
    let mut vpn_entries: Vec<RemoteVpnEntry> = Vec::new();

    for (index, (island_id, state)) in islands.iter().enumerate() {
        let mut island_entry = RemoteIslandEntry {
            island_id: [0u8; 64],
            island_id_len: 0,
            last_lsa_seq: state.last_seq,
            last_updated: state.last_updated,
            flags: state.flags,
            node_count: state.nodes.len() as u32,
            route_count: state.routes.len() as u32,
            vpn_count: state.vpns.len() as u32,
        };
        island_entry.island_id_len =
            copy_bytes_with_len(&mut island_entry.island_id, island_id) as u16;
        island_entries.push(island_entry);

        for node in state.nodes.iter() {
            let Ok(uuid) = Uuid::parse_str(&node.uuid) else {
                continue;
            };
            let mut entry = RemoteNodeEntry {
                uuid: *uuid.as_bytes(),
                name: [0u8; 256],
                name_len: 0,
                vpn_id: node.vpn_id,
                island_index: index as u16,
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
                next_hop_island: [0u8; 32],
                next_hop_island_len: 0,
                _pad: [0u8; 3],
                metric: route.metric,
                priority: 0,
                flags: FLAG_ACTIVE,
                island_index: index as u16,
                _reserved: [0u8; 14],
            };
            entry.prefix_len = copy_bytes_with_len(&mut entry.prefix, &route.prefix) as u16;
            entry.next_hop_island_len =
                copy_bytes_with_len(&mut entry.next_hop_island, &route.next_hop_island) as u8;
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
                island_index: index as u16,
                _reserved: [0u8; 18],
            };
            entry.pattern_len = copy_bytes_with_len(&mut entry.pattern, &vpn.pattern) as u16;
            vpn_entries.push(entry);
        }
    }

    if let Err(err) = writer.write_snapshot(
        &island_entries,
        &node_entries,
        &route_entries,
        &vpn_entries,
    ) {
        tracing::warn!("lsa snapshot write failed: {err}");
        return;
    }
    if let Some(snapshot) = writer.read_snapshot() {
        let mut guard = lsa_snapshot.lock().await;
        *guard = Some(snapshot);
    }
}

async fn lsa_refresh_loop(
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    island_id: &str,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
) {
    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        ticker.tick().await;
        let _ = refresh_lsa(
            &lsa_reader,
            &lsa_snapshot,
            island_id,
            &nodes,
            &peer_nodes,
            &static_routes,
            &fib,
        )
        .await;
    }
}

async fn lsa_broadcast_loop(ctx: Arc<WanContext>) {
    let mut ticker = time::interval(Duration::from_millis(ctx.hello_interval_ms));
    loop {
        ticker.tick().await;
        let _ = broadcast_lsa(&ctx, None).await;
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
            write_lsa_state(&ctx.lsa_state, &ctx.lsa_writer, &ctx.lsa_snapshot).await;
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
            if let Err(err) = handle_wan_connection(stream, ctx, false).await {
                tracing::warn!("wan connection error: {err}");
            }
        });
    }
}

async fn wan_connect_loop(address: String, ctx: Arc<WanContext>) {
    loop {
        match TcpStream::connect(&address).await {
            Ok(stream) => {
                tracing::info!(addr = %address, "wan connected");
                if let Err(err) = handle_wan_connection(stream, Arc::clone(&ctx), true).await {
                    tracing::warn!("wan uplink error: {err}");
                }
            }
            Err(err) => {
                tracing::warn!(addr = %address, "wan connect failed: {err}");
            }
        }
        time::sleep(Duration::from_secs(5)).await;
    }
}

async fn handle_wan_connection(
    stream: TcpStream,
    ctx: Arc<WanContext>,
    initiator: bool,
) -> Result<(), RouterError> {
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(err) = write_frame(&mut writer, &frame).await {
                tracing::warn!("wan write error: {err}");
                break;
            }
        }
    });

    let hello_payload = WanHelloPayload {
        protocol: "json-router/1.13".to_string(),
        router_id: ctx.router_uuid.to_string(),
        router_name: ctx.router_name.clone(),
        island_id: ctx.island_id.clone(),
        capabilities: vec!["lsa".to_string(), "forwarding".to_string()],
        timers: WanTimers {
            hello_interval_ms: ctx.hello_interval_ms,
            dead_interval_ms: ctx.dead_interval_ms,
        },
    };
    if initiator {
        let hello = build_wan_hello(&ctx.router_uuid.to_string(), &Uuid::new_v4().to_string(), hello_payload.clone());
        let _ = tx.send(serde_json::to_vec(&hello)?);
    }

    let frame = read_frame(&mut reader)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing HELLO"))?;
    let msg: Message = serde_json::from_slice(&frame)?;
    if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_HELLO) {
        writer_task.abort();
        return Ok(());
    }
    let peer_hello: WanHelloPayload = serde_json::from_value(msg.payload)?;
    if !ctx.authorized_islands.is_empty()
        && !ctx.authorized_islands.contains(&peer_hello.island_id)
    {
        let reject = build_wan_reject(
            &ctx.router_uuid.to_string(),
            &peer_hello.router_id,
            &Uuid::new_v4().to_string(),
            WanRejectPayload {
                reason: "ISLAND_NOT_AUTHORIZED".to_string(),
                message: format!("Island '{}' not authorized", peer_hello.island_id),
            },
        );
        let _ = tx.send(serde_json::to_vec(&reject)?);
        writer_task.abort();
        return Ok(());
    }

    if !initiator {
        let hello = build_wan_hello(&ctx.router_uuid.to_string(), &Uuid::new_v4().to_string(), hello_payload);
        let _ = tx.send(serde_json::to_vec(&hello)?);
    }

    let peer_uuid = Uuid::parse_str(&peer_hello.router_id)?;
    let accept = build_wan_accept(
        &ctx.router_uuid.to_string(),
        &peer_hello.router_id,
        &Uuid::new_v4().to_string(),
        WanAcceptPayload {
            peer_router_id: peer_hello.router_id.clone(),
            negotiated: WanNegotiated {
                protocol: "json-router/1.13".to_string(),
                hello_interval_ms: ctx.hello_interval_ms,
                dead_interval_ms: ctx.dead_interval_ms,
            },
        },
    );
    let _ = tx.send(serde_json::to_vec(&accept)?);

    {
        let mut guard = ctx.wan_peers.lock().await;
        guard.insert(
            peer_hello.island_id.clone(),
            WanPeer {
                sender: tx.clone(),
                router_uuid: peer_uuid,
                island_id: peer_hello.island_id.clone(),
            },
        );
    }

    let _ = broadcast_lsa(&ctx, Some(&peer_hello.island_id)).await;

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
                    handle_wan_message(&msg, &tx, &ctx).await?;
                }
            }
            None => break,
        }
    }
    {
        let mut guard = ctx.wan_peers.lock().await;
        guard.remove(&peer_hello.island_id);
    }
    writer_task.abort();
    Ok(())
}

async fn handle_wan_message(
    msg: &Message,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    ctx: &WanContext,
) -> Result<(), RouterError> {
    if msg.meta.msg_type == SYSTEM_KIND {
        if msg.meta.msg.as_deref() == Some(MSG_LSA) {
            let payload: LsaPayload = serde_json::from_value(msg.payload.clone())?;
            if apply_lsa_payload(&ctx.lsa_state, payload).await {
                write_lsa_state(&ctx.lsa_state, &ctx.lsa_writer, &ctx.lsa_snapshot).await;
            }
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
                            let data = serde_json::to_vec(msg)?;
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
    let src_info = {
        let snapshot = ctx.lsa_snapshot.lock().await;
        find_remote_node(&snapshot, src_uuid)
    };
    let Some(src_info) = src_info else {
        return Ok(());
    };

    if msg.routing.ttl == 0 {
        send_ttl_exceeded_to(msg, sender, ctx.router_uuid)?;
        return Ok(());
    }

    let mut senders: Vec<mpsc::UnboundedSender<Vec<u8>>> = Vec::new();
    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            let Ok(dst_uuid) = Uuid::parse_str(dst) else {
                send_unreachable_to(msg, sender, ctx.router_uuid, "INVALID_DST")?;
                return Ok(());
            };
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
                    senders.push(dst_handle.sender);
                } else {
                    send_unreachable_to(msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
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
                            send_ttl_exceeded_to(msg, sender, ctx.router_uuid)?;
                            return Ok(());
                        }
                        if send_to_peer_router(&ctx.peers, peer_node.router_uuid, msg).await? {
                            return Ok(());
                        }
                        send_unreachable_to(msg, sender, ctx.router_uuid, "PEER_UNAVAILABLE")?;
                    } else {
                        send_unreachable_to(msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                    }
                } else {
                    let remote = {
                        let snapshot = ctx.lsa_snapshot.lock().await;
                        find_remote_node(&snapshot, dst_uuid)
                    };
                    if let Some(remote) = remote {
                        if vpn_allows_between(
                            &msg.meta,
                            &src_info.name,
                            src_info.vpn_id,
                            &remote.name,
                            remote.vpn_id,
                        ) {
                            forward_to_island(
                                &remote.island_id,
                                msg,
                                true,
                                &ctx.peer_routers,
                                &ctx.peers,
                                &ctx.wan_peers,
                                ctx.router_uuid,
                                sender,
                            )
                            .await?;
                        } else {
                            send_unreachable_to(msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        send_unreachable_to(msg, sender, ctx.router_uuid, "NODE_NOT_FOUND")?;
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
                if !vpn_allows_between(
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
                senders.push(handle.sender.clone());
            }
            if msg.routing.ttl >= 2 {
                broadcast_to_peers(&ctx.peers, msg).await?;
            }
            if msg.routing.ttl >= 3 {
                broadcast_to_wan(&ctx.wan_peers, msg).await?;
            }
        }
        Destination::Resolve => {
            let resolved_target = match {
                let mut opa = ctx.opa.lock().await;
                opa.resolve_target(msg)
            } {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("opa resolve failed: {err}");
                    send_unreachable_to(msg, sender, ctx.router_uuid, "OPA_ERROR")?;
                    return Ok(());
                }
            };
            let Some(target) = resolved_target.as_deref() else {
                send_unreachable_to(msg, sender, ctx.router_uuid, "OPA_NO_TARGET")?;
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
                    send_unreachable_to(msg, sender, ctx.router_uuid, reason)?;
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
                            senders.push(dst_handle.sender.clone());
                        } else {
                            send_unreachable_to(msg, sender, ctx.router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        send_unreachable_to(msg, sender, ctx.router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if msg.routing.ttl <= 1 {
                        send_ttl_exceeded_to(msg, sender, ctx.router_uuid)?;
                        return Ok(());
                    }
                    if send_to_peer_router(&ctx.peers, peer_uuid, msg).await? {
                        return Ok(());
                    }
                    send_unreachable_to(msg, sender, ctx.router_uuid, "PEER_UNAVAILABLE")?;
                }
                ResolvedRoute::ForwardIsland(island_id) => {
                    forward_to_island(
                        &island_id,
                        msg,
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

    if !senders.is_empty() {
        let data = serde_json::to_vec(msg)?;
        for sender in senders {
            let _ = sender.send(data.clone());
        }
    }
    Ok(())
}

fn peer_socket_path(router_uuid: Uuid) -> PathBuf {
    let name = format!("irp-{}.sock", router_uuid.simple());
    PathBuf::from("/var/run/json-router").join(name)
}

async fn peer_discovery_loop(
    self_uuid: Uuid,
    self_router_name: &str,
    self_shm_name: &str,
    island_id: &str,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    config_version: Arc<Mutex<u64>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    opa: Arc<Mutex<OpaResolver>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    is_gateway: bool,
) {
    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        let regions = discover_peer_regions(self_uuid, self_shm_name, island_id);
        let mut updated_nodes: HashMap<Uuid, PeerNode> = HashMap::new();
        let mut peer_uuids: Vec<Uuid> = Vec::new();
        let mut updated_routers: HashMap<Uuid, PeerRouter> = HashMap::new();
        for region in regions {
            peer_uuids.push(region.router_uuid);
            updated_routers.insert(
                region.router_uuid,
                PeerRouter {
                    uuid: region.router_uuid,
                    name: region.router_name.clone(),
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
        rebuild_fib(&fib, &nodes, &peer_nodes, &static_routes, &lsa_snapshot).await;

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
                    let config_reader = Arc::clone(&config_reader);
                    let lsa_reader = Arc::clone(&lsa_reader);
                    let static_routes = Arc::clone(&static_routes);
                    let config_version = Arc::clone(&config_version);
                    let shm = Arc::clone(&shm);
                    let opa = Arc::clone(&opa);
                    let island_id = island_id.to_string();
                    let self_router_name = self_router_name.to_string();
                    let self_shm_name = self_shm_name.to_string();
                    let vpn_rules = Arc::clone(&vpn_rules);
                    let lsa_snapshot = Arc::clone(&lsa_snapshot);
                    let is_gateway = is_gateway;
                    tokio::spawn(async move {
                        let _ = connect_to_peer(
                            peer_uuid,
                            self_uuid,
                            &self_router_name,
                            &self_shm_name,
                            peers,
                            peer_nodes,
                            peer_routers,
                            nodes,
                            wan_peers,
                            config_reader,
                            lsa_reader,
                            static_routes,
                            vpn_rules,
                            config_version,
                            shm,
                            opa,
                            &island_id,
                            lsa_snapshot,
                            fib,
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

fn discover_peer_regions(
    self_uuid: Uuid,
    self_shm_name: &str,
    island_id: &str,
) -> Vec<PeerRegion> {
    let mut regions = Vec::new();
    let Ok(entries) = fs::read_dir("/dev/shm") else {
        return regions;
    };
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
        let reader = match RouterRegionReader::open_read_only(&shm_name) {
            Ok(reader) => reader,
            Err(_) => continue,
        };
        let Some(snapshot) = reader.read_snapshot() else {
            continue;
        };
        if snapshot.header.island_id != island_id {
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
            router_name: snapshot.header.router_name.clone(),
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
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    config_version: Arc<Mutex<u64>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    opa: Arc<Mutex<OpaResolver>>,
    island_id: &str,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    is_gateway: bool,
) -> Result<(), RouterError> {
    let socket_path = peer_socket_path(peer_uuid);
    let stream = UnixStream::connect(&socket_path).await?;
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut peers_guard = peers.lock().await;
        if peers_guard.contains_key(&peer_uuid) {
            return Ok(());
        }
        peers_guard.insert(peer_uuid, PeerHandle { sender: tx.clone() });
    }
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(err) = write_frame(&mut writer, &frame).await {
                tracing::warn!("peer write error: {err}");
                break;
            }
        }
    });

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

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
                    handle_peer_message(
                        &msg,
                        &peer_uuid,
                        &peers,
                        &peer_nodes,
                        &peer_routers,
                        &nodes,
                        &wan_peers,
                        &config_reader,
                        &lsa_reader,
                        &static_routes,
                        &vpn_rules,
                        &config_version,
                        &shm,
                        &opa,
                        island_id,
                        &fib,
                        self_uuid,
                        is_gateway,
                        &lsa_snapshot,
                    )
                    .await?;
                }
            }
            None => break,
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
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    lsa_reader: Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    config_version: Arc<Mutex<u64>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    opa: Arc<Mutex<OpaResolver>>,
    island_id: String,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    is_gateway: bool,
) -> Result<(), RouterError> {
    let (mut reader, mut writer) = stream.into_split();
    let frame = read_frame(&mut reader)
        .await?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing HELLO"))?;
    let msg: Message = serde_json::from_slice(&frame)?;
    if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_HELLO) {
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
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.insert(peer_uuid, PeerHandle { sender: tx.clone() });
    }
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if let Err(err) = write_frame(&mut writer, &frame).await {
                tracing::warn!("peer write error: {err}");
                break;
            }
        }
    });

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                if let Ok(msg) = serde_json::from_slice::<Message>(&frame) {
                    handle_peer_message(
                        &msg,
                        &peer_uuid,
                        &peers,
                        &peer_nodes,
                        &peer_routers,
                        &nodes,
                        &wan_peers,
                        &config_reader,
                        &lsa_reader,
                        &static_routes,
                        &vpn_rules,
                        &config_version,
                        &shm,
                        &opa,
                        &island_id,
                        &fib,
                        router_uuid,
                        is_gateway,
                        &lsa_snapshot,
                    )
                    .await?;
                }
            }
            None => break,
        }
    }
    {
        let mut peers_guard = peers.lock().await;
        peers_guard.remove(&peer_uuid);
    }
    writer_task.abort();
    Ok(())
}

async fn handle_peer_message(
    msg: &Message,
    peer_uuid: &Uuid,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    wan_peers: &Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    config_reader: &Arc<Mutex<Option<ConfigRegionReader>>>,
    lsa_reader: &Arc<Mutex<Option<LsaRegionReader>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: &Arc<Mutex<Vec<VpnAssignment>>>,
    config_version: &Arc<Mutex<u64>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    opa: &Arc<Mutex<OpaResolver>>,
    island_id: &str,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    router_uuid: Uuid,
    is_gateway: bool,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
) -> Result<(), RouterError> {
    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    if msg.meta.msg_type == SYSTEM_KIND
        && msg.meta.msg.as_deref() == Some(MSG_CONFIG_CHANGED)
    {
        let _ = refresh_config(
            config_reader,
            static_routes,
            vpn_rules,
            config_version,
            island_id,
            nodes,
            peer_nodes,
            shm,
            fib,
            lsa_snapshot,
            true,
        )
        .await;
        tracing::info!("config changed applied (peer)");
        return Ok(());
    }
    let src_node = {
        let peer_guard = peer_nodes.lock().await;
        peer_guard.get(&src_uuid).cloned()
    };
    let Some(src_node) = src_node else {
        if msg.meta.msg_type == SYSTEM_KIND {
            if let Some(msg_kind) = msg.meta.msg.as_deref() {
                if msg_kind == MSG_UNREACHABLE || msg_kind == MSG_TTL_EXCEEDED {
                    if let Destination::Unicast(dst) = &msg.routing.dst {
                        if let Ok(dst_uuid) = Uuid::parse_str(dst) {
                            let dst_handle = {
                                let nodes_guard = nodes.lock().await;
                                nodes_guard.get(&dst_uuid).cloned()
                            };
                            if let Some(dst_handle) = dst_handle {
                                let data = serde_json::to_vec(msg)?;
                                let _ = dst_handle.sender.send(data);
                            }
                        }
                    }
                }
            }
        }
        return Ok(());
    };
    let _ = refresh_lsa(
        lsa_reader,
        lsa_snapshot,
        island_id,
        nodes,
        peer_nodes,
        static_routes,
        fib,
    )
    .await;
    let mut senders: Vec<mpsc::UnboundedSender<Vec<u8>>> = Vec::new();
    if msg.routing.ttl == 0 {
        if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
            send_ttl_exceeded_to(msg, &peer.sender, router_uuid)?;
        }
        return Ok(());
    }

    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            let Ok(dst_uuid) = Uuid::parse_str(dst) else {
                return Ok(());
            };
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
                    senders.push(dst_handle.sender);
                } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    send_unreachable_to(msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                }
            } else {
                let remote = {
                    let snapshot = lsa_snapshot.lock().await;
                    find_remote_node(&snapshot, dst_uuid)
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
                            forward_to_island(
                                &remote.island_id,
                                msg,
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
                        send_unreachable_to(msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                    }
                } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    send_unreachable_to(msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
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
                if !vpn_allows_between(
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
                senders.push(handle.sender.clone());
            }
            if msg.routing.ttl >= 3 {
                if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    forward_broadcast_to_wan(
                        msg,
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
            let resolved_target = match {
                let mut opa = opa.lock().await;
                opa.resolve_target(msg)
            } {
                Ok(value) => value,
                Err(err) => {
                    tracing::warn!("opa resolve failed: {err}");
                    if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(msg, &peer.sender, router_uuid, "OPA_ERROR")?;
                    }
                    return Ok(());
                }
            };
            let Some(target) = resolved_target.as_deref() else {
                if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    send_unreachable_to(msg, &peer.sender, router_uuid, "OPA_NO_TARGET")?;
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
                        send_unreachable_to(msg, &peer.sender, router_uuid, reason)?;
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
                            senders.push(dst_handle.sender.clone());
                        } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            send_unreachable_to(msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if let Some(peer) = peers.lock().await.get(&peer_uuid).cloned() {
                        if msg.routing.ttl <= 1 {
                            send_ttl_exceeded_to(msg, &peer.sender, router_uuid)?;
                            return Ok(());
                        }
                    }
                    if send_to_peer_router(peers, peer_uuid, msg).await? {
                        return Ok(());
                    }
                    if let Some(peer) = peers.lock().await.get(&peer_uuid).cloned() {
                        send_unreachable_to(msg, &peer.sender, router_uuid, "PEER_UNAVAILABLE")?;
                    }
                }
                ResolvedRoute::ForwardIsland(island_id) => {
                    if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        forward_to_island(
                            &island_id,
                            msg,
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

    if !senders.is_empty() {
        let data = serde_json::to_vec(msg)?;
        for sender in senders {
            let _ = sender.send(data.clone());
        }
    }
    Ok(())
}

fn router_name(router_uuid: Uuid) -> String {
    format!("router:{}", router_uuid)
}

fn normalize_name(name: &str, island_id: &str) -> String {
    if name.contains('@') {
        name.to_string()
    } else {
        format!("{}@{}", name, island_id)
    }
}

struct RemoteNodeInfo {
    name: String,
    vpn_id: u32,
    island_id: String,
}

fn find_remote_node(snapshot: &Option<LsaSnapshot>, uuid: Uuid) -> Option<RemoteNodeInfo> {
    let snapshot = snapshot.as_ref()?;
    let mut islands: HashMap<u16, String> = HashMap::new();
    for (idx, island) in snapshot.islands.iter().enumerate() {
        if island.island_id_len == 0 {
            continue;
        }
        if island.flags & FLAG_STALE != 0 {
            continue;
        }
        let name = bytes_to_string(&island.island_id, island.island_id_len as usize).to_string();
        islands.insert(idx as u16, name);
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
        let island_id = islands.get(&node.island_index)?.to_string();
        let name = bytes_to_string(&node.name, node.name_len as usize).to_string();
        return Some(RemoteNodeInfo {
            name,
            vpn_id: node.vpn_id,
            island_id,
        });
    }
    None
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
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
    name: String,
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
    island_id: String,
}

#[derive(Clone, Debug, Default)]
struct RemoteIslandState {
    last_seq: u64,
    last_updated: u64,
    flags: u16,
    nodes: Vec<LsaNode>,
    routes: Vec<LsaRoute>,
    vpns: Vec<LsaVpn>,
}

struct WanContext {
    router_uuid: Uuid,
    router_name: String,
    island_id: String,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
    authorized_islands: Vec<String>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peer_routers: Arc<Mutex<std::collections::HashMap<Uuid, PeerRouter>>>,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    wan_peers: Arc<Mutex<std::collections::HashMap<String, WanPeer>>>,
    lsa_state: Arc<Mutex<std::collections::HashMap<String, RemoteIslandState>>>,
    lsa_writer: Arc<Mutex<Option<LsaRegionWriter>>>,
    lsa_snapshot: Arc<Mutex<Option<LsaSnapshot>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    vpn_rules: Arc<Mutex<Vec<VpnAssignment>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    broadcast_cache: Arc<Mutex<BroadcastCache>>,
    lsa_seq: Arc<Mutex<u64>>,
    opa: Arc<Mutex<OpaResolver>>,
}

#[derive(Clone, Debug)]
struct StaticRoute {
    pattern: String,
    match_kind: u8,
    action: u8,
    next_hop_island: String,
    priority: u16,
    metric: u32,
    installed_at: u64,
}

const ADMIN_DISTANCE_LOCAL: u8 = 0;
const ADMIN_DISTANCE_STATIC: u8 = 1;
const ADMIN_DISTANCE_LSA: u8 = 2;

#[derive(Clone, Debug)]
enum FibSource {
    LocalNode,
    PeerNode,
    StaticRoute,
    LsaNode,
    LsaRoute,
}

#[derive(Clone, Debug)]
enum FibNextHop {
    Local(Uuid),
    Router(Uuid),
    Island(String),
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
    ForwardIsland(String),
}

struct PeerRegion {
    router_uuid: Uuid,
    router_name: String,
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
        self.seen.retain(|_, ts| now.saturating_sub(*ts) <= self.ttl_ms);
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
    config_version: &Arc<Mutex<u64>>,
    island_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    force: bool,
) -> Option<ConfigSnapshot> {
    let mut last_snapshot = None;
    for attempt in 0..2 {
        let snapshot = {
            let mut reader = config_reader.lock().await;
            if force && attempt == 0 {
                *reader = None;
            }
            if reader.is_none() {
                if let Ok(new_reader) =
                    ConfigRegionReader::open_read_only(&format!("/jsr-config-{}", island_id))
                {
                    *reader = Some(new_reader);
                }
            }
            reader.as_ref().and_then(|r| r.read_snapshot())
        };
        let Some(snapshot) = snapshot else {
            return None;
        };

        let mut version_guard = config_version.lock().await;
        if force && attempt == 0 && snapshot.header.config_version == *version_guard {
            drop(version_guard);
            let mut reader = config_reader.lock().await;
            *reader = None;
            if let Ok(new_reader) =
                ConfigRegionReader::open_read_only(&format!("/jsr-config-{}", island_id))
            {
                *reader = Some(new_reader);
            }
            continue;
        }

        if force || snapshot.header.config_version != *version_guard {
            let mut routes_guard = static_routes.lock().await;
            *routes_guard = snapshot
                .routes
                .iter()
                .filter(|route| {
                    route.flags == 0 || (route.flags & FLAG_ACTIVE != 0)
                })
            .map(|route| StaticRoute {
                pattern: bytes_to_string(&route.prefix, route.prefix_len as usize).to_string(),
                match_kind: route.match_kind,
                action: route.action,
                next_hop_island: bytes_to_string(
                    &route.next_hop_island,
                    route.next_hop_island_len as usize,
                )
                .to_string(),
                priority: route.priority,
                metric: route.metric,
                installed_at: route.installed_at,
            })
            .collect();
        routes_guard.sort_by(|a, b| {
            (a.priority, a.metric).cmp(&(b.priority, b.metric))
        });
        drop(routes_guard);
        {
            let mut vpn_guard = vpn_rules.lock().await;
            *vpn_guard = snapshot.vpns.clone();
        }
        *version_guard = snapshot.header.config_version;
        tracing::info!(
            config_version = snapshot.header.config_version,
            routes = snapshot.routes.len(),
            vpns = snapshot.vpns.len(),
            "config snapshot updated"
        );
        reassign_vpns(&snapshot, nodes, shm).await;
        rebuild_fib(fib, nodes, peer_nodes, static_routes, lsa_snapshot).await;
    }
        last_snapshot = Some(snapshot);
        break;
    }
    last_snapshot
}

async fn refresh_lsa(
    lsa_reader: &Arc<Mutex<Option<LsaRegionReader>>>,
    lsa_snapshot: &Arc<Mutex<Option<LsaSnapshot>>>,
    island_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
) -> Option<LsaSnapshot> {
    let snapshot = {
        let mut reader = lsa_reader.lock().await;
        if reader.is_none() {
            if let Ok(new_reader) =
                LsaRegionReader::open_read_only(&format!("/jsr-lsa-{}", island_id))
            {
                *reader = Some(new_reader);
            }
        }
        reader.as_ref().and_then(|r| r.read_snapshot())
    };
    if let Some(snapshot) = snapshot.clone() {
        let mut guard = lsa_snapshot.lock().await;
        *guard = Some(snapshot.clone());
        rebuild_fib(fib, nodes, peer_nodes, static_routes, lsa_snapshot).await;
    }
    snapshot
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
        rules.push((entry.priority, pattern, entry.match_kind, entry.vpn_id, flags));
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
        let next_hop = if route.next_hop_island.is_empty() {
            None
        } else {
            Some(FibNextHop::Island(route.next_hop_island.clone()))
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
        let mut island_map: Vec<(String, u16, u16, u64)> = Vec::new();
        for (idx, island) in snapshot.islands.iter().enumerate() {
            if island.island_id_len == 0 {
                continue;
            }
            let name = bytes_to_string(&island.island_id, island.island_id_len as usize);
            if island.flags & FLAG_STALE != 0 {
                continue;
            }
            island_map.push((name.to_string(), idx as u16, island.flags, island.last_updated));
        }

        for node in snapshot.nodes.iter() {
            if node.name_len == 0 {
                continue;
            }
            if node.flags & (FLAG_DELETED | FLAG_STALE) != 0 {
                continue;
            }
            let island = island_map
                .iter()
                .find(|(_, idx, _, _)| *idx == node.island_index)
                .map(|(name, _, _, updated)| (name.clone(), *updated));
            let Some((island_id, updated_at)) = island else {
                continue;
            };
            let name = bytes_to_string(&node.name, node.name_len as usize).to_string();
            entries.push(FibEntry {
                pattern: name,
                match_kind: MATCH_EXACT,
                vpn_id: node.vpn_id,
                source: FibSource::LsaNode,
                next_hop: Some(FibNextHop::Island(island_id)),
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
            let island = island_map
                .iter()
                .find(|(_, idx, _, _)| *idx == route.island_index)
                .map(|(name, _, _, updated)| (name.clone(), *updated));
            let Some((island_id, updated_at)) = island else {
                continue;
            };
            let prefix = bytes_to_string(&route.prefix, route.prefix_len as usize).to_string();
            let mut next_hop_island =
                bytes_to_string(&route.next_hop_island, route.next_hop_island_len as usize)
                    .to_string();
            if next_hop_island.is_empty() {
                next_hop_island = island_id.clone();
            }
            let next_hop = if next_hop_island.is_empty() {
                None
            } else {
                Some(FibNextHop::Island(next_hop_island))
            };
            entries.push(FibEntry {
                pattern: prefix,
                match_kind: route.match_kind,
                vpn_id: 0,
                source: FibSource::LsaRoute,
                next_hop,
                admin_distance: ADMIN_DISTANCE_LSA,
                priority: route.priority,
                metric: route.metric,
                installed_at: updated_at,
                action: route.action,
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

fn is_global_scope(meta: &Meta) -> bool {
    matches!(meta.scope.as_deref(), Some(SCOPE_GLOBAL))
}

fn vpn_allows(src_name: &str, src_vpn: u32, dst_vpn: u32) -> bool {
    if is_system_node(src_name) {
        return true;
    }
    src_vpn == dst_vpn
}

fn vpn_allows_between(
    meta: &Meta,
    src_name: &str,
    src_vpn: u32,
    dst_name: &str,
    dst_vpn: u32,
) -> bool {
    if meta.msg_type == SYSTEM_KIND && is_global_scope(meta) {
        return true;
    }
    if is_system_node(src_name) || is_system_node(dst_name) {
        return true;
    }
    src_vpn == dst_vpn
}

fn can_route(src: &NodeHandle, dst: &NodeHandle) -> bool {
    vpn_allows(&src.name, src.vpn_id, dst.vpn_id)
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
        if let Some(src_island) = extract_island(&src.name) {
            let local_target = format!("{}@{}", base, src_island);
            let local = resolve_by_name_inner(&local_target, src, nodes, fib, meta).await?;
            if !matches!(local, ResolvedRoute::Unreachable("NODE_NOT_FOUND")) {
                return Ok(local);
            }
        }
        return resolve_by_name_any_island(base, src, nodes, fib, meta).await;
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
            FibSource::StaticRoute => {
                match entry.action {
                    ACTION_DROP => return Ok(ResolvedRoute::Drop),
                    ACTION_FORWARD => {
                        if let Some(FibNextHop::Island(island)) = &entry.next_hop {
                            return Ok(ResolvedRoute::ForwardIsland(island.clone()));
                        }
                        continue;
                    }
                    _ => continue,
                }
            }
            FibSource::LsaRoute => match entry.action {
                ACTION_DROP => return Ok(ResolvedRoute::Drop),
                ACTION_FORWARD => {
                    if let Some(FibNextHop::Island(island)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardIsland(island.clone()));
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
            FibSource::LsaNode => {
                let Some(FibNextHop::Island(island)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows_between(meta, &src.name, src.vpn_id, &entry.pattern, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardIsland(island.clone()));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
        }
    }
    Ok(ResolvedRoute::Unreachable("NODE_NOT_FOUND"))
}

async fn resolve_by_name_any_island(
    base: &str,
    src: &NodeHandle,
    nodes: &std::collections::HashMap<Uuid, NodeHandle>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    meta: &Meta,
) -> Result<ResolvedRoute, RouterError> {
    let fib_guard = fib.lock().await;
    for entry in fib_guard.iter() {
        let matched = match entry.source {
            FibSource::LocalNode | FibSource::PeerNode | FibSource::LsaNode => {
                match_any_island(base, &entry.pattern)
            }
            FibSource::StaticRoute | FibSource::LsaRoute => {
                let target = format!("{}@*", base);
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
                    if let Some(FibNextHop::Island(island)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardIsland(island.clone()));
                    }
                    continue;
                }
                _ => continue,
            },
            FibSource::LsaRoute => match entry.action {
                ACTION_DROP => return Ok(ResolvedRoute::Drop),
                ACTION_FORWARD => {
                    if let Some(FibNextHop::Island(island)) = &entry.next_hop {
                        return Ok(ResolvedRoute::ForwardIsland(island.clone()));
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
                if vpn_allows_between(meta, &src.name, src.vpn_id, &dst_handle.name, dst_handle.vpn_id) {
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
            FibSource::LsaNode => {
                let Some(FibNextHop::Island(island)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows_between(meta, &src.name, src.vpn_id, &entry.pattern, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardIsland(island.clone()));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
        }
    }
    Ok(ResolvedRoute::Unreachable("NODE_NOT_FOUND"))
}

fn extract_island(name: &str) -> Option<&str> {
    name.split_once('@').map(|(_, island)| island)
}

fn match_any_island(base: &str, full_name: &str) -> bool {
    let Some((name_base, _)) = full_name.split_once('@') else {
        return false;
    };
    if base.contains('*') {
        return wildcard_match(base.as_bytes(), name_base.as_bytes());
    }
    name_base == base
}
