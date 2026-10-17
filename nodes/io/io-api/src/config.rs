use anyhow::Result;
use fluxbee_sdk::{managed_node_config_path, FLUXBEE_NODE_NAME_ENV};
use io_common::relay::RelayPolicy;
use io_common::router_message::DEFAULT_TTL;
use serde_json::Value;
use std::path::PathBuf;

use crate::{ApiRelayConfig, Config, SpawnConfig};

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        let node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            anyhow::anyhow!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let hive_id = hive_from_node_name(&node_name).ok_or_else(|| {
            anyhow::anyhow!("invalid {FLUXBEE_NODE_NAME_ENV}='{node_name}': expected <name>@<hive>")
        })?;
        let spawn_cfg = load_spawn_config(&node_name)?;
        tracing::info!(path = %spawn_cfg.path.display(), "io-api loaded managed spawn config");
        let spawn_doc = &spawn_cfg.doc;

        Ok(Self {
            node_name,
            hive_id: hive_id.clone(),
            node_version: env("NODE_VERSION")
                .or_else(|| json_get_string(spawn_doc, "_system.runtime_version"))
                .unwrap_or_else(|| "1.0.0".to_string()),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| json_get_string(spawn_doc, "node.router_socket"))
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| json_get_string(spawn_doc, "node.uuid_persistence_dir"))
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string(spawn_doc, "node.config_dir"))
                    .unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            spawn_config_path: spawn_cfg.path,
            identity_target: env("IO_API_IDENTITY_TARGET")
                .or_else(|| json_get_string(spawn_doc, "node.identity_target"))
                .unwrap_or_else(|| "SY.identity@motherbee".to_string()),
            frontdesk_target: env("IO_API_FRONTDESK_TARGET")
                .or_else(|| json_get_string(spawn_doc, "node.frontdesk_target"))
                .unwrap_or_else(|| "SY.frontdesk.gov@motherbee".to_string()),
            admin_target: env("IO_API_ADMIN_TARGET")
                .or_else(|| json_get_string(spawn_doc, "node.admin_target"))
                .unwrap_or_else(|| "SY.admin@motherbee".to_string()),
            orchestrator_target: env("IO_API_ORCHESTRATOR_TARGET")
                .or_else(|| json_get_string(spawn_doc, "node.orchestrator_target"))
                .unwrap_or_else(|| format!("SY.orchestrator@{hive_id}")),
            identity_timeout_ms: env_u64("IO_API_IDENTITY_TIMEOUT_MS")
                .or_else(|| json_get_u64(spawn_doc, "node.identity_timeout_ms"))
                .unwrap_or(10_000),
            reconcile_interval_secs: env_u64("IO_API_RECONCILE_SECONDS")
                .or_else(|| json_get_u64(spawn_doc, "node.reconcile_interval_secs"))
                .filter(|value| *value > 0)
                .unwrap_or(30),
            ttl: env_u64("IO_API_TTL")
                .or_else(|| json_get_u64(spawn_doc, "node.ttl"))
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(DEFAULT_TTL),
            dedup_ttl_ms: env_u64("IO_API_DEDUP_TTL_MS")
                .or_else(|| json_get_u64(spawn_doc, "runtime.dedup_ttl_ms"))
                .unwrap_or(600_000),
            dedup_max_entries: env_u64("IO_API_DEDUP_MAX_ENTRIES")
                .or_else(|| json_get_u64(spawn_doc, "runtime.dedup_max_entries"))
                .and_then(|value| usize::try_from(value).ok())
                .unwrap_or(50_000),
            relay: ApiRelayConfig {
                window_ms: 0,
                max_open_sessions: 10_000,
                max_fragments_per_session: 8,
                max_bytes_per_session: 256 * 1024,
            },
        })
    }
}

pub(crate) fn api_relay_policy(
    config: &Config,
    effective_config: Option<&Value>,
) -> Result<RelayPolicy> {
    let relay_cfg = extract_runtime_relay_config(effective_config, &config.relay)?;
    api_relay_policy_from_config(&relay_cfg)
}

pub(crate) fn api_relay_policy_from_config(relay_cfg: &ApiRelayConfig) -> Result<RelayPolicy> {
    let mut policy = RelayPolicy {
        enabled: relay_cfg.window_ms > 0,
        relay_window_ms: relay_cfg.window_ms,
        max_open_sessions: relay_cfg.max_open_sessions,
        max_fragments_per_session: relay_cfg.max_fragments_per_session,
        max_bytes_per_session: relay_cfg.max_bytes_per_session,
        ..RelayPolicy::default()
    };
    policy.stale_session_ttl_ms = if policy.relay_window_ms == 0 {
        policy.enabled = false;
        0
    } else {
        policy
            .relay_window_ms
            .saturating_mul(4)
            .max(policy.relay_window_ms)
    };
    policy.validate().map_err(|err| anyhow::anyhow!(err))?;
    Ok(policy)
}

pub(crate) fn extract_runtime_dst_node(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|cfg| cfg.get("io"))
        .and_then(|io| io.get("dst_node"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(crate) fn extract_runtime_relay_config(
    effective_config: Option<&Value>,
    defaults: &ApiRelayConfig,
) -> Result<ApiRelayConfig> {
    let Some(relay) = effective_config
        .and_then(|cfg| cfg.get("io"))
        .and_then(|io| io.get("relay"))
        .and_then(Value::as_object)
    else {
        return Ok(defaults.clone());
    };
    Ok(ApiRelayConfig {
        window_ms: relay
            .get("window_ms")
            .and_then(Value::as_u64)
            .unwrap_or(defaults.window_ms),
        max_open_sessions: relay
            .get("max_open_sessions")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_open_sessions),
        max_fragments_per_session: relay
            .get("max_fragments_per_session")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_fragments_per_session),
        max_bytes_per_session: relay
            .get("max_bytes_per_session")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(defaults.max_bytes_per_session),
    })
}

pub(crate) fn load_spawn_config(node_name: &str) -> Result<SpawnConfig> {
    let path = managed_node_config_path(node_name)
        .map_err(|err| anyhow::anyhow!("failed to resolve managed config path: {err}"))?;
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        anyhow::anyhow!(
            "failed to read managed config file {}: {err}",
            path.display()
        )
    })?;
    let doc = serde_json::from_str::<Value>(&raw).map_err(|err| {
        anyhow::anyhow!(
            "failed to parse managed config JSON {}: {err}",
            path.display()
        )
    })?;
    Ok(SpawnConfig { path, doc })
}

fn hive_from_node_name(node_name: &str) -> Option<String> {
    node_name
        .split_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_u64(key: &str) -> Option<u64> {
    env(key).and_then(|value| value.parse().ok())
}

fn json_get_string(root: &Value, dotted_path: &str) -> Option<String> {
    json_get_path(root, dotted_path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn json_get_u64(root: &Value, dotted_path: &str) -> Option<u64> {
    json_get_path(root, dotted_path).and_then(|value| match value {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    })
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_defaults_to_passthrough() {
        let defaults = ApiRelayConfig::default();
        let extracted = extract_runtime_relay_config(Some(&serde_json::json!({})), &defaults)
            .expect("relay config");
        assert_eq!(extracted, defaults);
        assert!(
            !api_relay_policy_from_config(&extracted)
                .expect("relay policy")
                .enabled
        );
    }

    #[test]
    fn extracts_configured_destination_and_relay() {
        let effective = serde_json::json!({
            "io": {
                "dst_node":"AI.orders@worker",
                "relay": {"window_ms":250, "max_open_sessions":20}
            }
        });
        assert_eq!(
            extract_runtime_dst_node(Some(&effective)).as_deref(),
            Some("AI.orders@worker")
        );
        let relay = extract_runtime_relay_config(Some(&effective), &ApiRelayConfig::default())
            .expect("relay");
        assert_eq!(relay.window_ms, 250);
        assert_eq!(relay.max_open_sessions, 20);
    }
}
