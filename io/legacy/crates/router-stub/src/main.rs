#![forbid(unsafe_code)]

use anyhow::{anyhow, Context, Result};
use router_client::framing::{read_json_frame, write_json_frame};
use router_protocol::{Message, Meta, Routing};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use uuid::Uuid;

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let listen_addr: SocketAddr = env("ROUTER_STUB_ADDR")
        .unwrap_or_else(|| "127.0.0.1:19000".to_string())
        .parse()
        .context("invalid ROUTER_STUB_ADDR")?;

    let router_name = env("ROUTER_STUB_NAME").unwrap_or_else(|| "RT.stub@local".to_string());
    let router_uuid = env("ROUTER_STUB_UUID").unwrap_or_else(|| "router-stub".to_string());
    let vpn_id: u32 = env("ROUTER_STUB_VPN_ID")
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);

    let listener = TcpListener::bind(listen_addr).await?;
    tracing::info!(%listen_addr, %router_name, %vpn_id, "router-stub listening");

    loop {
        let (mut socket, peer) = listener.accept().await?;
        let router_name = router_name.clone();
        let router_uuid = router_uuid.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(&mut socket, peer, &router_uuid, &router_name, vpn_id).await
            {
                tracing::warn!(error=?e, %peer, "connection ended");
            }
        });
    }
}

async fn handle_connection(
    socket: &mut tokio::net::TcpStream,
    peer: SocketAddr,
    router_uuid: &str,
    router_name: &str,
    vpn_id: u32,
) -> Result<()> {
    let hello: Message = read_json_frame(socket).await?;
    if hello.meta.ty != "system" || hello.meta.msg.as_deref() != Some("HELLO") {
        return Err(anyhow!(
            "expected HELLO, got meta.type={} meta.msg={:?}",
            hello.meta.ty,
            hello.meta.msg
        ));
    }

    let node_uuid = hello
        .payload
        .get("uuid")
        .and_then(|v| v.as_str())
        .unwrap_or(&hello.routing.src)
        .to_string();

    let node_name = hello
        .payload
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>")
        .to_string();

    tracing::info!(%peer, %node_uuid, %node_name, "HELLO received");

    let announce = Message {
        routing: Routing {
            src: router_uuid.to_string(),
            dst: Some(node_uuid.clone()),
            ttl: 1,
            trace_id: hello.routing.trace_id.clone(),
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
            "name": node_name,
            "status": "registered",
            "vpn_id": vpn_id,
            "router_name": router_name
        }),
    };

    write_json_frame(socket, &announce).await?;

    loop {
        let msg: Message = read_json_frame(socket).await?;
        match (msg.meta.ty.as_str(), msg.meta.msg.as_deref()) {
            ("system", Some("WITHDRAW")) => {
                tracing::info!(%peer, reason=?msg.payload.get("reason"), "WITHDRAW received");
                return Ok(());
            }
            _ => {}
        }

        // Log inbound application messages.
        if msg.meta.ty == "user" {
            let content = msg
                .payload
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            tracing::info!(%peer, trace_id=%msg.routing.trace_id, %content, "inbound user message");

            // Minimal loopback: echo back to the same node using the IO reply_target.
            let maybe_reply_target = msg
                .meta
                .context
                .get("io")
                .and_then(|io| io.get("reply_target"))
                .cloned();

            if maybe_reply_target.is_some() {
                let reply = Message {
                    routing: Routing {
                        src: router_uuid.to_string(),
                        dst: Some(msg.routing.src.clone()),
                        ttl: 16,
                        trace_id: Uuid::new_v4().to_string(),
                    },
                    meta: Meta {
                        ty: "user".to_string(),
                        msg: None,
                        target: None,
                        action: None,
                        src_ilk: None,
                        dst_ilk: None,
                        context: msg.meta.context.clone(),
                    },
                    payload: serde_json::json!({
                        "type": "text",
                        "content": format!("echo: {}", content),
                    }),
                };

                write_json_frame(socket, &reply).await?;
            }
        }
    }
}
