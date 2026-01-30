use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio::time::{self, Duration};
use uuid::Uuid;

use crate::split::{ConnectionInfo, ConnectionState, NodeReceiver, NodeSender};
use crate::protocol::{
    build_hello, Message, NodeAnnouncePayload, NodeHelloPayload, MSG_ANNOUNCE, SYSTEM_KIND,
};
use crate::socket::connection::{read_frame, write_frame};

#[derive(Debug)]
pub struct NodeConfig {
    pub name: String,
    pub router_socket: PathBuf,
    pub uuid_persistence_dir: PathBuf,
    pub config_dir: PathBuf,
    pub version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("invalid announce")]
    InvalidAnnounce,
    #[error("handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("disconnected")]
    Disconnected,
    #[error("timeout")]
    Timeout,
}

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
}

pub async fn connect(
    config: &NodeConfig,
) -> Result<(NodeSender, NodeReceiver), NodeError> {
    let parts = connect_parts(config).await?;
    let info = Arc::new(ConnectionInfo::new(
        parts.uuid.to_string(),
        parts.full_name.clone(),
        parts.vpn_id,
        Arc::clone(&parts.state),
    ));
    let sender = NodeSender::new(parts.tx, Arc::clone(&info));
    let receiver = NodeReceiver::new(parts.rx, info);
    Ok((sender, receiver))
}

struct ConnectedParts {
    tx: mpsc::Sender<Vec<u8>>,
    rx: mpsc::Receiver<Result<Message, NodeError>>,
    uuid: Uuid,
    full_name: String,
    vpn_id: u32,
    router_name: String,
    state: Arc<ConnectionState>,
    manager_task: JoinHandle<()>,
}

async fn connect_parts(config: &NodeConfig) -> Result<ConnectedParts, NodeError> {
    let island_id = load_island_id(&config.config_dir)?;
    let (full_name, base_name) = normalize_name(&config.name, &island_id);
    let uuid = load_or_create_uuid(&config.uuid_persistence_dir, &base_name)?;

    let state = Arc::new(ConnectionState::new_connected());
    let (app_tx, internal_rx) = mpsc::channel::<Vec<u8>>(256);
    let (internal_tx, app_rx) = mpsc::channel::<Result<Message, NodeError>>(256);
    let shared_rx = Arc::new(Mutex::new(internal_rx));

    let (announce_tx, announce_rx) = oneshot::channel::<Result<NodeAnnouncePayload, NodeError>>();
    let manager_cfg = ManagerConfig {
        router_socket: config.router_socket.clone(),
        base_name: base_name.clone(),
        uuid: uuid.to_string(),
        full_name: full_name.clone(),
        version: config.version.clone(),
    };
    let manager_task = tokio::spawn(connection_manager_loop(
        manager_cfg,
        Arc::clone(&state),
        Arc::clone(&shared_rx),
        internal_tx,
        Some(announce_tx),
    ));

    let payload = match announce_rx.await {
        Ok(Ok(payload)) => payload,
        Ok(Err(err)) => {
            manager_task.abort();
            return Err(err);
        }
        Err(_) => {
            manager_task.abort();
            return Err(NodeError::HandshakeFailed("announce channel closed".into()));
        }
    };

    Ok(ConnectedParts {
        tx: app_tx,
        rx: app_rx,
        uuid,
        full_name: payload.name,
        vpn_id: payload.vpn_id,
        router_name: payload.router_name,
        state,
        manager_task,
    })
}

struct ManagerConfig {
    router_socket: PathBuf,
    base_name: String,
    uuid: String,
    full_name: String,
    version: String,
}

async fn connection_manager_loop(
    cfg: ManagerConfig,
    state: Arc<ConnectionState>,
    app_tx_rx: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    app_rx_tx: mpsc::Sender<Result<Message, NodeError>>,
    mut announce_tx: Option<oneshot::Sender<Result<NodeAnnouncePayload, NodeError>>>,
) {
    let mut backoff = Duration::from_millis(100);
    loop {
        if tx_queue_closed(&app_tx_rx).await {
            state.set_connected(false);
            break;
        }
        state.set_connected(false);
        drain_tx_queue(&app_tx_rx).await;
        match connect_stream(&cfg.router_socket, &cfg.base_name).await {
            Ok(mut stream) => {
                match perform_handshake(&mut stream, &cfg).await {
                    Ok(announce) => {
                        if let Some(tx) = announce_tx.take() {
                            let _ = tx.send(Ok(announce));
                        }
                        state.set_connected(true);
                        let (read_half, write_half) = stream.into_split();
                        let rx_state = Arc::clone(&state);
                        let tx_state = Arc::clone(&state);
                        let rx_task = tokio::spawn(rx_loop(
                            read_half,
                            app_rx_tx.clone(),
                            rx_state,
                        ));
                        let tx_task = tokio::spawn(tx_loop(
                            write_half,
                            Arc::clone(&app_tx_rx),
                            tx_state,
                        ));
                        tokio::select! {
                            _ = rx_task => {},
                            _ = tx_task => {},
                        }
                        rx_task.abort();
                        tx_task.abort();
                        state.set_connected(false);
                        backoff = Duration::from_millis(100);
                        continue;
                    }
                    Err(err) => {
                        if let Some(tx) = announce_tx.take() {
                            let _ = tx.send(Err(err));
                        }
                    }
                }
            }
            Err(err) => {
                if let Some(tx) = announce_tx.take() {
                    let _ = tx.send(Err(err));
                }
            }
        }
        time::sleep(backoff).await;
        backoff = std::cmp::min(backoff * 2, Duration::from_secs(30));
    }
}

async fn connect_stream(
    router_socket: &Path,
    base_name: &str,
) -> Result<UnixStream, NodeError> {
    let candidates = router_socket_candidates(router_socket, base_name)?;
    let mut last_err = None;
    for socket_path in candidates {
        match UnixStream::connect(&socket_path).await {
            Ok(stream) => {
                tracing::info!(
                    socket = %socket_path.display(),
                    "connected to router socket"
                );
                return Ok(stream);
            }
            Err(err) => {
                last_err = Some(err);
            }
        }
    }
    let err = last_err.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no router sockets found")
    });
    Err(err.into())
}

async fn perform_handshake(
    stream: &mut UnixStream,
    cfg: &ManagerConfig,
) -> Result<NodeAnnouncePayload, NodeError> {
    let trace_id = Uuid::new_v4().to_string();
    let hello = build_hello(
        &cfg.uuid,
        &trace_id,
        NodeHelloPayload {
            uuid: cfg.uuid.clone(),
            name: cfg.full_name.clone(),
            version: cfg.version.clone(),
        },
    );
    let payload = serde_json::to_vec(&hello)?;
    tracing::info!(name = %cfg.full_name, "sending HELLO");
    write_frame(stream, &payload).await?;

    tracing::info!("waiting for ANNOUNCE");
    let announce = read_frame(stream)
        .await?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "announce"))?;
    let msg: Message = serde_json::from_slice(&announce)?;
    if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_ANNOUNCE) {
        return Err(NodeError::HandshakeFailed("expected ANNOUNCE".to_string()));
    }
    Ok(serde_json::from_value(msg.payload)?)
}

async fn rx_loop(
    mut socket: OwnedReadHalf,
    tx: mpsc::Sender<Result<Message, NodeError>>,
    state: Arc<ConnectionState>,
) {
    loop {
        match read_frame(&mut socket).await {
            Ok(Some(bytes)) => match serde_json::from_slice::<Message>(&bytes) {
                Ok(msg) => {
                    if tx.send(Ok(msg)).await.is_err() {
                        state.set_connected(false);
                        break;
                    }
                }
                Err(err) => {
                    if tx.send(Err(NodeError::Json(err))).await.is_err() {
                        state.set_connected(false);
                        break;
                    }
                }
            },
            Ok(None) => {
                state.set_connected(false);
                let _ = tx
                    .send(Err(NodeError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "socket closed",
                    ))))
                    .await;
                break;
            }
            Err(err) => {
                state.set_connected(false);
                let _ = tx.send(Err(NodeError::Io(err))).await;
                break;
            }
        }
    }
}

async fn tx_loop(
    mut socket: OwnedWriteHalf,
    rx: Arc<Mutex<mpsc::Receiver<Vec<u8>>>>,
    state: Arc<ConnectionState>,
) {
    loop {
        let frame = {
            let mut guard = rx.lock().await;
            guard.recv().await
        };
        match frame {
            Some(frame) => {
                if write_frame(&mut socket, &frame).await.is_err() {
                    state.set_connected(false);
                    break;
                }
            }
            None => {
                state.set_connected(false);
                break;
            }
        }
    }
}

async fn drain_tx_queue(rx: &Arc<Mutex<mpsc::Receiver<Vec<u8>>>>) {
    let mut guard = rx.lock().await;
    while guard.try_recv().is_ok() {}
}

async fn tx_queue_closed(rx: &Arc<Mutex<mpsc::Receiver<Vec<u8>>>>) -> bool {
    let guard = rx.lock().await;
    guard.is_closed() && guard.is_empty()
}

fn load_island_id(config_dir: &Path) -> Result<String, NodeError> {
    let path = config_dir.join("island.yaml");
    let data = fs::read_to_string(&path)?;
    let island: IslandFile = serde_yaml::from_str(&data)?;
    Ok(island.island_id)
}

fn normalize_name(name: &str, island_id: &str) -> (String, String) {
    if let Some((base, _)) = name.split_once('@') {
        (name.to_string(), base.to_string())
    } else {
        (format!("{}@{}", name, island_id), name.to_string())
    }
}

fn load_or_create_uuid(dir: &Path, base_name: &str) -> Result<Uuid, NodeError> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.uuid", base_name));
    if path.exists() {
        let data = fs::read_to_string(&path)?;
        return Ok(Uuid::parse_str(data.trim())?);
    }
    let uuid = Uuid::new_v4();
    fs::write(&path, uuid.to_string())?;
    Ok(uuid)
}

fn router_socket_candidates(path: &Path, node_name: &str) -> Result<Vec<PathBuf>, NodeError> {
    if path.is_dir() {
        let mut sockets: Vec<PathBuf> = fs::read_dir(path)?
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|entry| {
                entry
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext == "sock")
                    .unwrap_or(false)
            })
            .filter(|entry| {
                entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| !name.starts_with("irp-"))
                    .unwrap_or(true)
            })
            .collect();
        sockets.sort();
        if sockets.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no router sockets found",
            )
            .into());
        }
        let start = (fnv1a64(node_name.as_bytes()) % sockets.len() as u64) as usize;
        let mut ordered = Vec::with_capacity(sockets.len());
        for offset in 0..sockets.len() {
            let idx = (start + offset) % sockets.len();
            ordered.push(sockets[idx].clone());
        }
        return Ok(ordered);
    }
    Ok(vec![path.to_path_buf()])
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
