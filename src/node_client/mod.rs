use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::net::UnixStream;
use uuid::Uuid;

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

#[derive(Debug)]
pub struct NodeClient {
    stream: UnixStream,
    uuid: Uuid,
    name: String,
    vpn_id: u32,
    router_name: String,
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
}

#[derive(Debug, Deserialize)]
struct IslandFile {
    island_id: String,
}

impl NodeClient {
    pub async fn connect(config: NodeConfig) -> Result<Self, NodeError> {
        let island_id = load_island_id(&config.config_dir)?;
        let (full_name, base_name) = normalize_name(&config.name, &island_id);
        let uuid = load_or_create_uuid(&config.uuid_persistence_dir, &base_name)?;
        let mut stream = UnixStream::connect(&config.router_socket).await?;

        let trace_id = Uuid::new_v4().to_string();
        let hello = build_hello(
            &uuid.to_string(),
            &trace_id,
            NodeHelloPayload {
                uuid: uuid.to_string(),
                name: full_name.clone(),
                version: config.version.clone(),
            },
        );
        let payload = serde_json::to_vec(&hello)?;
        write_frame(&mut stream, &payload).await?;

        let announce = read_frame(&mut stream)
            .await?
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "announce"))?;
        let msg: Message = serde_json::from_slice(&announce)?;
        if msg.meta.msg_type != SYSTEM_KIND || msg.meta.msg.as_deref() != Some(MSG_ANNOUNCE) {
            return Err(NodeError::InvalidAnnounce);
        }
        let payload: NodeAnnouncePayload = serde_json::from_value(msg.payload)?;

        Ok(Self {
            stream,
            uuid,
            name: payload.name,
            vpn_id: payload.vpn_id,
            router_name: payload.router_name,
        })
    }

    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn vpn_id(&self) -> u32 {
        self.vpn_id
    }

    pub fn router_name(&self) -> &str {
        &self.router_name
    }

    pub async fn send(&mut self, msg: &Message) -> Result<(), NodeError> {
        let data = serde_json::to_vec(msg)?;
        write_frame(&mut self.stream, &data).await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Message, NodeError> {
        let frame = read_frame(&mut self.stream)
            .await?
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "recv"))?;
        Ok(serde_json::from_slice(&frame)?)
    }
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
