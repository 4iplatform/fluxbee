use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::{sleep, timeout, Instant};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DiagError = Box<dyn Error + Send + Sync>;

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
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
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

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn load_hive_id(config_dir: &PathBuf) -> Result<String, DiagError> {
    let data = std::fs::read_to_string(config_dir.join("hive.yaml"))?;
    let hive: HiveFile = serde_yaml::from_str(&data)?;
    Ok(hive.hive_id)
}

async fn send_system_message(
    sender: &NodeSender,
    target: &str,
    msg_name: &str,
    payload: Value,
) -> Result<String, DiagError> {
    let trace_id = Uuid::new_v4().to_string();
    let message = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(msg_name.to_string()),
            scope: None,
            target: None,
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
    timeout_ms: u64,
) -> Result<Value, DiagError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
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
        if message.meta.msg_type != SYSTEM_KIND || message.routing.trace_id != trace_id {
            continue;
        }
        if message.meta.msg.as_deref() == Some(MSG_UNREACHABLE) {
            let reason = message
                .payload
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(format!(
                "unreachable while waiting {} trace_id={} reason={} original_dst={}",
                expected_msg, trace_id, reason, original_dst
            )
            .into());
        }
        if message.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED) {
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let last_hop = message
                .payload
                .get("last_hop")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(format!(
                "ttl_exceeded while waiting {} trace_id={} original_dst={} last_hop={}",
                expected_msg, trace_id, original_dst, last_hop
            )
            .into());
        }
        if message.meta.msg.as_deref() == Some(expected_msg) {
            return Ok(message.payload);
        }
    }
}

async fn system_call(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    target: &str,
    action: &str,
    payload: Value,
    timeout_ms: u64,
) -> Result<Value, DiagError> {
    let trace_id = send_system_message(sender, target, action, payload).await?;
    let response_action = format!("{action}_RESPONSE");
    wait_system_response(receiver, &trace_id, &response_action, timeout_ms).await
}

async fn system_call_with_fallback(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    target: &str,
    fallback_target: Option<&str>,
    action: &str,
    payload: Value,
    timeout_ms: u64,
) -> Result<(Value, String), DiagError> {
    let first = system_call(
        sender,
        receiver,
        target,
        action,
        payload.clone(),
        timeout_ms,
    )
    .await;
    match first {
        Ok(response) => {
            let status = response
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("error");
            let code = response.get("error_code").and_then(|v| v.as_str());
            let Some(fallback) = fallback_target else {
                return Ok((response, target.to_string()));
            };
            if fallback != target && status == "error" && code == Some("NOT_PRIMARY") {
                tracing::warn!(
                    target = %target,
                    fallback = %fallback,
                    action = action,
                    "identity target is replica (NOT_PRIMARY), retrying with fallback"
                );
                let second =
                    system_call(sender, receiver, fallback, action, payload, timeout_ms).await?;
                return Ok((second, fallback.to_string()));
            }
            Ok((response, target.to_string()))
        }
        Err(err) => {
            let err_text = err.to_string();
            let Some(fallback) = fallback_target else {
                return Err(err);
            };
            if fallback == target {
                return Err(err);
            }
            if !err_text.contains("reason=NODE_NOT_FOUND") {
                return Err(err);
            }
            tracing::warn!(
                target = %target,
                fallback = %fallback,
                action = action,
                error = %err_text,
                "primary identity target unreachable, retrying with fallback"
            );
            let second =
                system_call(sender, receiver, fallback, action, payload, timeout_ms).await?;
            Ok((second, fallback.to_string()))
        }
    }
}

fn payload_status_ok(payload: &Value) -> Result<(), DiagError> {
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("error");
    if status == "ok" {
        return Ok(());
    }
    let code = payload
        .get("error_code")
        .and_then(|v| v.as_str())
        .unwrap_or("UNKNOWN");
    let message = payload
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    Err(format!(
        "non-ok payload status={} code={} message={}",
        status, code, message
    )
    .into())
}

fn metric_alias_count(payload: &Value) -> Option<u64> {
    payload
        .get("metrics")
        .and_then(|v| v.get("alias_count"))
        .and_then(|v| v.as_u64())
}

#[tokio::main]
async fn main() -> Result<(), DiagError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(env_or("IDENTITY_MERGE_CONFIG_DIR", "/etc/fluxbee"));
    let hive_id = load_hive_id(&config_dir)?;
    let test_id = env_or(
        "IDENTITY_MERGE_TEST_ID",
        &format!("idmerge-{}", now_epoch_ms()),
    );
    let target = env_or("IDENTITY_MERGE_TARGET", &format!("SY.identity@{}", hive_id));
    let fallback_target = env_opt("IDENTITY_MERGE_FALLBACK_TARGET");
    let timeout_ms = env_u64("IDENTITY_MERGE_TIMEOUT_MS", 10_000);
    let wait_gc_secs = env_u64("IDENTITY_MERGE_WAIT_GC_SECS", 0);
    let require_alias_cleanup = env_bool("IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP", false);

    let channel_type_old = env_or("IDENTITY_MERGE_OLD_CHANNEL_TYPE", "io.test.merge");
    let address_old = env_or(
        "IDENTITY_MERGE_OLD_ADDRESS",
        &format!("merge-old-{}", test_id),
    );
    let channel_type_new = env_or("IDENTITY_MERGE_NEW_CHANNEL_TYPE", "io.test.merge.new");
    let address_new = env_or(
        "IDENTITY_MERGE_NEW_ADDRESS",
        &format!("merge-new-{}", test_id),
    );

    let io_node_name = env_or(
        "IDENTITY_MERGE_IO_NODE_NAME",
        &format!("IO.test.identity.merge.{}", test_id),
    );
    let frontdesk_node_name = env_or("IDENTITY_MERGE_FRONTDESK_NODE_NAME", "AI.frontdesk");

    let io_node_config = NodeConfig {
        name: io_node_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let (io_sender, mut io_receiver) = connect(&io_node_config).await?;

    let (metrics_before, mut effective_target) = system_call_with_fallback(
        &io_sender,
        &mut io_receiver,
        &target,
        fallback_target.as_deref(),
        "IDENTITY_METRICS",
        json!({}),
        timeout_ms,
    )
    .await?;
    payload_status_ok(&metrics_before)?;
    let alias_before = metric_alias_count(&metrics_before).unwrap_or(0);

    let old_ich_id = format!("ich:{}", Uuid::new_v4());
    let (provision_old, used_target) = system_call_with_fallback(
        &io_sender,
        &mut io_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_PROVISION",
        json!({
            "ich_id": old_ich_id,
            "channel_type": channel_type_old,
            "address": address_old,
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    payload_status_ok(&provision_old)?;
    let old_ilk_id = provision_old
        .get("ilk_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "ILK_PROVISION missing ilk_id".to_string())?
        .to_string();

    let frontdesk_config = NodeConfig {
        name: frontdesk_node_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let (frontdesk_sender, mut frontdesk_receiver) = connect(&frontdesk_config).await?;

    let (tenant_create, used_target) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "TNT_CREATE",
        json!({
            "name": format!("merge-diag-{}", test_id),
            "status": "active",
            "settings": {},
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    payload_status_ok(&tenant_create)?;
    let tenant_id = tenant_create
        .get("tenant_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "TNT_CREATE missing tenant_id".to_string())?
        .to_string();

    let canonical_ilk_id = env_or(
        "IDENTITY_MERGE_CANONICAL_ILK_ID",
        &format!("ilk:{}", Uuid::new_v4()),
    );
    let (register, used_target) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_REGISTER",
        json!({
            "ilk_id": canonical_ilk_id,
            "ilk_type": "human",
            "tenant_id": tenant_id,
            "identification": {
                "display_name": format!("Merge {}", test_id),
                "email": format!("merge-{}@diag.local", test_id),
            },
            "roles": [],
            "capabilities": [],
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    payload_status_ok(&register)?;

    let (add_channel, used_target) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_ADD_CHANNEL",
        json!({
            "ilk_id": canonical_ilk_id,
            "channel": {
                "ich_id": format!("ich:{}", Uuid::new_v4()),
                "type": channel_type_new,
                "address": address_new,
            },
            "merge_from_ilk_id": old_ilk_id,
            "change_reason": "identity merge diag",
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    payload_status_ok(&add_channel)?;

    let (metrics_after_merge, used_target) = system_call_with_fallback(
        &io_sender,
        &mut io_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "IDENTITY_METRICS",
        json!({}),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    payload_status_ok(&metrics_after_merge)?;
    let alias_after_merge = metric_alias_count(&metrics_after_merge).unwrap_or(0);

    let old_ich_id_second = format!("ich:{}", Uuid::new_v4());
    let (provision_old_again, used_target) = system_call_with_fallback(
        &io_sender,
        &mut io_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_PROVISION",
        json!({
            "ich_id": old_ich_id_second,
            "channel_type": channel_type_old,
            "address": address_old,
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    payload_status_ok(&provision_old_again)?;
    let resolved_old_channel_ilk = provision_old_again
        .get("ilk_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "second ILK_PROVISION missing ilk_id".to_string())?
        .to_string();

    if resolved_old_channel_ilk != canonical_ilk_id {
        return Err(format!(
            "merge not converged: old channel resolved to {} (expected {})",
            resolved_old_channel_ilk, canonical_ilk_id
        )
        .into());
    }

    let mut alias_after_wait = alias_after_merge;
    if wait_gc_secs > 0 {
        sleep(Duration::from_secs(wait_gc_secs)).await;
        let (metrics_after_wait, used_target) = system_call_with_fallback(
            &io_sender,
            &mut io_receiver,
            &effective_target,
            fallback_target.as_deref(),
            "IDENTITY_METRICS",
            json!({}),
            timeout_ms,
        )
        .await?;
        effective_target = used_target;
        payload_status_ok(&metrics_after_wait)?;
        alias_after_wait = metric_alias_count(&metrics_after_wait).unwrap_or(alias_after_merge);
    }

    if require_alias_cleanup && alias_after_wait >= alias_after_merge {
        return Err(format!(
            "alias cleanup expected but alias_count did not drop: before={} after_merge={} after_wait={}",
            alias_before, alias_after_merge, alias_after_wait
        )
        .into());
    }

    let _ = io_sender.close().await;
    let _ = frontdesk_sender.close().await;

    println!("STATUS=ok");
    println!("TEST_ID={}", test_id);
    println!("TARGET={}", target);
    println!("EFFECTIVE_TARGET={}", effective_target);
    println!("OLD_ILK_ID={}", old_ilk_id);
    println!("CANONICAL_ILK_ID={}", canonical_ilk_id);
    println!("RESOLVED_OLD_CHANNEL_ILK_ID={}", resolved_old_channel_ilk);
    println!("TENANT_ID={}", tenant_id);
    println!("ALIAS_COUNT_BEFORE={}", alias_before);
    println!("ALIAS_COUNT_AFTER_MERGE={}", alias_after_merge);
    println!("ALIAS_COUNT_AFTER_WAIT={}", alias_after_wait);
    println!("WAIT_GC_SECS={}", wait_gc_secs);
    println!(
        "ALIAS_CLEANUP_REQUIRED={}",
        if require_alias_cleanup { "1" } else { "0" }
    );
    Ok(())
}
