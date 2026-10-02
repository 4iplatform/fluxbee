pub mod frontdesk_contract;

use std::path::PathBuf;
use std::time::Duration;

use fluxbee_sdk::{managed_node_name, IdentityError, NodeConfig, NodeUuidMode};
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

/// The code a failed identity call is reported under: SY.identity's OWN `error_code`, verbatim,
/// when it answered (TENANT_PENDING, ILK_DELETED, DUPLICATE_EMAIL, ...); a transport code when it
/// did not answer.
pub fn identity_error_code(err: &IdentityError) -> String {
    match err {
        IdentityError::SystemRejected { error_code, .. }
        | IdentityError::ProvisionRejected { error_code, .. } => error_code.clone(),
        IdentityError::Unreachable { .. } => "UNREACHABLE".to_string(),
        IdentityError::TtlExceeded { .. } => "TTL_EXCEEDED".to_string(),
        IdentityError::Timeout { .. } | IdentityError::ActionTimeout { .. } => {
            "TIMEOUT".to_string()
        }
        IdentityError::InvalidRequest(_)
        | IdentityError::InvalidResponse(_)
        | IdentityError::Node(_)
        | IdentityError::Json(_) => "IDENTITY_ERROR".to_string(),
    }
}

/// Whether retrying the same identity request may succeed: SY.identity did not answer, or answered
/// with a transient condition (not the primary, DB not ready, DB write failed). Every other code it
/// answers with — TENANT_PENDING, TENANT_DELETED, ILK_DELETED, ILK_NOT_FOUND, SYSTEM_ILK_PROTECTED,
/// DUPLICATE_*, INVALID_*, UNAUTHORIZED_REGISTRAR, ... — is a final verdict on the request.
pub fn identity_error_is_transient(error_code: &str) -> bool {
    matches!(
        error_code,
        "UNREACHABLE"
            | "TTL_EXCEEDED"
            | "TIMEOUT"
            | "IDENTITY_ERROR"
            | "NOT_PRIMARY"
            | "DB_NOT_READY"
            | "DB_WRITE_FAILED"
    )
}

pub fn identity_error_to_tool_payload(err: &IdentityError) -> Value {
    let error_code = identity_error_code(err);
    json!({
        "status": "error",
        "retryable": identity_error_is_transient(&error_code),
        "error_code": error_code,
        "message": err.to_string()
    })
}

/// Log-safe rendering of a failed identity call. The free-text `message` SY.identity answers with
/// is never rendered: it can echo the submitted identification (a unique-violation DETAIL carries
/// the email). Transport errors carry only routing data and are rendered whole.
pub fn identity_error_log_summary(err: &IdentityError) -> String {
    match err {
        IdentityError::SystemRejected {
            action, error_code, ..
        } => format!("identity rejected {action}: error_code={error_code}"),
        IdentityError::ProvisionRejected { error_code, .. } => {
            format!("identity rejected ILK_PROVISION: error_code={error_code}")
        }
        other => other.to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rejected(error_code: &str, message: &str) -> IdentityError {
        IdentityError::SystemRejected {
            action: "ILK_REGISTER".to_string(),
            error_code: error_code.to_string(),
            message: message.to_string(),
        }
    }

    #[test]
    fn final_identity_rejections_keep_their_code_and_are_not_retryable() {
        for code in [
            "TENANT_PENDING",
            "TENANT_DELETED",
            "ILK_DELETED",
            "ILK_NOT_FOUND",
            "SYSTEM_ILK_PROTECTED",
            "DUPLICATE_EMAIL",
            "INVALID_TENANT",
            "INVALID_TENANT_TRANSITION",
            "UNAUTHORIZED_REGISTRAR",
            "UNKNOWN",
        ] {
            let payload = identity_error_to_tool_payload(&rejected(code, "failed to register ilk"));
            assert_eq!(payload["status"], "error");
            assert_eq!(payload["error_code"], code);
            assert_eq!(payload["retryable"], false, "{code} must be final");
        }
    }

    #[test]
    fn transient_identity_failures_are_retryable() {
        for code in ["NOT_PRIMARY", "DB_NOT_READY", "DB_WRITE_FAILED"] {
            let payload = identity_error_to_tool_payload(&rejected(code, "try again"));
            assert_eq!(payload["error_code"], code);
            assert_eq!(payload["retryable"], true, "{code} is transient");
        }
        let transport = [
            (
                IdentityError::Unreachable {
                    reason: "NODE_NOT_FOUND".to_string(),
                    original_dst: "SY.identity@motherbee".to_string(),
                },
                "UNREACHABLE",
            ),
            (
                IdentityError::TtlExceeded {
                    original_dst: "SY.identity@motherbee".to_string(),
                    last_hop: "RT.gateway@motherbee".to_string(),
                },
                "TTL_EXCEEDED",
            ),
            (
                IdentityError::ActionTimeout {
                    action: "ILK_REGISTER".to_string(),
                    trace_id: "trace-1".to_string(),
                    target: "SY.identity@motherbee".to_string(),
                    timeout_ms: 10_000,
                },
                "TIMEOUT",
            ),
            (
                IdentityError::InvalidResponse("unexpected response shape".to_string()),
                "IDENTITY_ERROR",
            ),
        ];
        for (err, code) in transport {
            let payload = identity_error_to_tool_payload(&err);
            assert_eq!(payload["error_code"], code);
            assert_eq!(payload["retryable"], true, "{code} is transient");
        }
    }

    #[test]
    fn identity_error_log_summary_omits_the_identity_message() {
        let err = rejected(
            "DUPLICATE_EMAIL",
            "failed to persist registered ilk: DETAIL: Key (email, tenant_id)=(juana@example.com, tnt:x)",
        );
        let summary = identity_error_log_summary(&err);
        assert_eq!(
            summary,
            "identity rejected ILK_REGISTER: error_code=DUPLICATE_EMAIL"
        );
        assert!(!summary.contains("juana@example.com"));
    }
}
