use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tokio::net::UnixStream;
use tokio::time::{self, Duration};
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
    pub async fn connect(config: &NodeConfig) -> Result<Self, NodeError> {
        let island_id = load_island_id(&config.config_dir)?;
        let (full_name, base_name) = normalize_name(&config.name, &island_id);
        let uuid = load_or_create_uuid(&config.uuid_persistence_dir, &base_name)?;
        let candidates = router_socket_candidates(&config.router_socket, &base_name)?;
        let mut last_err = None;
        let mut connected = None;
        for socket_path in candidates {
            match UnixStream::connect(&socket_path).await {
                Ok(stream) => {
                    connected = Some((socket_path, stream));
                    break;
                }
                Err(err) => {
                    last_err = Some(err);
                    continue;
                }
            }
        }
        let (socket_path, mut stream) = match connected {
            Some(value) => value,
            None => {
                let err = last_err.unwrap_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "no router sockets found")
                });
                return Err(err.into());
            }
        };
        tracing::info!(
            socket = %socket_path.display(),
            name = %full_name,
            "connected to router socket"
        );

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
        tracing::info!(name = %full_name, "sending HELLO");
        write_frame(&mut stream, &payload).await?;

        tracing::info!("waiting for ANNOUNCE");
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

    pub async fn connect_with_retry(
        config: &NodeConfig,
        delay: Duration,
    ) -> Result<Self, NodeError> {
        loop {
            match Self::connect(config).await {
                Ok(client) => return Ok(client),
                Err(err) => {
                    tracing::warn!("connect failed: {err}");
                    time::sleep(delay).await;
                }
            }
        }
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
