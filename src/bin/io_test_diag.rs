use std::error::Error;
use std::path::PathBuf;

use fluxbee_sdk::identity::{
    load_hive_id, provision_ilk, resolve_ilk_from_hive_config, IlkProvisionRequest,
};
use fluxbee_sdk::{connect, NodeConfig};
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
    let allow_provision = env_bool("IO_TEST_ALLOW_PROVISION", false);
    let provision_timeout_ms = env_u64("IO_TEST_PROVISION_TIMEOUT_MS", 8_000);
    let post_provision_wait_ms = env_u64("IO_TEST_POST_PROVISION_WAIT_MS", 5_000);
    let node_name = env_or("IO_TEST_NODE_NAME", "IO.test.diag");
    let node_version = env_or("IO_TEST_NODE_VERSION", "0.0.1");

    let mut mode = "lookup_hit".to_string();
    let mut provision_trace_id: Option<String> = None;
    let mut identity_target: Option<String> = None;
    let ilk_id = match resolve_ilk_from_hive_config(&config_dir, &channel_type, &address)? {
        Some(existing) => existing,
        None if !allow_provision => {
            return Err(format!(
                "no ILK mapping found for channel_type='{}' address='{}' and IO_TEST_ALLOW_PROVISION=0",
                channel_type, address
            )
            .into());
        }
        None => {
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
                config_dir: json_router::paths::config_dir(),
                version: node_version,
            };
            let (sender, mut receiver) = connect(&node_config).await?;
            let provisioned = provision_ilk(
                &sender,
                &mut receiver,
                IlkProvisionRequest {
                    target: &target,
                    ich_id: &ich_id,
                    channel_type: &channel_type,
                    address: &address,
                    timeout: Duration::from_millis(provision_timeout_ms),
                },
            )
            .await?;
            let _ = sender.close().await;
            provision_trace_id = Some(provisioned.trace_id.clone());
            identity_target = Some(target);
            wait_for_lookup_hit(&config_dir, &channel_type, &address, post_provision_wait_ms)
                .await?
                .unwrap_or(provisioned.ilk_id)
        }
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
    max_wait_ms: u64,
) -> Result<Option<String>, DynError> {
    if max_wait_ms == 0 {
        return Ok(None);
    }
    let deadline = Instant::now() + Duration::from_millis(max_wait_ms);
    loop {
        let resolved = resolve_ilk_from_hive_config(config_dir, channel_type, address)?;
        if resolved.is_some() {
            return Ok(resolved);
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
