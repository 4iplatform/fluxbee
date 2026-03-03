use std::path::PathBuf;
use std::time::Duration;

use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};

use crate::errors::Result;
use crate::message::Message;

#[derive(Debug, Clone)]
pub struct AiNodeConfig {
    pub name: String,
    pub router_socket: PathBuf,
    pub uuid_persistence_dir: PathBuf,
    pub config_dir: PathBuf,
    pub version: String,
}

impl From<AiNodeConfig> for NodeConfig {
    fn from(value: AiNodeConfig) -> Self {
        NodeConfig {
            name: value.name,
            router_socket: value.router_socket,
            uuid_persistence_dir: value.uuid_persistence_dir,
            config_dir: value.config_dir,
            version: value.version,
        }
    }
}

pub struct RouterClient {
    reader: RouterReader,
    writer: RouterWriter,
}

pub struct RouterReader {
    receiver: NodeReceiver,
}

#[derive(Clone)]
pub struct RouterWriter {
    sender: NodeSender,
}

impl RouterClient {
    pub async fn connect(config: AiNodeConfig) -> Result<Self> {
        let (sender, receiver) = connect(&config.into()).await?;
        Ok(Self {
            reader: RouterReader { receiver },
            writer: RouterWriter { sender },
        })
    }

    pub async fn read(&mut self) -> Result<Message> {
        self.reader.read().await
    }

    pub async fn read_timeout(&mut self, timeout: Duration) -> Result<Message> {
        self.reader.read_timeout(timeout).await
    }

    pub async fn write(&self, msg: Message) -> Result<()> {
        self.writer.write(msg).await?;
        Ok(())
    }

    pub fn uuid(&self) -> &str {
        self.writer.uuid()
    }

    pub fn split(self) -> (RouterReader, RouterWriter) {
        (self.reader, self.writer)
    }
}

impl RouterReader {
    pub async fn read(&mut self) -> Result<Message> {
        Ok(self.receiver.recv().await?)
    }

    pub async fn read_timeout(&mut self, timeout: Duration) -> Result<Message> {
        Ok(self.receiver.recv_timeout(timeout).await?)
    }
}

impl RouterWriter {
    pub async fn write(&self, msg: Message) -> Result<()> {
        self.sender.send(msg).await?;
        Ok(())
    }

    pub fn uuid(&self) -> &str {
        self.sender.uuid()
    }
}
