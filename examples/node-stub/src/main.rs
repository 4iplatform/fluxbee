use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
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

#[tokio::main]
async fn main() -> io::Result<()> {
    let uuid = env::var("JSR_NODE_UUID")
        .ok()
        .and_then(|value| Uuid::parse_str(&value).ok())
        .unwrap_or_else(Uuid::new_v4);
    let name = env::var("JSR_NODE_NAME").unwrap_or_else(|_| "WF.echo".to_string());
    let socket_path = env::var("JSR_NODE_SOCKET").unwrap_or_else(|_| {
        format!("/var/run/mesh/nodes/{}.sock", uuid)
    });

    let socket_path = PathBuf::from(socket_path);
    ensure_parent_dir(&socket_path)?;
    let _ = fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    println!("node-stub listening on {}", socket_path.display());

    loop {
        let (mut stream, _) = listener.accept().await?;
        println!("router connected");
        if let Err(err) = handle_connection(&mut stream, &uuid, &name).await {
            eprintln!("connection error: {err}");
        }
    }
}

async fn handle_connection(stream: &mut UnixStream, uuid: &Uuid, name: &str) -> io::Result<()> {
    let Some(frame) = read_frame(stream).await? else {
        return Ok(());
    };

    let msg: Message<serde_json::Value> = serde_json::from_slice(&frame)?;
    if msg.meta.kind == "system" && msg.meta.msg == "QUERY" {
        send_announce(stream, uuid, name).await?;
        maybe_send_test_message(stream, uuid).await?;
    }

    loop {
        match read_frame(stream).await? {
            Some(frame) => {
                if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&frame) {
                    if let Some(meta) = value.get("meta") {
                        let kind = meta.get("type").and_then(|v| v.as_str());
                        if kind == Some("system") {
                            println!("node-stub system: {}", value);
                            continue;
                        }
                    }
                }
                println!("node-stub received: {}", String::from_utf8_lossy(&frame));
            }
            None => break,
        }
    }

    Ok(())
}

async fn send_announce(stream: &mut UnixStream, uuid: &Uuid, name: &str) -> io::Result<()> {
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

async fn maybe_send_test_message(stream: &mut UnixStream, uuid: &Uuid) -> io::Result<()> {
    let target = env::var("JSR_SEND_TARGET").ok();
    let payload = env::var("JSR_SEND_TEXT").unwrap_or_else(|_| "hello".to_string());
    let ttl = env::var("JSR_SEND_TTL")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(16);
    let src_uuid = uuid.to_string();
    let Some(target) = target else {
        return Ok(());
    };

    let msg = serde_json::json!({
        "routing": {
            "src": src_uuid,
            "dst": null,
            "ttl": ttl,
            "trace_id": Uuid::new_v4().to_string(),
        },
        "meta": {
            "target": target,
        },
        "payload": {
            "type": "text",
            "content": payload,
        }
    });

    let data = serde_json::to_vec(&msg)?;
    time::sleep(Duration::from_millis(100)).await;
    write_frame(stream, &data).await
}

async fn read_frame(stream: &mut UnixStream) -> io::Result<Option<Vec<u8>>> {
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

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
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
