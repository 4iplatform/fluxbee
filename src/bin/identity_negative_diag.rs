use std::error::Error;
use std::path::PathBuf;

use fluxbee_sdk::identity::{
    identity_system_call, load_hive_id, IdentitySystemRequest, MSG_ILK_ADD_CHANNEL,
    MSG_ILK_PROVISION, MSG_ILK_REGISTER, MSG_TNT_CREATE,
};
use fluxbee_sdk::{connect, NodeConfig};
use serde_json::{json, Value};
use tokio::time::Duration;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(env_or("IDENTITY_NEGATIVE_CONFIG_DIR", "/etc/fluxbee"));
    let hive_id = load_hive_id(&config_dir)?;
    let test_id = env_or(
        "IDENTITY_NEGATIVE_TEST_ID",
        &format!("idneg-{}", chrono_like_now_ms()),
    );
    let timeout_ms = env_u64("IDENTITY_NEGATIVE_TIMEOUT_MS", 8_000);
    let timeout = Duration::from_millis(timeout_ms);
    let target = env_or(
        "IDENTITY_NEGATIVE_TARGET",
        &format!("SY.identity@{}", hive_id),
    );
    let fallback_target = env_opt("IDENTITY_NEGATIVE_FALLBACK_TARGET");

    // Case 1: unauthorized registrar for ILK_REGISTER.
    let unauthorized_name = env_or(
        "IDENTITY_NEGATIVE_UNAUTHORIZED_NODE_NAME",
        &format!("WF.identity.negative.{}@{}", test_id, hive_id),
    );
    let unauthorized_payload = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": format!("tnt:{}", Uuid::new_v4()),
        "identification": {
            "display_name": "unauthorized",
            "email": format!("unauth-{}@diag.local", test_id),
        },
        "roles": [],
        "capabilities": [],
    });
    let unauthorized_code = run_case_expect_error(
        &unauthorized_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        unauthorized_payload,
        timeout,
        "UNAUTHORIZED_REGISTRAR",
    )
    .await?;

    // Case 2: malformed ilk_id should fail with INVALID_REQUEST.
    let frontdesk_name = env_or(
        "IDENTITY_NEGATIVE_FRONTDESK_NODE_NAME",
        &format!("SY.frontdesk.gov@{}-neg-{}", hive_id, test_id),
    );
    let malformed_payload = json!({
        "ilk_id": "ilk:not-a-uuid",
        "ilk_type": "human",
        "tenant_id": format!("tnt:{}", Uuid::new_v4()),
        "identification": {
            "display_name": "malformed",
            "email": format!("malformed-{}@diag.local", test_id),
        },
        "roles": [],
        "capabilities": [],
    });
    let invalid_request_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        malformed_payload,
        timeout,
        "INVALID_REQUEST",
    )
    .await?;

    // Case 3: valid UUID tenant format, but missing tenant => INVALID_TENANT.
    let invalid_tenant_payload = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": format!("tnt:{}", Uuid::new_v4()),
        "identification": {
            "display_name": "missing-tenant",
            "email": format!("missing-tenant-{}@diag.local", test_id),
        },
        "roles": [],
        "capabilities": [],
    });
    let invalid_tenant_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        invalid_tenant_payload,
        timeout,
        "INVALID_TENANT",
    )
    .await?;

    // Case 4: duplicate email inside same tenant => DUPLICATE_EMAIL.
    let tnt_create_payload = json!({
        "name": format!("identity-negative-{}", test_id),
        "status": "active",
    });
    let tnt_create = run_case_expect_ok(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_TNT_CREATE,
        tnt_create_payload,
        timeout,
    )
    .await?;
    let tenant_id = tnt_create
        .get("tenant_id")
        .and_then(Value::as_str)
        .ok_or("missing tenant_id in TNT_CREATE response")?
        .to_string();

    let duplicate_email = format!("duplicate-email-{}@diag.local", test_id);
    let first_human_register = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": tenant_id.clone(),
        "identification": {
            "display_name": "dup-email-first",
            "email": duplicate_email,
        },
        "roles": [],
        "capabilities": [],
    });
    let first_human = run_case_expect_ok(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        first_human_register,
        timeout,
    )
    .await?;
    let first_human_ilk = first_human
        .get("ilk_id")
        .and_then(Value::as_str)
        .ok_or("missing ilk_id in first human register response")?
        .to_string();

    let second_human_register = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": tenant_id.clone(),
        "identification": {
            "display_name": "dup-email-second",
            "email": duplicate_email,
        },
        "roles": [],
        "capabilities": [],
    });
    let duplicate_email_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        second_human_register,
        timeout,
        "DUPLICATE_EMAIL",
    )
    .await?;

    // Case 5: duplicate ICH (channel_type + address + tenant_id) => DUPLICATE_ICH.
    let second_unique_human_register = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": tenant_id.clone(),
        "identification": {
            "display_name": "dup-ich-second-human",
            "email": format!("dup-ich-second-{}@diag.local", test_id),
        },
        "roles": [],
        "capabilities": [],
    });
    let second_human = run_case_expect_ok(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        second_unique_human_register,
        timeout,
    )
    .await?;
    let second_human_ilk = second_human
        .get("ilk_id")
        .and_then(Value::as_str)
        .ok_or("missing ilk_id in second human register response")?
        .to_string();

    let dup_ich_type = "identity.negative.ich";
    let dup_ich_address = format!("identity.negative.ich.{}", test_id);
    let first_add_channel = json!({
        "ilk_id": first_human_ilk,
        "channel": {
            "ich_id": format!("ich:{}", Uuid::new_v4()),
            "type": dup_ich_type,
            "address": dup_ich_address.clone(),
        },
        "change_reason": "identity negative duplicate ich baseline",
    });
    let _ = run_case_expect_ok(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_ADD_CHANNEL,
        first_add_channel,
        timeout,
    )
    .await?;

    let second_add_channel = json!({
        "ilk_id": second_human_ilk,
        "channel": {
            "ich_id": format!("ich:{}", Uuid::new_v4()),
            "type": dup_ich_type,
            "address": dup_ich_address.clone(),
        },
        "change_reason": "identity negative duplicate ich conflict",
    });
    let duplicate_ich_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_ADD_CHANNEL,
        second_add_channel,
        timeout,
        "DUPLICATE_ICH",
    )
    .await?;

    // Case 6 (optional): explicit NOT_PRIMARY against replica target.
    let not_primary_code = if let Some(replica_target) = env_opt("IDENTITY_NEGATIVE_REPLICA_TARGET")
    {
        let io_name = env_or(
            "IDENTITY_NEGATIVE_IO_NODE_NAME",
            &format!("IO.identity.negative.{}@{}", test_id, hive_id),
        );
        let payload = json!({
            "ich_id": format!("ich:{}", Uuid::new_v4()),
            "channel_type": "io.identity.negative",
            "address": format!("io.identity.negative.{}", test_id),
        });
        run_case_expect_error(
            &io_name,
            &replica_target,
            None,
            MSG_ILK_PROVISION,
            payload,
            timeout,
            "NOT_PRIMARY",
        )
        .await?
    } else {
        "SKIPPED".to_string()
    };

    println!("STATUS=ok");
    println!("TEST_ID={}", test_id);
    println!("TARGET={}", target);
    println!(
        "FALLBACK_TARGET={}",
        fallback_target.as_deref().unwrap_or("")
    );
    println!("UNAUTHORIZED_CODE={}", unauthorized_code);
    println!("INVALID_REQUEST_CODE={}", invalid_request_code);
    println!("INVALID_TENANT_CODE={}", invalid_tenant_code);
    println!("DUPLICATE_EMAIL_CODE={}", duplicate_email_code);
    println!("DUPLICATE_ICH_CODE={}", duplicate_ich_code);
    println!("NOT_PRIMARY_CODE={}", not_primary_code);
    Ok(())
}

async fn run_case_expect_ok(
    node_name: &str,
    target: &str,
    fallback_target: Option<&str>,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value, DynError> {
    let cfg = NodeConfig {
        name: node_name.to_string(),
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let (sender, mut receiver) = connect(&cfg).await?;
    let out = identity_system_call(
        &sender,
        &mut receiver,
        IdentitySystemRequest {
            target,
            fallback_target,
            action,
            payload,
            timeout,
        },
    )
    .await?;
    let _ = sender.close().await;
    let status = out
        .payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "ok" {
        return Err(format!(
            "unexpected non-ok response for {} from {} (effective={}): payload={}",
            action, target, out.effective_target, out.payload
        )
        .into());
    }
    Ok(out.payload)
}

async fn run_case_expect_error(
    node_name: &str,
    target: &str,
    fallback_target: Option<&str>,
    action: &str,
    payload: Value,
    timeout: Duration,
    expected_code: &str,
) -> Result<String, DynError> {
    let cfg = NodeConfig {
        name: node_name.to_string(),
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let (sender, mut receiver) = connect(&cfg).await?;
    let out = identity_system_call(
        &sender,
        &mut receiver,
        IdentitySystemRequest {
            target,
            fallback_target,
            action,
            payload,
            timeout,
        },
    )
    .await?;
    let _ = sender.close().await;

    let status = out
        .payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let code = out
        .payload
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if status != "error" || code != expected_code {
        return Err(format!(
            "unexpected response for {} from {} (effective={}): expected status=error code={}, got payload={}",
            action, target, out.effective_target, expected_code, out.payload
        )
        .into());
    }
    Ok(code)
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

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(default)
}

fn chrono_like_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
