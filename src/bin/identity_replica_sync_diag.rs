//! Lab tool (not CI): an identity write on the primary reaches a replica, live. Run it through
//! `scripts/identity_replica_sync_e2e.sh`, which creates the test tenant through SY.admin (the
//! frontdesk no longer creates tenants), waits until the replica has it, and passes it in
//! `IDENTITY_REPLICA_TENANT_ID`.
//!
//! It waits until the replica's counts reach the primary's (the baseline), then an IO node
//! provisions an ILK in the test tenant on the primary, and it waits until the replica's counts
//! reach the primary's again (the delta).

use std::error::Error;
use std::path::PathBuf;

use fluxbee_sdk::identity::{load_hive_id, MSG_IDENTITY_METRICS, MSG_ILK_PROVISION};
use fluxbee_sdk::rpc::{OperationalRouteProfile, RouterDispatcher, RpcError, SystemRpcRequest};
use fluxbee_sdk::NodeConfig;
use serde_json::{json, Value};
use tokio::time::{sleep, Duration, Instant};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn Error + Send + Sync>;

#[derive(Clone, Copy, Debug)]
struct IdentityMetrics {
    tenant_count: u64,
    ilk_count: u64,
    ich_count: u64,
}

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    fluxbee_sdk::logging::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(env_or("IDENTITY_REPLICA_CONFIG_DIR", "/etc/fluxbee"));
    let local_hive_id = load_hive_id(&config_dir)?;
    let test_id = env_or(
        "IDENTITY_REPLICA_TEST_ID",
        &format!("idrep-{}", chrono_like_now_ms()),
    );
    let timeout_ms = env_u64("IDENTITY_REPLICA_TIMEOUT_MS", 10_000);
    let convergence_timeout_ms = env_u64("IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS", 30_000);
    let poll_ms = env_u64("IDENTITY_REPLICA_POLL_MS", 250).max(50);
    let require_baseline_sync = env_bool("IDENTITY_REPLICA_REQUIRE_BASELINE_SYNC", true);

    let primary_target = env_or(
        "IDENTITY_REPLICA_PRIMARY_TARGET",
        &format!("SY.identity@{}", local_hive_id),
    );
    let replica_target = env_opt("IDENTITY_REPLICA_TARGET")
        .ok_or("IDENTITY_REPLICA_TARGET is required: the replica's SY.identity@<hive>")?;
    let primary_fallback = env_opt("IDENTITY_REPLICA_PRIMARY_FALLBACK_TARGET");
    // The frontdesk cannot create tenants (only SY.admin and SY.architect can): the test tenant
    // comes from SY.admin, through the script.
    let tenant_id = env_opt("IDENTITY_REPLICA_TENANT_ID").ok_or(
        "IDENTITY_REPLICA_TENANT_ID is required: an active tenant created through SY.admin \
         (scripts/identity_replica_sync_e2e.sh creates it)",
    )?;
    if !tenant_id
        .strip_prefix("tnt:")
        .is_some_and(|raw| Uuid::parse_str(raw).is_ok())
    {
        return Err(format!("IDENTITY_REPLICA_TENANT_ID '{tenant_id}' is not tnt:<uuid>").into());
    }

    let diag_node_name = env_or(
        "IDENTITY_REPLICA_DIAG_NODE_NAME",
        &format!("WF.identity.replica.{}@{}", test_id, local_hive_id),
    );
    let io_node_name = env_or(
        "IDENTITY_REPLICA_IO_NODE_NAME",
        &format!("IO.identity.replica.{}@{}", test_id, local_hive_id),
    );

    let mut diag_session = connect_node(&diag_node_name).await?;
    let mut io_session = connect_node(&io_node_name).await?;

    let baseline_primary = fetch_metrics(
        &mut diag_session,
        &primary_target,
        primary_fallback.as_deref(),
        timeout_ms,
    )
    .await?;
    let baseline_replica =
        fetch_metrics(&mut diag_session, &replica_target, None, timeout_ms).await?;

    let baseline_sync_ok = if require_baseline_sync {
        wait_for_replica_min_counts(
            &mut diag_session,
            &replica_target,
            baseline_primary,
            timeout_ms,
            convergence_timeout_ms,
            poll_ms,
        )
        .await?
    } else {
        baseline_replica
    };

    let provision = identity_call(
        &mut io_session,
        &primary_target,
        primary_fallback.as_deref(),
        MSG_ILK_PROVISION,
        json!({
            "ich_id": format!("ich:{}", Uuid::new_v4()),
            "channel_type": env_or("IDENTITY_REPLICA_CHANNEL_TYPE", "io.identity.replica"),
            "address": format!("io.identity.replica.{}", test_id),
            "tenant_id": tenant_id,
        }),
        timeout_ms,
    )
    .await?;
    ensure_payload_ok(MSG_ILK_PROVISION, &provision.payload)?;
    let provisioned_ilk_id = provision
        .payload
        .get("ilk_id")
        .and_then(Value::as_str)
        .ok_or("ILK_PROVISION missing ilk_id")?
        .to_string();

    let after_delta_primary = fetch_metrics(
        &mut diag_session,
        &primary_target,
        primary_fallback.as_deref(),
        timeout_ms,
    )
    .await?;
    if after_delta_primary.ilk_count <= baseline_primary.ilk_count {
        return Err(format!(
            "ilk_count did not increase in primary (before={} after={})",
            baseline_primary.ilk_count, after_delta_primary.ilk_count
        )
        .into());
    }
    if after_delta_primary.ich_count <= baseline_primary.ich_count {
        return Err(format!(
            "ich_count did not increase in primary (before={} after={})",
            baseline_primary.ich_count, after_delta_primary.ich_count
        )
        .into());
    }
    let after_delta_replica = wait_for_replica_min_counts(
        &mut diag_session,
        &replica_target,
        after_delta_primary,
        timeout_ms,
        convergence_timeout_ms,
        poll_ms,
    )
    .await?;

    println!("STATUS=ok");
    println!("TEST_ID={}", test_id);
    println!("PRIMARY_TARGET={}", primary_target);
    println!("REPLICA_TARGET={}", replica_target);
    println!("TENANT_ID={}", tenant_id);
    println!("PROVISIONED_ILK_ID={}", provisioned_ilk_id);
    println!("BASELINE_SYNC_OK=1");
    println!("DELTA_SYNC_OK=1");
    println!(
        "BASELINE_PRIMARY=tenant:{} ilk:{} ich:{}",
        baseline_primary.tenant_count, baseline_primary.ilk_count, baseline_primary.ich_count
    );
    println!(
        "BASELINE_REPLICA=tenant:{} ilk:{} ich:{}",
        baseline_replica.tenant_count, baseline_replica.ilk_count, baseline_replica.ich_count
    );
    println!(
        "AFTER_BASELINE_REPLICA=tenant:{} ilk:{} ich:{}",
        baseline_sync_ok.tenant_count, baseline_sync_ok.ilk_count, baseline_sync_ok.ich_count
    );
    println!(
        "AFTER_DELTA_PRIMARY=tenant:{} ilk:{} ich:{}",
        after_delta_primary.tenant_count,
        after_delta_primary.ilk_count,
        after_delta_primary.ich_count
    );
    println!(
        "AFTER_DELTA_REPLICA=tenant:{} ilk:{} ich:{}",
        after_delta_replica.tenant_count,
        after_delta_replica.ilk_count,
        after_delta_replica.ich_count
    );
    Ok(())
}

struct NodeSession {
    client: std::sync::Arc<RouterDispatcher>,
}

struct IdentityCallResult {
    payload: Value,
}

async fn connect_node(name: &str) -> Result<NodeSession, DynError> {
    let cfg = NodeConfig {
        name: name.to_string(),
        router_socket: json_router::paths::router_socket_dir(),
        uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
        uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
        config_dir: json_router::paths::config_dir(),
        version: "0.0.1".to_string(),
    };
    let profile = OperationalRouteProfile::builder().build()?;
    let client =
        RouterDispatcher::connect_with_retry(cfg, Duration::from_millis(100), profile).await?;
    Ok(NodeSession { client })
}

async fn identity_call(
    session: &mut NodeSession,
    target: &str,
    fallback_target: Option<&str>,
    action: &str,
    payload: Value,
    timeout_ms: u64,
) -> Result<IdentityCallResult, DynError> {
    let (payload, _effective_target) = identity_call_with_fallback(
        &session.client,
        target,
        fallback_target,
        action,
        payload,
        Duration::from_millis(timeout_ms),
    )
    .await?;
    Ok(IdentityCallResult { payload })
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

async fn fetch_metrics(
    session: &mut NodeSession,
    target: &str,
    fallback_target: Option<&str>,
    timeout_ms: u64,
) -> Result<IdentityMetrics, DynError> {
    let out = identity_call(
        session,
        target,
        fallback_target,
        MSG_IDENTITY_METRICS,
        json!({}),
        timeout_ms,
    )
    .await?;
    ensure_payload_ok(MSG_IDENTITY_METRICS, &out.payload)?;
    parse_metrics(&out.payload)
}

async fn wait_for_replica_min_counts(
    session: &mut NodeSession,
    replica_target: &str,
    min_counts: IdentityMetrics,
    timeout_ms: u64,
    converge_timeout_ms: u64,
    poll_ms: u64,
) -> Result<IdentityMetrics, DynError> {
    let deadline = Instant::now() + Duration::from_millis(converge_timeout_ms);
    loop {
        let metrics = fetch_metrics(session, replica_target, None, timeout_ms).await?;
        if metrics.tenant_count >= min_counts.tenant_count
            && metrics.ilk_count >= min_counts.ilk_count
            && metrics.ich_count >= min_counts.ich_count
        {
            return Ok(metrics);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "replica did not converge in time (expected at least tenant:{} ilk:{} ich:{}, got tenant:{} ilk:{} ich:{})",
                min_counts.tenant_count,
                min_counts.ilk_count,
                min_counts.ich_count,
                metrics.tenant_count,
                metrics.ilk_count,
                metrics.ich_count
            )
            .into());
        }
        sleep(Duration::from_millis(poll_ms)).await;
    }
}

fn parse_metrics(payload: &Value) -> Result<IdentityMetrics, DynError> {
    let metrics = payload
        .get("metrics")
        .ok_or("missing metrics in IDENTITY_METRICS payload")?;
    Ok(IdentityMetrics {
        tenant_count: metrics
            .get("tenant_count")
            .and_then(Value::as_u64)
            .ok_or("missing tenant_count")?,
        ilk_count: metrics
            .get("ilk_count")
            .and_then(Value::as_u64)
            .ok_or("missing ilk_count")?,
        ich_count: metrics
            .get("ich_count")
            .and_then(Value::as_u64)
            .ok_or("missing ich_count")?,
    })
}

fn ensure_payload_ok(action: &str, payload: &Value) -> Result<(), DynError> {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if status == "ok" {
        return Ok(());
    }
    let code = payload
        .get("error_code")
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN");
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("");
    Err(format!(
        "{} returned non-ok payload status={} code={} message={}",
        action, status, code, message
    )
    .into())
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

fn chrono_like_now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
