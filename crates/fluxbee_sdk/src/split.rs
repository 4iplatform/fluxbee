use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use serde_json;
use tokio::sync::{mpsc, Notify};
use tokio::time::{self, Duration};
use uuid::Uuid;

use crate::node_client::NodeError;
use crate::protocol::NodeAnnouncePayload;
use crate::socket::connection::MAX_FRAME_SIZE;
use crate::protocol::{build_withdraw, Destination, Message};
use crate::send_normalization::normalize_outbound_message;

pub(crate) struct ConnectionState {
    connected: AtomicBool,
    notify: Notify,
}

impl ConnectionState {
    pub(crate) fn new_connected() -> Self {
        Self {
            connected: AtomicBool::new(true),
            notify: Notify::new(),
        }
    }

    pub(crate) fn set_connected(&self, value: bool) {
        let prev = self.connected.swap(value, Ordering::SeqCst);
        if value && !prev {
            self.notify.notify_waiters();
        }
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub(crate) async fn wait_connected(&self) {
        loop {
            if self.is_connected() {
                return;
            }
            self.notify.notified().await;
        }
    }
}

pub(crate) struct ConnectionInfo {
    uuid: String,
    full_name: RwLock<Arc<str>>,
    vpn_id: AtomicU32,
    router_name: RwLock<Arc<str>>,
    state: Arc<ConnectionState>,
}

impl ConnectionInfo {
    pub(crate) fn new(
        uuid: String,
        full_name: String,
        vpn_id: u32,
        router_name: String,
        state: Arc<ConnectionState>,
    ) -> Self {
        Self {
            uuid,
            full_name: RwLock::new(Arc::<str>::from(full_name)),
            vpn_id: AtomicU32::new(vpn_id),
            router_name: RwLock::new(Arc::<str>::from(router_name)),
            state,
        }
    }

    pub(crate) fn update_from_announce(&self, announce: &NodeAnnouncePayload) {
        if let Ok(mut full_name) = self.full_name.write() {
            *full_name = Arc::<str>::from(announce.name.as_str());
        }
        self.vpn_id.store(announce.vpn_id, Ordering::SeqCst);
        if let Ok(mut router_name) = self.router_name.write() {
            *router_name = Arc::<str>::from(announce.router_name.as_str());
        }
    }

    fn full_name(&self) -> Arc<str> {
        self.full_name
            .read()
            .map(|value| Arc::clone(&value))
            .unwrap_or_else(|_| Arc::<str>::from(""))
    }

    fn vpn_id(&self) -> u32 {
        self.vpn_id.load(Ordering::SeqCst)
    }

    #[allow(dead_code)]
    fn router_name(&self) -> Arc<str> {
        self.router_name
            .read()
            .map(|value| Arc::clone(&value))
            .unwrap_or_else(|_| Arc::<str>::from(""))
    }
}

#[derive(Clone)]
pub struct NodeSender {
    tx: mpsc::Sender<Vec<u8>>,
    info: Arc<ConnectionInfo>,
}

pub struct NodeReceiver {
    rx: mpsc::Receiver<Result<Message, NodeError>>,
    info: Arc<ConnectionInfo>,
}

impl NodeSender {
    pub(crate) fn new(tx: mpsc::Sender<Vec<u8>>, info: Arc<ConnectionInfo>) -> Self {
        Self { tx, info }
    }

    pub async fn send(&self, msg: Message) -> Result<(), NodeError> {
        let msg = normalize_outbound_message(msg);
        let data = serde_json::to_vec(&msg)?;
        if data.len() > MAX_FRAME_SIZE {
            tracing::error!(
                frame_len = data.len(),
                max = MAX_FRAME_SIZE,
                "FRAME_TOO_LARGE — message rejected before send; fix the sending node"
            );
            return Err(NodeError::MessageTooLarge { size: data.len(), max: MAX_FRAME_SIZE });
        }
        self.tx.send(data).await.map_err(|_| {
            self.info.state.set_connected(false);
            NodeError::Disconnected
        })?;
        Ok(())
    }

    pub async fn close(self) -> Result<(), NodeError> {
        let trace_id = Uuid::new_v4().to_string();
        let msg = build_withdraw(
            &self.info.uuid,
            Destination::Resolve,
            &trace_id,
            &self.info.uuid,
        );
        self.send(msg).await?;
        Ok(())
    }

    pub fn uuid(&self) -> &str {
        &self.info.uuid
    }

    pub fn full_name(&self) -> Arc<str> {
        self.info.full_name()
    }
}

impl NodeReceiver {
    pub(crate) fn new(
        rx: mpsc::Receiver<Result<Message, NodeError>>,
        info: Arc<ConnectionInfo>,
    ) -> Self {
        Self { rx, info }
    }

    pub async fn recv(&mut self) -> Result<Message, NodeError> {
        match self.rx.recv().await {
            Some(Ok(msg)) => Ok(msg),
            Some(Err(err)) => Err(err),
            None => {
                self.info.state.set_connected(false);
                Err(NodeError::Disconnected)
            }
        }
    }

    pub async fn recv_timeout(&mut self, timeout: Duration) -> Result<Message, NodeError> {
        match time::timeout(timeout, self.recv()).await {
            Ok(result) => result,
            Err(_) => Err(NodeError::Timeout),
        }
    }

    pub fn try_recv(&mut self) -> Option<Message> {
        match self.rx.try_recv() {
            Ok(Ok(msg)) => Some(msg),
            Ok(Err(_)) => {
                self.info.state.set_connected(false);
                None
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => None,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                self.info.state.set_connected(false);
                None
            }
        }
    }

    pub fn is_connected(&self) -> bool {
        self.info.state.is_connected()
    }

    pub async fn wait_connected(&self) {
        self.info.state.wait_connected().await;
    }

    pub fn uuid(&self) -> &str {
        &self.info.uuid
    }

    pub fn full_name(&self) -> Arc<str> {
        self.info.full_name()
    }

    pub fn vpn_id(&self) -> u32 {
        self.info.vpn_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_info_updates_announce_metadata() {
        let info = ConnectionInfo::new(
            "uuid-1".to_string(),
            "SY.test@old".to_string(),
            1,
            "router-old".to_string(),
            Arc::new(ConnectionState::new_connected()),
        );
        let announce = NodeAnnouncePayload {
            uuid: "uuid-1".to_string(),
            name: "SY.test@new".to_string(),
            status: "ok".to_string(),
            vpn_id: 9,
            router_name: "router-new".to_string(),
        };

        info.update_from_announce(&announce);

        assert_eq!(info.full_name().as_ref(), "SY.test@new");
        assert_eq!(info.vpn_id(), 9);
        assert_eq!(info.router_name().as_ref(), "router-new");
    }
}
