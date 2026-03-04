#![forbid(unsafe_code)]

use router_protocol::Message;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{mpsc, watch};

pub mod framing;
pub mod handshake;

#[derive(Debug, Clone)]
struct ConnectionInfo {
    vpn_id: u32,
    router_name: String,
}

#[derive(Debug, Clone)]
pub struct NodeConfig {
    pub name: String,
    pub island_id: String,
    pub node_uuid: String,
    pub version: String,
    /// Router endpoint. Supported formats:
    /// - `tcp:host:port` (or `host:port`)
    /// - `unix:/path/to/socket` (unix only)
    pub router_addr: String,
    pub queue_capacity: usize,
}

#[derive(Debug, Clone)]
pub struct NodeSender {
    tx: mpsc::Sender<Message>,
    connected: Arc<AtomicBool>,
    shutdown_tx: watch::Sender<bool>,
    node_uuid: String,
    full_name: String,
}

impl NodeSender {
    pub async fn send(&self, _msg: Message) -> Result<(), NodeError> {
        if !self.connected.load(Ordering::Relaxed) {
            return Err(NodeError::Disconnected);
        }
        self.tx.send(_msg).await.map_err(|_| NodeError::Disconnected)
    }

    pub async fn close(self) -> Result<(), NodeError> {
        // Best-effort: signal shutdown and let the connection manager send WITHDRAW.
        let _ = self.shutdown_tx.send(true);
        Ok(())
    }

    pub fn uuid(&self) -> &str {
        &self.node_uuid
    }

    pub fn full_name(&self) -> &str {
        &self.full_name
    }
}

#[derive(Debug)]
pub struct NodeReceiver {
    rx: mpsc::Receiver<Message>,
    connected: Arc<AtomicBool>,
    conn_info: Arc<std::sync::RwLock<ConnectionInfo>>,
    node_uuid: String,
    full_name: String,
}

impl NodeReceiver {
    pub async fn recv(&mut self) -> Result<Message, NodeError> {
        self.rx.recv().await.ok_or(NodeError::Disconnected)
    }

    pub async fn recv_timeout(&mut self, timeout: std::time::Duration) -> Result<Message, NodeError> {
        match tokio::time::timeout(timeout, self.recv()).await {
            Ok(r) => r,
            Err(_) => Err(NodeError::Timeout),
        }
    }

    pub fn try_recv(&mut self) -> Option<Message> {
        self.rx.try_recv().ok()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub async fn wait_connected(&self) {
        while !self.is_connected() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub fn uuid(&self) -> &str {
        &self.node_uuid
    }

    pub fn full_name(&self) -> &str {
        &self.full_name
    }

    pub fn vpn_id(&self) -> u32 {
        self.conn_info
            .read()
            .map(|i| i.vpn_id)
            .unwrap_or_default()
    }

    pub fn router_name(&self) -> String {
        self.conn_info
            .read()
            .map(|i| i.router_name.clone())
            .unwrap_or_default()
    }
}

pub async fn connect(_config: NodeConfig) -> Result<(NodeSender, NodeReceiver), NodeError> {
    let (mut stream, full_name) =
        connect_stream(&_config.router_addr, &_config.name, &_config.island_id).await?;

    let announce = handshake::hello_announce(
        &mut stream,
        handshake::Hello {
            node_uuid: _config.node_uuid.clone(),
            full_name: full_name.clone(),
            version: _config.version.clone(),
            trace_id: new_trace_id(),
        },
    )
    .await
    .map_err(|e| NodeError::HandshakeFailed(e.to_string()))?;

    let connected = Arc::new(AtomicBool::new(true));
    let conn_info = Arc::new(std::sync::RwLock::new(ConnectionInfo {
        vpn_id: announce.vpn_id,
        router_name: announce.router_name,
    }));

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let (tx_to_socket, rx_from_sender) = mpsc::channel::<Message>(_config.queue_capacity);
    let (tx_to_receiver, rx_to_user) = mpsc::channel::<Message>(_config.queue_capacity);

    tokio::spawn(connection_manager(
        _config.clone(),
        full_name.clone(),
        connected.clone(),
        conn_info.clone(),
        stream,
        rx_from_sender,
        tx_to_receiver,
        shutdown_rx,
    ));

    let sender = NodeSender {
        tx: tx_to_socket,
        connected: connected.clone(),
        shutdown_tx,
        node_uuid: _config.node_uuid.clone(),
        full_name: full_name.clone(),
    };

    let receiver = NodeReceiver {
        rx: rx_to_user,
        connected,
        conn_info,
        node_uuid: _config.node_uuid,
        full_name,
    };

    Ok((sender, receiver))
}

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("disconnected")]
    Disconnected,

    #[error("timeout")]
    Timeout,

    #[error("handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("unsupported on this platform: {0}")]
    Unsupported(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

fn new_trace_id() -> String {
    // UUID generation will be handled centrally later; keep it simple for now.
    format!("trace-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos())
}

type DynStream = Box<dyn AsyncReadWriteUnpinSend>;

async fn connect_stream(
    router_addr: &str,
    name: &str,
    island_id: &str,
) -> Result<(DynStream, String), NodeError> {
    let full_name = format!("{name}@{island_id}");

    if let Some(path) = router_addr.strip_prefix("unix:") {
        #[cfg(unix)]
        {
            let stream = tokio::net::UnixStream::connect(path).await?;
            return Ok((Box::new(stream), full_name));
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            return Err(NodeError::Unsupported("unix sockets".to_string()));
        }
    }

    let addr = router_addr.strip_prefix("tcp:").unwrap_or(router_addr);
    let stream = tokio::net::TcpStream::connect(addr).await?;
    Ok((Box::new(stream), full_name))
}

trait AsyncReadWriteUnpinSend:
    tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}
impl<T> AsyncReadWriteUnpinSend for T where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static
{
}

async fn connection_manager(
    config: NodeConfig,
    full_name: String,
    connected: Arc<AtomicBool>,
    conn_info: Arc<std::sync::RwLock<ConnectionInfo>>,
    initial_stream: DynStream,
    mut rx_from_sender: mpsc::Receiver<Message>,
    tx_to_receiver: mpsc::Sender<Message>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    let mut stream = Some(initial_stream);
    let mut backoff = Duration::from_millis(100);

    loop {
        if *shutdown_rx.borrow() {
            connected.store(false, Ordering::Relaxed);
            break;
        }

        let mut stream_to_use = match stream.take() {
            Some(s) => s,
            None => {
                match connect_stream(&config.router_addr, &config.name, &config.island_id).await {
                    Ok((s, _)) => s,
                    Err(_) => {
                        connected.store(false, Ordering::Relaxed);
                        tokio::time::sleep(backoff).await;
                        backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                        continue;
                    }
                }
            }
        };

        // After a reconnect, we need to re-run HELLO/ANNOUNCE.
        if !connected.load(Ordering::Relaxed) {
            match handshake::hello_announce(
                &mut stream_to_use,
                handshake::Hello {
                    node_uuid: config.node_uuid.clone(),
                    full_name: full_name.clone(),
                    version: config.version.clone(),
                    trace_id: new_trace_id(),
                },
            )
            .await
            {
                Ok(announce) => {
                    if let Ok(mut guard) = conn_info.write() {
                        guard.vpn_id = announce.vpn_id;
                        guard.router_name = announce.router_name;
                    }
                    connected.store(true, Ordering::Relaxed);
                    backoff = Duration::from_millis(100);
                }
                Err(_) => {
                    connected.store(false, Ordering::Relaxed);
                    tokio::time::sleep(backoff).await;
                    backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
                    continue;
                }
            }
        }

        let (mut stream_read, mut stream_write) = tokio::io::split(stream_to_use);

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        let _ = send_withdraw(&mut stream_write, &config.node_uuid, "shutdown").await;
                        connected.store(false, Ordering::Relaxed);
                        return;
                    }
                }
                inbound = crate::framing::read_json_frame::<_, Message>(&mut stream_read) => {
                    match inbound {
                        Ok(msg) => {
                            if tx_to_receiver.send(msg).await.is_err() {
                                connected.store(false, Ordering::Relaxed);
                                return;
                            }
                        }
                        Err(_) => {
                            break;
                        }
                    }
                }
                outbound = rx_from_sender.recv() => {
                    match outbound {
                        Some(msg) => {
                            if crate::framing::write_json_frame(&mut stream_write, &msg).await.is_err() {
                                break;
                            }
                        }
                        None => {
                            let _ = send_withdraw(&mut stream_write, &config.node_uuid, "sender_dropped").await;
                            connected.store(false, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            }
        }

        // Disconnected: set flag, clear queue (as per spec), and reconnect with backoff.
        connected.store(false, Ordering::Relaxed);
        while rx_from_sender.try_recv().is_ok() {}
        stream = None;
        tokio::time::sleep(backoff).await;
        backoff = backoff.saturating_mul(2).min(Duration::from_secs(30));
    }
}

async fn send_withdraw<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    node_uuid: &str,
    reason: &str,
) -> Result<(), framing::FramingError> {
    let msg = Message {
        routing: router_protocol::Routing {
            src: node_uuid.to_string(),
            dst: None,
            ttl: 1,
            trace_id: new_trace_id(),
        },
        meta: router_protocol::Meta {
            ty: "system".to_string(),
            msg: Some("WITHDRAW".to_string()),
            target: None,
            action: None,
            src_ilk: None,
            dst_ilk: None,
            context: serde_json::json!({}),
        },
        payload: serde_json::json!({ "reason": reason }),
    };
    crate::framing::write_json_frame(writer, &msg).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{read_json_frame, write_json_frame};
    use router_protocol::{Meta, Routing};
    use tokio::io::duplex;

    fn make_msg(trace_id: &str) -> Message {
        Message {
            routing: Routing {
                src: "node-uuid".to_string(),
                dst: None,
                ttl: 1,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                ty: "user".to_string(),
                msg: None,
                target: None,
                action: None,
                src_ilk: None,
                dst_ilk: None,
                context: serde_json::json!({}),
            },
            payload: serde_json::json!({ "type": "text", "content": "hi" }),
        }
    }

    #[tokio::test]
    async fn connection_loop_writes_and_reads_frames() {
        let (node_side, mut router_side) = duplex(32 * 1024);

        let config = NodeConfig {
            name: "AI.support.l1".to_string(),
            island_id: "production".to_string(),
            node_uuid: "node-uuid".to_string(),
            version: "1.0".to_string(),
            router_addr: "tcp:127.0.0.1:1".to_string(),
            queue_capacity: 8,
        };

        let connected = Arc::new(AtomicBool::new(true));
        let conn_info = Arc::new(std::sync::RwLock::new(ConnectionInfo {
            vpn_id: 10,
            router_name: "RT.primary@production".to_string(),
        }));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let (tx_to_socket, rx_from_sender) = mpsc::channel::<Message>(8);
        let (tx_to_receiver, mut rx_to_user) = mpsc::channel::<Message>(8);

        let tasks = tokio::spawn(connection_manager(
            config,
            "AI.support.l1@production".to_string(),
            connected.clone(),
            conn_info,
            Box::new(node_side),
            rx_from_sender,
            tx_to_receiver,
            shutdown_rx,
        ));

        // Node -> Router
        tx_to_socket.send(make_msg("t1")).await.unwrap();
        let on_router: Message = read_json_frame(&mut router_side).await.unwrap();
        assert_eq!(on_router.routing.trace_id, "t1");

        // Router -> Node
        write_json_frame(&mut router_side, &make_msg("t2"))
            .await
            .unwrap();
        let on_node = rx_to_user.recv().await.unwrap();
        assert_eq!(on_node.routing.trace_id, "t2");

        let _ = shutdown_tx.send(true);
        drop(tx_to_socket);
        drop(router_side);

        tasks.await.unwrap();
        assert!(!connected.load(Ordering::Relaxed));
    }
}
