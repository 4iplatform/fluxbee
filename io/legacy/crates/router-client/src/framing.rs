#![forbid(unsafe_code)]

use std::io;

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_INLINE_JSON_BYTES: usize = 64 * 1024;

#[derive(Debug, Error)]
pub enum FramingError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),

    #[error("frame too large: {len} > {max}")]
    FrameTooLarge { len: usize, max: usize },

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

pub async fn write_frame<W>(writer: &mut W, json_bytes: &[u8]) -> Result<(), FramingError>
where
    W: AsyncWrite + Unpin,
{
    if json_bytes.len() > MAX_INLINE_JSON_BYTES {
        return Err(FramingError::FrameTooLarge {
            len: json_bytes.len(),
            max: MAX_INLINE_JSON_BYTES,
        });
    }

    let len_u32 = u32::try_from(json_bytes.len())
        .map_err(|_| FramingError::FrameTooLarge {
            len: json_bytes.len(),
            max: usize::MAX,
        })?;

    writer.write_all(&len_u32.to_be_bytes()).await?;
    writer.write_all(json_bytes).await?;
    Ok(())
}

pub async fn read_frame<R>(reader: &mut R) -> Result<Vec<u8>, FramingError>
where
    R: AsyncRead + Unpin,
{
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > MAX_INLINE_JSON_BYTES {
        return Err(FramingError::FrameTooLarge {
            len,
            max: MAX_INLINE_JSON_BYTES,
        });
    }

    let mut json = vec![0u8; len];
    reader.read_exact(&mut json).await?;
    Ok(json)
}

pub async fn write_json_frame<W, T>(writer: &mut W, message: &T) -> Result<(), FramingError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let json = serde_json::to_vec(message)?;
    write_frame(writer, &json).await
}

pub async fn read_json_frame<R, T>(reader: &mut R) -> Result<T, FramingError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let json = read_frame(reader).await?;
    Ok(serde_json::from_slice(&json)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use router_protocol::{Message, Meta, Routing};
    use tokio::io::duplex;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn roundtrip_single_message() {
        let (mut a, mut b) = duplex(1024);

        let msg = Message {
            routing: Routing {
                src: "node-1".to_string(),
                dst: None,
                ttl: 1,
                trace_id: "trace-1".to_string(),
            },
            meta: Meta {
                ty: "system".to_string(),
                msg: Some("HELLO".to_string()),
                target: None,
                action: None,
                src_ilk: None,
                dst_ilk: None,
                context: serde_json::json!({}),
            },
            payload: serde_json::json!({ "hello": "world" }),
        };

        write_json_frame(&mut a, &msg).await.unwrap();
        let decoded: Message = read_json_frame(&mut b).await.unwrap();

        assert_eq!(decoded.routing.trace_id, "trace-1");
        assert_eq!(decoded.meta.msg.as_deref(), Some("HELLO"));
        assert_eq!(decoded.payload["hello"], "world");
    }

    #[tokio::test]
    async fn reads_multiple_frames_back_to_back() {
        let (mut a, mut b) = duplex(4096);

        let make = |trace_id: &str| Message {
            routing: Routing {
                src: "node-1".to_string(),
                dst: None,
                ttl: 1,
                trace_id: trace_id.to_string(),
            },
            meta: Meta {
                ty: "system".to_string(),
                msg: None,
                target: None,
                action: None,
                src_ilk: None,
                dst_ilk: None,
                context: serde_json::json!({}),
            },
            payload: serde_json::json!({}),
        };

        write_json_frame(&mut a, &make("t1")).await.unwrap();
        write_json_frame(&mut a, &make("t2")).await.unwrap();
        write_json_frame(&mut a, &make("t3")).await.unwrap();

        let m1: Message = read_json_frame(&mut b).await.unwrap();
        let m2: Message = read_json_frame(&mut b).await.unwrap();
        let m3: Message = read_json_frame(&mut b).await.unwrap();

        assert_eq!(m1.routing.trace_id, "t1");
        assert_eq!(m2.routing.trace_id, "t2");
        assert_eq!(m3.routing.trace_id, "t3");
    }

    #[tokio::test]
    async fn rejects_frames_over_limit() {
        let (mut a, mut b) = duplex(1024);

        let oversized = vec![b'a'; MAX_INLINE_JSON_BYTES + 1];
        let err = write_frame(&mut a, &oversized).await.unwrap_err();
        match err {
            FramingError::FrameTooLarge { len, max } => {
                assert_eq!(len, MAX_INLINE_JSON_BYTES + 1);
                assert_eq!(max, MAX_INLINE_JSON_BYTES);
            }
            other => panic!("unexpected error: {other:?}"),
        }

        let len = (MAX_INLINE_JSON_BYTES + 1) as u32;
        a.write_all(&len.to_be_bytes()).await.unwrap();
        let body_prefix = vec![b'{'; 8];
        a.write_all(&body_prefix).await.unwrap();

        let err = read_frame(&mut b).await.unwrap_err();
        assert!(matches!(err, FramingError::FrameTooLarge { .. }));
    }
}
