use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tokio::time;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
struct Message<T> {
    routing: serde_json::Value,
    meta: Meta,
    payload: T,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    #[serde(rename = "type")]
    kind: String,
    msg: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnnouncePayload {
    uuid: String,
    name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Idle,
    SenderFixed,
    ResponderOnly,
}

struct SharedState {
    role: Role,
    seq: u64,
}

#[tokio::main]
async fn main() -> io::Result<()> {
    let uuid = env::var("JSR_NODE_UUID")
        .ok()
        .and_then(|value| Uuid::parse_str(&value).ok())
        .unwrap_or_else(Uuid::new_v4);
    let name = env::var("JSR_NODE_NAME").unwrap_or_else(|_| "WF.echo".to_string());
    let socket_path = env::var("JSR_NODE_SOCKET")
        .unwrap_or_else(|_| format!("/var/run/mesh/nodes/{}.sock", uuid));

    let socket_path = PathBuf::from(socket_path);
    ensure_parent_dir(&socket_path)?;
    let _ = fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    println!("node-stub listening on {}", socket_path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        println!("router connected");
        if let Err(err) = handle_connection(stream, &uuid, &name).await {
            eprintln!("connection error: {err}");
        }
    }
}

async fn handle_connection(stream: UnixStream, uuid: &Uuid, name: &str) -> io::Result<()> {
    let (mut reader, mut writer) = stream.into_split();
    let Some(frame) = read_frame(&mut reader).await? else {
        return Ok(());
    };

    let msg: Message<serde_json::Value> = serde_json::from_slice(&frame)?;
    if msg.meta.kind == "system" && msg.meta.msg == "QUERY" {
        send_announce(&mut writer, uuid, name).await?;
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let state = Arc::new(Mutex::new(SharedState {
        role: Role::Idle,
        seq: 0,
    }));

    let sender_state = Arc::clone(&state);
    let sender_name = name.to_string();
    let sender_uuid = *uuid;
    let sender_tx = tx.clone();
    tokio::spawn(async move {
        send_loop(sender_tx, sender_uuid, &sender_name, sender_state).await;
    });

    tokio::spawn(async move {
        while let Some(payload) = rx.recv().await {
            if write_frame(&mut writer, &payload).await.is_err() {
                break;
            }
        }
    });

    loop {
        match read_frame(&mut reader).await? {
            Some(frame) => {
                handle_incoming(&frame, &state, uuid, name, &tx).await?;
            }
            None => break,
        }
    }

    Ok(())
}

async fn send_announce<W>(stream: &mut W, uuid: &Uuid, name: &str) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let msg = Message {
        routing: serde_json::Value::Null,
        meta: Meta {
            kind: "system".to_string(),
            msg: "ANNOUNCE".to_string(),
        },
        payload: AnnouncePayload {
            uuid: uuid.to_string(),
            name: name.to_string(),
        },
    };
    let data = serde_json::to_vec(&msg)?;
    write_frame(stream, &data).await
}

async fn send_loop(
    tx: mpsc::UnboundedSender<Vec<u8>>,
    uuid: Uuid,
    name: &str,
    state: Arc<Mutex<SharedState>>,
) {
    let target = match env::var("JSR_SEND_TARGET").ok() {
        Some(target) => target,
        None => return,
    };
    let interval_ms = env::var("JSR_SEND_INTERVAL_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1000);
    let min_delay_ms = env::var("JSR_SEND_MIN_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(300);
    let max_delay_ms = env::var("JSR_SEND_MAX_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1200);

    loop {
        let role = { state.lock().await.role };
        match role {
            Role::ResponderOnly => {
                time::sleep(Duration::from_millis(200)).await;
            }
            Role::Idle => {
                let delay = jitter_ms(min_delay_ms, max_delay_ms);
                time::sleep(Duration::from_millis(delay)).await;
                if send_ping(&tx, &uuid, name, &target, &state).await.is_err() {
                    break;
                }
            }
            Role::SenderFixed => {
                if send_ping(&tx, &uuid, name, &target, &state).await.is_err() {
                    break;
                }
                time::sleep(Duration::from_millis(interval_ms)).await;
            }
        }
    }
}

async fn send_ping(
    tx: &mpsc::UnboundedSender<Vec<u8>>,
    uuid: &Uuid,
    name: &str,
    target: &str,
    state: &Arc<Mutex<SharedState>>,
) -> Result<(), ()> {
    let seq = {
        let mut state = state.lock().await;
        state.seq += 1;
        state.role = Role::SenderFixed;
        state.seq
    };
    let msg = serde_json::json!({
        "routing": {
            "src": uuid.to_string(),
            "dst": null,
            "ttl": 16,
            "trace_id": Uuid::new_v4().to_string(),
        },
        "meta": {
            "target": target,
        },
        "payload": {
            "type": "ping",
            "seq": seq,
            "from": name,
        }
    });
    println!("SE: ping seq={} target={}", seq, target);
    let data = serde_json::to_vec(&msg).map_err(|_| ())?;
    tx.send(data).map_err(|_| ())
}

fn send_pong(
    tx: &mpsc::UnboundedSender<Vec<u8>>,
    uuid: &Uuid,
    name: &str,
    dst_uuid: Option<String>,
    seq: u64,
) -> Result<(), ()> {
    let msg = serde_json::json!({
        "routing": {
            "src": uuid.to_string(),
            "dst": dst_uuid,
            "ttl": 16,
            "trace_id": Uuid::new_v4().to_string(),
        },
        "meta": {},
        "payload": {
            "type": "pong",
            "seq": seq,
            "from": name,
        }
    });
    println!("SE: pong seq={}", seq);
    let data = serde_json::to_vec(&msg).map_err(|_| ())?;
    tx.send(data).map_err(|_| ())
}

async fn read_frame<R>(stream: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    let mut read = 0usize;
    while read < len_buf.len() {
        let n = stream.read(&mut len_buf[read..]).await?;
        if n == 0 {
            if read == 0 {
                return Ok(None);
            }
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial frame header"));
        }
        read += n;
    }

    let len = u32::from_be_bytes(len_buf) as usize;
    let mut buf = vec![0u8; len];
    let mut read = 0usize;
    while read < buf.len() {
        let n = stream.read(&mut buf[read..]).await?;
        if n == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "partial frame body"));
        }
        read += n;
    }
    Ok(Some(buf))
}

async fn write_frame<W>(stream: &mut W, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = payload.len() as u32;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(payload).await?;
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

async fn handle_incoming(
    frame: &[u8],
    state: &Arc<Mutex<SharedState>>,
    uuid: &Uuid,
    name: &str,
    tx: &mpsc::UnboundedSender<Vec<u8>>,
) -> io::Result<()> {
    let value: serde_json::Value = match serde_json::from_slice(frame) {
        Ok(value) => value,
        Err(_) => {
            println!("RE: {}", String::from_utf8_lossy(frame));
            return Ok(());
        }
    };

    if let Some(meta) = value.get("meta") {
        let kind = meta.get("type").and_then(|v| v.as_str());
        if kind == Some("system") {
            println!("RE: system {}", value);
            return Ok(());
        }
    }

    let payload_type = value
        .get("payload")
        .and_then(|payload| payload.get("type"))
        .and_then(|kind| kind.as_str());
    let seq = value
        .get("payload")
        .and_then(|payload| payload.get("seq"))
        .and_then(|seq| seq.as_u64())
        .unwrap_or(0);

    let dst_uuid = value
        .get("routing")
        .and_then(|routing| routing.get("src"))
        .and_then(|src| src.as_str())
        .map(|s| s.to_string());

    match payload_type {
        Some("ping") => {
            println!("RE: ping seq={}", seq);
            {
                let mut state = state.lock().await;
                if state.role == Role::Idle {
                    state.role = Role::ResponderOnly;
                }
            }
            let _ = send_pong(tx, uuid, name, dst_uuid, seq);
        }
        Some("pong") => {
            println!("RE: pong seq={}", seq);
        }
        _ => {
            println!("RE: {}", value);
        }
    }
    Ok(())
}

fn jitter_ms(min_ms: u64, max_ms: u64) -> u64 {
    if max_ms <= min_ms {
        return min_ms;
    }
    let span = max_ms - min_ms;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    min_ms + (now % (span + 1))
}
