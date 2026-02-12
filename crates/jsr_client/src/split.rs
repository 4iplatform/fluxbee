use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json;
use tokio::sync::{mpsc, Notify};
use tokio::time::{self, Duration};
use uuid::Uuid;

use crate::node_client::NodeError;
use crate::protocol::{build_withdraw, Destination, Message};

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
    full_name: String,
    vpn_id: u32,
    state: Arc<ConnectionState>,
}

impl ConnectionInfo {
    pub(crate) fn new(
        uuid: String,
        full_name: String,
        vpn_id: u32,
        state: Arc<ConnectionState>,
    ) -> Self {
        Self {
            uuid,
            full_name,
            vpn_id,
            state,
        }
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
        let data = serde_json::to_vec(&msg)?;
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

    pub fn full_name(&self) -> &str {
        &self.info.full_name
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

    pub fn full_name(&self) -> &str {
        &self.info.full_name
    }

    pub fn vpn_id(&self) -> u32 {
        self.info.vpn_id
    }
}
