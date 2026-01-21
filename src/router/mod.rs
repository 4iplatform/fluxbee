use std::io;
use std::path::Path;
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

use crate::config::RouterConfig;
use crate::protocol::{
    build_announce, build_ttl_exceeded, build_unreachable, Destination, Message,
    NodeAnnouncePayload, NodeHelloPayload, MSG_HELLO, MSG_WITHDRAW, SYSTEM_KIND,
};
use crate::shm::{
    now_epoch_ms, ConfigRegionReader, ConfigSnapshot, RouterRegionWriter, FLAG_ACTIVE, MATCH_EXACT,
    MATCH_GLOB, MATCH_PREFIX,
};
use crate::socket::connection::{read_frame, write_frame};

#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct Router {
    cfg: RouterConfig,
    shm: Arc<Mutex<RouterRegionWriter>>,
    nodes: Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
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
            config_reader: Arc::new(Mutex::new(config_reader)),
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

        loop {
            let (stream, _) = listener.accept().await?;
            let shm = Arc::clone(&self.shm);
            let router_name = self.cfg.router_l2_name.clone();
            let router_uuid = self.cfg.router_uuid;
            let island_id = self.cfg.island_id.clone();
            let nodes = Arc::clone(&self.nodes);
            let config_reader = Arc::clone(&self.config_reader);
            tokio::spawn(async move {
                if let Err(err) = handle_node(
                    stream,
                    shm,
                    nodes,
                    config_reader,
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
    config_reader: Arc<Mutex<Option<ConfigRegionReader>>>,
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
    let vpn_id = {
        let snapshot = {
            let mut reader = config_reader.lock().await;
            if reader.is_none() {
                if let Ok(new_reader) =
                    ConfigRegionReader::open_read_only(&format!("/jsr-config-{}", island_id))
                {
                    *reader = Some(new_reader);
                }
            }
            reader.as_ref().and_then(|r| r.read_snapshot())
        };
        assign_vpn(&node_name, snapshot.as_ref())
    };
    let connected_at = now_epoch_ms();
    {
        let mut shm = shm.lock().await;
        shm.register_node(node_uuid, &node_name, vpn_id, connected_at)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    }
    {
        let mut nodes = nodes.lock().await;
        nodes.insert(
            node_uuid,
            NodeHandle {
                name: node_name.clone(),
                vpn_id,
                sender: tx.clone(),
            },
        );
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
    let _ = tx.send(data);

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
                    if msg.meta.msg_type == SYSTEM_KIND
                        && msg.meta.msg.as_deref() == Some(MSG_WITHDRAW)
                    {
                        break;
                    }
                    handle_message(&msg, &nodes, router_uuid).await?;
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
    writer_task.abort();
    tracing::info!(node = %node_uuid, "node disconnected");
    Ok(())
}

async fn handle_message(
    msg: &Message,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    router_uuid: Uuid,
) -> Result<(), RouterError> {
    if msg.routing.ttl == 0 {
        send_ttl_exceeded(msg, nodes, router_uuid).await?;
        return Ok(());
    }

    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    let nodes_guard = nodes.lock().await;
    let Some(src_handle) = nodes_guard.get(&src_uuid) else {
        return Ok(());
    };
    match &msg.routing.dst {
        Destination::Unicast(dst) => {
            let Ok(dst_uuid) = Uuid::parse_str(dst) else {
                send_unreachable(msg, nodes, router_uuid, "INVALID_DST").await?;
                return Ok(());
            };
            if let Some(dst_handle) = nodes_guard.get(&dst_uuid) {
                if can_route(src_handle, dst_handle) {
                    let _ = dst_handle.sender.send(serde_json::to_vec(msg)?);
                } else {
                    send_unreachable(msg, nodes, router_uuid, "VPN_BLOCKED").await?;
                }
            } else {
                send_unreachable(msg, nodes, router_uuid, "NODE_NOT_FOUND").await?;
            }
        }
        Destination::Broadcast => {
            let target = msg.meta.target.as_deref();
            for (uuid, handle) in nodes_guard.iter() {
                if *uuid == src_uuid {
                    continue;
                }
                if !can_route(src_handle, handle) {
                    continue;
                }
                if let Some(pattern) = target {
                    if !pattern_match(pattern, &handle.name) {
                        continue;
                    }
                }
                let _ = handle.sender.send(serde_json::to_vec(msg)?);
            }
        }
        Destination::Resolve => {
            send_unreachable(msg, nodes, router_uuid, "OPA_UNAVAILABLE").await?;
        }
    }
    Ok(())
}

async fn send_unreachable(
    msg: &Message,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    router_uuid: Uuid,
    reason: &str,
) -> Result<(), RouterError> {
    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    let nodes_guard = nodes.lock().await;
    let Some(src_handle) = nodes_guard.get(&src_uuid) else {
        return Ok(());
    };
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
    let _ = src_handle.sender.send(serde_json::to_vec(&err)?);
    Ok(())
}

async fn send_ttl_exceeded(
    msg: &Message,
    nodes: &Arc<Mutex<std::collections::HashMap<Uuid, NodeHandle>>>,
    router_uuid: Uuid,
) -> Result<(), RouterError> {
    let src_uuid = match Uuid::parse_str(&msg.routing.src) {
        Ok(uuid) => uuid,
        Err(_) => return Ok(()),
    };
    let nodes_guard = nodes.lock().await;
    let Some(src_handle) = nodes_guard.get(&src_uuid) else {
        return Ok(());
    };
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
    let _ = src_handle.sender.send(serde_json::to_vec(&err)?);
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
}

fn assign_vpn(name: &str, snapshot: Option<&ConfigSnapshot>) -> u32 {
    if name.starts_with("SY.") || name.starts_with("RT.") {
        return 0;
    }
    let Some(snapshot) = snapshot else {
        return 0;
    };
    let mut rules: Vec<(u16, &str, u8, u32, u16)> = Vec::new();
    for entry in &snapshot.vpns {
        let flags = entry.flags;
        if flags != 0 && flags & FLAG_ACTIVE == 0 {
            continue;
        }
        let pattern = bytes_to_string(&entry.pattern, entry.pattern_len as usize);
        rules.push((entry.priority, pattern, entry.match_kind, entry.vpn_id, flags));
    }
    rules.sort_by_key(|rule| rule.0);
    for (_, pattern, match_kind, vpn_id, _) in rules {
        if pattern_match_kind(match_kind, pattern, name) {
            return vpn_id;
        }
    }
    0
}

fn bytes_to_string(buf: &[u8], len: usize) -> &str {
    let len = len.min(buf.len());
    std::str::from_utf8(&buf[..len]).unwrap_or("")
}

fn can_route(src: &NodeHandle, dst: &NodeHandle) -> bool {
    if src.name.starts_with("SY.") || src.name.starts_with("RT.") {
        return true;
    }
    src.vpn_id == dst.vpn_id
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
