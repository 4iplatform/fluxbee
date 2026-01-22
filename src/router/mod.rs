use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{self, Duration};
use uuid::Uuid;

use crate::config::RouterConfig;
use crate::protocol::{
    build_announce, build_router_hello, build_ttl_exceeded, build_unreachable, Destination,
    Message, NodeAnnouncePayload, NodeHelloPayload, RouterHelloPayload, MSG_CONFIG_CHANGED,
    MSG_HELLO, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_WITHDRAW, SYSTEM_KIND,
};
use crate::shm::{
    now_epoch_ms, ConfigRegionReader, ConfigSnapshot, RouterRegionReader, RouterRegionWriter,
    ACTION_DROP, ACTION_FORWARD, FLAG_ACTIVE, FLAG_DELETED, FLAG_STALE, HEARTBEAT_INTERVAL_MS,
    HEARTBEAT_STALE_MS, MATCH_EXACT, MATCH_GLOB, MATCH_PREFIX,
};
use crate::socket::connection::{read_frame, write_frame};

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
}

pub struct Router {
    cfg: RouterConfig,
    shm: Arc<Mutex<RouterRegionWriter>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_version: Arc<Mutex<u64>>,
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
        Self {
            cfg,
            shm: Arc::new(Mutex::new(shm)),
            nodes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            peer_nodes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            peers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            config_reader: Arc::new(Mutex::new(config_reader)),
            static_routes: Arc::new(Mutex::new(Vec::new())),
            fib: Arc::new(Mutex::new(Vec::new())),
            config_version: Arc::new(Mutex::new(0)),
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
        tokio::spawn(async move {
            let mut ticker = time::interval(Duration::from_millis(HEARTBEAT_INTERVAL_MS));
            loop {
                ticker.tick().await;
                let mut shm = shm.lock().await;
                shm.update_heartbeat();
            }
        });

        let peers = Arc::clone(&self.peers);
        let peer_nodes = Arc::clone(&self.peer_nodes);
        let nodes = Arc::clone(&self.nodes);
        let static_routes = Arc::clone(&self.static_routes);
        let fib = Arc::clone(&self.fib);
        let config_reader = Arc::clone(&self.config_reader);
        let config_version = Arc::clone(&self.config_version);
        let shm = Arc::clone(&self.shm);
        let router_uuid = self.cfg.router_uuid;
        let router_name = self.cfg.router_l2_name.clone();
        let shm_name = self.cfg.shm_name.clone();
        let island_id = self.cfg.island_id.clone();
        tokio::spawn(async move {
            peer_discovery_loop(
                router_uuid,
                &router_name,
                &shm_name,
                &island_id,
                peers,
                peer_nodes,
                nodes,
                config_reader,
                static_routes,
                config_version,
                shm,
                fib,
            )
            .await;
        });

        let peers = Arc::clone(&self.peers);
        let peer_nodes = Arc::clone(&self.peer_nodes);
        let nodes = Arc::clone(&self.nodes);
        let fib = Arc::clone(&self.fib);
        let config_reader = Arc::clone(&self.config_reader);
        let static_routes = Arc::clone(&self.static_routes);
        let config_version = Arc::clone(&self.config_version);
        let shm = Arc::clone(&self.shm);
        let island_id = self.cfg.island_id.clone();
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
                let nodes = Arc::clone(&nodes);
                let fib = Arc::clone(&fib);
                let config_reader = Arc::clone(&config_reader);
                let static_routes = Arc::clone(&static_routes);
                let config_version = Arc::clone(&config_version);
                let shm = Arc::clone(&shm);
                let island_id = island_id.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_peer_incoming(
                        stream,
                        router_uuid,
                        peers,
                        peer_nodes,
                        nodes,
                        fib,
                        config_reader,
                        static_routes,
                        config_version,
                        shm,
                        island_id,
                    )
                    .await
                    {
                        tracing::warn!("peer connection error: {err}");
                    }
                });
            }
        });

        loop {
            let (stream, _) = listener.accept().await?;
            tracing::info!("incoming connection accepted");
            let shm = Arc::clone(&self.shm);
            let router_name = self.cfg.router_l2_name.clone();
            let router_uuid = self.cfg.router_uuid;
            let island_id = self.cfg.island_id.clone();
            let nodes = Arc::clone(&self.nodes);
            let peer_nodes = Arc::clone(&self.peer_nodes);
            let peers = Arc::clone(&self.peers);
            let config_reader = Arc::clone(&self.config_reader);
            let static_routes = Arc::clone(&self.static_routes);
            let fib = Arc::clone(&self.fib);
            let config_version = Arc::clone(&self.config_version);
            tokio::spawn(async move {
                if let Err(err) = handle_node(
                    stream,
                    shm,
                    nodes,
                    peer_nodes,
                    peers,
                    config_reader,
                    static_routes,
                    fib,
                    config_version,
                    router_uuid,
                    &router_name,
                    &island_id,
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
    peers: Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_version: Arc<Mutex<u64>>,
    router_uuid: Uuid,
    router_name: &str,
    island_id: &str,
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
        &config_version,
        island_id,
        &nodes,
        &peer_nodes,
        &shm,
        &fib,
        false,
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
    rebuild_fib(&fib, &nodes, &peer_nodes, &static_routes).await;

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
                                &config_version,
                                island_id,
                                &nodes,
                                &peer_nodes,
                                &shm,
                                &fib,
                                true,
                            )
                            .await;
                            tracing::info!("config changed applied");
                            continue;
                        }
                    }
                    let snapshot = refresh_config(
                        &config_reader,
                        &static_routes,
                        &config_version,
                        island_id,
                        &nodes,
                        &peer_nodes,
                        &shm,
                        &fib,
                        false,
                    )
                    .await;
                    handle_message(
                        &msg,
                        &nodes,
                        &fib,
                        &peer_nodes,
                        &peers,
                        router_uuid,
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
    rebuild_fib(&fib, &nodes, &peer_nodes, &static_routes).await;
    writer_task.abort();
    tracing::info!(node = %node_uuid, "node disconnected");
    Ok(())
}

async fn handle_message(
    msg: &Message,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    peers: &Arc<Mutex<std::collections::HashMap<Uuid, PeerHandle>>>,
    router_uuid: Uuid,
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
                if vpn_allows(&src_handle.name, src_handle.vpn_id, dst_handle.vpn_id) {
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
                    if vpn_allows(&src_handle.name, src_handle.vpn_id, peer_node.vpn_id) {
                        if send_to_peer_router(peers, peer_node.router_uuid, msg).await? {
                            return Ok(());
                        }
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "PEER_UNAVAILABLE")?;
                    } else {
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
                    }
                } else {
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, "NODE_NOT_FOUND")?;
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
                if !vpn_allows(&src_handle.name, src_handle.vpn_id, handle.vpn_id) {
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
        }
        Destination::Resolve => {
            let Some(target) = msg.meta.target.as_deref() else {
                send_unreachable_to(msg, &src_handle.sender, router_uuid, "MISSING_TARGET")?;
                return Ok(());
            };
            let nodes_guard = nodes.lock().await;
            let route = resolve_by_name(target, &src_handle, &nodes_guard, fib).await?;
            match route {
                ResolvedRoute::Drop => {}
                ResolvedRoute::Unreachable(reason) => {
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, reason)?;
                }
                ResolvedRoute::Deliver(dst_uuid) => {
                    if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                        if vpn_allows(&src_handle.name, src_handle.vpn_id, dst_handle.vpn_id) {
                            senders.push(dst_handle.sender.clone());
                        } else {
                            send_unreachable_to(msg, &src_handle.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else {
                        send_unreachable_to(msg, &src_handle.sender, router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                ResolvedRoute::ForwardRouter(peer_uuid) => {
                    if send_to_peer_router(peers, peer_uuid, msg).await? {
                        return Ok(());
                    }
                    send_unreachable_to(msg, &src_handle.sender, router_uuid, "PEER_UNAVAILABLE")?;
                }
                ResolvedRoute::ForwardIsland(_) => {
                    send_unreachable_to(
                        msg,
                        &src_handle.sender,
                        router_uuid,
                        "INTER_ISLAND_UNSUPPORTED",
                    )?;
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
    let data = serde_json::to_vec(msg)?;
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
    let data = serde_json::to_vec(msg)?;
    for peer in peers_guard.values() {
        let _ = peer.sender.send(data.clone());
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
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    config_version: Arc<Mutex<u64>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
) {
    let mut ticker = time::interval(Duration::from_secs(5));
    loop {
        let regions = discover_peer_regions(self_uuid, self_shm_name, island_id);
        let mut updated_nodes: HashMap<Uuid, PeerNode> = HashMap::new();
        let mut peer_uuids: Vec<Uuid> = Vec::new();
        for region in regions {
            peer_uuids.push(region.router_uuid);
            for node in region.nodes {
                updated_nodes.insert(node.uuid, node);
            }
        }
        {
            let mut peer_guard = peer_nodes.lock().await;
            *peer_guard = updated_nodes;
        }
        rebuild_fib(&fib, &nodes, &peer_nodes, &static_routes).await;

        for peer_uuid in peer_uuids {
            if should_initiate_peer(self_uuid, peer_uuid) {
                let already_connected = {
                    let peers_guard = peers.lock().await;
                    peers_guard.contains_key(&peer_uuid)
                };
                if !already_connected {
                    let peers = Arc::clone(&peers);
                    let peer_nodes = Arc::clone(&peer_nodes);
                    let nodes = Arc::clone(&nodes);
                    let fib = Arc::clone(&fib);
                    let config_reader = Arc::clone(&config_reader);
                    let static_routes = Arc::clone(&static_routes);
                    let config_version = Arc::clone(&config_version);
                    let shm = Arc::clone(&shm);
                    let island_id = island_id.to_string();
                    let self_router_name = self_router_name.to_string();
                    let self_shm_name = self_shm_name.to_string();
                    tokio::spawn(async move {
                        let _ = connect_to_peer(
                            peer_uuid,
                            self_uuid,
                            &self_router_name,
                            &self_shm_name,
                            peers,
                            peer_nodes,
                            nodes,
                            config_reader,
                            static_routes,
                            config_version,
                            shm,
                            &island_id,
                            fib,
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
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    config_version: Arc<Mutex<u64>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    island_id: &str,
    fib: Arc<Mutex<Vec<FibEntry>>>,
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
                        &nodes,
                        &config_reader,
                        &static_routes,
                        &config_version,
                        &shm,
                        island_id,
                        &fib,
                        self_uuid,
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
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    fib: Arc<Mutex<Vec<FibEntry>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: Arc<Mutex<Vec<StaticRoute>>>,
    config_version: Arc<Mutex<u64>>,
    shm: Arc<Mutex<RouterRegionWriter>>,
    island_id: String,
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
                        &nodes,
                        &config_reader,
                        &static_routes,
                        &config_version,
                        &shm,
                        &island_id,
                        &fib,
                        router_uuid,
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
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    config_reader: &Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    config_version: &Arc<Mutex<u64>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    island_id: &str,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
    router_uuid: Uuid,
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
            config_version,
            island_id,
            nodes,
            peer_nodes,
            shm,
            fib,
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
                if vpn_allows(&src_node.name, src_node.vpn_id, dst_handle.vpn_id) {
                    senders.push(dst_handle.sender);
                } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                    send_unreachable_to(msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                }
            } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                send_unreachable_to(msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
            }
        }
        Destination::Broadcast => {
            let target = msg.meta.target.as_deref();
            let nodes_guard = nodes.lock().await;
            for (uuid, handle) in nodes_guard.iter() {
                if *uuid == src_uuid {
                    continue;
                }
                if !vpn_allows(&src_node.name, src_node.vpn_id, handle.vpn_id) {
                    continue;
                }
                if let Some(pattern) = target {
                    if !pattern_match(pattern, &handle.name) {
                        continue;
                    }
                }
                senders.push(handle.sender.clone());
            }
        }
        Destination::Resolve => {
            let Some(target) = msg.meta.target.as_deref() else {
                return Ok(());
            };
            let nodes_guard = nodes.lock().await;
            let route = resolve_by_name(target, &NodeHandle {
                name: src_node.name.clone(),
                vpn_id: src_node.vpn_id,
                sender: mpsc::unbounded_channel().0,
                connected_at: 0,
            }, &nodes_guard, fib).await?;
            match route {
                ResolvedRoute::Drop => {}
                ResolvedRoute::Deliver(dst_uuid) => {
                    if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                        if vpn_allows(&src_node.name, src_node.vpn_id, dst_handle.vpn_id) {
                            senders.push(dst_handle.sender.clone());
                        } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                            send_unreachable_to(msg, &peer.sender, router_uuid, "VPN_BLOCKED")?;
                        }
                    } else if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
                    }
                }
                _ => {
                    if let Some(peer) = peers.lock().await.get(peer_uuid).cloned() {
                        send_unreachable_to(msg, &peer.sender, router_uuid, "NODE_NOT_FOUND")?;
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

#[derive(Clone)]
struct PeerHandle {
    sender: mpsc::UnboundedSender<Vec<u8>>,
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

#[derive(Clone, Debug)]
enum FibSource {
    LocalNode,
    PeerNode,
    StaticRoute,
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
    nodes: Vec<PeerNode>,
}

async fn refresh_config(
    config_reader: &Arc<Mutex<Option<ConfigRegionReader>>>,
    static_routes: &Arc<Mutex<Vec<StaticRoute>>>,
    config_version: &Arc<Mutex<u64>>,
    island_id: &str,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    peer_nodes: &Arc<Mutex<std::collections::HashMap<Uuid, PeerNode>>>,
    shm: &Arc<Mutex<RouterRegionWriter>>,
    fib: &Arc<Mutex<Vec<FibEntry>>>,
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
        *version_guard = snapshot.header.config_version;
        tracing::info!(
            config_version = snapshot.header.config_version,
            routes = snapshot.routes.len(),
            vpns = snapshot.vpns.len(),
            "config snapshot updated"
        );
        reassign_vpns(&snapshot, nodes, shm).await;
        rebuild_fib(fib, nodes, peer_nodes, static_routes).await;
    }
        last_snapshot = Some(snapshot);
        break;
    }
    last_snapshot
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

fn vpn_allows(src_name: &str, src_vpn: u32, dst_vpn: u32) -> bool {
    if is_system_node(src_name) {
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
            FibSource::LocalNode => {
                let Some(FibNextHop::Local(dst_uuid)) = &entry.next_hop else {
                    continue;
                };
                let Some(dst_handle) = nodes.get(dst_uuid) else {
                    continue;
                };
                if vpn_allows(&src.name, src.vpn_id, dst_handle.vpn_id) {
                    return Ok(ResolvedRoute::Deliver(*dst_uuid));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
            FibSource::PeerNode => {
                let Some(FibNextHop::Router(peer_uuid)) = &entry.next_hop else {
                    continue;
                };
                if vpn_allows(&src.name, src.vpn_id, entry.vpn_id) {
                    return Ok(ResolvedRoute::ForwardRouter(*peer_uuid));
                }
                return Ok(ResolvedRoute::Unreachable("VPN_BLOCKED"));
            }
        }
    }
    Ok(ResolvedRoute::Unreachable("NODE_NOT_FOUND"))
}
