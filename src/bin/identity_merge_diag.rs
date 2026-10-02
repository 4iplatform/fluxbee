//! Lab tool (not CI): merging a temporary ILK into a registered person, live against SY.identity.
//! Run it through `scripts/identity_merge_alias_e2e.sh`, which creates the test tenant through
//! SY.admin (the frontdesk no longer creates tenants) and passes it in `IDENTITY_MERGE_TENANT_ID`.
//!
//! In the test tenant it registers a person and merges two temporaries into them:
//!   A. `ILK_ADD_CHANNEL` with `merge_from_ilk_id` (the merge addressed explicitly);
//!   B. `ILK_REGISTER` of a temporary with the person's email (the merge by email): the reply
//!      says `merged` and `merged_from_ilk_id`, and it only fills the fields the person lacked.
//! After each merge the temporary's channel resolves to the person.
//!
//! It connects as an IO node (`ILK_PROVISION`) and under the frontdesk's name (`ILK_REGISTER`,
//! `ILK_ADD_CHANNEL`): the callers SY.identity authorizes for those messages. The values it
//! registers are synthetic and derived from the test id.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::rpc::{OperationalRouteProfile, RouterDispatcher, RpcError, SystemRpcRequest};
use fluxbee_sdk::NodeConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::sleep;
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
) -> Result<(Value, String), DiagError> {
    let response_msg = format!("{action}_RESPONSE");
    let fallback_eligible = || -> Option<String> {
        fallback_target
            .map(str::trim)
            .filter(|fb| !fb.is_empty() && *fb != target)
            .map(str::to_string)
    };
    let primary = client
        .send_system_rpc(SystemRpcRequest {
            target,
            request_msg: action,
            response_msg: &response_msg,
            payload: payload.clone(),
            timeout,
        })
        .await;
    match primary {
        Ok(msg) => {
            let status = msg.payload.get("status").and_then(Value::as_str);
            let code = msg.payload.get("error_code").and_then(Value::as_str);
            if status == Some("error") && code == Some("NOT_PRIMARY") {
                if let Some(fb) = fallback_eligible() {
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
                if let Some(fb) = fallback_eligible() {
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

/// SY.identity as the diag reaches it: later calls go to the target that answered the last one.
struct Identity {
    effective_target: String,
    fallback_target: Option<String>,
    timeout: Duration,
}

impl Identity {
    /// `action`'s reply, which must be `status: ok`.
    async fn call_ok(
        &mut self,
        client: &RouterDispatcher,
        action: &str,
        payload: Value,
    ) -> Result<Value, DiagError> {
        let (reply, used_target) = identity_call_with_fallback(
            client,
            &self.effective_target,
            self.fallback_target.as_deref(),
            action,
            payload,
            self.timeout,
        )
        .await?;
        self.effective_target = used_target;
        payload_status_ok(&reply).map_err(|err| format!("{action}: {err}"))?;
        Ok(reply)
    }

    /// The ilk an IO node gets for `channel_type`/`address` in `tenant_id`, with its
    /// registration_status: a new temporary the first time, then whoever holds the channel.
    async fn provision(
        &mut self,
        io_client: &RouterDispatcher,
        tenant_id: &str,
        channel_type: &str,
        address: &str,
    ) -> Result<(String, String), DiagError> {
        let reply = self
            .call_ok(
                io_client,
                "ILK_PROVISION",
                json!({
                    "ich_id": format!("ich:{}", Uuid::new_v4()),
                    "channel_type": channel_type,
                    "address": address,
                    "tenant_id": tenant_id,
                }),
            )
            .await?;
        Ok((
            reply_str(&reply, "ilk_id", "ILK_PROVISION")?,
            reply_str(&reply, "registration_status", "ILK_PROVISION")?,
        ))
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

fn reply_str(reply: &Value, field: &str, action: &str) -> Result<String, DiagError> {
    reply
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("{action} reply has no {field}: {reply}").into())
}

fn expect_eq(what: &str, got: &str, expected: &str) -> Result<(), DiagError> {
    if got == expected {
        return Ok(());
    }
    Err(format!("{what}: expected {expected}, got {got}").into())
}

fn metric_alias_count(payload: &Value) -> Option<u64> {
    payload
        .get("metrics")
        .and_then(|v| v.get("alias_count"))
        .and_then(|v| v.as_u64())
}

fn connect_config(name: String) -> NodeConfig {
    NodeConfig {
        name,
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    }
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
    // The frontdesk cannot create tenants (only SY.admin and SY.architect can): the test tenant
    // comes from SY.admin, through the script.
    let tenant_id = env_opt("IDENTITY_MERGE_TENANT_ID").ok_or(
        "IDENTITY_MERGE_TENANT_ID is required: an active tenant created through SY.admin \
         (scripts/identity_merge_alias_e2e.sh creates one)",
    )?;
    if !tenant_id
        .strip_prefix("tnt:")
        .is_some_and(|raw| Uuid::parse_str(raw).is_ok())
    {
        return Err(format!("IDENTITY_MERGE_TENANT_ID '{tenant_id}' is not tnt:<uuid>").into());
    }
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
    let channel_type_email = env_or("IDENTITY_MERGE_EMAIL_CHANNEL_TYPE", "io.test.merge.email");
    let address_email = env_or(
        "IDENTITY_MERGE_EMAIL_ADDRESS",
        &format!("merge-email-{}", test_id),
    );

    let io_node_name = env_or(
        "IDENTITY_MERGE_IO_NODE_NAME",
        &format!("IO.test.identity.merge.{}", test_id),
    );
    let frontdesk_node_name = env_or(
        "IDENTITY_MERGE_FRONTDESK_NODE_NAME",
        &format!("SY.frontdesk.gov@{}", hive_id),
    );

    let io_client = RouterDispatcher::connect_with_retry(
        connect_config(io_node_name),
        Duration::from_millis(100),
        OperationalRouteProfile::builder().build()?,
    )
    .await?;
    let frontdesk_client = RouterDispatcher::connect_with_retry(
        connect_config(frontdesk_node_name),
        Duration::from_millis(100),
        OperationalRouteProfile::builder().build()?,
    )
    .await?;
    let mut identity = Identity {
        effective_target: target.clone(),
        fallback_target,
        timeout: Duration::from_millis(timeout_ms),
    };

    let metrics_before = identity
        .call_ok(&io_client, "IDENTITY_METRICS", json!({}))
        .await?;
    let alias_before = metric_alias_count(&metrics_before).unwrap_or(0);

    // The registered person.
    let canonical_ilk_id = env_or(
        "IDENTITY_MERGE_CANONICAL_ILK_ID",
        &format!("ilk:{}", Uuid::new_v4()),
    );
    let email = format!("merge-{}@diag.local", test_id);
    let kept_display_name = format!("Merge {}", test_id);
    let register = identity
        .call_ok(
            &frontdesk_client,
            "ILK_REGISTER",
            json!({
                "ilk_id": canonical_ilk_id,
                "ilk_type": "human",
                "tenant_id": tenant_id,
                "identification": {
                    "display_name": kept_display_name,
                    "email": email,
                },
            }),
        )
        .await?;
    expect_eq(
        "ILK_REGISTER of the person: ilk_id",
        &reply_str(&register, "ilk_id", "ILK_REGISTER")?,
        &canonical_ilk_id,
    )?;
    if register.get("merged").and_then(Value::as_bool) != Some(false) {
        return Err(
            format!("ILK_REGISTER of the person: expected merged=false: {register}").into(),
        );
    }

    // A. ILK_ADD_CHANNEL with merge_from_ilk_id.
    let (old_ilk_id, old_status) = identity
        .provision(&io_client, &tenant_id, &channel_type_old, &address_old)
        .await?;
    expect_eq("ILK_PROVISION (A)", &old_status, "temporary")?;
    identity
        .call_ok(
            &frontdesk_client,
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
        )
        .await?;
    let (resolved_old_channel_ilk, _) = identity
        .provision(&io_client, &tenant_id, &channel_type_old, &address_old)
        .await?;
    expect_eq(
        "the merged channel (A) resolves to the person",
        &resolved_old_channel_ilk,
        &canonical_ilk_id,
    )?;

    // B. ILK_REGISTER of a temporary with the person's email: the merge by email. It must keep
    // the person's display_name and fill the phone the person lacked.
    let (email_temp_ilk_id, email_temp_status) = identity
        .provision(&io_client, &tenant_id, &channel_type_email, &address_email)
        .await?;
    expect_eq("ILK_PROVISION (B)", &email_temp_status, "temporary")?;
    let filled_phone = format!("diag-phone-{}", test_id);
    let merge = identity
        .call_ok(
            &frontdesk_client,
            "ILK_REGISTER",
            json!({
                "ilk_id": email_temp_ilk_id,
                "ilk_type": "human",
                "tenant_id": tenant_id,
                "identification": {
                    "display_name": format!("Merge again {}", test_id),
                    "email": email,
                    "phone": filled_phone,
                },
            }),
        )
        .await?;
    if merge.get("merged").and_then(Value::as_bool) != Some(true) {
        return Err(format!("ILK_REGISTER by email: expected merged=true: {merge}").into());
    }
    let email_merge_ilk_id = reply_str(&merge, "ilk_id", "ILK_REGISTER")?;
    let email_merged_from = reply_str(&merge, "merged_from_ilk_id", "ILK_REGISTER")?;
    expect_eq(
        "ILK_REGISTER by email: ilk_id",
        &email_merge_ilk_id,
        &canonical_ilk_id,
    )?;
    expect_eq(
        "ILK_REGISTER by email: merged_from_ilk_id",
        &email_merged_from,
        &email_temp_ilk_id,
    )?;
    expect_eq(
        "ILK_REGISTER by email: registration_status",
        &reply_str(&merge, "registration_status", "ILK_REGISTER")?,
        "complete",
    )?;
    let (resolved_email_channel_ilk, _) = identity
        .provision(&io_client, &tenant_id, &channel_type_email, &address_email)
        .await?;
    expect_eq(
        "the merged channel (B) resolves to the person",
        &resolved_email_channel_ilk,
        &canonical_ilk_id,
    )?;

    let metrics_after_merge = identity
        .call_ok(&io_client, "IDENTITY_METRICS", json!({}))
        .await?;
    let alias_after_merge = metric_alias_count(&metrics_after_merge).unwrap_or(0);

    let mut alias_after_wait = alias_after_merge;
    if wait_gc_secs > 0 {
        sleep(Duration::from_secs(wait_gc_secs)).await;
        let metrics_after_wait = identity
            .call_ok(&io_client, "IDENTITY_METRICS", json!({}))
            .await?;
        alias_after_wait = metric_alias_count(&metrics_after_wait).unwrap_or(alias_after_merge);
    }

    if require_alias_cleanup && alias_after_wait >= alias_after_merge {
        return Err(format!(
            "alias cleanup expected but alias_count did not drop: before={} after_merge={} after_wait={}",
            alias_before, alias_after_merge, alias_after_wait
        )
        .into());
    }

    println!("STATUS=ok");
    println!("TEST_ID={}", test_id);
    println!("TARGET={}", target);
    println!("EFFECTIVE_TARGET={}", identity.effective_target);
    println!("TENANT_ID={}", tenant_id);
    println!("CANONICAL_ILK_ID={}", canonical_ilk_id);
    println!("OLD_ILK_ID={}", old_ilk_id);
    println!("RESOLVED_OLD_CHANNEL_ILK_ID={}", resolved_old_channel_ilk);
    println!("EMAIL_TEMP_ILK_ID={}", email_temp_ilk_id);
    println!("EMAIL_MERGE_ILK_ID={}", email_merge_ilk_id);
    println!("EMAIL_MERGED_FROM_ILK_ID={}", email_merged_from);
    println!(
        "RESOLVED_EMAIL_CHANNEL_ILK_ID={}",
        resolved_email_channel_ilk
    );
    // What the script checks through SY.admin (synthetic values derived from the test id).
    println!("KEPT_DISPLAY_NAME={}", kept_display_name);
    println!("FILLED_PHONE={}", filled_phone);
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
