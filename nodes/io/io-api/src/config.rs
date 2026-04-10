use anyhow::Result;
use fluxbee_sdk::{managed_node_config_path, FLUXBEE_NODE_NAME_ENV};
use io_common::io_control_plane_store::default_state_dir;
use io_common::relay::RelayPolicy;
use io_common::router_message::DEFAULT_TTL;
use io_common::text_v1_blob::IoBlobRuntimeConfig;
use serde_json::Value;
use std::path::PathBuf;

use crate::{ApiRelayConfig, Config, SpawnConfig};

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        let resolved_node_name = env(FLUXBEE_NODE_NAME_ENV).ok_or_else(|| {
            anyhow::anyhow!("missing required env {FLUXBEE_NODE_NAME_ENV} for managed spawn")
        })?;
        let resolved_island_id = hive_from_node_name(&resolved_node_name).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid {FLUXBEE_NODE_NAME_ENV}='{resolved_node_name}': expected <name>@<hive>"
            )
        })?;
        let spawn_cfg = load_spawn_config(&resolved_node_name)?;
        tracing::info!(path = %spawn_cfg.path.display(), "io-api loaded spawn config");
        let spawn_doc = Some(&spawn_cfg.doc);

        let listen_address = env("IO_API_LISTEN_ADDRESS")
            .or_else(|| {
                json_get_string_opt(
                    spawn_doc,
                    &[
                        "config.listen.address",
                        "listen.address",
                        "node.listen.address",
                    ],
                )
            })
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let listen_port = env("IO_API_LISTEN_PORT")
            .and_then(|value| value.parse::<u16>().ok())
            .or_else(|| {
                json_get_u16_opt(
                    spawn_doc,
                    &["config.listen.port", "listen.port", "node.listen.port"],
                )
            })
            .unwrap_or(8080);

        Ok(Self {
            node_name: resolved_node_name.clone(),
            island_id: resolved_island_id.clone(),
            node_version: env("NODE_VERSION")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_doc,
                        &["_system.runtime_version", "runtime.version", "node.version"],
                    )
                })
                .unwrap_or_else(|| "0.1".to_string()),
            listen_addr: format!("{listen_address}:{listen_port}"),
            router_socket: PathBuf::from(
                env("ROUTER_SOCKET")
                    .or_else(|| {
                        json_get_string_opt(spawn_doc, &["node.router_socket", "router_socket"])
                    })
                    .unwrap_or_else(|| "/var/run/fluxbee/routers".to_string()),
            ),
            uuid_persistence_dir: PathBuf::from(
                env("UUID_PERSISTENCE_DIR")
                    .or_else(|| {
                        json_get_string_opt(
                            spawn_doc,
                            &["node.uuid_persistence_dir", "uuid_persistence_dir"],
                        )
                    })
                    .unwrap_or_else(|| "/var/lib/fluxbee/state/nodes".to_string()),
            ),
            config_dir: PathBuf::from(
                env("CONFIG_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.config_dir", "config_dir"]))
                    .unwrap_or_else(|| "/etc/fluxbee".to_string()),
            ),
            state_dir: PathBuf::from(
                env("STATE_DIR")
                    .or_else(|| json_get_string_opt(spawn_doc, &["node.state_dir", "state_dir"]))
                    .unwrap_or_else(|| default_state_dir().display().to_string()),
            ),
            spawn_config_path: spawn_cfg.path,
            identity_target: env("IDENTITY_TARGET")
                .or_else(|| {
                    json_get_string_opt(
                        spawn_doc,
                        &[
                            "config.identity.target",
                            "identity.target",
                            "identity_target",
                        ],
                    )
                })
                .unwrap_or_else(|| format!("SY.identity@{resolved_island_id}")),
            identity_timeout_ms: env("IDENTITY_TIMEOUT_MS")
                .and_then(|value| value.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(
                        spawn_doc,
                        &[
                            "config.identity.timeout_ms",
                            "identity.timeout_ms",
                            "identity_timeout_ms",
                        ],
                    )
                })
                .unwrap_or(10_000),
            ttl: env("TTL")
                .and_then(|value| value.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(spawn_doc, &["node.ttl", "ttl"])
                        .and_then(|v| u32::try_from(v).ok())
                })
                .unwrap_or(DEFAULT_TTL),
            dedup_ttl_ms: env("DEDUP_TTL_MS")
                .and_then(|value| value.parse().ok())
                .or_else(|| {
                    json_get_u64_opt(
                        spawn_doc,
                        &[
                            "config.runtime.dedup_ttl_ms",
                            "runtime.dedup_ttl_ms",
                            "dedup_ttl_ms",
                        ],
                    )
                })
                .unwrap_or(600_000),
            dedup_max_entries: env("DEDUP_MAX_ENTRIES")
                .and_then(|value| value.parse().ok())
                .or_else(|| {
                    json_get_usize_opt(
                        spawn_doc,
                        &[
                            "config.runtime.dedup_max_entries",
                            "runtime.dedup_max_entries",
                            "dedup_max_entries",
                        ],
                    )
                })
                .unwrap_or(50_000),
            relay: ApiRelayConfig {
                window_ms: env("IO_API_RELAY_WINDOW_MS")
                    .and_then(|value| value.parse().ok())
                    .or_else(|| {
                        json_get_u64_opt(
                            spawn_doc,
                            &[
                                "config.io.relay.window_ms",
                                "io.relay.window_ms",
                                "relay.window_ms",
                            ],
                        )
                    })
                    .unwrap_or(0),
                max_open_sessions: env("IO_API_RELAY_MAX_OPEN_SESSIONS")
                    .and_then(|value| value.parse().ok())
                    .or_else(|| {
                        json_get_usize_opt(
                            spawn_doc,
                            &[
                                "config.io.relay.max_open_sessions",
                                "io.relay.max_open_sessions",
                                "relay.max_open_sessions",
                            ],
                        )
                    })
                    .unwrap_or(10_000),
                max_fragments_per_session: env("IO_API_RELAY_MAX_FRAGMENTS")
                    .and_then(|value| value.parse().ok())
                    .or_else(|| {
                        json_get_usize_opt(
                            spawn_doc,
                            &[
                                "config.io.relay.max_fragments_per_session",
                                "io.relay.max_fragments_per_session",
                                "relay.max_fragments_per_session",
                            ],
                        )
                    })
                    .unwrap_or(8),
                max_bytes_per_session: env("IO_API_RELAY_MAX_BYTES")
                    .and_then(|value| value.parse().ok())
                    .or_else(|| {
                        json_get_usize_opt(
                            spawn_doc,
                            &[
                                "config.io.relay.max_bytes_per_session",
                                "io.relay.max_bytes_per_session",
                                "relay.max_bytes_per_session",
                            ],
                        )
                    })
                    .unwrap_or(256 * 1024),
            },
            blob_runtime: IoBlobRuntimeConfig {
                blob_root: PathBuf::from(
                    env("BLOB_ROOT")
                        .or_else(|| json_get_string_opt(spawn_doc, &["blob.path", "io.blob.path"]))
                        .unwrap_or_else(|| "/var/lib/fluxbee/blob".to_string()),
                ),
                max_blob_bytes: env("BLOB_MAX_BYTES")
                    .and_then(|value| value.parse().ok())
                    .or_else(|| {
                        json_get_u64_opt(
                            spawn_doc,
                            &["blob.max_blob_bytes", "io.blob.max_blob_bytes"],
                        )
                    })
                    .unwrap_or(IoBlobRuntimeConfig::default().max_blob_bytes),
                text_v1: IoBlobRuntimeConfig::default().text_v1,
                resolve_retry: IoBlobRuntimeConfig::default().resolve_retry,
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
    if policy.relay_window_ms == 0 {
        policy.enabled = false;
        policy.stale_session_ttl_ms = 0;
    } else {
        policy.stale_session_ttl_ms = policy
            .relay_window_ms
            .saturating_mul(4)
            .max(policy.relay_window_ms);
    }
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

fn extract_runtime_relay_config(
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

fn hive_from_node_name(node_name: &str) -> Option<String> {
    node_name
        .split_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|value| !value.is_empty())
}

fn json_get_string_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<String> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(value) = json_get_path(doc, path).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn json_get_u64_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<u64> {
    let doc = doc?;
    for path in dotted_paths {
        if let Some(value) = json_get_path(doc, path) {
            if let Some(number) = value.as_u64() {
                return Some(number);
            }
            if let Some(text) = value.as_str() {
                if let Ok(number) = text.parse::<u64>() {
                    return Some(number);
                }
            }
        }
    }
    None
}

fn json_get_u16_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<u16> {
    json_get_u64_opt(doc, dotted_paths).and_then(|value| u16::try_from(value).ok())
}

fn json_get_usize_opt(doc: Option<&Value>, dotted_paths: &[&str]) -> Option<usize> {
    json_get_u64_opt(doc, dotted_paths).and_then(|value| usize::try_from(value).ok())
}

fn json_get_path<'a>(root: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = root;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}
