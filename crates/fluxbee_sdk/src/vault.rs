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
    target: &str,
    key: &str,
    timeout: Duration,
) -> Result<VaultGetResponse, VaultError> {
    let payload = json!(VaultGetRequest {
        key: key.to_string()
    });
    let response =
        send_action_once(sender, receiver, target, MSG_VAULT_GET, payload, timeout).await?;
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

pub async fn vault_get_with_retry(
    sender: &NodeSender,
    receiver: &mut NodeReceiver,
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
    target: &str,
    action: &str,
    payload: Value,
    timeout: Duration,
) -> Result<Value, VaultError> {
    let trace_id = Uuid::new_v4().to_string();
    let msg = Message {
        routing: Routing {
            src: sender.uuid().to_string(),
            src_l2_name: None,
            dst: Destination::Unicast(target.to_string()),
            ttl: 16,
            trace_id: trace_id.clone(),
        },
        meta: Meta {
            msg_type: SYSTEM_KIND.to_string(),
            msg: Some(action.to_string()),
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
