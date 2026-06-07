use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fluxbee_sdk::identity::{
    load_hive_id, resolve_ilk_from_hive_config, IdentityShmError, MSG_ILK_PROVISION,
    MSG_ILK_PROVISION_RESPONSE,
};
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, SYSTEM_KIND};
use fluxbee_sdk::rpc::{
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, RpcCommandReceiver,
    SystemRpcRequest,
};
use fluxbee_sdk::{try_handle_default_node_status, NodeConfig, NodeSender};
use serde_json::{json, Value};
use tokio::time::timeout;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn std::error::Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_or("JSR_LOG_LEVEL", "info")))
        .init();

    let cfg = build_node_config("IO.test", "0.1.0", "IO_TEST");
    let config_dir = cfg.config_dir.clone();
    let channel_type = env_or("IO_TEST_CHANNEL_TYPE", "io.test.demo");
    let address = env_or(
        "IO_TEST_ADDRESS",
        &format!("io.test.demo.{}", Uuid::new_v4().simple()),
    );
    let text = env_or("IO_TEST_TEXT", "hello from IO.test");
    let wait_reply_ms = env_u64("IO_TEST_WAIT_REPLY_MS", 8_000);
    let probe_id = env_or(
        "IO_TEST_PROBE_ID",
        &format!("probe-{}", Uuid::new_v4().simple()),
    );

    tracing::info!(
        node = %cfg.name,
        version = %cfg.version,
        channel_type = %channel_type,
        address = %address,
        "starting IO.test"
    );

    // Profile: `user_reply` catches inbound user-msg replies; status_get is
    // routed pre_pending so the responder handler runs while RPCs may be
    // pending in flight.
    let profile = OperationalRouteProfile::builder()
        .command_channel("status_get")
        .command_channel("user_reply")
        .pre_pending_rule(
            RouteMatch::exact(SYSTEM_KIND, "NODE_STATUS_GET"),
            RouteTarget::Command("status_get"),
        )
        .post_pending_rule(
            RouteMatch::any_msg_type("user"),
            RouteTarget::Command("user_reply"),
        )
        .build()?;
    let client =
        RouterDispatcher::connect_with_retry(cfg, Duration::from_millis(100), profile).await?;
    let sender = client.sender_snapshot();
    // Drain the status_get channel in the background so the router never
    // blocks on unanswered NODE_STATUS_GET probes.
    {
        let status_client = std::sync::Arc::clone(&client);
        let mut status_rx = status_client
            .take_command_receiver("status_get")
            .await
            .map_err(|err| -> DynError { err.to_string().into() })?;
        let status_sender = status_client.sender_snapshot();
        tokio::spawn(async move {
            while let Some(msg) = status_rx.recv().await {
                let _ = try_handle_default_node_status(&status_sender, &msg).await;
            }
        });
    }
    let mut user_rx = client
        .take_command_receiver("user_reply")
        .await
        .map_err(|err| -> DynError { err.to_string().into() })?;
    let src_ilk = resolve_or_provision_ilk(&client, &config_dir, &channel_type, &address).await?;
    tracing::info!(src_ilk = %src_ilk, "using src_ilk for routing probe");

    let trace_id = send_probe(&sender, &src_ilk, &probe_id, &text).await?;
    let reply = wait_for_reply(&mut user_rx, &trace_id, wait_reply_ms).await?;

    let handled_by = reply
        .payload
        .get("handled_by")
        .and_then(Value::as_str)
        .unwrap_or_default();
    println!("STATUS=ok");
    println!("TRACE_ID={trace_id}");
    println!("SRC_ILK={src_ilk}");
    println!("PROBE_ID={probe_id}");
    println!("HANDLED_BY={handled_by}");
    Ok(())
}

async fn resolve_or_provision_ilk(
    client: &RouterDispatcher,
    config_dir: &Path,
    channel_type: &str,
    address: &str,
) -> Result<String, DynError> {
    if let Some(explicit) = env_opt("IO_TEST_SRC_ILK") {
        return Ok(explicit);
    }

    match resolve_ilk_from_hive_config(config_dir, channel_type, address, "") {
        Ok(Some(existing)) => return Ok(existing),
        Ok(None) => {}
        Err(err) if is_lookup_unavailable(&err) => {
            tracing::warn!(error = %err, "identity SHM lookup unavailable; attempting provision");
        }
        Err(err) => return Err(err.into()),
    }

    if !env_bool("IO_TEST_ALLOW_PROVISION", false) {
        return Err(format!(
            "no ILK mapping found for channel_type='{}' address='{}'; set IO_TEST_SRC_ILK or IO_TEST_ALLOW_PROVISION=1",
            channel_type, address
        )
        .into());
    }

    let hive_id = load_hive_id(config_dir)?;
    let target = env_or("IO_TEST_IDENTITY_TARGET", &format!("SY.identity@{hive_id}"));
    let ich_id = format!("ich:{}", Uuid::new_v4());
    let timeout_ms = env_u64("IO_TEST_PROVISION_TIMEOUT_MS", 8_000);
    let payload = json!({
        "ich_id": ich_id,
        "channel_type": channel_type,
        "address": address,
    });
    let response = client
        .send_system_rpc(SystemRpcRequest {
            target: &target,
            request_msg: MSG_ILK_PROVISION,
            response_msg: MSG_ILK_PROVISION_RESPONSE,
            payload,
            timeout: Duration::from_millis(timeout_ms),
        })
        .await?;
    let status = response
        .payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    if status != "ok" {
        return Err(format!(
            "ILK_PROVISION returned non-ok status={} payload={}",
            status, response.payload
        )
        .into());
    }
    let ilk_id = response
        .payload
        .get("ilk_id")
        .and_then(Value::as_str)
        .ok_or("ILK_PROVISION response missing ilk_id")?
        .to_string();
    Ok(ilk_id)
}

async fn send_probe(
    sender: &NodeSender,
    src_ilk: &str,
    probe_id: &str,
    text: &str,
) -> Result<String, DynError> {
    let trace_id = Uuid::new_v4().to_string();
    let send_started = Instant::now();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Resolve,
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            src_ilk: Some(src_ilk.to_string()),
            scope: None,
            target: Some("io.test.route.probe".to_string()),
            action: None,
            priority: None,
            context: Some(json!({
                "probe_id": probe_id,
            })),
            ..Meta::default()
        },
        payload: json!({
            "kind": "io_test_probe",
            "probe_id": probe_id,
            "text": text,
        }),
    };
    sender.send(msg).await?;
    tracing::info!(
        trace_id = %trace_id,
        src_ilk = %src_ilk,
        probe_id = %probe_id,
        elapsed_us = send_started.elapsed().as_micros() as u64,
        "IO.test sent routing probe"
    );
    Ok(trace_id)
}

async fn wait_for_reply(
    receiver: &mut RpcCommandReceiver,
    expected_trace_id: &str,
    timeout_ms: u64,
) -> Result<Message, DynError> {
    let wait_started = Instant::now();
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            tracing::warn!(
                trace_id = %expected_trace_id,
                elapsed_us = wait_started.elapsed().as_micros() as u64,
                "IO.test timed out waiting for reply"
            );
            return Err(format!("timeout waiting reply trace_id={expected_trace_id}").into());
        }
        let incoming = match timeout(remaining, receiver.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => {
                return Err(format!(
                    "user_reply channel closed waiting trace_id={expected_trace_id}"
                )
                .into());
            }
            Err(_) => {
                tracing::warn!(
                    trace_id = %expected_trace_id,
                    elapsed_us = wait_started.elapsed().as_micros() as u64,
                    "IO.test timed out waiting for reply"
                );
                return Err(format!("timeout waiting reply trace_id={expected_trace_id}").into());
            }
        };

        tracing::info!(
            expected_trace_id = %expected_trace_id,
            incoming_trace_id = %incoming.routing.trace_id,
            incoming_type = %incoming.meta.msg_type,
            incoming_msg = incoming.meta.msg.as_deref().unwrap_or(""),
            elapsed_us = wait_started.elapsed().as_micros() as u64,
            "IO.test received message while waiting for reply"
        );

        if incoming.routing.trace_id != expected_trace_id {
            continue;
        }
        tracing::info!(
            trace_id = %expected_trace_id,
            elapsed_us = wait_started.elapsed().as_micros() as u64,
            "IO.test received matching user reply"
        );
        return Ok(incoming);
    }
}

fn build_node_config(default_name: &str, default_version: &str, prefix: &str) -> NodeConfig {
    let node_name_env = format!("{prefix}_NODE_NAME");
    NodeConfig {
        name: fluxbee_sdk::managed_node_name(default_name, &[node_name_env.as_str()]),
        router_socket: PathBuf::from(env_or(
            &format!("{prefix}_ROUTER_SOCKET_DIR"),
            "/var/run/fluxbee/routers",
        )),
        uuid_persistence_dir: PathBuf::from(env_or(
            &format!("{prefix}_UUID_PERSISTENCE_DIR"),
            "/var/lib/fluxbee/state/nodes",
        )),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: PathBuf::from(env_or(&format!("{prefix}_CONFIG_DIR"), "/etc/fluxbee")),
        version: env_or(&format!("{prefix}_NODE_VERSION"), default_version),
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn is_lookup_unavailable(err: &IdentityShmError) -> bool {
    match err {
        IdentityShmError::Nix(errno) => {
            matches!(
                *errno,
                nix::errno::Errno::ENOENT | nix::errno::Errno::EACCES | nix::errno::Errno::EPERM
            )
        }
        IdentityShmError::Io(io_err) => io_err.kind() == std::io::ErrorKind::NotFound,
        IdentityShmError::SeqLockTimeout => true,
        _ => false,
    }
}
