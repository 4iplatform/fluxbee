use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use json_router::shm::{OpaRegionReader, OPA_STATUS_ERROR, OPA_STATUS_LOADING, OPA_STATUS_OK};
use serde::Deserialize;
use serde_json::json;

type DiagError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Deserialize)]
struct HiveFile {
    hive_id: String,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u64_opt(key: &str) -> Option<u64> {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn load_hive_id(config_dir: &std::path::Path) -> Result<String, DiagError> {
    let data = std::fs::read_to_string(config_dir.join("hive.yaml"))?;
    let hive: HiveFile = serde_yaml::from_str(&data)?;
    Ok(hive.hive_id)
}

fn status_name(status: u8) -> &'static str {
    match status {
        OPA_STATUS_OK => "ok",
        OPA_STATUS_ERROR => "error",
        OPA_STATUS_LOADING => "loading",
        _ => "unknown",
    }
}

fn main() -> Result<(), DiagError> {
    let config_dir = json_router::paths::config_dir();
    let hive_id = env_or("OPA_HIVE_ID", &load_hive_id(&config_dir)?);
    let shm_name = format!("/jsr-opa-{hive_id}");
    let expected_status = std::env::var("OPA_EXPECT_STATUS").ok();
    let min_version = env_u64_opt("OPA_MIN_VERSION");
    let max_heartbeat_age_ms = env_u64_opt("OPA_MAX_HEARTBEAT_AGE_MS");

    let reader = OpaRegionReader::open_read_only(&shm_name)?;
    let header = reader
        .read_header()
        .ok_or("failed to read OPA SHM header snapshot")?;

    let heartbeat_age_ms = now_epoch_ms().saturating_sub(header.heartbeat);
    let status = status_name(header.status);

    if let Some(expected) = expected_status.as_deref() {
        if status != expected {
            return Err(format!("OPA status mismatch: expected={expected} actual={status}").into());
        }
    }
    if let Some(min) = min_version {
        if header.policy_version < min {
            return Err(format!(
                "OPA policy_version too low: expected_min={min} actual={}",
                header.policy_version
            )
            .into());
        }
    }
    if let Some(max_age) = max_heartbeat_age_ms {
        if heartbeat_age_ms > max_age {
            return Err(format!(
                "OPA heartbeat too old: max_age_ms={max_age} actual_age_ms={heartbeat_age_ms}"
            )
            .into());
        }
    }

    println!(
        "{}",
        json!({
            "status": "ok",
            "shm_name": shm_name,
            "hive_id": hive_id,
            "opa_status": status,
            "opa_status_code": header.status,
            "policy_version": header.policy_version,
            "entrypoint": header.entrypoint,
            "wasm_size": header.wasm_size,
            "updated_at": header.updated_at,
            "heartbeat": header.heartbeat,
            "heartbeat_age_ms": heartbeat_age_ms,
            "owner_uuid": header.owner_uuid.to_string(),
            "owner_pid": header.owner_pid,
        })
    );
    Ok(())
}
