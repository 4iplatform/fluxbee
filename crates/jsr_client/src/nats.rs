use std::io;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::{timeout, Instant};

const CONNECT_LINE: &str = "CONNECT {\"lang\":\"rust\",\"version\":\"0.1\",\"verbose\":false,\"pedantic\":false,\"tls_required\":false}\r\n";
const NATS_LOG_LINE_MAX_LEN: usize = 220;
const PUBLISH_SYNC_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum NatsError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("timeout: {0}")]
    Timeout(String),
}

#[derive(Clone, Debug)]
pub struct NatsMessage {
    pub subject: String,
    pub sid: String,
    pub reply_to: Option<String>,
    pub payload: Vec<u8>,
}

pub async fn publish(endpoint: &str, subject: &str, payload: &[u8]) -> Result<(), NatsError> {
    let addr = endpoint_to_addr(endpoint)?;
    let publish_started = Instant::now();
    let stream = timeout(PUBLISH_SYNC_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map_err(|_| NatsError::Timeout(format!("publish connect timeout to {endpoint}")))??;
    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
    writer_half
        .write_all(format!("PUB {subject} {}\r\n", payload.len()).as_bytes())
        .await?;
    writer_half.write_all(payload).await?;
    writer_half.write_all(b"\r\n").await?;
    writer_half.write_all(b"PING\r\n").await?;
    writer_half.flush().await?;

    let mut reader = BufReader::new(reader_half);
    let sync_started = Instant::now();
    loop {
        let mut line = String::new();
        let n = timeout(PUBLISH_SYNC_TIMEOUT, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                NatsError::Timeout(format!("publish timeout waiting server ack on subject {subject}"))
            })??;
        if n == 0 {
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            continue;
        }
        if line == "PING" {
            writer_half.write_all(b"PONG\r\n").await?;
            writer_half.flush().await?;
            continue;
        }
        if line == "PONG" {
            tracing::debug!(
                endpoint = %endpoint,
                subject = %subject,
                payload_bytes = payload.len(),
                sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                elapsed_ms = publish_started.elapsed().as_millis() as u64,
                "nats publish completed with server ack"
            );
            break;
        }
        if line.starts_with("-ERR") {
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
    }
    Ok(())
}

pub async fn request(
    endpoint: &str,
    request_subject: &str,
    request_payload: &[u8],
    response_subject: &str,
    sid: u32,
    timeout_duration: Duration,
) -> Result<Vec<u8>, NatsError> {
    let request_started = Instant::now();
    tracing::debug!(
        endpoint = %endpoint,
        request_subject = %request_subject,
        response_subject = %response_subject,
        sid = sid,
        timeout_ms = timeout_duration.as_millis() as u64,
        request_bytes = request_payload.len(),
        "nats request start"
    );

    let addr = endpoint_to_addr(endpoint)?;
    let connect_started = Instant::now();
    let stream = timeout(timeout_duration, TcpStream::connect(&addr))
        .await
        .map_err(|_| {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                timeout_ms = timeout_duration.as_millis() as u64,
                elapsed_ms = connect_started.elapsed().as_millis() as u64,
                "nats request connect timeout"
            );
            NatsError::Timeout(format!("connect timeout to {endpoint}"))
        })??;
    tracing::debug!(
        endpoint = %endpoint,
        request_subject = %request_subject,
        response_subject = %response_subject,
        sid = sid,
        elapsed_ms = connect_started.elapsed().as_millis() as u64,
        "nats request tcp connected"
    );

    let (reader_half, mut writer_half) = stream.into_split();
    writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
    writer_half
        .write_all(format!("SUB {response_subject} {sid}\r\n").as_bytes())
        .await?;
    writer_half.write_all(b"PING\r\n").await?;
    writer_half.flush().await?;
    tracing::debug!(
        endpoint = %endpoint,
        request_subject = %request_subject,
        response_subject = %response_subject,
        sid = sid,
        elapsed_ms = request_started.elapsed().as_millis() as u64,
        "nats request sent CONNECT/SUB/PING"
    );

    let mut reader = BufReader::new(reader_half);
    let deadline = tokio::time::Instant::now() + timeout_duration;
    let sync_started = Instant::now();
    loop {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                "nats request timeout waiting subscription ack"
            );
            return Err(NatsError::Timeout(format!(
                "request timeout waiting subscription ack on {response_subject}"
            )));
        }

        let mut line = String::new();
        let n = timeout(remaining, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                tracing::warn!(
                    endpoint = %endpoint,
                    request_subject = %request_subject,
                    response_subject = %response_subject,
                    sid = sid,
                    elapsed_ms = request_started.elapsed().as_millis() as u64,
                    sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                    "nats request timeout while reading subscription ack line"
                );
                NatsError::Timeout(format!(
                    "request timeout waiting subscription ack on {response_subject}"
                ))
            })??;
        if n == 0 {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request socket closed while waiting subscription ack"
            );
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                line = %summarize_nats_line(line),
                "nats request sync loop ignored server line"
            );
            continue;
        }
        if line == "PING" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                "nats request sync loop received PING; sending PONG"
            );
            writer_half.write_all(b"PONG\r\n").await?;
            writer_half.flush().await?;
            continue;
        }
        if line == "PONG" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                sync_elapsed_ms = sync_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request subscription sync completed"
            );
            break;
        }
        if line.starts_with("-ERR") {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                error_line = %summarize_nats_line(line),
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request server returned error during sync"
            );
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(remaining, reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    tracing::warn!(
                        endpoint = %endpoint,
                        request_subject = %request_subject,
                        response_subject = %response_subject,
                        sid = sid,
                        message_subject = %msg.subject,
                        payload_len = msg.payload_len,
                        elapsed_ms = request_started.elapsed().as_millis() as u64,
                        "nats request timeout draining pre-ack payload"
                    );
                    NatsError::Timeout(format!(
                        "request timeout draining pre-ack payload on {response_subject}"
                    ))
                })??;
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                message_subject = %msg.subject,
                payload_len = msg.payload_len,
                "nats request drained pre-ack message frame"
            );
            continue;
        }
    }

    let publish_started = Instant::now();
    match publish(endpoint, request_subject, request_payload).await {
        Ok(()) => {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                publish_elapsed_ms = publish_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                request_bytes = request_payload.len(),
                "nats request payload published"
            );
        }
        Err(err) => {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                publish_elapsed_ms = publish_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                error = %err,
                "nats request publish failed"
            );
            return Err(err);
        }
    }

    let wait_response_started = Instant::now();
    loop {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request timeout waiting response message"
            );
            return Err(NatsError::Timeout(format!(
                "request timeout waiting response on {response_subject}"
            )));
        }

        let mut line = String::new();
        let n = timeout(remaining, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                tracing::warn!(
                    endpoint = %endpoint,
                    request_subject = %request_subject,
                    response_subject = %response_subject,
                    sid = sid,
                    wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                    elapsed_ms = request_started.elapsed().as_millis() as u64,
                    "nats request timeout waiting response header"
                );
                NatsError::Timeout(format!(
                    "request timeout waiting response header on {response_subject}"
                ))
            })??;
        if n == 0 {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request socket closed while waiting response"
            );
            return Err(NatsError::Io(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "nats socket closed",
            )));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                line = %summarize_nats_line(line),
                "nats request response loop ignored server line"
            );
            continue;
        }
        if line == "PING" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                "nats request response loop received PING; sending PONG"
            );
            writer_half.write_all(b"PONG\r\n").await?;
            writer_half.flush().await?;
            continue;
        }
        if line == "PONG" {
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                "nats request response loop received PONG"
            );
            continue;
        }
        if line.starts_with("-ERR") {
            tracing::warn!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                error_line = %summarize_nats_line(line),
                wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                elapsed_ms = request_started.elapsed().as_millis() as u64,
                "nats request server returned error while waiting response"
            );
            return Err(NatsError::Protocol(format!("nats error: {line}")));
        }
        if line.starts_with("MSG ") {
            let msg = parse_msg_line(line)?;
            let mut payload = vec![0u8; msg.payload_len + 2];
            timeout(remaining, reader.read_exact(&mut payload))
                .await
                .map_err(|_| {
                    tracing::warn!(
                        endpoint = %endpoint,
                        request_subject = %request_subject,
                        response_subject = %response_subject,
                        sid = sid,
                        message_subject = %msg.subject,
                        payload_len = msg.payload_len,
                        wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                        elapsed_ms = request_started.elapsed().as_millis() as u64,
                        "nats request timeout reading response payload"
                    );
                    NatsError::Timeout(format!(
                        "request timeout reading response payload on {response_subject}"
                    ))
                })??;
            payload.truncate(msg.payload_len);
            if msg.subject == response_subject {
                tracing::debug!(
                    endpoint = %endpoint,
                    request_subject = %request_subject,
                    response_subject = %response_subject,
                    sid = sid,
                    payload_bytes = payload.len(),
                    wait_response_elapsed_ms = wait_response_started.elapsed().as_millis() as u64,
                    elapsed_ms = request_started.elapsed().as_millis() as u64,
                    "nats request response matched reply subject"
                );
                return Ok(payload);
            }
            tracing::debug!(
                endpoint = %endpoint,
                request_subject = %request_subject,
                response_subject = %response_subject,
                sid = sid,
                message_subject = %msg.subject,
                payload_len = msg.payload_len,
                "nats request response loop ignored unrelated message"
            );
        }
    }
}

pub struct NatsSubscriber {
    endpoint: String,
    subject: String,
    sid: u32,
    queue: Option<String>,
}

impl NatsSubscriber {
    pub fn new(endpoint: String, subject: String, sid: u32) -> Self {
        Self {
            endpoint,
            subject,
            sid,
            queue: None,
        }
    }

    pub fn with_queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    pub async fn run<F, Fut>(&self, mut handler: F) -> Result<(), NatsError>
    where
        F: FnMut(NatsMessage) -> Fut,
        Fut: std::future::Future<Output = Result<(), NatsError>>,
    {
        let addr = endpoint_to_addr(&self.endpoint)?;
        let stream = TcpStream::connect(addr).await?;
        let (reader_half, mut writer_half) = stream.into_split();
        writer_half.write_all(CONNECT_LINE.as_bytes()).await?;
        let sub_line = match self.queue.as_deref() {
            Some(queue) if !queue.trim().is_empty() => {
                format!("SUB {} {} {}\r\n", self.subject, queue.trim(), self.sid)
            }
            _ => format!("SUB {} {}\r\n", self.subject, self.sid),
        };
        writer_half.write_all(sub_line.as_bytes()).await?;
        writer_half.write_all(b"PING\r\n").await?;
        writer_half.flush().await?;

        let mut reader = BufReader::new(reader_half);
        loop {
            let mut line = String::new();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                return Err(NatsError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "nats socket closed",
                )));
            }
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() || line.starts_with("INFO ") || line.starts_with("+OK") {
                continue;
            }
            if line == "PING" {
                writer_half.write_all(b"PONG\r\n").await?;
                writer_half.flush().await?;
                continue;
            }
            if line == "PONG" {
                continue;
            }
            if line.starts_with("-ERR") {
                return Err(NatsError::Protocol(format!("nats error: {line}")));
            }
            if line.starts_with("MSG ") {
                let header = parse_msg_line(line)?;
                let mut payload = vec![0u8; header.payload_len + 2];
                reader.read_exact(&mut payload).await?;
                payload.truncate(header.payload_len);
                let msg = NatsMessage {
                    subject: header.subject,
                    sid: header.sid,
                    reply_to: header.reply_to,
                    payload,
                };
                handler(msg.clone()).await?;
                if let Some(reply_to) = msg.reply_to {
                    writer_half
                        .write_all(format!("PUB {reply_to} 0\r\n\r\n").as_bytes())
                        .await?;
                    writer_half.flush().await?;
                }
            }
        }
    }
}

fn endpoint_to_addr(endpoint: &str) -> Result<String, NatsError> {
    let trimmed = endpoint.trim();
    if trimmed.is_empty() {
        return Err(NatsError::Protocol("empty nats endpoint".to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("nats://") {
        if rest.is_empty() {
            return Err(NatsError::Protocol("invalid nats endpoint".to_string()));
        }
        return Ok(rest.to_string());
    }
    Ok(trimmed.to_string())
}

fn summarize_nats_line(line: &str) -> String {
    if line.len() <= NATS_LOG_LINE_MAX_LEN {
        return line.to_string();
    }
    let mut out = line[..NATS_LOG_LINE_MAX_LEN].to_string();
    out.push_str("...");
    out
}

#[derive(Debug)]
struct MsgFrameHeader {
    subject: String,
    sid: String,
    reply_to: Option<String>,
    payload_len: usize,
}

fn parse_msg_line(msg_line: &str) -> Result<MsgFrameHeader, NatsError> {
    let parts: Vec<&str> = msg_line.split_whitespace().collect();
    if parts.len() != 4 && parts.len() != 5 {
        return Err(NatsError::Protocol(format!("invalid MSG line: {msg_line}")));
    }
    if parts[0] != "MSG" {
        return Err(NatsError::Protocol(format!("invalid MSG line: {msg_line}")));
    }
    let subject = parts[1].trim();
    let sid = parts[2].trim();
    if subject.is_empty() || sid.is_empty() {
        return Err(NatsError::Protocol(format!("invalid MSG line: {msg_line}")));
    }
    let (reply_to, len_raw) = if parts.len() == 4 {
        (None, parts[3])
    } else {
        (Some(parts[3]), parts[4])
    };
    let payload_len = len_raw
        .parse::<usize>()
        .map_err(|err| NatsError::Protocol(format!("invalid MSG length '{len_raw}': {err}")))?;
    Ok(MsgFrameHeader {
        subject: subject.to_string(),
        sid: sid.to_string(),
        reply_to: reply_to.map(|v| v.to_string()),
        payload_len,
    })
}
