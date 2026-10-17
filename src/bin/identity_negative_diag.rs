//! Lab tool (not CI): the codes SY.identity refuses identity writes with, live. Run it through
//! `scripts/identity_negative_e2e.sh`, which creates the test tenant through SY.admin (the
//! frontdesk no longer creates tenants) and passes it in `IDENTITY_NEGATIVE_TENANT_ID`.
//!
//! It connects under the names SY.identity authorizes (or not) for each message: an unknown WF
//! node, the frontdesk (`ILK_REGISTER`, `ILK_ADD_CHANNEL`) and an IO node (`ILK_PROVISION`). The
//! values it registers are synthetic and derived from the test id.

use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use fluxbee_sdk::identity::{
    load_hive_id, DEFAULT_ROOT_TENANT_ID, MSG_ILK_ADD_CHANNEL, MSG_ILK_PROVISION, MSG_ILK_REGISTER,
    MSG_TNT_CREATE,
};
use fluxbee_sdk::rpc::{OperationalRouteProfile, RouterDispatcher, RpcError, SystemRpcRequest};
use fluxbee_sdk::NodeConfig;
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tokio::time::Duration;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    fluxbee_sdk::logging::fmt()
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
    // The frontdesk cannot create tenants (only SY.admin and SY.architect can): the test tenant
    // comes from SY.admin, through the script.
    let tenant_id = env_opt("IDENTITY_NEGATIVE_TENANT_ID").ok_or(
        "IDENTITY_NEGATIVE_TENANT_ID is required: an active tenant created through SY.admin \
         (scripts/identity_negative_e2e.sh creates one)",
    )?;
    if !tenant_id
        .strip_prefix("tnt:")
        .is_some_and(|raw| Uuid::parse_str(raw).is_ok())
    {
        return Err(format!("IDENTITY_NEGATIVE_TENANT_ID '{tenant_id}' is not tnt:<uuid>").into());
    }

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
    // Canonical name: SY.identity only authorizes the configured frontdesk
    // node name (no suffixes after @hive — those malform the hive segment).
    let frontdesk_name = env_or(
        "IDENTITY_NEGATIVE_FRONTDESK_NODE_NAME",
        &format!("SY.frontdesk.gov@{}", hive_id),
    );
    let malformed_payload = json!({
        "ilk_id": "ilk:not-a-uuid",
        "ilk_type": "human",
        "tenant_id": format!("tnt:{}", Uuid::new_v4()),
        "identification": {
            "display_name": "malformed",
            "email": format!("malformed-{}@diag.local", test_id),
        },
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

    // Case 4: the frontdesk creates no tenants => UNAUTHORIZED_REGISTRAR.
    let frontdesk_tnt_create_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_TNT_CREATE,
        json!({
            "name": format!("identity-negative-frontdesk-{}", test_id),
            "status": "active",
        }),
        timeout,
        "UNAUTHORIZED_REGISTRAR",
    )
    .await?;

    // Case 5: a new ilk with an email another ilk of the tenant has => DUPLICATE_EMAIL (only a
    // temporary of the tenant merges into its holder).
    let duplicate_email = format!("duplicate-email-{}@diag.local", test_id);
    let first_human_register = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": tenant_id.clone(),
        "identification": {
            "display_name": "dup-email-first",
            "email": duplicate_email,
        },
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

    // Case 6: duplicate ICH (channel_type + address + tenant_id) => DUPLICATE_ICH.
    let second_unique_human_register = json!({
        "ilk_id": format!("ilk:{}", Uuid::new_v4()),
        "ilk_type": "human",
        "tenant_id": tenant_id.clone(),
        "identification": {
            "display_name": "dup-ich-second-human",
            "email": format!("dup-ich-second-{}@diag.local", test_id),
        },
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

    // Case 7: a complete ilk that registers the email of another => DUPLICATE_EMAIL, too: a
    // complete ilk does not merge (it keeps its channels).
    let complete_duplicate_email_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        json!({
            "ilk_id": second_human_ilk,
            "ilk_type": "human",
            "tenant_id": tenant_id.clone(),
            "identification": {
                "display_name": "dup-email-complete",
                "email": duplicate_email,
            },
        }),
        timeout,
        "DUPLICATE_EMAIL",
    )
    .await?;

    // Case 8: nobody registers a person into the root tenant => TENANT_ROOT_NOT_REGISTRABLE. A
    // root-tenant IO node still provisions the person (a provision without a tenant lands in the
    // root tenant), and the person stays temporary.
    let io_name = env_or(
        "IDENTITY_NEGATIVE_IO_NODE_NAME",
        &format!("IO.identity.negative.{}@{}", test_id, hive_id),
    );
    let root_channel_type = "io.identity.negative.root";
    let root_address = format!("io.identity.negative.root.{}", test_id);
    let root_provision = || {
        json!({
            "ich_id": format!("ich:{}", Uuid::new_v4()),
            "channel_type": root_channel_type,
            "address": root_address,
        })
    };
    let root_temp = run_case_expect_ok(
        &io_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_PROVISION,
        root_provision(),
        timeout,
    )
    .await?;
    let root_temp_ilk = root_temp
        .get("ilk_id")
        .and_then(Value::as_str)
        .ok_or("missing ilk_id in the root-tenant ILK_PROVISION response")?
        .to_string();
    // Printed now, so the script can clean it up even if a later case fails.
    println!("ROOT_TEMP_ILK_ID={}", root_temp_ilk);
    let root_tenant_code = run_case_expect_error(
        &frontdesk_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_REGISTER,
        json!({
            "ilk_id": root_temp_ilk,
            "ilk_type": "human",
            "tenant_id": DEFAULT_ROOT_TENANT_ID,
            "identification": {
                "display_name": "root-tenant-person",
                "email": format!("root-tenant-{}@diag.local", test_id),
            },
        }),
        timeout,
        "TENANT_ROOT_NOT_REGISTRABLE",
    )
    .await?;
    let root_again = run_case_expect_ok(
        &io_name,
        &target,
        fallback_target.as_deref(),
        MSG_ILK_PROVISION,
        root_provision(),
        timeout,
    )
    .await?;
    let root_temp_status = root_again
        .get("registration_status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if root_again.get("ilk_id").and_then(Value::as_str) != Some(root_temp_ilk.as_str())
        || root_temp_status != "temporary"
    {
        return Err(format!(
            "the root-tenant person must stay the same temporary ilk {root_temp_ilk}, got {root_again}"
        )
        .into());
    }

    // Case 9 (optional): explicit NOT_PRIMARY against replica target.
    let not_primary_code = if let Some(replica_target) = env_opt("IDENTITY_NEGATIVE_REPLICA_TARGET")
    {
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
    println!("TENANT_ID={}", tenant_id);
    println!("UNAUTHORIZED_CODE={}", unauthorized_code);
    println!("INVALID_REQUEST_CODE={}", invalid_request_code);
    println!("INVALID_TENANT_CODE={}", invalid_tenant_code);
    println!("FRONTDESK_TNT_CREATE_CODE={}", frontdesk_tnt_create_code);
    println!("DUPLICATE_EMAIL_CODE={}", duplicate_email_code);
    println!("DUPLICATE_ICH_CODE={}", duplicate_ich_code);
    println!(
        "COMPLETE_DUPLICATE_EMAIL_CODE={}",
        complete_duplicate_email_code
    );
    println!("ROOT_TENANT_CODE={}", root_tenant_code);
    println!("ROOT_TEMP_STATUS={}", root_temp_status);
    println!("NOT_PRIMARY_CODE={}", not_primary_code);
    Ok(())
}

/// The connection under `node_name`, one per name for the whole run. The UUID is persisted per
/// name, and the router refuses a second connection with a UUID that is still connected (A-44).
async fn connection(node_name: &str) -> Result<Arc<RouterDispatcher>, DynError> {
    static CONNECTIONS: OnceLock<Mutex<HashMap<String, Arc<RouterDispatcher>>>> = OnceLock::new();
    let mut connections = CONNECTIONS.get_or_init(Default::default).lock().await;
    if let Some(client) = connections.get(node_name) {
        return Ok(Arc::clone(client));
    }
    let cfg = NodeConfig {
        name: node_name.to_string(),
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let profile = OperationalRouteProfile::builder().build()?;
    let client =
        RouterDispatcher::connect_with_retry(cfg, Duration::from_millis(100), profile).await?;
    connections.insert(node_name.to_string(), Arc::clone(&client));
    Ok(client)
}

async fn run_case_expect_ok(
    node_name: &str,
    target: &str,
    fallback_target: Option<&str>,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value, DynError> {
    let client = connection(node_name).await?;
    let (payload, effective_target) =
        identity_call_with_fallback(&client, target, fallback_target, action, payload, timeout)
            .await?;
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status != "ok" {
        return Err(format!(
            "unexpected non-ok response for {} from {} (effective={}): payload={}",
            action, target, effective_target, payload
        )
        .into());
    }
    Ok(payload)
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
    let client = connection(node_name).await?;
    let (payload, effective_target) =
        identity_call_with_fallback(&client, target, fallback_target, action, payload, timeout)
            .await?;

    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let code = payload
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if status != "error" || code != expected_code {
        return Err(format!(
            "unexpected response for {} from {} (effective={}): expected status=error code={}, got payload={}",
            action, target, effective_target, expected_code, payload
        )
        .into());
    }
    Ok(code)
}

/// Replicates the SDK `identity_system_call` fallback semantics over the new
/// `RouterDispatcher::send_system_rpc`. Retries on transport `UNREACHABLE` with
/// `reason=NODE_NOT_FOUND` or on payload `status=error, error_code=NOT_PRIMARY`,
/// against `fallback_target` when supplied and distinct.
async fn identity_call_with_fallback(
    client: &RouterDispatcher,
    target: &str,
    fallback_target: Option<&str>,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<(Value, String), DynError> {
    let response_msg = format!("{action}_RESPONSE");
    let primary = client
        .send_system_rpc(SystemRpcRequest {
            target,
            request_msg: action,
            response_msg: &response_msg,
            payload: payload.clone(),
            timeout,
        })
        .await;

    let fallback_eligible = |fb: Option<&str>| -> Option<String> {
        fb.map(str::trim)
            .filter(|fb| !fb.is_empty() && *fb != target)
            .map(str::to_string)
    };

    match primary {
        Ok(msg) => {
            let p = &msg.payload;
            let status = p.get("status").and_then(Value::as_str);
            let code = p.get("error_code").and_then(Value::as_str);
            if status == Some("error") && code == Some("NOT_PRIMARY") {
                if let Some(fb) = fallback_eligible(fallback_target) {
                    let retry = client
                        .send_system_rpc(SystemRpcRequest {
                            target: &fb,
                            request_msg: action,
                            response_msg: &response_msg,
                            payload,
                            timeout,
                        })
                        .await?;
                    return Ok((retry.payload, fb));
                }
            }
            Ok((msg.payload, target.to_string()))
        }
        Err(RpcError::Unreachable {
            reason,
            original_dst,
        }) => {
            if reason == "NODE_NOT_FOUND" {
                if let Some(fb) = fallback_eligible(fallback_target) {
                    let retry = client
                        .send_system_rpc(SystemRpcRequest {
                            target: &fb,
                            request_msg: action,
                            response_msg: &response_msg,
                            payload,
                            timeout,
                        })
                        .await?;
                    return Ok((retry.payload, fb));
                }
            }
            Err(format!(
                "identity transport unreachable reason={reason} original_dst={original_dst}"
            )
            .into())
        }
        Err(other) => Err(other.to_string().into()),
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
