#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use fluxbee_sdk::protocol::{
    Destination, Message as WireMessage, Meta, Routing, MSG_TTL_EXCEEDED, MSG_UNREACHABLE,
    SYSTEM_KIND,
};
use fluxbee_sdk::{NodeError, NodeReceiver, NodeSender, MSG_ILK_PROVISION};
use tokio::sync::Mutex;
use tokio::time::Instant;
use uuid::Uuid;

use crate::identity::{IdentityError, IdentityProvisioner, ResolveOrCreateInput};

const MSG_ILK_PROVISION_RESPONSE: &str = "ILK_PROVISION_RESPONSE";

pub struct RouterInbox {
    receiver: NodeReceiver,
    backlog: VecDeque<WireMessage>,
}

impl RouterInbox {
    pub fn new(receiver: NodeReceiver) -> Self {
        Self {
            receiver,
            backlog: VecDeque::new(),
        }
    }

    pub async fn recv_next_timeout(
        &mut self,
        timeout: Duration,
    ) -> anyhow::Result<Option<WireMessage>> {
        if let Some(msg) = self.backlog.pop_front() {
            return Ok(Some(msg));
        }
        match self.receiver.recv_timeout(timeout).await {
            Ok(msg) => Ok(Some(msg)),
            Err(NodeError::Timeout) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    pub async fn recv_for_trace_id(
        &mut self,
        trace_id: &str,
        timeout: Duration,
    ) -> Result<WireMessage, IdentityError> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(IdentityError::Timeout);
            }
            if let Some(idx) = self
                .backlog
                .iter()
                .position(|msg| msg.routing.trace_id == trace_id)
            {
                return Ok(self.backlog.remove(idx).expect("backlog idx"));
            }
            let remaining = deadline.saturating_duration_since(now);
            match self.receiver.recv_timeout(remaining).await {
                Ok(msg) => {
                    if msg.routing.trace_id == trace_id {
                        return Ok(msg);
                    }
                    self.backlog.push_back(msg);
                }
                Err(NodeError::Timeout) => return Err(IdentityError::Timeout),
                Err(_) => return Err(IdentityError::Unavailable),
            }
        }
    }
}

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

pub struct FluxbeeIdentityProvisioner {
    sender: NodeSender,
    inbox: Arc<Mutex<RouterInbox>>,
    config: IdentityProvisionConfig,
}

impl FluxbeeIdentityProvisioner {
    pub fn new(
        sender: NodeSender,
        inbox: Arc<Mutex<RouterInbox>>,
        config: IdentityProvisionConfig,
    ) -> Self {
        Self {
            sender,
            inbox,
            config,
        }
    }

    async fn call_provision_target(
        &self,
        target: &str,
        input: &ResolveOrCreateInput,
    ) -> Result<String, IdentityError> {
        let normalized_channel = normalize_identity_field(&input.channel, true);
        let normalized_address = normalize_identity_field(&input.external_id, true);
        let trace_id = Uuid::new_v4().to_string();
        let req = WireMessage {
            routing: Routing {
                src: self.sender.uuid().to_string(),
                dst: Destination::Unicast(target.to_string()),
                ttl: 16,
                trace_id: trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_ILK_PROVISION.to_string()),
                src_ilk: None,
                scope: None,
                target: None,
                action: None,
                priority: None,
                context: None,
            },
            payload: serde_json::json!({
                "ich_id": stable_ich_id(&normalized_channel, &normalized_address),
                "channel_type": normalized_channel,
                "address": normalized_address,
            }),
        };
        self.sender
            .send(req)
            .await
            .map_err(|_| IdentityError::Unavailable)?;
        tracing::debug!(
            trace_id = %trace_id,
            target = %target,
            channel = %normalized_channel,
            address = %normalized_address,
            "identity provision request sent"
        );

        let msg = {
            let mut inbox = self.inbox.lock().await;
            inbox
                .recv_for_trace_id(&trace_id, self.config.timeout)
                .await?
        };
        tracing::debug!(
            trace_id = %trace_id,
            target = %target,
            response_msg = %msg.meta.msg.as_deref().unwrap_or(""),
            "identity provision response matched"
        );
        parse_provision_response(msg)
    }
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

fn stable_ich_id(channel: &str, external_id: &str) -> String {
    // Canonical identity ids in Fluxbee use prefixed UUIDs (`ich:<uuid>`).
    // We derive a deterministic UUIDv5 from `(channel, external_id)`.
    let key = format!("{channel}:{external_id}");
    let uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes());
    format!("ich:{uuid}")
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
        let a = stable_ich_id("sim-new", "user.provision.abc1");
        let b = stable_ich_id("sim-new", "user.provision.abc1");
        assert_eq!(a, b);
    }

    #[test]
    fn stable_ich_id_changes_when_input_changes() {
        let a = stable_ich_id("sim-new", "user.provision.abc1");
        let b = stable_ich_id("sim-new", "user.provision.abc2");
        assert_ne!(a, b);
    }
}
