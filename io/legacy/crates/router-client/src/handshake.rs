#![forbid(unsafe_code)]

use serde::Deserialize;
use thiserror::Error;

use crate::framing::{read_json_frame, write_json_frame, FramingError};
use router_protocol::{Message, Meta, Routing};

#[derive(Debug, Error)]
pub enum HandshakeError {
    #[error(transparent)]
    Framing(#[from] FramingError),

    #[error("unexpected message during handshake: meta.type={meta_type:?} meta.msg={meta_msg:?}")]
    UnexpectedMessage { meta_type: Option<String>, meta_msg: Option<String> },

    #[error("invalid ANNOUNCE payload: {0}")]
    InvalidAnnouncePayload(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct Hello {
    pub node_uuid: String,
    pub full_name: String,
    pub version: String,
    pub trace_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Announce {
    pub uuid: String,
    pub name: String,
    pub status: String,
    pub vpn_id: u32,
    pub router_name: String,
}

pub async fn hello_announce<S>(stream: &mut S, hello: Hello) -> Result<Announce, HandshakeError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let hello_msg = Message {
        routing: Routing {
            src: hello.node_uuid.clone(),
            dst: None,
            ttl: 1,
            trace_id: hello.trace_id,
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
        payload: serde_json::json!({
            "uuid": hello.node_uuid,
            "name": hello.full_name,
            "version": hello.version,
        }),
    };

    write_json_frame(stream, &hello_msg).await?;

    let announce_msg: Message = read_json_frame(stream).await?;
    let meta_type = Some(announce_msg.meta.ty.clone());
    let meta_msg = announce_msg.meta.msg.clone();

    if announce_msg.meta.ty != "system" || announce_msg.meta.msg.as_deref() != Some("ANNOUNCE") {
        return Err(HandshakeError::UnexpectedMessage { meta_type, meta_msg });
    }

    Ok(serde_json::from_value(announce_msg.payload)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framing::{read_json_frame, write_json_frame};
    use tokio::io::duplex;

    #[tokio::test]
    async fn handshake_sends_hello_and_parses_announce() {
        let (mut node_side, mut router_side) = duplex(8 * 1024);

        let router = tokio::spawn(async move {
            let hello_msg: Message = read_json_frame(&mut router_side).await.unwrap();
            assert_eq!(hello_msg.meta.ty, "system");
            assert_eq!(hello_msg.meta.msg.as_deref(), Some("HELLO"));
            assert_eq!(hello_msg.payload["name"], "AI.support.l1@production");

            let node_uuid = hello_msg.routing.src.clone();
            let trace_id = hello_msg.routing.trace_id.clone();
            let full_name = hello_msg
                .payload
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap()
                .to_string();

            let announce_msg = Message {
                routing: Routing {
                    src: "router-uuid".to_string(),
                    dst: Some(node_uuid.clone()),
                    ttl: 1,
                    trace_id,
                },
                meta: Meta {
                    ty: "system".to_string(),
                    msg: Some("ANNOUNCE".to_string()),
                    target: None,
                    action: None,
                    src_ilk: None,
                    dst_ilk: None,
                    context: serde_json::json!({}),
                },
                payload: serde_json::json!({
                    "uuid": node_uuid,
                    "name": full_name,
                    "status": "registered",
                    "vpn_id": 10,
                    "router_name": "RT.primary@production"
                }),
            };

            write_json_frame(&mut router_side, &announce_msg)
                .await
                .unwrap();
        });

        let announce = hello_announce(
            &mut node_side,
            Hello {
                node_uuid: "node-uuid".to_string(),
                full_name: "AI.support.l1@production".to_string(),
                version: "1.0".to_string(),
                trace_id: "trace-uuid".to_string(),
            },
        )
        .await
        .unwrap();

        assert_eq!(announce.vpn_id, 10);
        assert_eq!(announce.router_name, "RT.primary@production");

        router.await.unwrap();
    }
}
