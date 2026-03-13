#![forbid(unsafe_code)]

use std::time::Duration;

use serde_json::Value;

use crate::identity::{IdentityResolver, ResolveOrCreateInput};
use crate::io_context::{wrap_in_meta_context, IoContext};
use crate::reliability::{dedup_key_from_io_context, DedupCache};
use crate::router_message::{build_user_message, new_trace_id};
use fluxbee_sdk::protocol::Message;

#[derive(Debug, Clone)]
pub struct InboundConfig {
    pub ttl: u32,
    pub dedup_ttl: Duration,
    pub dedup_max_entries: usize,
    pub dst_node: Option<String>,
}

impl Default for InboundConfig {
    fn default() -> Self {
        Self {
            ttl: 16,
            dedup_ttl: Duration::from_millis(10 * 60 * 1000),
            dedup_max_entries: 50_000,
            dst_node: None,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct InboundStats {
    pub dedup_hits: u64,
    pub dedup_misses: u64,
}

#[derive(Debug)]
pub struct InboundProcessor {
    node_uuid: String,
    ttl: u32,
    dst_node: Option<String>,
    dedup: DedupCache,
    stats: InboundStats,
}

#[derive(Debug)]
pub enum InboundOutcome {
    SendNow(Message),
    DroppedDuplicate,
}

impl InboundProcessor {
    pub fn new(node_uuid: impl Into<String>, config: InboundConfig) -> Self {
        let ttl = config.ttl;
        let dst_node = config.dst_node;
        let dedup = DedupCache::new(config.dedup_ttl, config.dedup_max_entries);

        Self {
            node_uuid: node_uuid.into(),
            ttl,
            dst_node,
            dedup,
            stats: InboundStats::default(),
        }
    }

    pub fn stats(&self) -> InboundStats {
        self.stats
    }

    pub async fn process_inbound(
        &mut self,
        identity: &dyn IdentityResolver,
        identity_input: ResolveOrCreateInput,
        io_context: IoContext,
        payload: Value,
    ) -> InboundOutcome {
        let dedup_key = dedup_key_from_io_context(&io_context);
        if self.dedup.is_duplicate_and_mark(&dedup_key) {
            self.stats.dedup_hits += 1;
            return InboundOutcome::DroppedDuplicate;
        }
        self.stats.dedup_misses += 1;

        let trace_id = new_trace_id();
        let src_ilk = match identity.lookup(&identity_input.channel, &identity_input.external_id) {
            Ok(v) => v,
            Err(_) => None,
        };

        InboundOutcome::SendNow(build_user_message(
            &self.node_uuid,
            self.dst_node.clone(),
            self.ttl,
            trace_id,
            src_ilk,
            None,
            wrap_in_meta_context(&io_context),
            payload,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::IdentityError;
    use crate::identity::MockIdentityResolver;
    use crate::io_context::slack_inbound_io_context;

    struct AlwaysMiss;
    impl IdentityResolver for AlwaysMiss {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn lookup(&self, _channel: &str, _external_id: &str) -> Result<Option<String>, IdentityError> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn dedup_drops_second_message() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = MockIdentityResolver::new();
        let io = slack_inbound_io_context("T", "U", "C", None, "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o1 = p.process_inbound(&id, input.clone(), io.clone(), payload.clone()).await;
        assert!(matches!(o1, InboundOutcome::SendNow(_)));

        let o2 = p.process_inbound(&id, input, io, payload).await;
        assert!(matches!(o2, InboundOutcome::DroppedDuplicate));
    }

    #[tokio::test]
    async fn miss_forwards_with_null_src_ilk() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = AlwaysMiss;
        let io = slack_inbound_io_context("T", "U", "C", None, "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, input, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        let src = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("src_ilk"));
        assert_eq!(src, Some(&serde_json::Value::Null));
    }

    #[tokio::test]
    async fn forwards_thread_id_at_context_top_level_for_ai_nodes() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = MockIdentityResolver::new();
        let io = slack_inbound_io_context("T", "U", "C", Some("171234.567"), "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, input, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        let thread_id = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("thread_id"))
            .and_then(|v| v.as_str());
        assert_eq!(thread_id, Some("171234.567"));
    }
}
