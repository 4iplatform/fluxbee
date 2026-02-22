use std::error::Error;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jsr_client::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use jsr_client::{connect, NodeConfig, NodeReceiver, NodeSender};
use serde::Deserialize;
use serde_json::json;
use tokio::time::{timeout, Instant};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DiagError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Copy, Debug)]
enum RouteMode {
    Unicast,
    Resolve,
}

impl RouteMode {
    fn from_env(raw: &str) -> Result<Self, DiagError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "unicast" => Ok(Self::Unicast),
            "resolve" => Ok(Self::Resolve),
            other => {
                Err(format!("invalid ORCH_ROUTE_MODE='{other}' (use: unicast|resolve)").into())
            }
        }
    }
}

enum WaitOutcome {
    Response(Message),
    Unreachable {
        reason: String,
        original_dst: String,
    },
    TtlExceeded {
        original_dst: String,
        last_hop: String,
    },
}

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
}

fn now_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
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

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn load_hive_id(config_dir: &std::path::Path) -> Result<String, DiagError> {
    let data = std::fs::read_to_string(config_dir.join("hive.yaml"))?;
    let hive: HiveFile = serde_yaml::from_str(&data)?;
    Ok(hive.hive_id)
}

async fn send_system_message(
    sender: &NodeSender,
    target: &str,
    route_mode: RouteMode,
    msg_name: &str,
    payload: serde_json::Value,
) -> Result<String, DiagError> {
    let trace_id = Uuid::new_v4().to_string();
    let (dst, meta_target) = match route_mode {
        RouteMode::Unicast => (Destination::Unicast(target.to_string()), None),
        RouteMode::Resolve => (Destination::Resolve, Some(target.to_string())),
    };
    let message = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst,
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg_name.to_string()),
            scope: None,
            target: meta_target,
            action: None,
            priority: None,
            context: None,
        },
        payload,
    };
    sender.send(message).await?;
    Ok(trace_id)
}

async fn wait_system_response(
    receiver: &mut NodeReceiver,
    trace_id: &str,
    expected_msg: &str,
    timeout_secs: u64,
) -> Result<WaitOutcome, DiagError> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(
                format!("timeout waiting {} for trace_id={}", expected_msg, trace_id).into(),
            );
        }
        let message = match timeout(remaining, receiver.recv()).await {
            Ok(message) => message?,
            Err(_) => {
                return Err(
                    format!("timeout waiting {} for trace_id={}", expected_msg, trace_id).into(),
                );
            }
        };
        if message.meta.msg_type == SYSTEM_KIND
            && message.routing.trace_id == trace_id
            && message.meta.msg.as_deref() == Some(MSG_UNREACHABLE)
        {
            let reason = message
                .payload
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            return Ok(WaitOutcome::Unreachable {
                reason: reason.to_string(),
                original_dst: original_dst.to_string(),
            });
        }
        if message.meta.msg_type == SYSTEM_KIND
            && message.routing.trace_id == trace_id
            && message.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED)
        {
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            let last_hop = message
                .payload
                .get("last_hop")
                .and_then(|value| value.as_str())
                .unwrap_or("-");
            return Ok(WaitOutcome::TtlExceeded {
                original_dst: original_dst.to_string(),
                last_hop: last_hop.to_string(),
            });
        }
        if message.meta.msg_type == SYSTEM_KIND
            && message.meta.msg.as_deref() == Some(expected_msg)
            && message.routing.trace_id == trace_id
        {
            return Ok(WaitOutcome::Response(message));
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), DiagError> {
    let log_level = std::env::var("JSR_LOG_LEVEL").unwrap_or_else(|_| "info".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = json_router::paths::config_dir();
    let state_dir = json_router::paths::state_dir();
    let socket_dir = json_router::paths::router_socket_dir();
    let local_hive = load_hive_id(&config_dir)?;

    let target_hive = env_or("ORCH_TARGET_HIVE", "worker-220");
    let runtime = env_or("ORCH_RUNTIME", "wf.orch.diag");
    let version = env_or("ORCH_VERSION", "0.0.1");
    let timeout_secs = env_u64("ORCH_TIMEOUT_SECS", 45);
    let send_kill = env_bool("ORCH_SEND_KILL", true);
    let send_runtime_update = env_bool("ORCH_SEND_RUNTIME_UPDATE", true);
    let unit = env_or("ORCH_UNIT", &format!("fluxbee-orch-e2e-{}", now_epoch_ms()));
    let route_mode = RouteMode::from_env(&env_or("ORCH_ROUTE_MODE", "unicast"))?;
    let expected_spawn_unreachable_reason = env_non_empty("ORCH_EXPECT_SPAWN_UNREACHABLE_REASON");
    let target = format!("SY.orchestrator@{}", local_hive);

    let node_config = NodeConfig {
        name: "WF.orch.diag".to_string(),
        router_socket: socket_dir,
        uuid_persistence_dir: state_dir.join("nodes"),
        config_dir,
        version: "1.0".to_string(),
    };
    let (sender, mut receiver) = connect(&node_config).await?;

    if send_runtime_update {
        let runtime_update_payload = json!({
            "version": now_epoch_ms() as u64,
            "updated_at": format!("{}", now_epoch_ms()),
            "runtimes": {
                (runtime.clone()): {
                    "current": version.clone(),
                    "available": [version.clone()],
                }
            }
        });

        let update_trace = send_system_message(
            &sender,
            &target,
            route_mode,
            "RUNTIME_UPDATE",
            runtime_update_payload,
        )
        .await?;
        tracing::info!(
            trace_id = %update_trace,
            target = %target,
            route_mode = ?route_mode,
            runtime = %runtime,
            version = %version,
            "sent RUNTIME_UPDATE"
        );
    }

    let spawn_payload = json!({
        "runtime": runtime,
        "version": version,
        "unit": unit,
        "target": target_hive,
    });
    let spawn_trace =
        send_system_message(&sender, &target, route_mode, "SPAWN_NODE", spawn_payload).await?;
    let spawn_outcome = wait_system_response(
        &mut receiver,
        &spawn_trace,
        "SPAWN_NODE_RESPONSE",
        timeout_secs,
    )
    .await?;
    let spawn_response = match spawn_outcome {
        WaitOutcome::Response(message) => {
            if let Some(expected_reason) = expected_spawn_unreachable_reason.as_deref() {
                return Err(format!(
                    "expected UNREACHABLE reason={} but got SPAWN_NODE_RESPONSE payload={}",
                    expected_reason, message.payload
                )
                .into());
            }
            message
        }
        WaitOutcome::Unreachable {
            reason,
            original_dst,
        } => {
            if let Some(expected_reason) = expected_spawn_unreachable_reason.as_deref() {
                if reason == expected_reason {
                    tracing::info!(
                        trace_id = %spawn_trace,
                        reason = %reason,
                        original_dst = %original_dst,
                        "received expected UNREACHABLE for SPAWN_NODE"
                    );
                    println!(
                        "{}",
                        json!({
                            "status": "ok",
                            "target": target,
                            "target_hive": target_hive,
                            "runtime": runtime,
                            "version": version,
                            "unit": unit,
                            "kill_sent": false,
                            "expected_unreachable_reason": expected_reason,
                            "received_reason": reason,
                            "route_mode": format!("{route_mode:?}").to_ascii_lowercase(),
                        })
                    );
                    return Ok(());
                }
                return Err(format!(
                    "expected UNREACHABLE reason={} but got reason={} original_dst={}",
                    expected_reason, reason, original_dst
                )
                .into());
            }
            return Err(format!(
                "router returned UNREACHABLE while waiting SPAWN_NODE_RESPONSE trace_id={} reason={} original_dst={}",
                spawn_trace, reason, original_dst
            )
            .into());
        }
        WaitOutcome::TtlExceeded {
            original_dst,
            last_hop,
        } => {
            return Err(format!(
                "router returned TTL_EXCEEDED while waiting SPAWN_NODE_RESPONSE trace_id={} original_dst={} last_hop={}",
                spawn_trace, original_dst, last_hop
            )
            .into());
        }
    };
    let spawn_status = spawn_response
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("error");

    tracing::info!(
        trace_id = %spawn_trace,
        target_hive = %target_hive,
        unit = %unit,
        route_mode = ?route_mode,
        status = %spawn_status,
        payload = %spawn_response.payload,
        "received SPAWN_NODE_RESPONSE"
    );

    if spawn_status != "ok" {
        return Err(format!("SPAWN_NODE failed: {}", spawn_response.payload).into());
    }

    if send_kill {
        let kill_payload = json!({
            "unit": unit,
            "target": target_hive,
        });
        let kill_trace =
            send_system_message(&sender, &target, route_mode, "KILL_NODE", kill_payload).await?;
        let kill_outcome = wait_system_response(
            &mut receiver,
            &kill_trace,
            "KILL_NODE_RESPONSE",
            timeout_secs,
        )
        .await?;
        let kill_response = match kill_outcome {
            WaitOutcome::Response(message) => message,
            WaitOutcome::Unreachable {
                reason,
                original_dst,
            } => {
                return Err(format!(
                    "router returned UNREACHABLE while waiting KILL_NODE_RESPONSE trace_id={} reason={} original_dst={}",
                    kill_trace, reason, original_dst
                )
                .into())
            }
            WaitOutcome::TtlExceeded {
                original_dst,
                last_hop,
            } => {
                return Err(format!(
                    "router returned TTL_EXCEEDED while waiting KILL_NODE_RESPONSE trace_id={} original_dst={} last_hop={}",
                    kill_trace, original_dst, last_hop
                )
                .into())
            }
        };
        let kill_status = kill_response
            .payload
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        tracing::info!(
            trace_id = %kill_trace,
            unit = %unit,
            status = %kill_status,
            payload = %kill_response.payload,
            "received KILL_NODE_RESPONSE"
        );
        if kill_status != "ok" {
            return Err(format!("KILL_NODE failed: {}", kill_response.payload).into());
        }
    }

    println!(
        "{}",
        json!({
            "status": "ok",
            "target": target,
            "target_hive": target_hive,
            "runtime": runtime,
            "version": version,
            "unit": unit,
            "kill_sent": send_kill,
            "route_mode": format!("{route_mode:?}").to_ascii_lowercase(),
        })
    );
    Ok(())
}
