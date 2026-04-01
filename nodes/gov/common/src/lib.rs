use std::path::PathBuf;
use std::time::Duration;

use fluxbee_sdk::{managed_node_name, NodeConfig, NodeUuidMode};
use serde_json::{json, Value};

pub fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

pub fn env_opt(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

pub fn build_node_config(default_name: &str, default_version: &str) -> NodeConfig {
    let name = managed_node_name(default_name, &["GOV_NODE_NAME"]);
    let version = env_or("GOV_NODE_VERSION", default_version);
    let router_socket = PathBuf::from(env_or("GOV_ROUTER_SOCKET_DIR", "/var/run/fluxbee/routers"));
    let uuid_persistence_dir = PathBuf::from(env_or(
        "GOV_UUID_PERSISTENCE_DIR",
        "/var/lib/fluxbee/state/nodes",
    ));
    let config_dir = PathBuf::from(env_or("GOV_CONFIG_DIR", "/etc/fluxbee"));

    NodeConfig {
        name,
        router_socket,
        uuid_persistence_dir,
        uuid_mode: NodeUuidMode::Persistent,
        config_dir,
        version,
    }
}

#[derive(Debug, Clone)]
pub struct GovIdentityConfig {
    pub target: String,
    pub fallback_target: Option<String>,
    pub timeout: Duration,
}

impl Default for GovIdentityConfig {
    fn default() -> Self {
        Self {
            target: "SY.identity@motherbee".to_string(),
            fallback_target: Some("SY.identity@motherbee".to_string()),
            timeout: Duration::from_secs(10),
        }
    }
}

pub fn gov_identity_config_from_env() -> GovIdentityConfig {
    let mut cfg = GovIdentityConfig::default();
    if let Some(target) = env_opt("GOV_IDENTITY_TARGET").or_else(|| env_opt("IDENTITY_TARGET")) {
        cfg.target = target;
    }
    if let Some(fallback) =
        env_opt("GOV_IDENTITY_FALLBACK_TARGET").or_else(|| env_opt("IDENTITY_FALLBACK_TARGET"))
    {
        cfg.fallback_target = Some(fallback);
    }
    if let Some(timeout_ms) = env_opt("GOV_IDENTITY_TIMEOUT_MS")
        .or_else(|| env_opt("IDENTITY_TIMEOUT_MS"))
        .and_then(|value| value.parse::<u64>().ok())
    {
        cfg.timeout = Duration::from_millis(timeout_ms);
    }
    cfg
}

pub fn identity_error_to_tool_payload(msg: String) -> Value {
    let upper = msg.to_ascii_uppercase();
    let (error_code, retryable) = if upper.contains("NOT_PRIMARY") {
        ("NOT_PRIMARY", true)
    } else if upper.contains("UNREACHABLE") {
        ("UNREACHABLE", true)
    } else if upper.contains("TTL EXCEEDED") || upper.contains("TTL_EXCEEDED") {
        ("TTL_EXCEEDED", true)
    } else if upper.contains("TIMEOUT") {
        ("TIMEOUT", true)
    } else if upper.contains("INVALID_") {
        ("INVALID_REQUEST", false)
    } else if upper.contains("UNAUTHORIZED_REGISTRAR") {
        ("UNAUTHORIZED_REGISTRAR", false)
    } else {
        ("IDENTITY_ERROR", true)
    };

    json!({
        "status": "error",
        "error_code": error_code,
        "message": msg,
        "retryable": retryable
    })
}

pub const GOV_IDENTITY_TENANT_ID_ENV: &str = "GOV_IDENTITY_TENANT_ID";

pub fn looks_like_tenant_id(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("tnt:") else {
        return false;
    };
    uuid::Uuid::parse_str(rest.trim()).is_ok()
}

pub fn resolve_tenant_id_for_register(
    explicit_tenant_id: Option<&str>,
    tenant_hint: Option<&str>,
    default_tenant_id: Option<&str>,
) -> Option<String> {
    let explicit = explicit_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| looks_like_tenant_id(v))
        .map(ToString::to_string);
    if explicit.is_some() {
        return explicit;
    }

    let hint = tenant_hint
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| looks_like_tenant_id(v))
        .map(ToString::to_string);
    if hint.is_some() {
        return hint;
    }

    let cfg_default = default_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .filter(|v| looks_like_tenant_id(v))
        .map(ToString::to_string);
    if cfg_default.is_some() {
        return cfg_default;
    }

    env_opt(GOV_IDENTITY_TENANT_ID_ENV).filter(|v| looks_like_tenant_id(v))
}

pub fn tenant_resolution_source(
    explicit_tenant_id: Option<&str>,
    tenant_hint: Option<&str>,
    default_tenant_id: Option<&str>,
) -> &'static str {
    let explicit_ok = explicit_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(looks_like_tenant_id);
    if explicit_ok {
        return "args.tenant_id";
    }

    let hint_ok = tenant_hint
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(looks_like_tenant_id);
    if hint_ok {
        return "identity_candidate.tenant_hint";
    }

    let cfg_ok = default_tenant_id
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(looks_like_tenant_id);
    if cfg_ok {
        return "effective_config.tenant_id";
    }

    let env_ok = env_opt(GOV_IDENTITY_TENANT_ID_ENV)
        .as_deref()
        .is_some_and(looks_like_tenant_id);
    if env_ok {
        return GOV_IDENTITY_TENANT_ID_ENV;
    }

    "missing"
}
