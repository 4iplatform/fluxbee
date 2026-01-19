pub mod forward;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time::{self, Duration, Instant};
use uuid::Uuid;

use crate::config::Config;
use crate::protocol::system::{
    build_error, build_hello, build_lsa, build_query, build_sync_reply, build_sync_request,
    build_uplink_accept, build_uplink_reject, AnnouncePayload, HelloCapabilities, HelloPayload,
    LsaPayload, Message, NodeDescriptor, SyncReplyPayload, SyncRequestPayload, UplinkAcceptPayload,
    UplinkRejectPayload, WithdrawPayload, MSG_ANNOUNCE, MSG_HELLO, MSG_LSA, MSG_SYNC_REPLY,
    MSG_SYNC_REQUEST, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, MSG_UPLINK_ACCEPT, MSG_UPLINK_REJECT,
    MSG_WITHDRAW, SYSTEM_KIND,
};
use crate::shm::{
    ShmError, ShmReader, ShmSnapshot, ShmWriter, AD_LSA, FLAG_ACTIVE, LINK_LOCAL, MATCH_EXACT,
    MATCH_GLOB, MATCH_PREFIX, ROUTE_LSA,
};
use crate::socket::connection::{read_frame, write_frame};
use tracing::{debug, info, warn};

const ANNOUNCE_TIMEOUT_MS: u64 = 5_000;
const BROADCAST_CACHE_TTL_SECS: u64 = 60;
const CONFIG_SHM_PREFIX: &str = "/jsr-config-";

pub struct Router {
    cfg: Config,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    local_snapshot: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    fabric: Arc<Mutex<FabricManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
}

impl Router {
    pub fn new(cfg: Config) -> Result<Self, ShmError> {
        let shm = ShmWriter::open_or_create(cfg.router_uuid, &cfg.island_id, &cfg.shm_name)?;
        Ok(Self {
            cfg,
            shm: Arc::new(Mutex::new(shm)),
            fib: Arc::new(Mutex::new(Vec::new())),
            nodes: Arc::new(Mutex::new(HashMap::new())),
            local_snapshot: Arc::new(Mutex::new(None)),
            uplinks: Arc::new(Mutex::new(UplinkManager::new())),
            fabric: Arc::new(Mutex::new(FabricManager::new())),
            broadcast_cache: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub async fn run(self) -> Result<(), ShmError> {
        let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();
        let mut connected: HashSet<PathBuf> = HashSet::new();
        let mut ticker = time::interval(Duration::from_millis(self.cfg.scan_interval_ms));
        let mut fib_ticker = time::interval(Duration::from_millis(self.cfg.fib_refresh_ms));
        let mut heartbeat_ticker =
            time::interval(Duration::from_millis(self.cfg.heartbeat_interval_ms));
        let mut peers = PeerManager::new(self.cfg.peer_shm_names.clone());
        let autodiscover_peers = self.cfg.peer_shm_names.is_empty();
        let fib = Arc::clone(&self.fib);
        let nodes = Arc::clone(&self.nodes);
        let snapshot_cache = Arc::clone(&self.local_snapshot);
        let uplinks = Arc::clone(&self.uplinks);
        let fabric = Arc::clone(&self.fabric);
        let broadcast_cache = Arc::clone(&self.broadcast_cache);
        let local_router_uuid = self.cfg.router_uuid;

        if let Some(addr) = self.cfg.uplink_listen.clone() {
            let shm = Arc::clone(&self.shm);
            let fib = Arc::clone(&self.fib);
            let nodes = Arc::clone(&self.nodes);
            let snapshot_cache = Arc::clone(&self.local_snapshot);
            let uplinks = Arc::clone(&self.uplinks);
            let broadcast_cache = Arc::clone(&self.broadcast_cache);
            let fabric = Arc::clone(&self.fabric);
            let hello_interval_ms = self.cfg.hello_interval_ms;
            let dead_interval_ms = self.cfg.dead_interval_ms;
            tokio::spawn(async move {
                if let Err(err) = uplink_listen_loop(
                    addr,
                    shm,
                    fib,
                    nodes,
                    snapshot_cache,
                    uplinks,
                    broadcast_cache,
                    fabric,
                    local_router_uuid,
                    hello_interval_ms,
                    dead_interval_ms,
                )
                .await
                {
                    warn!(target: "json_router", "uplink listen error: {}", err);
                }
            });
        }

        if !self.cfg.uplink_peers.is_empty() {
            let shm = Arc::clone(&self.shm);
            let fib = Arc::clone(&self.fib);
            let nodes = Arc::clone(&self.nodes);
            let snapshot_cache = Arc::clone(&self.local_snapshot);
            let uplinks = Arc::clone(&self.uplinks);
            let peers_cfg = self.cfg.uplink_peers.clone();
            let broadcast_cache = Arc::clone(&self.broadcast_cache);
            let fabric = Arc::clone(&self.fabric);
            let hello_interval_ms = self.cfg.hello_interval_ms;
            let dead_interval_ms = self.cfg.dead_interval_ms;
            tokio::spawn(async move {
                uplink_connect_loop(
                    peers_cfg,
                    shm,
                    fib,
                    nodes,
                    snapshot_cache,
                    uplinks,
                    broadcast_cache,
                    fabric,
                    local_router_uuid,
                    hello_interval_ms,
                    dead_interval_ms,
                )
                .await;
            });
        }

        {
            let fabric = Arc::clone(&self.fabric);
            let shm = Arc::clone(&self.shm);
            let fib = Arc::clone(&self.fib);
            let nodes = Arc::clone(&self.nodes);
            let snapshot_cache = Arc::clone(&self.local_snapshot);
            let uplinks = Arc::clone(&self.uplinks);
            let broadcast_cache = Arc::clone(&self.broadcast_cache);
            let socket_path = fabric_socket_path(&self.cfg.node_socket_dir, &self.cfg.router_uuid);
            tokio::spawn(async move {
                if let Err(err) = fabric_listen_loop(
                    socket_path,
                    shm,
                    fib,
                    nodes,
                    snapshot_cache,
                    uplinks,
                    fabric,
                    broadcast_cache,
                )
                .await
                {
                    warn!(target: "json_router", "fabric listen error: {}", err);
                }
            });
        }

        loop {
            tokio::select! {
                _ = heartbeat_ticker.tick() => {
                    let mut shm = self.shm.lock().await;
                    shm.update_heartbeat();
                }
                _ = ticker.tick() => {
                    let new_paths = scan_node_sockets(&self.cfg.node_socket_dir).await;
                    for path in new_paths {
                        if connected.contains(&path) {
                            continue;
                        }
                        match UnixStream::connect(&path).await {
                            Ok(stream) => {
                                connected.insert(path.clone());
                                let shm = Arc::clone(&self.shm);
                                let tx = tx.clone();
                                let fib = Arc::clone(&fib);
                                let nodes = Arc::clone(&nodes);
                                let snapshot_cache = Arc::clone(&snapshot_cache);
                                let uplinks = Arc::clone(&uplinks);
                                let fabric = Arc::clone(&fabric);
                                let broadcast_cache = Arc::clone(&broadcast_cache);
                                tokio::spawn(async move {
                                    handle_node_connection(
                                        path.clone(),
                                        stream,
                                        shm,
                                        fib,
                                        nodes,
                                        snapshot_cache,
                                        uplinks,
                                        fabric,
                                        broadcast_cache,
                                        local_router_uuid,
                                    )
                                    .await;
                                    let _ = tx.send(path);
                                });
                            }
                            Err(_) => {}
                        }
                    }
                }
                Some(path) = rx.recv() => {
                    connected.remove(&path);
                }
                _ = fib_ticker.tick() => {
                    let snapshot = {
                        let shm = self.shm.lock().await;
                        shm.read_snapshot()
                    };
                    if autodiscover_peers {
                        let names = discover_peer_shm_names(&self.cfg);
                        peers.update(names);
                    }
                    let peer_snapshots = peers.refresh();
                    if !peer_snapshots.is_empty() {
                        for peer in &peer_snapshots {
                            let peer_uuid = match Uuid::from_slice(&peer.router_uuid) {
                                Ok(uuid) => uuid,
                                Err(_) => continue,
                            };
                            if peer_uuid == local_router_uuid {
                                continue;
                            }
                            let should_connect = {
                                let mut fabric = fabric.lock().await;
                                fabric.ensure_connect(peer_uuid)
                            };
                            if !should_connect {
                                continue;
                            }
                            let socket_path =
                                fabric_socket_path(&self.cfg.node_socket_dir, &peer_uuid);
                            let shm = Arc::clone(&self.shm);
                            let fib = Arc::clone(&self.fib);
                            let nodes = Arc::clone(&self.nodes);
                            let snapshot_cache = Arc::clone(&self.local_snapshot);
                            let uplinks = Arc::clone(&self.uplinks);
                            let fabric = Arc::clone(&self.fabric);
                            let broadcast_cache = Arc::clone(&self.broadcast_cache);
                            tokio::spawn(async move {
                                fabric_connect_loop(
                                    peer_uuid,
                                    socket_path,
                                    shm,
                                    fib,
                                    nodes,
                                    snapshot_cache,
                                    uplinks,
                                    fabric,
                                    broadcast_cache,
                                    local_router_uuid,
                                )
                                .await;
                            });
                        }
                    }
                    {
                        let mut fabric = fabric.lock().await;
                        fabric.refresh_peer_nodes(&peer_snapshots);
                    }
                    if let Some(local_snapshot) = snapshot {
                        let uplink_nodes = {
                            let uplinks = uplinks.lock().await;
                            uplinks.snapshot_nodes()
                        };
                        let mut new_fib =
                            build_fib(&local_snapshot, &peer_snapshots, &uplink_nodes);
                        new_fib.sort_by(route_cmp);
                        info!(
                            target: "json_router",
                            fib_routes = new_fib.len(),
                            peers = peer_snapshots.len(),
                            uplink_nodes = uplink_nodes.len(),
                            "FIB refreshed"
                        );
                        let mut fib = fib.lock().await;
                        *fib = new_fib;
                        let mut cached = snapshot_cache.lock().await;
                        *cached = Some(local_snapshot);
                    }
                }
            }
        }
    }
}

async fn scan_node_sockets(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(_) => return out,
    };

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("sock") {
            continue;
        }
        out.push(path);
    }
    out
}

async fn handle_node_connection(
    _path: PathBuf,
    stream: UnixStream,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    local_snapshot: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    fabric: Arc<Mutex<FabricManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
    local_router_uuid: Uuid,
) {
    let (mut reader, mut writer) = stream.into_split();
    if let Err(err) = send_query(&mut writer).await {
        let _ = err;
        return;
    }

    let announce = match read_announce(&mut reader).await {
        Ok(announce) => announce,
        Err(_) => return,
    };

    let node_uuid = match Uuid::parse_str(&announce.uuid) {
        Ok(uuid) => uuid,
        Err(_) => return,
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut shm = shm.lock().await;
        let _ = shm.register_node(node_uuid, &announce.name);
    }
    send_lsa(&shm, &uplinks).await;
    info!(
        target: "json_router",
        node = %announce.name,
        uuid = %node_uuid,
        "node registered"
    );

    {
        let mut nodes = nodes.lock().await;
        nodes.insert(node_uuid, tx);
    }

    let mut write_task = tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if write_frame(&mut writer, &payload).await.is_err() {
                break;
            }
        }
    });

    loop {
        match read_frame(&mut reader).await {
            Ok(Some(buf)) => {
                if let Ok(msg) = serde_json::from_slice::<Message>(&buf) {
                    if msg.meta.kind == SYSTEM_KIND && msg.meta.msg == MSG_WITHDRAW {
                        if let Ok(payload) =
                            serde_json::from_value::<WithdrawPayload>(msg.payload)
                        {
                            if let Ok(uuid) = Uuid::parse_str(&payload.uuid) {
                                {
                                    let mut shm = shm.lock().await;
                                    let _ = shm.unregister_node(uuid);
                                }
                                info!(
                                    target: "json_router",
                                    uuid = %uuid,
                                    "node withdrawn"
                                );
                                send_lsa(&shm, &uplinks).await;
                            }
                        }
                        break;
                    }
                }
                handle_forward(
                    &buf,
                    &shm,
                    &fib,
                    &nodes,
                    &local_snapshot,
                    &uplinks,
                    &fabric,
                    &broadcast_cache,
                    local_router_uuid,
                )
                .await;
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }

    {
        let mut shm = shm.lock().await;
        let _ = shm.unregister_node(node_uuid);
    }
    info!(
        target: "json_router",
        uuid = %node_uuid,
        "node disconnected"
    );
    send_lsa(&shm, &uplinks).await;
    {
        let mut nodes = nodes.lock().await;
        nodes.remove(&node_uuid);
    }
    write_task.abort();
}

async fn send_query<W>(stream: &mut W) -> Result<(), ShmError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let msg = build_query();
    let payload = serde_json::to_vec(&msg)?;
    write_frame(stream, &payload).await?;
    Ok(())
}

async fn read_announce<R>(stream: &mut R) -> Result<AnnouncePayload, ShmError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let buf = time::timeout(Duration::from_millis(ANNOUNCE_TIMEOUT_MS), read_frame(stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "announce timeout"))??;

    let Some(buf) = buf else {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "announce eof").into());
    };

    let msg = serde_json::from_slice::<Message>(&buf)?;
    if msg.meta.kind != SYSTEM_KIND || msg.meta.msg != MSG_ANNOUNCE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "expected ANNOUNCE").into());
    }

    let payload = serde_json::from_value::<AnnouncePayload>(msg.payload)?;
    Ok(payload)
}

enum Resolution {
    Local(Uuid),
    Uplink(u32),
    Fabric(Uuid),
    None,
}

async fn handle_forward(
    buf: &[u8],
    shm: &Arc<Mutex<ShmWriter>>,
    fib: &Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: &Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    local_snapshot: &Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: &Arc<Mutex<UplinkManager>>,
    fabric: &Arc<Mutex<FabricManager>>,
    broadcast_cache: &Arc<Mutex<HashMap<Uuid, Instant>>>,
    local_router_uuid: Uuid,
) {
    let value: serde_json::Value = match serde_json::from_slice(buf) {
        Ok(value) => value,
        Err(_) => return,
    };

    let dst_value = value
        .get("routing")
        .and_then(|routing| routing.get("dst"))
        .and_then(|dst| dst.as_str());

    let src_uuid = value
        .get("routing")
        .and_then(|routing| routing.get("src"))
        .and_then(|src| src.as_str())
        .and_then(|src| Uuid::parse_str(src).ok());
    let ttl = value
        .get("routing")
        .and_then(|routing| routing.get("ttl"))
        .and_then(|ttl| ttl.as_i64());

    let dst_uuid = value
        .get("routing")
        .and_then(|routing| routing.get("dst"))
        .and_then(|dst| dst.as_str())
        .and_then(|dst| Uuid::parse_str(dst).ok());

    let target = value
        .get("meta")
        .and_then(|meta| meta.get("target"))
        .and_then(|target| target.as_str())
        .map(|s| s.to_string());

    if let Some(ttl) = ttl {
        if ttl <= 0 {
            warn!(
                target: "json_router",
                src = src_uuid.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string()),
                "TTL exceeded"
            );
            send_error(nodes, src_uuid, MSG_TTL_EXCEEDED, "ttl", "ttl exhausted").await;
            return;
        }
    }

    if matches!(dst_value, Some("broadcast")) {
        let trace_id = value
            .get("routing")
            .and_then(|routing| routing.get("trace_id"))
            .and_then(|trace_id| trace_id.as_str())
            .and_then(|trace_id| Uuid::parse_str(trace_id).ok());

        if let Some(trace_id) = trace_id {
            if broadcast_seen(broadcast_cache, trace_id).await {
                return;
            }
        }

        let snapshot = {
            let cached = local_snapshot.lock().await;
            cached.clone()
        };
        let snapshot = match snapshot {
            Some(snapshot) => snapshot,
            None => {
                let shm = shm.lock().await;
                match shm.read_snapshot() {
                    Some(snapshot) => snapshot,
                    None => return,
                }
            }
        };

        let mut local_targets = Vec::new();
        {
            let nodes_map = nodes.lock().await;
            for node in snapshot.nodes {
                if !node.is_active() {
                    continue;
                }
                let uuid = match Uuid::from_slice(&node.uuid) {
                    Ok(uuid) => uuid,
                    Err(_) => continue,
                };
                if src_uuid == Some(uuid) {
                    continue;
                }
                let name_len = node.name_len as usize;
                let name = &node.name[..name_len];
                if let Some(target) = &target {
                    if !target_matches(target, name) {
                        continue;
                    }
                }
                if let Some(sender) = nodes_map.get(&uuid) {
                    local_targets.push(sender.clone());
                }
            }
        }

        let local_count = local_targets.len();
        if let Ok(outgoing) = serde_json::to_vec(&value) {
            for sender in &local_targets {
                let _ = sender.send(outgoing.clone());
            }
        }

        let forwarded = ttl.unwrap_or(0) > 1;
        info!(
            target: "json_router",
            local = local_count,
            forwarded = forwarded,
            "broadcast delivered"
        );

        if forwarded {
            let mut outgoing = value;
            decrement_ttl(&mut outgoing);
            if let Ok(outgoing) = serde_json::to_vec(&outgoing) {
                let mut uplinks = uplinks.lock().await;
                uplinks.broadcast(outgoing.clone());
                let mut fabric = fabric.lock().await;
                fabric.broadcast(outgoing);
            }
        }

        return;
    }

    let mut outgoing = value;
    if let Some(dst) = dst_uuid {
        if let Some(sender) = {
            let nodes = nodes.lock().await;
            nodes.get(&dst).cloned()
        } {
            decrement_ttl(&mut outgoing);
            if let Ok(outgoing) = serde_json::to_vec(&outgoing) {
                let _ = sender.send(outgoing);
            }
            return;
        }

        let link_id = {
            let uplinks = uplinks.lock().await;
            uplinks.route_uuid(&dst)
        };
        if let Some(link_id) = link_id {
            decrement_ttl(&mut outgoing);
            if let Ok(outgoing) = serde_json::to_vec(&outgoing) {
                let mut uplinks = uplinks.lock().await;
                if uplinks.send_on_link(link_id, outgoing).is_err() {
                    warn!(target: "json_router", link_id, "uplink send failed");
                }
            }
            return;
        }

        let peer_uuid = {
            let fabric = fabric.lock().await;
            fabric.route_uuid(&dst)
        };
        if let Some(peer_uuid) = peer_uuid {
            decrement_ttl(&mut outgoing);
            if let Ok(outgoing) = serde_json::to_vec(&outgoing) {
                let mut fabric = fabric.lock().await;
                if fabric.send_to_peer(peer_uuid, outgoing).is_err() {
                    warn!(target: "json_router", peer = %peer_uuid, "fabric send failed");
                }
            }
            return;
        }

        warn!(
            target: "json_router",
            dst = %dst,
            "no route found"
        );
        send_error(nodes, src_uuid, MSG_UNREACHABLE, "no_route", "destination not found").await;
        return;
    }

    let resolution = if let Some(target) = target {
        resolve_target(&target, shm, fib, local_snapshot, local_router_uuid).await
    } else {
        Resolution::None
    };

    if matches!(resolution, Resolution::None) {
        let _ = fib;
        warn!(
            target: "json_router",
            src = src_uuid.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string()),
            "no route found"
        );
        send_error(nodes, src_uuid, MSG_UNREACHABLE, "no_route", "destination not found").await;
        return;
    }

    if let Resolution::Local(dst_uuid) = &resolution {
        if let Some(routing) = outgoing.get_mut("routing") {
            routing["dst"] = serde_json::Value::String(dst_uuid.to_string());
        }
    }

    decrement_ttl(&mut outgoing);
    let outgoing = match serde_json::to_vec(&outgoing) {
        Ok(data) => data,
        Err(_) => return,
    };

    match resolution {
        Resolution::Local(dst_uuid) => {
            let sender = {
                let nodes = nodes.lock().await;
                nodes.get(&dst_uuid).cloned()
            };

            if let Some(sender) = sender {
                info!(
                    target: "json_router",
                    src = src_uuid.as_ref().map(|id| id.to_string()).unwrap_or_else(|| "unknown".to_string()),
                    dst = %dst_uuid,
                    "forwarded"
                );
                let _ = sender.send(outgoing);
            } else {
                warn!(
                    target: "json_router",
                    dst = %dst_uuid,
                    "destination not connected"
                );
                send_error(nodes, src_uuid, MSG_UNREACHABLE, "no_route", "destination not connected").await;
            }
        }
        Resolution::Uplink(link_id) => {
            let mut uplinks = uplinks.lock().await;
            if uplinks.send_on_link(link_id, outgoing).is_err() {
                warn!(target: "json_router", link_id, "uplink send failed");
            }
        }
        Resolution::Fabric(peer_uuid) => {
            let mut fabric = fabric.lock().await;
            if fabric.send_to_peer(peer_uuid, outgoing).is_err() {
                warn!(target: "json_router", peer = %peer_uuid, "fabric send failed");
            }
        }
        Resolution::None => {}
    }
}

async fn resolve_target(
    target: &str,
    shm: &Arc<Mutex<ShmWriter>>,
    fib: &Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    local_snapshot: &Arc<Mutex<Option<ShmSnapshot>>>,
    local_router_uuid: Uuid,
) -> Resolution {
    let snapshot = {
        let cached = local_snapshot.lock().await;
        cached.clone()
    };
    let snapshot = match snapshot {
        Some(snapshot) => snapshot,
        None => {
            let shm = shm.lock().await;
            match shm.read_snapshot() {
                Some(snapshot) => snapshot,
                None => return Resolution::None,
            }
        }
    };

    let fib = fib.lock().await;
    let route = match fib
        .iter()
        .find(|route| route.is_active() && route_matches(route, target))
    {
        Some(route) => route,
        None => return Resolution::None,
    };

    if route.out_link != LINK_LOCAL {
        return Resolution::Uplink(route.out_link);
    }

    if let Ok(next_hop_uuid) = Uuid::from_slice(&route.next_hop_router) {
        if next_hop_uuid != local_router_uuid {
            return Resolution::Fabric(next_hop_uuid);
        }
    }

    match_local_node(route, &snapshot.nodes)
        .map(Resolution::Local)
        .unwrap_or(Resolution::None)
}

fn route_matches(route: &crate::shm::RouteEntry, target: &str) -> bool {
    let target_bytes = target.as_bytes();
    let prefix_len = route.prefix_len as usize;
    if prefix_len == 0 || prefix_len > route.prefix.len() {
        return false;
    }
    let pattern = &route.prefix[..prefix_len];
    match route.match_kind {
        MATCH_EXACT => target_bytes == pattern,
        MATCH_PREFIX => {
            target_bytes.starts_with(pattern)
                && (target_bytes.len() == prefix_len || target_bytes.get(prefix_len) == Some(&b'.'))
        }
        MATCH_GLOB => wildcard_match(pattern, target_bytes),
        _ => false,
    }
}

fn wildcard_match(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0usize;
    let mut t = 0usize;
    let mut star = None;
    let mut match_idx = 0usize;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == text[t] || pattern[p] == b'?') {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            match_idx = t;
            p += 1;
        } else if let Some(star_pos) = star {
            p = star_pos + 1;
            match_idx += 1;
            t = match_idx;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

fn match_local_node(
    route: &crate::shm::RouteEntry,
    nodes: &[crate::shm::nodes::NodeEntry],
) -> Option<Uuid> {
    let pattern = route_prefix(route);
    for node in nodes {
        if !node.is_active() {
            continue;
        }
        let name_len = node.name_len as usize;
        let name = &node.name[..name_len];
        let matches = match route.match_kind {
            MATCH_EXACT => name == pattern,
            MATCH_PREFIX => {
                name.starts_with(pattern)
                    && (name.len() == pattern.len() || name.get(pattern.len()) == Some(&b'.'))
            }
            MATCH_GLOB => wildcard_match(route_prefix(route), name),
            _ => false,
        };
        if matches {
            return Uuid::from_slice(&node.uuid).ok();
        }
    }
    None
}

fn route_prefix(route: &crate::shm::RouteEntry) -> &[u8] {
    &route.prefix[..route.prefix_len as usize]
}

fn decrement_ttl(value: &mut serde_json::Value) {
    if let Some(ttl) = value
        .get_mut("routing")
        .and_then(|routing| routing.get_mut("ttl"))
        .and_then(|ttl| ttl.as_i64())
    {
        let new_ttl = (ttl - 1).max(0);
        if let Some(routing) = value.get_mut("routing") {
            routing["ttl"] = serde_json::Value::Number(new_ttl.into());
        }
    }
}

async fn broadcast_seen(
    cache: &Arc<Mutex<HashMap<Uuid, Instant>>>,
    trace_id: Uuid,
) -> bool {
    let mut cache = cache.lock().await;
    let now = Instant::now();
    let ttl = Duration::from_secs(BROADCAST_CACHE_TTL_SECS);
    cache.retain(|_, seen_at| now.duration_since(*seen_at) < ttl);
    if cache.contains_key(&trace_id) {
        true
    } else {
        cache.insert(trace_id, now);
        false
    }
}

fn target_matches(target: &str, name: &[u8]) -> bool {
    if let Some(prefix) = target.strip_suffix(".*") {
        let prefix_bytes = prefix.as_bytes();
        return name.starts_with(prefix_bytes)
            && (name.len() == prefix_bytes.len() || name.get(prefix_bytes.len()) == Some(&b'.'));
    }

    if target.contains('*') || target.contains('?') {
        return wildcard_match(target.as_bytes(), name);
    }

    name == target.as_bytes()
}

fn fabric_socket_dir(node_socket_dir: &Path) -> PathBuf {
    node_socket_dir
        .parent()
        .map(|parent| parent.join("routers"))
        .unwrap_or_else(|| node_socket_dir.to_path_buf())
}

fn fabric_socket_path(node_socket_dir: &Path, router_uuid: &Uuid) -> PathBuf {
    let dir = fabric_socket_dir(node_socket_dir);
    dir.join(format!("{}.sock", router_uuid.simple()))
}

#[derive(serde::Deserialize)]
struct DiscoveryRouterConfig {
    router: DiscoveryRouterSection,
    #[serde(default)]
    paths: DiscoveryPaths,
}

#[derive(serde::Deserialize)]
struct DiscoveryRouterSection {
    name: String,
    island_id: String,
}

#[derive(serde::Deserialize, Default)]
struct DiscoveryPaths {
    #[serde(default = "default_state_dir")]
    state_dir: PathBuf,
}

#[derive(serde::Deserialize)]
struct DiscoveryIdentity {
    shm: DiscoveryIdentityShm,
}

#[derive(serde::Deserialize)]
struct DiscoveryIdentityShm {
    name: String,
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("./state")
}

fn discover_peer_shm_names(cfg: &Config) -> Vec<String> {
    let mut names = Vec::new();

    let prefix = cfg.shm_prefix.trim_start_matches('/');
    if Path::new("/dev/shm").is_dir() {
        if let Ok(entries) = std::fs::read_dir("/dev/shm") {
            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                if !name.starts_with(prefix) {
                    continue;
                }
                let shm_name = if name.starts_with('/') {
                    name.to_string()
                } else {
                    format!("/{}", name)
                };
                if shm_name == cfg.shm_name || shm_name.starts_with(CONFIG_SHM_PREFIX) {
                    continue;
                }
                names.push(shm_name);
            }
        }
    }

    if !names.is_empty() {
        names.sort();
        names.dedup();
        return names;
    }

    let routers_dir = cfg.config_dir.join("routers");
    let Ok(entries) = std::fs::read_dir(&routers_dir) else {
        return names;
    };

    for entry in entries.flatten() {
        let router_dir = entry.path();
        let config_path = router_dir.join("config.yaml");
        let Ok(config_data) = std::fs::read_to_string(&config_path) else {
            continue;
        };
        let Ok(router_cfg) = serde_yaml::from_str::<DiscoveryRouterConfig>(&config_data) else {
            continue;
        };
        if router_cfg.router.island_id != cfg.island_id {
            continue;
        }
        if router_cfg.router.name == cfg.router_name {
            continue;
        }
        let mut identity_data = None;
        let state_dir = &router_cfg.paths.state_dir;
        let candidates = if state_dir.is_relative() {
            vec![state_dir.clone(), cfg.config_dir.join(state_dir)]
        } else {
            vec![state_dir.clone()]
        };
        for candidate in candidates {
            let identity_path = candidate.join(&router_cfg.router.name).join("identity.yaml");
            if let Ok(data) = std::fs::read_to_string(&identity_path) {
                identity_data = Some(data);
                break;
            }
        }
        let Some(identity_data) = identity_data else {
            continue;
        };
        let Ok(identity) = serde_yaml::from_str::<DiscoveryIdentity>(&identity_data) else {
            continue;
        };
        if identity.shm.name == cfg.shm_name {
            continue;
        }
        names.push(identity.shm.name);
    }

    names.sort();
    names.dedup();
    names
}

struct PeerManager {
    peers: Vec<PeerHandle>,
}

struct PeerHandle {
    name: String,
    reader: Option<ShmReader>,
    generation: Option<u64>,
}

struct PeerSnapshot {
    router_uuid: [u8; 16],
    nodes: Vec<crate::shm::nodes::NodeEntry>,
    routes: Vec<crate::shm::routes::RouteEntry>,
}

impl PeerManager {
    fn new(names: Vec<String>) -> Self {
        let peers = names
            .into_iter()
            .map(|name| PeerHandle {
                name,
                reader: None,
                generation: None,
            })
            .collect();
        Self { peers }
    }

    fn update(&mut self, names: Vec<String>) {
        let mut existing: HashMap<String, PeerHandle> = self
            .peers
            .drain(..)
            .map(|peer| (peer.name.clone(), peer))
            .collect();
        let mut peers = Vec::new();
        for name in names {
            if let Some(peer) = existing.remove(&name) {
                peers.push(peer);
            } else {
                peers.push(PeerHandle {
                    name,
                    reader: None,
                    generation: None,
                });
            }
        }
        self.peers = peers;
    }

    fn refresh(&mut self) -> Vec<PeerSnapshot> {
        let mut out = Vec::new();
        for peer in &mut self.peers {
            if peer.reader.is_none() {
                if let Ok(reader) = ShmReader::open_read_only(&peer.name) {
                    peer.reader = Some(reader);
                    peer.generation = None;
                } else {
                    continue;
                }
            }

            let Some(reader) = peer.reader.as_ref() else {
                continue;
            };

            let snapshot = match reader.read_snapshot() {
                Some(snapshot) => snapshot,
                None => continue,
            };

            if heartbeat_stale(snapshot.heartbeat) {
                peer.reader = None;
                peer.generation = None;
                continue;
            }

            if let Some(prev) = peer.generation {
                if prev != snapshot.generation {
                    peer.reader = None;
                    peer.generation = None;
                    continue;
                }
            }

            peer.generation = Some(snapshot.generation);
            out.push(PeerSnapshot {
                router_uuid: snapshot.router_uuid,
                nodes: snapshot.nodes,
                routes: snapshot.routes,
            });
        }
        out
    }
}

#[derive(Clone, Debug)]
struct UplinkNode {
    uuid: Uuid,
    name: String,
    link_id: u32,
    router_id: Option<Uuid>,
}

fn build_fib(
    local: &ShmSnapshot,
    peers: &[PeerSnapshot],
    uplink_nodes: &[UplinkNode],
) -> Vec<crate::shm::RouteEntry> {
    let mut fib = Vec::new();

    for route in &local.routes {
        if route.is_active() {
            fib.push(*route);
        }
    }

    for peer in peers {
        for node in &peer.nodes {
            if !node.is_active() {
                continue;
            }
            let mut route = crate::shm::RouteEntry::default();
            let name_len = node.name_len as usize;
            route.prefix[..name_len].copy_from_slice(&node.name[..name_len]);
            route.prefix_len = node.name_len;
            route.match_kind = MATCH_EXACT;
            route.route_type = ROUTE_LSA;
            route.next_hop_router = peer.router_uuid;
            route.out_link = LINK_LOCAL;
            route.metric = 1;
            route.admin_distance = AD_LSA;
            route.flags = FLAG_ACTIVE;
            fib.push(route);
        }

    }

    for uplink in uplink_nodes {
        let mut route = crate::shm::RouteEntry::default();
        let name_bytes = uplink.name.as_bytes();
        let name_len = name_bytes.len().min(route.prefix.len());
        if name_len == 0 {
            continue;
        }
        route.prefix[..name_len].copy_from_slice(&name_bytes[..name_len]);
        route.prefix_len = name_len as u16;
        route.match_kind = MATCH_EXACT;
        route.route_type = ROUTE_LSA;
        if let Some(router_id) = uplink.router_id {
            route.next_hop_router = *router_id.as_bytes();
        }
        route.out_link = uplink.link_id;
        route.metric = 1;
        route.admin_distance = AD_LSA;
        route.flags = FLAG_ACTIVE;
        fib.push(route);
    }

    fib
}

fn route_cmp(a: &crate::shm::RouteEntry, b: &crate::shm::RouteEntry) -> std::cmp::Ordering {
    b.prefix_len
        .cmp(&a.prefix_len)
        .then_with(|| a.admin_distance.cmp(&b.admin_distance))
        .then_with(|| a.metric.cmp(&b.metric))
}

fn heartbeat_stale(heartbeat: u64) -> bool {
    let now = now_epoch_ms();
    now.saturating_sub(heartbeat) > crate::shm::HEARTBEAT_STALE_MS
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

async fn send_error(
    nodes: &Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    src_uuid: Option<Uuid>,
    msg: &str,
    reason: &str,
    detail: &str,
) {
    let Some(src_uuid) = src_uuid else {
        return;
    };
    let payload = build_error(msg, reason, detail, &src_uuid.to_string());
    let data = match serde_json::to_vec(&payload) {
        Ok(data) => data,
        Err(_) => return,
    };
    let sender = {
        let nodes = nodes.lock().await;
        nodes.get(&src_uuid).cloned()
    };
    if let Some(sender) = sender {
        let _ = sender.send(data);
    }
}

async fn send_lsa(shm: &Arc<Mutex<ShmWriter>>, uplinks: &Arc<Mutex<UplinkManager>>) {
    let snapshot = {
        let shm = shm.lock().await;
        shm.read_snapshot()
    };
    let Some(snapshot) = snapshot else {
        return;
    };
    let router_uuid = Uuid::from_slice(&snapshot.router_uuid).unwrap_or_else(|_| Uuid::nil());
    let mut nodes = Vec::new();
    for node in snapshot.nodes {
        if !node.is_active() {
            continue;
        }
        let name = String::from_utf8_lossy(&node.name[..node.name_len as usize]).to_string();
        let uuid = Uuid::from_slice(&node.uuid).unwrap_or_else(|_| Uuid::nil());
        nodes.push(NodeDescriptor {
            uuid: uuid.to_string(),
            name,
        });
    }
    let lsa = build_lsa(router_uuid, nodes);
    if let Ok(payload) = serde_json::to_vec(&lsa) {
        let mut uplinks = uplinks.lock().await;
        info!(
            target: "json_router",
            router = %router_uuid,
            nodes = lsa.payload.nodes.len(),
            "LSA sent"
        );
        uplinks.broadcast(payload);
    }
}

fn send_sync_request(
    router_uuid: Uuid,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> Result<(), ()> {
    let msg = build_sync_request(router_uuid);
    send_system_frame(sender, &msg)
}

async fn send_sync_reply(
    shm: &Arc<Mutex<ShmWriter>>,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> Result<(), ()> {
    let snapshot = {
        let shm = shm.lock().await;
        shm.read_snapshot()
    };
    let Some(snapshot) = snapshot else {
        return Err(());
    };
    let router_uuid = Uuid::from_slice(&snapshot.router_uuid).unwrap_or_else(|_| Uuid::nil());
    let mut nodes = Vec::new();
    for node in snapshot.nodes {
        if !node.is_active() {
            continue;
        }
        let name = String::from_utf8_lossy(&node.name[..node.name_len as usize]).to_string();
        let uuid = Uuid::from_slice(&node.uuid).unwrap_or_else(|_| Uuid::nil());
        nodes.push(NodeDescriptor {
            uuid: uuid.to_string(),
            name,
        });
    }
    let msg = build_sync_reply(router_uuid, nodes);
    send_system_frame(sender, &msg)
}

async fn handle_lsa_frame(
    buf: &[u8],
    link_id: u32,
    uplinks: &Arc<Mutex<UplinkManager>>,
) -> bool {
    let Ok(msg) = serde_json::from_slice::<Message<serde_json::Value>>(buf) else {
        return false;
    };
    if msg.meta.kind != SYSTEM_KIND || msg.meta.msg != MSG_LSA {
        return false;
    }
    let Ok(payload) = serde_json::from_value::<LsaPayload>(msg.payload) else {
        return true;
    };
    let router_id = Uuid::parse_str(&payload.router_id).ok();
    let mut uplinks = uplinks.lock().await;
    uplinks.update_lsa(link_id, router_id, payload.nodes);
    info!(
        target: "json_router",
        link_id = link_id,
        nodes = uplinks.peer_nodes.get(&link_id).map(|m| m.len()).unwrap_or(0),
        "LSA received"
    );
    true
}

async fn handle_sync_frame(
    buf: &[u8],
    link_id: u32,
    uplinks: &Arc<Mutex<UplinkManager>>,
) -> bool {
    let Ok(msg) = serde_json::from_slice::<Message<serde_json::Value>>(buf) else {
        return false;
    };
    if msg.meta.kind != SYSTEM_KIND || msg.meta.msg != MSG_SYNC_REPLY {
        return false;
    }
    let Ok(payload) = serde_json::from_value::<SyncReplyPayload>(msg.payload) else {
        return true;
    };
    let router_id = Uuid::parse_str(&payload.router_id).ok();
    let mut uplinks = uplinks.lock().await;
    uplinks.update_lsa(link_id, router_id, payload.nodes);
    info!(
        target: "json_router",
        link_id = link_id,
        nodes = uplinks.peer_nodes.get(&link_id).map(|m| m.len()).unwrap_or(0),
        "SYNC reply received"
    );
    true
}

async fn handle_sync_request(
    buf: &[u8],
    shm: &Arc<Mutex<ShmWriter>>,
    sender: &mpsc::UnboundedSender<Vec<u8>>,
) -> bool {
    let Ok(msg) = serde_json::from_slice::<Message<serde_json::Value>>(buf) else {
        return false;
    };
    if msg.meta.kind != SYSTEM_KIND || msg.meta.msg != MSG_SYNC_REQUEST {
        return false;
    }
    let Ok(_payload) = serde_json::from_value::<SyncRequestPayload>(msg.payload) else {
        return true;
    };
    let _ = send_sync_reply(shm, sender).await;
    true
}

struct FabricManager {
    links: HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>,
    connecting: HashSet<Uuid>,
    peer_nodes: HashMap<Uuid, Uuid>,
}

impl FabricManager {
    fn new() -> Self {
        Self {
            links: HashMap::new(),
            connecting: HashSet::new(),
            peer_nodes: HashMap::new(),
        }
    }

    fn ensure_connect(&mut self, peer_uuid: Uuid) -> bool {
        if self.links.contains_key(&peer_uuid) || self.connecting.contains(&peer_uuid) {
            return false;
        }
        self.connecting.insert(peer_uuid);
        true
    }

    fn clear_connecting(&mut self, peer_uuid: Uuid) {
        self.connecting.remove(&peer_uuid);
    }

    fn register_link(&mut self, peer_uuid: Uuid, sender: mpsc::UnboundedSender<Vec<u8>>) -> bool {
        if self.links.contains_key(&peer_uuid) {
            return false;
        }
        self.links.insert(peer_uuid, sender);
        true
    }

    fn remove_link(&mut self, peer_uuid: Uuid) {
        self.links.remove(&peer_uuid);
        self.connecting.remove(&peer_uuid);
    }

    fn send_to_peer(&mut self, peer_uuid: Uuid, payload: Vec<u8>) -> Result<(), ()> {
        if let Some(sender) = self.links.get(&peer_uuid) {
            sender.send(payload).map_err(|_| ())
        } else {
            Err(())
        }
    }

    fn broadcast(&mut self, payload: Vec<u8>) {
        for sender in self.links.values() {
            let _ = sender.send(payload.clone());
        }
    }

    fn refresh_peer_nodes(&mut self, peers: &[PeerSnapshot]) {
        let mut map = HashMap::new();
        for peer in peers {
            let peer_uuid = match Uuid::from_slice(&peer.router_uuid) {
                Ok(uuid) => uuid,
                Err(_) => continue,
            };
            for node in &peer.nodes {
                if !node.is_active() {
                    continue;
                }
                if let Ok(uuid) = Uuid::from_slice(&node.uuid) {
                    map.insert(uuid, peer_uuid);
                }
            }
        }
        self.peer_nodes = map;
    }

    fn route_uuid(&self, uuid: &Uuid) -> Option<Uuid> {
        self.peer_nodes.get(uuid).copied()
    }
}

struct UplinkManager {
    links: HashMap<u32, mpsc::UnboundedSender<Vec<u8>>>,
    peer_nodes: HashMap<u32, HashMap<Uuid, String>>,
    peer_router_ids: HashMap<u32, Uuid>,
    next_link_id: u32,
}

impl UplinkManager {
    fn new() -> Self {
        Self {
            links: HashMap::new(),
            peer_nodes: HashMap::new(),
            peer_router_ids: HashMap::new(),
            next_link_id: 1,
        }
    }

    fn allocate_link_id(&mut self) -> u32 {
        let id = self.next_link_id;
        self.next_link_id = self.next_link_id.saturating_add(1);
        id
    }

    fn register_link(&mut self, link_id: u32, sender: mpsc::UnboundedSender<Vec<u8>>) {
        self.links.insert(link_id, sender);
    }

    fn register_peer_router(&mut self, link_id: u32, router_id: Uuid) {
        self.peer_router_ids.insert(link_id, router_id);
    }

    fn send_on_link(&mut self, link_id: u32, payload: Vec<u8>) -> Result<(), ()> {
        if let Some(sender) = self.links.get(&link_id) {
            sender.send(payload).map_err(|_| ())
        } else {
            Err(())
        }
    }

    fn broadcast(&mut self, payload: Vec<u8>) {
        for sender in self.links.values() {
            let _ = sender.send(payload.clone());
        }
    }

    fn update_lsa(&mut self, link_id: u32, router_id: Option<Uuid>, nodes: Vec<NodeDescriptor>) {
        let mut map = HashMap::new();
        for node in nodes {
            if let Ok(uuid) = Uuid::parse_str(&node.uuid) {
                map.insert(uuid, node.name);
            }
        }
        if let Some(router_id) = router_id {
            self.peer_router_ids.insert(link_id, router_id);
        }
        self.peer_nodes.insert(link_id, map);
    }


    fn route_uuid(&self, uuid: &Uuid) -> Option<u32> {
        for (link_id, nodes) in &self.peer_nodes {
            if nodes.contains_key(uuid) {
                return Some(*link_id);
            }
        }
        None
    }

    fn snapshot_nodes(&self) -> Vec<UplinkNode> {
        let mut out = Vec::new();
        for (link_id, nodes) in &self.peer_nodes {
            let router_id = self.peer_router_ids.get(link_id).cloned();
            for (uuid, name) in nodes {
                out.push(UplinkNode {
                    uuid: *uuid,
                    name: name.clone(),
                    link_id: *link_id,
                    router_id,
                });
            }
        }
        out
    }

}

enum FabricRole {
    Initiator,
    Acceptor,
}

async fn fabric_listen_loop(
    socket_path: PathBuf,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    snapshot_cache: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    fabric: Arc<Mutex<FabricManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
) -> io::Result<()> {
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let _ = tokio::fs::remove_file(&socket_path).await;
    let listener = tokio::net::UnixListener::bind(&socket_path)?;
    info!(target: "json_router", socket = %socket_path.display(), "fabric listening");

    loop {
        let (stream, _) = listener.accept().await?;
        let shm = Arc::clone(&shm);
        let fib = Arc::clone(&fib);
        let nodes = Arc::clone(&nodes);
        let snapshot_cache = Arc::clone(&snapshot_cache);
        let uplinks = Arc::clone(&uplinks);
        let fabric = Arc::clone(&fabric);
        let broadcast_cache = Arc::clone(&broadcast_cache);
        tokio::spawn(async move {
            if let Err(err) = handle_fabric_connection(
                stream,
                FabricRole::Acceptor,
                shm,
                fib,
                nodes,
                snapshot_cache,
                uplinks,
                fabric,
                broadcast_cache,
            )
            .await
            {
                warn!(target: "json_router", "fabric accept error: {}", err);
            }
        });
    }
}

async fn fabric_connect_loop(
    peer_uuid: Uuid,
    socket_path: PathBuf,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    snapshot_cache: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    fabric: Arc<Mutex<FabricManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
    local_router_uuid: Uuid,
) {
    let result = UnixStream::connect(&socket_path).await;
    let ok = result.is_ok();
    if let Ok(stream) = result {
        let _ = handle_fabric_connection(
            stream,
            FabricRole::Initiator,
            shm,
            fib,
            nodes,
            snapshot_cache,
            uplinks,
            Arc::clone(&fabric),
            broadcast_cache,
        )
        .await;
    }
    let mut fabric = fabric.lock().await;
    fabric.clear_connecting(peer_uuid);
    if fabric.links.contains_key(&peer_uuid) {
        return;
    }
    if peer_uuid == local_router_uuid {
        return;
    }
    if !ok {
        debug!(
            target: "json_router",
            peer = %peer_uuid,
            socket = %socket_path.display(),
            "fabric connect failed"
        );
    }
}

async fn handle_fabric_connection(
    stream: UnixStream,
    role: FabricRole,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    snapshot_cache: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    fabric: Arc<Mutex<FabricManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let (router_uuid, island_id, shm_name) = {
        let shm = shm.lock().await;
        let name = shm.name().to_string();
        let router_uuid = Uuid::from_slice(&shm.header().router_uuid).unwrap_or_else(|_| Uuid::nil());
        let island_id = {
            let raw = &shm.header().island_id[..shm.header().island_id_len as usize];
            String::from_utf8_lossy(raw).to_string()
        };
        (router_uuid, island_id, name)
    };

    let hello = build_hello(
        router_uuid,
        &island_id,
        &shm_name,
        1,
        0,
        0,
        HelloCapabilities {
            sync: false,
            lsa: true,
            forwarding: true,
        },
    );

    let peer = match role {
        FabricRole::Initiator => {
            send_system_message(&mut writer, &hello).await?;
            read_hello(&mut reader).await?
        }
        FabricRole::Acceptor => {
            let peer = read_hello(&mut reader).await?;
            send_system_message(&mut writer, &hello).await?;
            peer
        }
    };

    let peer_uuid = match Uuid::parse_str(&peer.router_id) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut fabric = fabric.lock().await;
        if !fabric.register_link(peer_uuid, tx) {
            return Ok(());
        }
    }

    tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if write_frame(&mut writer, &payload).await.is_err() {
                break;
            }
        }
    });

    loop {
        match read_frame(&mut reader).await? {
            Some(buf) => {
                handle_forward(
                    &buf,
                    &shm,
                    &fib,
                    &nodes,
                    &snapshot_cache,
                    &uplinks,
                    &fabric,
                    &broadcast_cache,
                    router_uuid,
                )
                .await;
            }
            None => break,
        }
    }

    let mut fabric = fabric.lock().await;
    fabric.remove_link(peer_uuid);
    Ok(())
}

async fn uplink_listen_loop(
    addr: String,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    snapshot_cache: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
    fabric: Arc<Mutex<FabricManager>>,
    local_router_uuid: Uuid,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
) -> io::Result<()> {
    let listener = TcpListener::bind(&addr).await?;
    info!(target: "json_router", addr = %addr, "uplink listening");
    loop {
        let (stream, _) = listener.accept().await?;
        let shm = Arc::clone(&shm);
        let fib = Arc::clone(&fib);
        let nodes = Arc::clone(&nodes);
        let snapshot_cache = Arc::clone(&snapshot_cache);
        let uplinks = Arc::clone(&uplinks);
        let broadcast_cache = Arc::clone(&broadcast_cache);
        let fabric = Arc::clone(&fabric);
        tokio::spawn(async move {
            let link_id = {
                let mut uplinks = uplinks.lock().await;
                uplinks.allocate_link_id()
            };
            if let Err(err) = handle_uplink_connection(
                stream,
                UplinkRole::Acceptor,
                link_id,
                shm,
                fib,
                nodes,
                snapshot_cache,
                uplinks,
                broadcast_cache,
                fabric,
                local_router_uuid,
                hello_interval_ms,
                dead_interval_ms,
            )
            .await
            {
                warn!(target: "json_router", "uplink accept error: {}", err);
            }
        });
    }
}

async fn uplink_connect_loop(
    peers: Vec<crate::config::UplinkPeerConfig>,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    snapshot_cache: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
    fabric: Arc<Mutex<FabricManager>>,
    local_router_uuid: Uuid,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
) {
    for peer in peers {
        let shm = Arc::clone(&shm);
        let fib = Arc::clone(&fib);
        let nodes = Arc::clone(&nodes);
        let snapshot_cache = Arc::clone(&snapshot_cache);
        let uplinks = Arc::clone(&uplinks);
        let broadcast_cache = Arc::clone(&broadcast_cache);
        let fabric = Arc::clone(&fabric);
        tokio::spawn(async move {
            loop {
                match TcpStream::connect(&peer.addr).await {
                    Ok(stream) => {
                        let link_id = peer.link_id;
                        if let Err(err) = handle_uplink_connection(
                            stream,
                            UplinkRole::Initiator,
                            link_id,
                            Arc::clone(&shm),
                            Arc::clone(&fib),
                            Arc::clone(&nodes),
                            Arc::clone(&snapshot_cache),
                            Arc::clone(&uplinks),
                            Arc::clone(&broadcast_cache),
                            Arc::clone(&fabric),
                            local_router_uuid,
                            hello_interval_ms,
                            dead_interval_ms,
                        )
                        .await
                        {
                            warn!(
                                target: "json_router",
                                addr = %peer.addr,
                                "uplink connect error: {}",
                                err
                            );
                        }
                    }
                    Err(_) => {}
                }
                time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
}

enum UplinkRole {
    Initiator,
    Acceptor,
}

async fn handle_uplink_connection(
    stream: TcpStream,
    role: UplinkRole,
    link_id: u32,
    shm: Arc<Mutex<ShmWriter>>,
    fib: Arc<Mutex<Vec<crate::shm::RouteEntry>>>,
    nodes: Arc<Mutex<HashMap<Uuid, mpsc::UnboundedSender<Vec<u8>>>>>,
    snapshot_cache: Arc<Mutex<Option<ShmSnapshot>>>,
    uplinks: Arc<Mutex<UplinkManager>>,
    broadcast_cache: Arc<Mutex<HashMap<Uuid, Instant>>>,
    fabric: Arc<Mutex<FabricManager>>,
    local_router_uuid: Uuid,
    hello_interval_ms: u64,
    dead_interval_ms: u64,
) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let (router_uuid, island_id, shm_name) = {
        let shm = shm.lock().await;
        let name = shm.name().to_string();
        let router_uuid = Uuid::from_slice(&shm.header().router_uuid).unwrap_or_else(|_| Uuid::nil());
        let island_id = {
            let raw = &shm.header().island_id[..shm.header().island_id_len as usize];
            String::from_utf8_lossy(raw).to_string()
        };
        (router_uuid, island_id, name)
    };
    let capabilities = HelloCapabilities {
        sync: false,
        lsa: true,
        forwarding: true,
    };
    let mut hello_seq: u64 = 1;
    let hello = {
        build_hello(
            router_uuid,
            &island_id,
            &shm_name,
            hello_seq,
            hello_interval_ms,
            dead_interval_ms,
            capabilities.clone(),
        )
    };

    let peer = match role {
        UplinkRole::Initiator => {
            send_system_message(&mut writer, &hello).await?;
            read_hello(&mut reader).await?
        }
        UplinkRole::Acceptor => {
            let peer = read_hello(&mut reader).await?;
            send_system_message(&mut writer, &hello).await?;
            peer
        }
    };

    if peer.protocol != "json-router/1" {
        let reject = build_uplink_reject(
            "PROTOCOL_MISMATCH",
            "unsupported protocol",
            Some("json-router/1"),
        );
        let _ = send_system_message(&mut writer, &reject).await;
        return Err(io::Error::new(io::ErrorKind::InvalidData, "protocol mismatch"));
    }

    let accept = build_uplink_accept(&peer.router_id, hello_interval_ms, dead_interval_ms);
    send_system_message(&mut writer, &accept).await?;
    match read_uplink_response(&mut reader).await? {
        UplinkResponse::Accept(_) => {}
        UplinkResponse::Reject(payload) => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("uplink rejected: {}", payload.reason),
            ));
        }
    }

    info!(
        target: "json_router",
        link_id,
        peer = %peer.router_id,
        "uplink established"
    );

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let mut uplinks = uplinks.lock().await;
        uplinks.register_link(link_id, tx.clone());
        if let Ok(peer_uuid) = Uuid::parse_str(&peer.router_id) {
            uplinks.register_peer_router(link_id, peer_uuid);
        }
    }
    send_lsa(&shm, &uplinks).await;
    send_sync_request(router_uuid, &tx).ok();

    let tx_hello = tx.clone();
    tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if write_frame(&mut writer, &payload).await.is_err() {
                break;
            }
        }
    });

    let mut last_hello = Instant::now();
    let mut hello_tick = time::interval(Duration::from_millis(hello_interval_ms));
    let mut dead_tick = time::interval(Duration::from_millis(dead_interval_ms));

    loop {
        tokio::select! {
            _ = hello_tick.tick() => {
                hello_seq = hello_seq.wrapping_add(1);
                let hello = build_hello(
                    router_uuid,
                    &island_id,
                    &shm_name,
                    hello_seq,
                    hello_interval_ms,
                    dead_interval_ms,
                    capabilities.clone(),
                );
                if send_system_frame(&tx_hello, &hello).is_err() {
                    break;
                }
            }
            _ = dead_tick.tick() => {
                if last_hello.elapsed() > Duration::from_millis(dead_interval_ms) {
                    warn!(
                        target: "json_router",
                        link_id,
                        "uplink dead detected"
                    );
                    break;
                }
            }
            res = read_frame(&mut reader) => {
                match res? {
                    Some(buf) => {
                        if handle_hello_frame(&buf, &mut last_hello) {
                            continue;
                        }
                        if handle_lsa_frame(&buf, link_id, &uplinks).await {
                            continue;
                        }
                        if handle_sync_frame(&buf, link_id, &uplinks).await {
                            continue;
                        }
                        if handle_sync_request(&buf, &shm, &tx).await {
                            continue;
                        }
                        handle_forward(
                            &buf,
                            &shm,
                            &fib,
                            &nodes,
                            &snapshot_cache,
                            &uplinks,
                            &fabric,
                            &broadcast_cache,
                            local_router_uuid,
                        )
                        .await;
                    }
                    None => break,
                }
            }
        }
    }

    Ok(())
}

async fn read_hello<R>(reader: &mut R) -> io::Result<HelloPayload>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(buf) = read_frame(reader).await? else {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "uplink hello eof"));
    };
    let msg = serde_json::from_slice::<Message<HelloPayload>>(&buf)?;
    if msg.meta.kind != SYSTEM_KIND || msg.meta.msg != MSG_HELLO {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "expected HELLO"));
    }
    Ok(msg.payload)
}

enum UplinkResponse {
    Accept(UplinkAcceptPayload),
    Reject(UplinkRejectPayload),
}

async fn read_uplink_response<R>(reader: &mut R) -> io::Result<UplinkResponse>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(buf) = read_frame(reader).await? else {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "uplink accept eof"));
    };
    let msg = serde_json::from_slice::<Message<serde_json::Value>>(&buf)?;
    if msg.meta.kind != SYSTEM_KIND {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "expected system message"));
    }
    match msg.meta.msg.as_str() {
        MSG_UPLINK_ACCEPT => {
            let payload = serde_json::from_value::<UplinkAcceptPayload>(msg.payload)?;
            Ok(UplinkResponse::Accept(payload))
        }
        MSG_UPLINK_REJECT => {
            let payload = serde_json::from_value::<UplinkRejectPayload>(msg.payload)?;
            Ok(UplinkResponse::Reject(payload))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected UPLINK_ACCEPT or UPLINK_REJECT",
        )),
    }
}

fn handle_hello_frame(buf: &[u8], last_hello: &mut Instant) -> bool {
    let Ok(msg) = serde_json::from_slice::<Message<HelloPayload>>(buf) else {
        return false;
    };
    if msg.meta.kind != SYSTEM_KIND || msg.meta.msg != MSG_HELLO {
        return false;
    }
    *last_hello = Instant::now();
    true
}

async fn send_system_message<W, T>(writer: &mut W, msg: &Message<T>) -> io::Result<()>
where
    T: serde::Serialize,
    W: tokio::io::AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(msg)?;
    write_frame(writer, &payload).await
}

fn send_system_frame<T>(
    sender: &mpsc::UnboundedSender<Vec<u8>>,
    msg: &Message<T>,
) -> Result<(), ()>
where
    T: serde::Serialize,
{
    let payload = serde_json::to_vec(msg).map_err(|_| ())?;
    sender.send(payload).map_err(|_| ())
}
