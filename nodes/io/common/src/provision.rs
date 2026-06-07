#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_sdk::protocol::{
    Destination, Message as WireMessage, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE,
    SYSTEM_KIND,
};
use fluxbee_sdk::{
    stable_ich_id, PendingMatcher, RouteMatch, RouterDispatcher, RpcError, RpcRequestLabels,
    MSG_ILK_ADD_CHANNEL, MSG_ILK_PROVISION, MSG_ILK_PROVISION_RESPONSE,
};

use crate::identity::{IdentityError, IdentityProvisioner, ResolveOrCreateInput};

#[derive(Debug, Clone)]
pub struct IdentityProvisionConfig {
    pub target: String,
    pub timeout: Duration,
}

impl Default for IdentityProvisionConfig {
    fn default() -> Self {
        Self {
            target: "SY.identity@motherbee".to_string(),
            timeout: Duration::from_secs(10),
        }
    }
}

/// Identity provisioner backed by the canonical `RouterDispatcher`. Replaces
/// the previous `RouterInbox`-based plumbing.
pub struct FluxbeeIdentityProvisioner {
    dispatcher: Arc<RouterDispatcher>,
    config: IdentityProvisionConfig,
}

impl FluxbeeIdentityProvisioner {
    pub fn new(dispatcher: Arc<RouterDispatcher>, config: IdentityProvisionConfig) -> Self {
        Self { dispatcher, config }
    }

    async fn call_provision_target(
        &self,
        target: &str,
        input: &ResolveOrCreateInput,
    ) -> Result<String, IdentityError> {
        strict_provision_ilk(&self.dispatcher, &self.config, target, input).await
    }
}

pub async fn strict_provision_ilk(
    dispatcher: &Arc<RouterDispatcher>,
    config: &IdentityProvisionConfig,
    target: &str,
    input: &ResolveOrCreateInput,
) -> Result<String, IdentityError> {
    let normalized_channel = normalize_identity_field(&input.channel, true);
    let normalized_address = normalize_identity_field(&input.external_id, true);
    let ich_id = stable_ich_id(
        &normalized_channel,
        &normalized_address,
        input.tenant_id.as_deref().unwrap_or(""),
    )
    .map_err(|err| IdentityError::Other(format!("invalid provision ICH seed: {err}")))?;
    let mut payload = serde_json::json!({
        "ich_id": ich_id,
        "channel_type": normalized_channel,
        "address": normalized_address,
    });
    if let Some(tenant_id) = input
        .tenant_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        payload["tenant_id"] = serde_json::json!(tenant_id);
    }
    if let Some(ilk_type) = input
        .ilk_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        payload["ilk_type"] = serde_json::json!(ilk_type);
    }
    let sender = dispatcher.sender_snapshot();
    let req = WireMessage {
        routing: Routing {
            src: String::new(),
            src_l2_name: Some(sender.full_name().to_string()),
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: String::new(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_ILK_PROVISION.to_string()),
            ..Meta::default()
        },
        payload,
    };
    let matcher = system_request_matcher(MSG_ILK_PROVISION_RESPONSE);
    let labels = RpcRequestLabels::new(target, MSG_ILK_PROVISION, MSG_ILK_PROVISION_RESPONSE);
    let msg = dispatcher
        .send_with_matcher(req, matcher, labels, config.timeout)
        .await
        .map_err(map_rpc_err)?;
    tracing::debug!(
        target = %target,
        channel = %normalized_channel,
        address = %normalized_address,
        response_msg = %msg.meta.msg.as_deref().unwrap_or(""),
        "identity provision response matched"
    );
    parse_provision_response(msg)
}

#[derive(Debug, Clone)]
pub struct EnsureOwnIchResult {
    pub ilk_id: String,
    pub ich_id: String,
    pub owner_l2_name: Option<String>,
    pub enabled: bool,
}

pub async fn ensure_own_ich(
    dispatcher: &Arc<RouterDispatcher>,
    config: &IdentityProvisionConfig,
    target: &str,
    self_ilk_id: &str,
    self_tenant_id: &str,
    channel_type: &str,
    address: &str,
) -> Result<EnsureOwnIchResult, IdentityError> {
    let normalized_channel = normalize_identity_field(channel_type, true);
    let normalized_address = normalize_identity_field(address, true);
    let normalized_self_ilk_id = self_ilk_id.trim();
    let normalized_self_tenant_id = self_tenant_id.trim();
    if normalized_self_ilk_id.is_empty()
        || normalized_self_tenant_id.is_empty()
        || normalized_channel.is_empty()
        || normalized_address.is_empty()
    {
        return Err(IdentityError::Other(
            "self_ilk_id, self_tenant_id, channel_type and address must be non-empty".to_string(),
        ));
    }
    let ich_id = stable_ich_id(
        &normalized_channel,
        &normalized_address,
        normalized_self_tenant_id,
    )
    .map_err(|err| IdentityError::Other(format!("invalid own ICH seed: {err}")))?;
    let sender = dispatcher.sender_snapshot();
    let req = WireMessage {
        routing: Routing {
            src: String::new(),
            src_l2_name: Some(sender.full_name().to_string()),
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: String::new(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(MSG_ILK_ADD_CHANNEL.to_string()),
            ..Meta::default()
        },
        payload: serde_json::json!({
            "ilk_id": normalized_self_ilk_id,
            "channel": {
                "ich_id": ich_id,
                "type": normalized_channel,
                "address": normalized_address,
            }
        }),
    };
    let matcher = system_request_matcher("ILK_ADD_CHANNEL_RESPONSE");
    let labels = RpcRequestLabels::new(target, MSG_ILK_ADD_CHANNEL, "ILK_ADD_CHANNEL_RESPONSE");
    let msg = dispatcher
        .send_with_matcher(req, matcher, labels, config.timeout)
        .await
        .map_err(map_rpc_err)?;
    if msg.meta.msg.as_deref() != Some("ILK_ADD_CHANNEL_RESPONSE") {
        if msg.meta.msg.as_deref() == Some(MSG_UNREACHABLE)
            || msg.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED)
        {
            return Err(IdentityError::Unavailable);
        }
        return Err(IdentityError::Other(
            "invalid ILK_ADD_CHANNEL response".to_string(),
        ));
    }
    let status = msg
        .payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !status.eq_ignore_ascii_case("ok") {
        let code = msg
            .payload
            .get("error_code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        return Err(IdentityError::Other(format!(
            "ILK_ADD_CHANNEL rejected: {code}"
        )));
    }
    let ilk_id = msg
        .payload
        .get("ilk_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| IdentityError::Other("ILK_ADD_CHANNEL response missing ilk_id".to_string()))?
        .to_string();
    let owner_l2_name = msg
        .payload
        .get("owner_l2_name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    let enabled = msg
        .payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(EnsureOwnIchResult {
        ilk_id,
        ich_id,
        owner_l2_name,
        enabled,
    })
}

#[async_trait]
impl IdentityProvisioner for FluxbeeIdentityProvisioner {
    async fn provision(
        &self,
        input: &ResolveOrCreateInput,
    ) -> Result<Option<String>, IdentityError> {
        match self.call_provision_target(&self.config.target, input).await {
            Ok(src_ilk) => Ok(Some(src_ilk)),
            Err(IdentityError::Unavailable) | Err(IdentityError::Other(_)) => {
                tracing::warn!(
                    target = %self.config.target,
                    channel = %input.channel,
                    external_id = %input.external_id,
                    "identity provision unavailable; falling back to null src_ilk"
                );
                Ok(None)
            }
            Err(IdentityError::Timeout) | Err(IdentityError::Miss) => {
                tracing::warn!(
                    target = %self.config.target,
                    channel = %input.channel,
                    external_id = %input.external_id,
                    timeout_ms = self.config.timeout.as_millis() as u64,
                    "identity provision timeout/miss; falling back to null src_ilk"
                );
                Ok(None)
            }
        }
    }
}

fn system_request_matcher(response_msg: &str) -> PendingMatcher {
    PendingMatcher::new(
        vec![RouteMatch::exact(SYSTEM_KIND, response_msg)],
        vec![
            RouteMatch::exact(SYSTEM_KIND, MSG_UNREACHABLE),
            RouteMatch::exact(SYSTEM_KIND, MSG_TTL_EXCEEDED),
        ],
        vec![RouteMatch::any_msg_type(SYSTEM_KIND)],
    )
}

fn map_rpc_err(err: RpcError) -> IdentityError {
    match err {
        RpcError::Timeout { .. } => IdentityError::Timeout,
        RpcError::Disconnected | RpcError::Node(_) => IdentityError::Unavailable,
        other => IdentityError::Other(other.to_string()),
    }
}

fn parse_provision_response(msg: WireMessage) -> Result<String, IdentityError> {
    if msg.meta.msg.as_deref() == Some(MSG_ILK_PROVISION_RESPONSE) {
        let status = msg
            .payload
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if status.eq_ignore_ascii_case("ok") {
            if let Some(ilk_id) = msg.payload.get("ilk_id").and_then(|v| v.as_str()) {
                if !ilk_id.trim().is_empty() {
                    return Ok(ilk_id.to_string());
                }
            }
            return Err(IdentityError::Other(
                "provision response missing ilk_id".to_string(),
            ));
        }
        let code = msg
            .payload
            .get("error_code")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        return Err(IdentityError::Other(format!("provision rejected: {code}")));
    }
    if msg.meta.msg.as_deref() == Some(MSG_UNREACHABLE)
        || msg.meta.msg.as_deref() == Some(MSG_TTL_EXCEEDED)
    {
        return Err(IdentityError::Unavailable);
    }
    Err(IdentityError::Other(
        "invalid provision response".to_string(),
    ))
}

fn normalize_identity_field(value: &str, lowercase: bool) -> String {
    let trimmed = value.trim();
    if lowercase {
        trimmed.to_ascii_lowercase()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::stable_ich_id;

    #[test]
    fn stable_ich_id_is_deterministic() {
        let a = stable_ich_id("sim-new", "user.provision.abc1", "tnt:tenant-a")
            .expect("stable ich id");
        let b = stable_ich_id("sim-new", "user.provision.abc1", "tnt:tenant-a")
            .expect("stable ich id");
        assert_eq!(a, b);
    }

    #[test]
    fn stable_ich_id_changes_when_input_changes() {
        let a = stable_ich_id("sim-new", "user.provision.abc1", "tnt:tenant-a")
            .expect("stable ich id");
        let b = stable_ich_id("sim-new", "user.provision.abc2", "tnt:tenant-a")
            .expect("stable ich id");
        assert_ne!(a, b);
    }

    #[test]
    fn stable_ich_id_changes_when_tenant_changes() {
        let a = stable_ich_id("sim-new", "user.provision.abc1", "tnt:tenant-a")
            .expect("stable ich id");
        let b = stable_ich_id("sim-new", "user.provision.abc1", "tnt:tenant-b")
            .expect("stable ich id");
        assert_ne!(a, b);
    }
}
