use std::error::Error;
use std::path::PathBuf;

use fluxbee_sdk::identity::{
    load_hive_id, resolve_ilk_from_hive_config, IdentityShmError, MSG_ILK_PROVISION,
    MSG_ILK_PROVISION_RESPONSE,
};
use fluxbee_sdk::rpc::{OperationalRouteProfile, RpcClient, RpcError, SystemRpcRequest};
use fluxbee_sdk::NodeConfig;
use serde_json::{json, Value};
use tokio::time::{sleep, Duration, Instant};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

type DynError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), DynError> {
    let log_level = env_or("JSR_LOG_LEVEL", "info");
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(log_level))
        .init();

    let config_dir = PathBuf::from(env_or("IO_TEST_CONFIG_DIR", "/etc/fluxbee"));
    let channel_type = env_or("IO_TEST_CHANNEL_TYPE", "io.test");
    let address = env_or("IO_TEST_ADDRESS", "io.test.default");
    let tenant_id = env_or("IO_TEST_TENANT_ID", "");
    let allow_provision = env_bool("IO_TEST_ALLOW_PROVISION", false);
    let provision_timeout_ms = env_u64("IO_TEST_PROVISION_TIMEOUT_MS", 8_000);
    let post_provision_wait_ms = env_u64("IO_TEST_POST_PROVISION_WAIT_MS", 5_000);
    let node_name = env_or("IO_TEST_NODE_NAME", "IO.test.diag");
    let node_version = env_or("IO_TEST_NODE_VERSION", "0.0.1");

    let mut mode = "lookup_hit".to_string();
    let mut provision_trace_id: Option<String> = None;
    let mut identity_target: Option<String> = None;
    let lookup = resolve_ilk_from_hive_config(&config_dir, &channel_type, &address, &tenant_id);
    let ilk_id = match lookup {
        Ok(Some(existing)) => existing,
        Ok(None) if !allow_provision => {
            return Err(format!(
                "no ILK mapping found for channel_type='{}' address='{}' and IO_TEST_ALLOW_PROVISION=0",
                channel_type, address
            )
            .into());
        }
        Ok(None) => {
            mode = "provisioned".to_string();
            let hive_id = load_hive_id(&config_dir)?;
            let target = env_or(
                "IO_TEST_IDENTITY_TARGET",
                &format!("SY.identity@{}", hive_id),
            );
            let ich_id = format!("ich:{}", Uuid::new_v4());
            let node_config = NodeConfig {
                name: node_name,
                router_socket: json_router::paths::router_socket_dir(),
                uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
                uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
                config_dir: json_router::paths::config_dir(),
                version: node_version,
            };
            let profile = OperationalRouteProfile::builder().build()?;
            let client =
                RpcClient::connect_with_retry(node_config, Duration::from_millis(100), profile)
                    .await?;
            let provisioned = provision_ilk_with_fallback(
                &client,
                &target,
                &ich_id,
                &channel_type,
                &address,
                &tenant_id,
                Duration::from_millis(provision_timeout_ms),
                env_opt("IO_TEST_IDENTITY_FALLBACK_TARGET"),
            )
            .await?;
            provision_trace_id = Some(provisioned.trace_id.clone());
            identity_target = Some(provisioned.target);
            wait_for_lookup_hit(
                &config_dir,
                &channel_type,
                &address,
                &tenant_id,
                post_provision_wait_ms,
            )
            .await?
            .unwrap_or(provisioned.ilk_id)
        }
        Err(err) if allow_provision && is_lookup_unavailable(&err) => {
            mode = "provisioned_lookup_unavailable".to_string();
            tracing::warn!(
                error = %err,
                channel_type = %channel_type,
                address = %address,
                "identity SHM lookup unavailable; continuing with ILK_PROVISION"
            );
            let hive_id = load_hive_id(&config_dir)?;
            let target = env_or(
                "IO_TEST_IDENTITY_TARGET",
                &format!("SY.identity@{}", hive_id),
            );
            let ich_id = format!("ich:{}", Uuid::new_v4());
            let node_config = NodeConfig {
                name: node_name,
                router_socket: json_router::paths::router_socket_dir(),
                uuid_persistence_dir: json_router::paths::state_dir().join("nodes"),
                uuid_mode: fluxbee_sdk::NodeUuidMode::Persistent,
                config_dir: json_router::paths::config_dir(),
                version: node_version,
            };
            let profile = OperationalRouteProfile::builder().build()?;
            let client =
                RpcClient::connect_with_retry(node_config, Duration::from_millis(100), profile)
                    .await?;
            let provisioned = provision_ilk_with_fallback(
                &client,
                &target,
                &ich_id,
                &channel_type,
                &address,
                &tenant_id,
                Duration::from_millis(provision_timeout_ms),
                env_opt("IO_TEST_IDENTITY_FALLBACK_TARGET"),
            )
            .await?;
            provision_trace_id = Some(provisioned.trace_id.clone());
            identity_target = Some(provisioned.target);
            wait_for_lookup_hit(
                &config_dir,
                &channel_type,
                &address,
                &tenant_id,
                post_provision_wait_ms,
            )
            .await?
            .unwrap_or(provisioned.ilk_id)
        }
        Err(err) => return Err(err.into()),
    };

    tracing::info!(
        mode = %mode,
        channel_type = %channel_type,
        address = %address,
        ilk_id = %ilk_id,
        "io test diag completed"
    );

    println!("STATUS=ok");
    println!("MODE={}", mode);
    println!("CHANNEL_TYPE={}", channel_type);
    println!("ADDRESS={}", address);
    println!("ILK_ID={}", ilk_id);
    if let Some(target) = identity_target {
        println!("IDENTITY_TARGET={}", target);
    }
    if let Some(trace_id) = provision_trace_id {
        println!("PROVISION_TRACE_ID={}", trace_id);
    }

    Ok(())
}

async fn wait_for_lookup_hit(
    config_dir: &PathBuf,
    channel_type: &str,
    address: &str,
    tenant_id: &str,
    max_wait_ms: u64,
) -> Result<Option<String>, DynError> {
    if max_wait_ms == 0 {
        return Ok(None);
    }
    let deadline = Instant::now() + Duration::from_millis(max_wait_ms);
    loop {
        match resolve_ilk_from_hive_config(config_dir, channel_type, address, tenant_id) {
            Ok(Some(ilk)) => return Ok(Some(ilk)),
            Ok(None) => {}
            Err(err) if is_lookup_unavailable(&err) => {
                // Some workers may not expose identity SHM locally yet.
                // In that case caller should rely on the provisioned ILK id.
                return Ok(None);
            }
            Err(err) => return Err(err.into()),
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        sleep(Duration::from_millis(150)).await;
    }
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

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn is_lookup_unavailable(err: &IdentityShmError) -> bool {
    match err {
        IdentityShmError::Nix(errno) => *errno == nix::errno::Errno::ENOENT,
        IdentityShmError::Io(io_err) => io_err.kind() == std::io::ErrorKind::NotFound,
        IdentityShmError::SeqLockTimeout => true,
        _ => false,
    }
}

struct ProvisionOutcome {
    ilk_id: String,
    trace_id: String,
    target: String,
}

async fn provision_ilk_with_fallback(
    client: &RpcClient,
    target: &str,
    ich_id: &str,
    channel_type: &str,
    address: &str,
    tenant_id: &str,
    timeout: Duration,
    fallback_target: Option<String>,
) -> Result<ProvisionOutcome, DynError> {
    let mut payload = json!({
        "ich_id": ich_id,
        "channel_type": channel_type,
        "address": address,
    });
    if !tenant_id.trim().is_empty() {
        payload["tenant_id"] = Value::String(tenant_id.to_string());
    }
    let first = send_provision_once(client, target, payload.clone(), timeout).await;
    match first {
        Ok((ilk_id, trace_id)) => Ok(ProvisionOutcome {
            ilk_id,
            trace_id,
            target: target.to_string(),
        }),
        Err(ProvisionAttemptError::NotPrimary { message }) => {
            let Some(fallback) = fallback_target else {
                return Err(format!("identity provision rejected NOT_PRIMARY: {message}").into());
            };
            if fallback == target {
                return Err(format!("identity provision rejected NOT_PRIMARY: {message}").into());
            }
            tracing::warn!(
                target = %target,
                fallback = %fallback,
                "identity target is replica (NOT_PRIMARY), retrying with fallback target"
            );
            let (ilk_id, trace_id) =
                send_provision_once(client, &fallback, payload, timeout).await?;
            Ok(ProvisionOutcome {
                ilk_id,
                trace_id,
                target: fallback,
            })
        }
        Err(ProvisionAttemptError::Unreachable {
            reason,
            original_dst,
        }) if reason == "NODE_NOT_FOUND" => {
            let Some(fallback) = fallback_target else {
                return Err(format!(
                    "identity transport unreachable reason={reason} original_dst={original_dst}"
                )
                .into());
            };
            if fallback == target {
                return Err(format!(
                    "identity transport unreachable reason={reason} original_dst={original_dst}"
                )
                .into());
            }
            tracing::warn!(
                target = %target,
                fallback = %fallback,
                "primary identity target unreachable (NODE_NOT_FOUND), retrying with fallback target"
            );
            let (ilk_id, trace_id) =
                send_provision_once(client, &fallback, payload, timeout).await?;
            Ok(ProvisionOutcome {
                ilk_id,
                trace_id,
                target: fallback,
            })
        }
        Err(other) => Err(other.into()),
    }
}

#[derive(Debug, thiserror::Error)]
enum ProvisionAttemptError {
    #[error("identity provision rejected NOT_PRIMARY: {message}")]
    NotPrimary { message: String },
    #[error("identity transport unreachable: reason={reason} original_dst={original_dst}")]
    Unreachable {
        reason: String,
        original_dst: String,
    },
    #[error("{0}")]
    Other(String),
}

async fn send_provision_once(
    client: &RpcClient,
    target: &str,
    payload: Value,
    timeout: Duration,
) -> Result<(String, String), ProvisionAttemptError> {
    match client
        .send_system_rpc(SystemRpcRequest {
            target,
            request_msg: MSG_ILK_PROVISION,
            response_msg: MSG_ILK_PROVISION_RESPONSE,
            payload,
            timeout,
        })
        .await
    {
        Ok(msg) => {
            let trace_id = msg.routing.trace_id.clone();
            let status = msg
                .payload
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("error");
            if status != "ok" {
                let code = msg
                    .payload
                    .get("error_code")
                    .and_then(Value::as_str)
                    .unwrap_or("UNKNOWN");
                let message = msg
                    .payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("identity returned non-ok status")
                    .to_string();
                if code == "NOT_PRIMARY" {
                    return Err(ProvisionAttemptError::NotPrimary { message });
                }
                return Err(ProvisionAttemptError::Other(format!(
                    "ILK_PROVISION error_code={code} message={message}"
                )));
            }
            let ilk_id = msg
                .payload
                .get("ilk_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ProvisionAttemptError::Other(
                        "ILK_PROVISION response missing ilk_id".to_string(),
                    )
                })?
                .to_string();
            Ok((ilk_id, trace_id))
        }
        Err(RpcError::Unreachable {
            reason,
            original_dst,
        }) => Err(ProvisionAttemptError::Unreachable {
            reason,
            original_dst,
        }),
        Err(other) => Err(ProvisionAttemptError::Other(other.to_string())),
    }
}
