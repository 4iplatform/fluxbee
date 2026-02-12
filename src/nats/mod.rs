use std::io;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub const SUBJECT_STORAGE_TURNS: &str = "storage.turns";

const CONNECT_LINE: &str = "CONNECT {\"lang\":\"rust\",\"version\":\"0.1\",\"verbose\":false,\"pedantic\":false,\"tls_required\":false}\r\n";

#[derive(Clone)]
pub struct NatsPublisher {
    endpoint: String,
    subject: String,
}

impl NatsPublisher {
    pub fn new(endpoint: String, subject: String) -> Self {
        Self { endpoint, subject }
    }

    pub async fn publish(&self, payload: &[u8]) -> Result<(), io::Error> {
        publish(&self.endpoint, &self.subject, payload).await
    }
}

pub async fn publish(endpoint: &str, subject: &str, payload: &[u8]) -> Result<(), io::Error> {
    let addr = endpoint_to_addr(endpoint)?;
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(CONNECT_LINE.as_bytes()).await?;
    stream
        .write_all(format!("PUB {subject} {}\r\n", payload.len()).as_bytes())
        .await?;
    stream.write_all(payload).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    Ok(())
}

pub struct NatsSubscriber {
    endpoint: String,
    subject: String,
    sid: u32,
}

impl NatsSubscriber {
    pub fn new(endpoint: String, subject: String, sid: u32) -> Self {
        Self {
            endpoint,
            subject,
            sid,
        }
    }

    pub async fn run<F, Fut>(&self, mut handler: F) -> Result<(), io::Error>
    where
        F: FnMut(Vec<u8>) -> Fut,
        Fut: std::future::Future<Output = Result<(), io::Error>>,
    {
        let addr = endpoint_to_addr(&self.endpoint)?;
        let stream = TcpStream::connect(addr).await?;
        let (reader_half, mut writer_half) = stream.into_split();
        writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
        writer_half
            .write_all(format!("SUB {} {}\r\n", self.subject, self.sid).as_bytes())
            .await?;
        writer_half.write_all(b"PING\r\n").await?;
        writer_half.flush().await?;

        let mut reader = BufReader::new(reader_half);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "nats socket closed",
                ));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            if line == "PING" {
                writer_half.write_all(b"PONG\r\n").await?;
                writer_half.flush().await?;
                continue;
            }
            if line == "PONG" || line.starts_with("+OK") || line.starts_with("INFO ") {
                continue;
            }
            if line.starts_with("-ERR") {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("nats error: {line}"),
                ));
            }
            if line.starts_with("MSG ") {
                let payload_len = parse_payload_len(line)?;
                let mut payload = vec![0u8; payload_len + 2];
                reader.read_exact(&mut payload).await?;
                payload.truncate(payload_len);
                handler(payload).await?;
            }
        }
    }
}

fn endpoint_to_addr(endpoint: &str) -> Result<String, io::Error> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty nats endpoint",
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("nats://") {
        if rest.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid nats endpoint",
            ));
        }
        return Ok(rest.to_string());
    }
    Ok(trimmed.to_string())
}

fn parse_payload_len(msg_line: &str) -> Result<usize, io::Error> {
    let mut parts = msg_line.split_whitespace();
    let _ = parts.next(); // MSG
    let _subject = parts.next();
    let _sid = parts.next();
    let maybe_reply_or_len = parts.next();
    let last = parts.next();
    let len_str = match (maybe_reply_or_len, last) {
        (Some(len), None) => len,
        (Some(_reply), Some(len)) => len,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid MSG line: {msg_line}"),
            ))
        }
    };
    len_str.parse::<usize>().map_err(|err| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid MSG length '{len_str}': {err}"),
        )
    })
}
