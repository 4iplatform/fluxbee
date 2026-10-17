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
/// answers with — TENANT_PENDING, TENANT_SUSPENDED, TENANT_DELETED, TENANT_ROOT_NOT_REGISTRABLE,
/// ILK_DELETED, ILK_NOT_FOUND, SYSTEM_ILK_PROTECTED, DUPLICATE_*, INVALID_*,
/// UNAUTHORIZED_REGISTRAR, ... — is a final verdict.
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

pub fn looks_like_tenant_id(raw: &str) -> bool {
    let Some(rest) = raw.strip_prefix("tnt:") else {
        return false;
    };
    uuid::Uuid::parse_str(rest.trim()).is_ok()
}

/// The tenant of the case cannot be read: nothing is registered, and no tenant is created.
pub const CASE_TENANT_MISSING: &str = "missing_tenant_id";
/// The tenant the producer informed is not the tenant of the case's ILK: nothing is registered.
pub const CASE_TENANT_MISMATCH: &str = "tenant_mismatch";
/// The case belongs to the root tenant, where nobody registers (operator decision 2026-10-02):
/// SY.identity's own code, which the frontdesk also answers by itself, without calling it.
pub const TENANT_ROOT_NOT_REGISTRABLE: &str = "TENANT_ROOT_NOT_REGISTRABLE";

/// The tenant a registration goes into: the one SY.identity holds for the case's temporary ILK.
/// The IO node that provisioned it gave it its own tenant (io.api and io.cloud, the tenant they
/// were called for). A tenant the producer informed (`frontdesk_handoff.tenant_id`) must be that
/// same one. There is no other source: the frontdesk takes no tenant from the person, the LLM,
/// its config or its environment, and never creates one. The root tenant is never one: a person
/// provisioned there (by a root-tenant IO node) cannot be registered and stays temporary. The
/// error is the tool error code.
pub fn resolve_case_tenant(
    ilk_tenant: Option<&str>,
    informed_tenant: Option<&str>,
) -> Result<String, &'static str> {
    let Some(ilk_tenant) = ilk_tenant
        .map(str::trim)
        .filter(|tenant| looks_like_tenant_id(tenant))
    else {
        return Err(CASE_TENANT_MISSING);
    };
    let informed = informed_tenant
        .map(str::trim)
        .filter(|tenant| !tenant.is_empty());
    if informed.is_some_and(|informed| informed != ilk_tenant) {
        return Err(CASE_TENANT_MISMATCH);
    }
    if ilk_tenant == fluxbee_sdk::DEFAULT_ROOT_TENANT_ID {
        return Err(TENANT_ROOT_NOT_REGISTRABLE);
    }
    Ok(ilk_tenant.to_string())
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
            "TENANT_SUSPENDED",
            "TENANT_DELETED",
            TENANT_ROOT_NOT_REGISTRABLE,
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

    const CASE_TENANT: &str = "tnt:11111111-1111-4111-8111-111111111111";
    const OTHER_TENANT: &str = "tnt:22222222-2222-4222-8222-222222222222";

    #[test]
    fn the_case_tenant_is_the_tenant_of_its_ilk() {
        assert_eq!(
            resolve_case_tenant(Some(CASE_TENANT), None).as_deref(),
            Ok(CASE_TENANT)
        );
        // A producer that informs the same tenant agrees with the ilk.
        assert_eq!(
            resolve_case_tenant(Some(CASE_TENANT), Some(&format!(" {CASE_TENANT} "))).as_deref(),
            Ok(CASE_TENANT)
        );
        // One that informs another is refused, never followed.
        assert_eq!(
            resolve_case_tenant(Some(CASE_TENANT), Some(OTHER_TENANT)),
            Err(CASE_TENANT_MISMATCH)
        );
        assert_eq!(
            resolve_case_tenant(Some(CASE_TENANT), Some("Acme SA")),
            Err(CASE_TENANT_MISMATCH)
        );
    }

    #[test]
    fn without_the_ilk_tenant_there_is_no_tenant() {
        // An informed tenant alone does not make one: it only checks the ilk's.
        assert_eq!(
            resolve_case_tenant(None, Some(CASE_TENANT)),
            Err(CASE_TENANT_MISSING)
        );
        assert_eq!(resolve_case_tenant(None, None), Err(CASE_TENANT_MISSING));
        assert_eq!(
            resolve_case_tenant(Some("acme"), None),
            Err(CASE_TENANT_MISSING)
        );
    }

    #[test]
    fn a_case_of_the_root_tenant_has_no_tenant_to_register_into() {
        let root = fluxbee_sdk::DEFAULT_ROOT_TENANT_ID;
        assert_eq!(
            resolve_case_tenant(Some(root), None),
            Err(TENANT_ROOT_NOT_REGISTRABLE)
        );
        // The producer informing it changes nothing.
        assert_eq!(
            resolve_case_tenant(Some(root), Some(root)),
            Err(TENANT_ROOT_NOT_REGISTRABLE)
        );
        // Neither does one that names another tenant: that is a contradiction first.
        assert_eq!(
            resolve_case_tenant(Some(root), Some(CASE_TENANT)),
            Err(CASE_TENANT_MISMATCH)
        );
        // And the root tenant informed for a case of another is no way into it.
        assert_eq!(
            resolve_case_tenant(Some(CASE_TENANT), Some(root)),
            Err(CASE_TENANT_MISMATCH)
        );
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
