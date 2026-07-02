#![forbid(unsafe_code)]

//! Adapter authentication/authorization for the intermediate direct-HTTP path.
//!
//! This is the `AdapterAuthValidator` layer described in the linkedhelper auth
//! contract (`contrato_auth_vault_io_linkedhelper_v1.md`). It validates that an
//! inbound `/v1/poll` request carries a recognized adapter identity + secret and
//! targets this node's managed instance binding.
//!
//! It is deliberately isolated from the HTTP/axum layer (it takes plain
//! `Option<&str>` inputs and returns a transport-agnostic decision) so the same
//! validation can later move to the Edge without dragging node internals along,
//! exactly as the contract mandates ("ubicación intermedia: IO.linkedhelper /
//! ubicación final: Edge LinkedHelper").

/// Transport-agnostic category of an auth rejection. The caller maps each
/// variant to a concrete status (HTTP today, whatever the Edge uses later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatus {
    /// 400 — malformed/inconsistent request (missing or mismatched fields).
    BadRequest,
    /// 401 — caller could not prove the adapter secret.
    Unauthorized,
    /// 403 — caller authenticated but is not bound to this managed instance.
    Forbidden,
    /// 503 — the node cannot currently validate (secret unresolved, etc.).
    Unavailable,
}

/// A single, fully-formed rejection carrying the stable error code + message
/// the poll endpoint already surfaces to adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRejection {
    pub status: AuthStatus,
    pub error_code: String,
    pub error_message: String,
}

impl AuthRejection {
    fn new(status: AuthStatus, error_code: &str, error_message: &str) -> Self {
        Self {
            status,
            error_code: error_code.to_string(),
            error_message: error_message.to_string(),
        }
    }
}

pub type AuthResult = Result<(), AuthRejection>;

/// Immutable authorization facts derived from the node's active binding.
#[derive(Debug, Clone)]
pub struct AdapterAuthValidator {
    adapter_id: String,
    managed_instance_id: String,
    local_instance_id: Option<String>,
    adapter_secret: Option<String>,
}

/// The parts of an inbound request that authentication depends on. All fields
/// are borrowed and un-normalized; the validator trims/normalizes internally.
#[derive(Debug, Default, Clone)]
pub struct InboundAuthRequest<'a> {
    pub header_adapter_id: Option<&'a str>,
    pub bearer: Option<&'a str>,
    pub body_adapter_id: Option<&'a str>,
    pub body_managed_instance_id: Option<&'a str>,
    pub body_local_instance_id: Option<&'a str>,
}

fn norm(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

impl AdapterAuthValidator {
    pub fn new(
        adapter_id: impl Into<String>,
        managed_instance_id: impl Into<String>,
        local_instance_id: Option<String>,
        adapter_secret: Option<String>,
    ) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            managed_instance_id: managed_instance_id.into(),
            local_instance_id: local_instance_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            adapter_secret: adapter_secret
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        }
    }

    /// Validate an inbound request against this binding. Returns `Ok(())` only
    /// when every check in the auth contract (§6) passes; otherwise the first
    /// failing check's rejection. Check order mirrors the contract so error
    /// codes stay stable for adapters.
    pub fn validate(&self, req: &InboundAuthRequest<'_>) -> AuthResult {
        // 1. adapter id header present (§6.1)
        let header_adapter_id = norm(req.header_adapter_id).ok_or_else(|| {
            AuthRejection::new(
                AuthStatus::BadRequest,
                "missing_adapter_id",
                "X-Fluxbee-Adapter-Id header is required",
            )
        })?;

        // 2. Authorization: Bearer present (§6.2/§6.3)
        let bearer = norm(req.bearer).ok_or_else(|| {
            AuthRejection::new(
                AuthStatus::Unauthorized,
                "missing_bearer_token",
                "Authorization: Bearer <adapter_secret> is required",
            )
        })?;

        // 3. header adapter id is the one bound to this node (§6.4)
        if header_adapter_id != self.adapter_id {
            return Err(AuthRejection::new(
                AuthStatus::Forbidden,
                "adapter_not_allowed",
                "adapter_id is not authorized for this managed instance",
            ));
        }

        // 4. this node actually resolved its secret from Vault (§5 error states)
        let expected_secret = self.adapter_secret.as_deref().ok_or_else(|| {
            AuthRejection::new(
                AuthStatus::Unavailable,
                "auth_secret_unavailable",
                "linkedhelper node could not resolve its adapter secret",
            )
        })?;

        // 5. bearer matches the resolved secret (§6.5)
        if expected_secret != bearer {
            return Err(AuthRejection::new(
                AuthStatus::Unauthorized,
                "invalid_adapter_secret",
                "invalid adapter secret",
            ));
        }

        // 6. body adapter_id must agree with the header (defensive; §7 rules)
        if norm(req.body_adapter_id) != Some(header_adapter_id) {
            return Err(AuthRejection::new(
                AuthStatus::BadRequest,
                "adapter_id_mismatch",
                "adapter_id body/header mismatch",
            ));
        }

        // 7. payload managed_instance_id must match this node (§6.6)
        if norm(req.body_managed_instance_id) != Some(self.managed_instance_id.as_str()) {
            return Err(AuthRejection::new(
                AuthStatus::Forbidden,
                "managed_instance_id_mismatch",
                "managed_instance_id does not match this node binding",
            ));
        }

        // 8. when the binding pins a local_instance_id, the payload must match (§6.7)
        if let Some(expected_local) = self.local_instance_id.as_deref() {
            if norm(req.body_local_instance_id) != Some(expected_local) {
                return Err(AuthRejection::new(
                    AuthStatus::Forbidden,
                    "local_instance_id_mismatch",
                    "local_instance_id does not match this node binding",
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validator() -> AdapterAuthValidator {
        AdapterAuthValidator::new(
            "adp_123",
            "lhmi_001",
            Some("123456".to_string()),
            Some("s3cret".to_string()),
        )
    }

    fn good_request<'a>() -> InboundAuthRequest<'a> {
        InboundAuthRequest {
            header_adapter_id: Some("adp_123"),
            bearer: Some("s3cret"),
            body_adapter_id: Some("adp_123"),
            body_managed_instance_id: Some("lhmi_001"),
            body_local_instance_id: Some("123456"),
        }
    }

    fn reject(req: &InboundAuthRequest<'_>) -> AuthRejection {
        validator().validate(req).expect_err("expected rejection")
    }

    #[test]
    fn accepts_fully_valid_request() {
        assert!(validator().validate(&good_request()).is_ok());
    }

    #[test]
    fn tolerates_surrounding_whitespace() {
        let mut req = good_request();
        req.header_adapter_id = Some(" adp_123 ");
        req.bearer = Some(" s3cret ");
        assert!(validator().validate(&req).is_ok());
    }

    #[test]
    fn missing_adapter_id_header_is_bad_request() {
        let mut req = good_request();
        req.header_adapter_id = None;
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::BadRequest);
        assert_eq!(rej.error_code, "missing_adapter_id");
    }

    #[test]
    fn missing_bearer_is_unauthorized() {
        let mut req = good_request();
        req.bearer = Some("   ");
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::Unauthorized);
        assert_eq!(rej.error_code, "missing_bearer_token");
    }

    #[test]
    fn wrong_adapter_id_is_forbidden() {
        let mut req = good_request();
        req.header_adapter_id = Some("adp_999");
        req.body_adapter_id = Some("adp_999");
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::Forbidden);
        assert_eq!(rej.error_code, "adapter_not_allowed");
    }

    #[test]
    fn unresolved_secret_is_unavailable() {
        let v = AdapterAuthValidator::new("adp_123", "lhmi_001", None, None);
        let rej = v.validate(&good_request()).expect_err("expected rejection");
        assert_eq!(rej.status, AuthStatus::Unavailable);
        assert_eq!(rej.error_code, "auth_secret_unavailable");
    }

    #[test]
    fn wrong_secret_is_unauthorized() {
        let mut req = good_request();
        req.bearer = Some("nope");
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::Unauthorized);
        assert_eq!(rej.error_code, "invalid_adapter_secret");
    }

    #[test]
    fn body_header_adapter_mismatch_is_bad_request() {
        let mut req = good_request();
        req.body_adapter_id = Some("adp_other");
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::BadRequest);
        assert_eq!(rej.error_code, "adapter_id_mismatch");
    }

    #[test]
    fn wrong_managed_instance_is_forbidden() {
        let mut req = good_request();
        req.body_managed_instance_id = Some("lhmi_999");
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::Forbidden);
        assert_eq!(rej.error_code, "managed_instance_id_mismatch");
    }

    #[test]
    fn wrong_local_instance_is_forbidden() {
        let mut req = good_request();
        req.body_local_instance_id = Some("999999");
        let rej = reject(&req);
        assert_eq!(rej.status, AuthStatus::Forbidden);
        assert_eq!(rej.error_code, "local_instance_id_mismatch");
    }

    #[test]
    fn local_instance_not_pinned_accepts_any() {
        let v = AdapterAuthValidator::new(
            "adp_123",
            "lhmi_001",
            None,
            Some("s3cret".to_string()),
        );
        let mut req = good_request();
        req.body_local_instance_id = None;
        assert!(v.validate(&req).is_ok());
    }
}
