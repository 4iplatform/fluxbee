use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use fluxbee_sdk::{connect, NodeConfig, NodeReceiver, NodeSender};
use json_router::shm::IdentityRegionReader;
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

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
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

fn parse_prefixed_uuid_bytes(value: &str, prefix: &str) -> Result<[u8; 16], DiagError> {
    let expected = format!("{prefix}:");
    let raw = value
        .trim()
        .strip_prefix(&expected)
        .ok_or_else(|| format!("expected {prefix}:<uuid>, got '{value}'"))?;
    let uuid = Uuid::parse_str(raw)?;
    Ok(*uuid.as_bytes())
}

fn format_prefixed_uuid(prefix: &str, bytes: &[u8; 16]) -> String {
    format!("{prefix}:{}", Uuid::from_bytes(*bytes))
}

fn registration_status_name(code: u8) -> &'static str {
    match code {
        0 => "temporary",
        1 => "partial",
        2 => "complete",
        _ => "unknown",
    }
}

fn src_ilk_from_message(msg: &Message) -> Option<String> {
    msg.meta
        .context
        .as_ref()
        .and_then(Value::as_object)
        .and_then(|ctx| ctx.get("src_ilk"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
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
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let original_dst = message
                .payload
                .get("original_dst")
                .and_then(Value::as_str)
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
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let last_hop = message
                .payload
                .get("last_hop")
                .and_then(Value::as_str)
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
    match system_call(
        sender,
        receiver,
        target,
        action,
        payload.clone(),
        timeout_ms,
    )
    .await
    {
        Ok(response) => Ok((response, target.to_string())),
        Err(err) => {
            let err_text = err.to_string();
            let Some(fallback) = fallback_target else {
                return Err(err);
            };
            if fallback == target || !err_text.contains("reason=NODE_NOT_FOUND") {
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

fn payload_status(payload: &Value) -> (&str, Option<&str>) {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let code = payload.get("error_code").and_then(Value::as_str);
    (status, code)
}

fn require_payload_ok(payload: &Value, action: &str) -> Result<(), DiagError> {
    let (status, code) = payload_status(payload);
    if status == "ok" {
        return Ok(());
    }
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
    Err(format!(
        "{action} returned non-ok payload status={} code={} message={}",
        status,
        code.unwrap_or("UNKNOWN"),
        message
    )
    .into())
}

fn read_identity_ilk_state(
    hive_id: &str,
    ilk_id: &str,
) -> Result<Option<(String, String)>, DiagError> {
    let ilk_bytes = parse_prefixed_uuid_bytes(ilk_id, "ilk")?;
    let shm_name = format!("/jsr-identity-{hive_id}");
    let reader = IdentityRegionReader::open_read_only_auto(&shm_name)?;
    let snapshot = reader
        .read_snapshot()
        .ok_or_else(|| format!("failed to read identity snapshot from {}", shm_name))?;
    for ilk in &snapshot.ilks {
        if ilk.flags == 0 {
            continue;
        }
        if ilk.ilk_id == ilk_bytes {
            let tenant_id = format_prefixed_uuid("tnt", &ilk.tenant_id);
            let registration_status = registration_status_name(ilk.registration_status).to_string();
            return Ok(Some((tenant_id, registration_status)));
        }
    }
    Ok(None)
}

async fn wait_identity_ilk_status(
    hive_id: &str,
    ilk_id: &str,
    expected_status: &str,
    timeout_ms: u64,
) -> Result<(), DiagError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some((_tenant_id, status)) = read_identity_ilk_state(hive_id, ilk_id)? {
            if status == expected_status {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "identity SHM did not converge for ilk={} status={} within {}ms",
                ilk_id, expected_status, timeout_ms
            )
            .into());
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn wait_identity_ilk_state(
    hive_id: &str,
    ilk_id: &str,
    expected_tenant_id: &str,
    expected_status: &str,
    timeout_ms: u64,
) -> Result<(), DiagError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        if let Some((tenant_id, status)) = read_identity_ilk_state(hive_id, ilk_id)? {
            if tenant_id == expected_tenant_id && status == expected_status {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "identity SHM did not converge for ilk={} tenant={} status={} within {}ms",
                ilk_id, expected_tenant_id, expected_status, timeout_ms
            )
            .into());
        }
        sleep(Duration::from_millis(200)).await;
    }
}

async fn send_resolve_probe(
    sender: &NodeSender,
    src_ilk: &str,
    probe_id: &str,
) -> Result<String, DiagError> {
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            dst: Destination::Resolve,
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: "user".to_string(),
            msg: None,
            scope: None,
            target: Some("identity.provision.complete.diag".to_string()),
            action: None,
            priority: None,
            context: Some(json!({
                "src_ilk": src_ilk,
                "probe_id": probe_id,
            })),
        },
        payload: json!({
            "probe_id": probe_id,
            "kind": "identity_provision_complete",
        }),
    };
    sender.send(msg).await?;
    Ok(trace_id)
}

async fn wait_frontdesk_probe(
    receiver: &mut NodeReceiver,
    expected_trace_id: &str,
    timeout_ms: u64,
) -> Result<Message, DiagError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "timeout waiting frontdesk probe trace_id={}",
                expected_trace_id
            )
            .into());
        }
        let msg = match timeout(remaining, receiver.recv()).await {
            Ok(msg) => msg?,
            Err(_) => {
                return Err(format!(
                    "timeout waiting frontdesk probe trace_id={}",
                    expected_trace_id
                )
                .into());
            }
        };
        if msg.routing.trace_id != expected_trace_id {
            continue;
        }
        if msg.meta.msg_type == SYSTEM_KIND {
            if msg.meta.msg.as_deref() == Some(MSG_UNREACHABLE) {
                let reason = msg
                    .payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Err(format!("probe got UNREACHABLE reason={}", reason).into());
            }
            continue;
        }
        return Ok(msg);
    }
}

#[tokio::main]
async fn main() -> Result<(), DiagError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(env_or(
        "IDENTITY_PROVISION_COMPLETE_CONFIG_DIR",
        "/etc/fluxbee",
    ));
    let hive_id = load_hive_id(&config_dir)?;
    let test_id = env_or(
        "IDENTITY_PROVISION_COMPLETE_TEST_ID",
        &format!("idprov-{}", now_epoch_ms()),
    );
    let timeout_ms = env_u64("IDENTITY_PROVISION_COMPLETE_TIMEOUT_MS", 12_000);
    let target = env_or(
        "IDENTITY_PROVISION_COMPLETE_TARGET",
        &format!("SY.identity@{}", hive_id),
    );
    let fallback_target = env_opt("IDENTITY_PROVISION_COMPLETE_FALLBACK_TARGET");

    let channel_type = env_or(
        "IDENTITY_PROVISION_COMPLETE_CHANNEL_TYPE",
        "io.test.identity",
    );
    let address = env_or(
        "IDENTITY_PROVISION_COMPLETE_ADDRESS",
        &format!("io.test.identity.{}", test_id),
    );

    let io_node_name = env_or(
        "IDENTITY_PROVISION_COMPLETE_IO_NODE_NAME",
        &format!("IO.test.identity.provision.{}", test_id),
    );
    let frontdesk_node_name = env_or(
        "IDENTITY_PROVISION_COMPLETE_FRONTDESK_NODE_NAME",
        &format!("AI.frontdesk@{}", hive_id),
    );

    let io_cfg = NodeConfig {
        name: io_node_name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let (io_sender, mut io_receiver) = connect(&io_cfg).await?;

    let frontdesk_cfg = NodeConfig {
        name: frontdesk_node_name.clone(),
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let (frontdesk_sender, mut frontdesk_receiver) = connect(&frontdesk_cfg).await?;

    let (tenant_create, mut effective_target) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &target,
        fallback_target.as_deref(),
        "TNT_CREATE",
        json!({
            "name": format!("idprov-tenant-{}", test_id),
            "status": "active",
            "settings": {},
        }),
        timeout_ms,
    )
    .await?;
    require_payload_ok(&tenant_create, "TNT_CREATE")?;
    let tenant_id = tenant_create
        .get("tenant_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "TNT_CREATE missing tenant_id".to_string())?
        .to_string();

    let (provision, used_target) = system_call_with_fallback(
        &io_sender,
        &mut io_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_PROVISION",
        json!({
            "ich_id": format!("ich:{}", Uuid::new_v4()),
            "channel_type": channel_type,
            "address": address,
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    require_payload_ok(&provision, "ILK_PROVISION")?;
    let provision_status = provision
        .get("registration_status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if provision_status != "temporary" {
        return Err(format!(
            "ILK_PROVISION expected registration_status=temporary, got {}",
            provision_status
        )
        .into());
    }
    let ilk_id = provision
        .get("ilk_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "ILK_PROVISION missing ilk_id".to_string())?
        .to_string();
    let _ = parse_prefixed_uuid_bytes(&ilk_id, "ilk")?;
    wait_identity_ilk_status(&hive_id, &ilk_id, "temporary", timeout_ms).await?;

    let probe_trace_id = send_resolve_probe(&io_sender, &ilk_id, &test_id).await?;
    let probe_msg =
        wait_frontdesk_probe(&mut frontdesk_receiver, &probe_trace_id, timeout_ms).await?;
    let routed_src_ilk = src_ilk_from_message(&probe_msg).unwrap_or_default();
    if routed_src_ilk != ilk_id {
        return Err(format!(
            "frontdesk probe src_ilk mismatch expected={} got={}",
            ilk_id, routed_src_ilk
        )
        .into());
    }

    let (register, used_target) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_REGISTER",
        json!({
            "ilk_id": ilk_id,
            "ilk_type": "human",
            "tenant_id": tenant_id,
            "identification": {
                "display_name": format!("Identity Provision {}", test_id),
                "email": format!("idprov-{}@diag.local", test_id),
            },
            "roles": [],
            "capabilities": [],
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    require_payload_ok(&register, "ILK_REGISTER")?;

    wait_identity_ilk_state(&hive_id, &ilk_id, &tenant_id, "complete", timeout_ms).await?;

    let (tenant_create_2, used_target) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "TNT_CREATE",
        json!({
            "name": format!("idprov-tenant2-{}", test_id),
            "status": "active",
            "settings": {},
        }),
        timeout_ms,
    )
    .await?;
    effective_target = used_target;
    require_payload_ok(&tenant_create_2, "TNT_CREATE(second)")?;
    let tenant_id_2 = tenant_create_2
        .get("tenant_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "TNT_CREATE(second) missing tenant_id".to_string())?
        .to_string();

    let (register_second, _) = system_call_with_fallback(
        &frontdesk_sender,
        &mut frontdesk_receiver,
        &effective_target,
        fallback_target.as_deref(),
        "ILK_REGISTER",
        json!({
            "ilk_id": ilk_id,
            "ilk_type": "human",
            "tenant_id": tenant_id_2,
            "identification": {
                "display_name": format!("Identity Provision {}", test_id),
                "email": format!("idprov-{}@diag.local", test_id),
            },
            "roles": [],
            "capabilities": [],
        }),
        timeout_ms,
    )
    .await?;
    let (second_status, second_code) = payload_status(&register_second);
    if second_status != "error" || second_code != Some("INVALID_TENANT_TRANSITION") {
        return Err(format!(
            "second ILK_REGISTER expected INVALID_TENANT_TRANSITION, got status={} code={}",
            second_status,
            second_code.unwrap_or("NONE")
        )
        .into());
    }

    let _ = io_sender.close().await;
    let _ = frontdesk_sender.close().await;

    println!("STATUS=ok");
    println!("TEST_ID={}", test_id);
    println!("TARGET={}", target);
    println!("EFFECTIVE_TARGET={}", effective_target);
    println!("HIVE_ID={}", hive_id);
    println!("ILK_ID={}", ilk_id);
    println!("TENANT_ID={}", tenant_id);
    println!("TENANT_REASSIGN_BLOCK_CODE=INVALID_TENANT_TRANSITION");
    println!("FRONTDESK_NODE={}", frontdesk_node_name);
    println!("FLOW=provision_route_complete");
    Ok(())
}
