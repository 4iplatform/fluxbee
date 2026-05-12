use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{self, Instant};
use uuid::Uuid;

use crate::protocol::{
    Destination, Message, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE, SYSTEM_KIND,
};
use crate::{NodeError, NodeReceiver, NodeSender};

pub const VAULT_REF_PREFIX: &str = "vault://";

pub const MSG_VAULT_PUT: &str = "VAULT_PUT";
pub const MSG_VAULT_PUT_RESPONSE: &str = "VAULT_PUT_RESPONSE";
pub const MSG_VAULT_GET: &str = "VAULT_GET";
pub const MSG_VAULT_GET_RESPONSE: &str = "VAULT_GET_RESPONSE";
pub const MSG_VAULT_GET_METADATA: &str = "VAULT_GET_METADATA";
pub const MSG_VAULT_GET_METADATA_RESPONSE: &str = "VAULT_GET_METADATA_RESPONSE";
pub const MSG_VAULT_LIST: &str = "VAULT_LIST";
pub const MSG_VAULT_LIST_RESPONSE: &str = "VAULT_LIST_RESPONSE";
pub const MSG_VAULT_DELETE: &str = "VAULT_DELETE";
pub const MSG_VAULT_DELETE_RESPONSE: &str = "VAULT_DELETE_RESPONSE";
pub const MSG_VAULT_ROTATE: &str = "VAULT_ROTATE";
pub const MSG_VAULT_ROTATE_RESPONSE: &str = "VAULT_ROTATE_RESPONSE";
pub const MSG_VAULT_ROLLBACK: &str = "VAULT_ROLLBACK";
pub const MSG_VAULT_ROLLBACK_RESPONSE: &str = "VAULT_ROLLBACK_RESPONSE";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VaultMetadata {
    pub tenant_id: String,
    pub owner_ilk: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPutRequest {
    pub key: String,
    pub value: Value,
    pub metadata: VaultMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultPutResponse {
    pub status: String,
    pub key: String,
    pub version: i64,
    #[serde(default)]
    pub changed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultGetRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultGetMetadataRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultValueResponse {
    pub status: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<VaultMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

pub type VaultGetResponse = VaultValueResponse;
pub type VaultGetMetadataResponse = VaultValueResponse;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VaultListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<VaultFilter>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultSecretSummary {
    pub key: String,
    pub metadata: VaultMetadata,
    pub version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_accessed_at: Option<String>,
    pub access_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultListResponse {
    pub status: String,
    pub count: usize,
    #[serde(default)]
    pub secrets: Vec<VaultSecretSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultKeyRequest {
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRotateRequest {
    pub key: String,
    pub value: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultDeleteResponse {
    pub status: String,
    pub key: String,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRotateResponse {
    pub status: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRollbackResponse {
    pub status: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct VaultRetryPolicy {
    pub max_elapsed: Duration,
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub jitter_ratio: f64,
}

impl Default for VaultRetryPolicy {
    fn default() -> Self {
        Self {
            max_elapsed: Duration::from_secs(60),
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(5),
            jitter_ratio: 0.20,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("node error: {0}")]
    Node(#[from] NodeError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("vault action timed out action={action} trace_id={trace_id} target={target} timeout_ms={timeout_ms}")]
    ActionTimeout {
        action: String,
        trace_id: String,
        target: String,
        timeout_ms: u64,
    },
    #[error("vault returned error code={code} message={message}")]
    Service { code: String, message: String },
    #[error("invalid vault ref")]
    InvalidVaultRef,
    #[error("vault response missing value field for key={key}")]
    EmptyValue { key: String },
}

/// Identity of the caller of a vault L2 action.
///
/// Both fields are mandatory:
/// - `src_ilk`: caller's own ILK (`ilk:<uuid>`), resolved at boot from
///   identity SHM via `fluxbee_sdk::identity::wait_for_self_system_ilk_id`.
///   Vault uses it to look up the caller's `tenant_id` and `ilk_type` for
///   same-tenant/owner authorisation (vault spec D1, VA-D2/D4/D5).
/// - `src_l2_name`: caller's full L2 name (`SY.foo@motherbee`), used by vault
///   for the `SY.admin@*` / `SY.architect@*` override fast-path (VA-D3).
///
/// Callers that omit these will be rejected by vault for any tenant-scoped
/// auth path. The override fast-path only covers admin/architect.
#[derive(Debug, Clone, Copy)]
pub struct VaultCaller<'a> {
    pub src_ilk: &'a str,
    pub src_l2_name: &'a str,
}

impl<'a> VaultCaller<'a> {
    pub fn new(src_ilk: &'a str, src_l2_name: &'a str) -> Self {
        Self {
            src_ilk,
            src_l2_name,
        }
    }
}

pub fn parse_vault_ref(value: &str) -> Result<&str, VaultError> {
    let trimmed = value.trim();
    let key = trimmed
        .strip_prefix(VAULT_REF_PREFIX)
        .ok_or(VaultError::InvalidVaultRef)?;
    if key.trim().is_empty() || key != key.trim() {
        return Err(VaultError::InvalidVaultRef);
    }
    Ok(key)
}

pub async fn vault_get(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    key: &str,
    timeout: Duration,
) -> Result<VaultGetResponse, VaultError> {
    let payload = json!(VaultGetRequest {
        key: key.to_string()
    });
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_GET,
        payload,
        timeout,
    )
    .await?;
    let parsed: VaultGetResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_get_metadata(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    key: &str,
    timeout: Duration,
) -> Result<VaultGetMetadataResponse, VaultError> {
    let payload = json!(VaultGetMetadataRequest {
        key: key.to_string()
    });
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_GET_METADATA,
        payload,
        timeout,
    )
    .await?;
    let parsed: VaultGetMetadataResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_put(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    request: VaultPutRequest,
    timeout: Duration,
) -> Result<VaultPutResponse, VaultError> {
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_PUT,
        json!(request),
        timeout,
    )
    .await?;
    let parsed: VaultPutResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_list(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    filter: Option<VaultFilter>,
    timeout: Duration,
) -> Result<VaultListResponse, VaultError> {
    let payload = json!(VaultListRequest { filter });
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_LIST,
        payload,
        timeout,
    )
    .await?;
    let parsed: VaultListResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_delete(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    key: &str,
    timeout: Duration,
) -> Result<VaultDeleteResponse, VaultError> {
    let payload = json!(VaultKeyRequest {
        key: key.to_string()
    });
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_DELETE,
        payload,
        timeout,
    )
    .await?;
    let parsed: VaultDeleteResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_rotate(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    key: &str,
    value: Value,
    timeout: Duration,
) -> Result<VaultRotateResponse, VaultError> {
    let payload = json!(VaultRotateRequest {
        key: key.to_string(),
        value
    });
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_ROTATE,
        payload,
        timeout,
    )
    .await?;
    let parsed: VaultRotateResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_rollback(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    key: &str,
    timeout: Duration,
) -> Result<VaultRollbackResponse, VaultError> {
    let payload = json!(VaultKeyRequest {
        key: key.to_string()
    });
    let response = send_action_once(
        sender,
        receiver,
        caller,
        target,
        MSG_VAULT_ROLLBACK,
        payload,
        timeout,
    )
    .await?;
    let parsed: VaultRollbackResponse = serde_json::from_value(response)?;
    ensure_ok(
        &parsed.status,
        parsed.error_code.as_deref(),
        parsed.message.as_deref(),
    )?;
    Ok(parsed)
}

pub async fn vault_get_with_retry(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    key_or_ref: &str,
    policy: VaultRetryPolicy,
) -> Result<VaultGetResponse, VaultError> {
    let key = key_or_ref
        .strip_prefix(VAULT_REF_PREFIX)
        .map(str::to_string)
        .unwrap_or_else(|| key_or_ref.to_string());
    let started = Instant::now();
    let mut delay = policy.initial_delay;
    loop {
        match vault_get(
            sender,
            receiver,
            caller,
            target,
            &key,
            delay.min(Duration::from_secs(5)),
        )
        .await
        {
            Ok(response) => return Ok(response),
            Err(err) if should_retry(&err) && started.elapsed() < policy.max_elapsed => {
                let sleep_for = jittered_delay(delay, policy.jitter_ratio);
                time::sleep(sleep_for).await;
                delay = std::cmp::min(delay.saturating_mul(2), policy.max_delay);
            }
            Err(err) => return Err(err),
        }
    }
}

/// Resolve a `vault://<key>` reference to its stored plaintext value.
///
/// Wrapper over `parse_vault_ref` + `vault_get_with_retry` that's the single
/// entry-point Phase J consumers (`ai.generic`, `sy.cognition`, `sy.architect`,
/// `sy.admin`, `sy.identity`, `sy.storage`) should call when they need to read
/// a secret. Targets `SY.vault@<hive_id>` automatically. The caller decides
/// what "degraded" means when this returns `Err` (most callers should log
/// and continue with their service in degraded mode).
///
/// Returns the raw `value` JSON the secret was stored as (often
/// `{"<field>": "<plaintext>"}` for nested secrets, or a plain string).
pub async fn resolve_vault_ref(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    hive_id: &str,
    vault_ref: &str,
    policy: VaultRetryPolicy,
) -> Result<Value, VaultError> {
    let key = parse_vault_ref(vault_ref)?.to_string();
    let target = format!("SY.vault@{}", hive_id);
    let response =
        vault_get_with_retry(sender, receiver, caller, &target, &key, policy).await?;
    response.value.ok_or(VaultError::EmptyValue { key })
}

fn ensure_ok(
    status: &str,
    error_code: Option<&str>,
    message: Option<&str>,
) -> Result<(), VaultError> {
    if status.eq_ignore_ascii_case("ok") {
        return Ok(());
    }
    Err(VaultError::Service {
        code: error_code.unwrap_or("VAULT_ERROR").to_string(),
        message: message
            .unwrap_or("vault returned non-ok status")
            .to_string(),
    })
}

fn should_retry(err: &VaultError) -> bool {
    match err {
        VaultError::Node(NodeError::Timeout) => true,
        VaultError::Node(NodeError::Disconnected) => true,
        VaultError::Service { code, .. } => {
            matches!(code.as_str(), "VAULT_UNAVAILABLE" | "KEY_NOT_FOUND")
        }
        _ => false,
    }
}

fn jittered_delay(delay: Duration, ratio: f64) -> Duration {
    if ratio <= 0.0 {
        return delay;
    }
    let millis = delay.as_millis() as u64;
    if millis == 0 {
        return delay;
    }
    let spread = ((millis as f64) * ratio).round() as u64;
    if spread == 0 {
        return delay;
    }
    let jitter = (Uuid::new_v4().as_u128() as u64) % (spread * 2 + 1);
    let adjusted = millis.saturating_sub(spread).saturating_add(jitter);
    Duration::from_millis(adjusted.max(1))
}

async fn send_action_once(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
    caller: VaultCaller<'_>,
    target: &str,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value, VaultError> {
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: Some(caller.src_l2_name.to_string()),
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(action.to_string()),
            src_ilk: Some(caller.src_ilk.to_string()),
            target: Some(target.to_string()),
            ..Meta::default()
        },
        payload,
    };
    sender.send(msg).await?;
    let expected = response_action_for(action);
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(VaultError::ActionTimeout {
                action: action.to_string(),
                trace_id,
                target: target.to_string(),
                timeout_ms: timeout.as_millis() as u64,
            });
        }
        let incoming = receiver.recv_timeout(deadline - now).await?;
        if incoming.routing.trace_id != trace_id || incoming.meta.msg_type != SYSTEM_KIND {
            continue;
        }
        match incoming.meta.msg.as_deref() {
            Some(MSG_UNREACHABLE) | Some(MSG_TTL_EXCEEDED) => {
                return Err(VaultError::Service {
                    code: "VAULT_UNAVAILABLE".to_string(),
                    message: "vault target unavailable".to_string(),
                });
            }
            Some(msg_name) if msg_name == expected => return Ok(incoming.payload),
            _ => continue,
        }
    }
}

fn response_action_for(action: &str) -> &'static str {
    match action {
        MSG_VAULT_PUT => MSG_VAULT_PUT_RESPONSE,
        MSG_VAULT_GET => MSG_VAULT_GET_RESPONSE,
        MSG_VAULT_GET_METADATA => MSG_VAULT_GET_METADATA_RESPONSE,
        MSG_VAULT_LIST => MSG_VAULT_LIST_RESPONSE,
        MSG_VAULT_DELETE => MSG_VAULT_DELETE_RESPONSE,
        MSG_VAULT_ROTATE => MSG_VAULT_ROTATE_RESPONSE,
        MSG_VAULT_ROLLBACK => MSG_VAULT_ROLLBACK_RESPONSE,
        _ => MSG_VAULT_GET_RESPONSE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_vault_ref_accepts_valid_ref() {
        assert_eq!(
            parse_vault_ref("vault://sys:openai-api-key").unwrap(),
            "sys:openai-api-key"
        );
    }

    #[test]
    fn parse_vault_ref_rejects_plain_key() {
        assert!(matches!(
            parse_vault_ref("sys:openai-api-key"),
            Err(VaultError::InvalidVaultRef)
        ));
    }
}
