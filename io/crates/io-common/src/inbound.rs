#![forbid(unsafe_code)]

use std::time::Duration;

use serde_json::Value;

use crate::identity::{IdentityProvisioner, IdentityResolver, ResolveOrCreateInput};
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
    pub provision_on_miss: bool,
}

impl Default for InboundConfig {
    fn default() -> Self {
        Self {
            ttl: 16,
            dedup_ttl: Duration::from_millis(10 * 60 * 1000),
            dedup_max_entries: 50_000,
            dst_node: None,
            provision_on_miss: true,
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct InboundStats {
    pub dedup_hits: u64,
    pub dedup_misses: u64,
    pub identity_lookup_hits: u64,
    pub identity_lookup_misses: u64,
    pub identity_lookup_errors: u64,
    pub identity_provision_success: u64,
    pub identity_provision_errors: u64,
    pub identity_fallback_null: u64,
}

#[derive(Debug)]
pub struct InboundProcessor {
    node_uuid: String,
    ttl: u32,
    dst_node: Option<String>,
    provision_on_miss: bool,
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
            provision_on_miss: config.provision_on_miss,
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
        provisioner: Option<&dyn IdentityProvisioner>,
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
        let mut src_ilk = match identity.lookup(&identity_input.channel, &identity_input.external_id) {
            Ok(Some(src_ilk)) => {
                self.stats.identity_lookup_hits += 1;
                tracing::debug!(
                    channel = %identity_input.channel,
                    external_id = %identity_input.external_id,
                    src_ilk = %src_ilk,
                    "identity lookup hit"
                );
                Some(src_ilk)
            }
            Ok(None) => {
                self.stats.identity_lookup_misses += 1;
                tracing::debug!(
                    channel = %identity_input.channel,
                    external_id = %identity_input.external_id,
                    "identity lookup miss"
                );
                None
            }
            Err(error) => {
                self.stats.identity_lookup_errors += 1;
                tracing::warn!(
                    ?error,
                    channel = %identity_input.channel,
                    external_id = %identity_input.external_id,
                    "identity lookup error; treating as miss"
                );
                None
            }
        };

        if src_ilk.is_none() && self.provision_on_miss {
            if let Some(provisioner) = provisioner {
                match provisioner.provision(&identity_input).await {
                    Ok(Some(provisioned_ilk)) => {
                        self.stats.identity_provision_success += 1;
                        tracing::info!(
                            channel = %identity_input.channel,
                            external_id = %identity_input.external_id,
                            src_ilk = %provisioned_ilk,
                            "identity provisioned on miss"
                        );
                        identity.remember(
                            &identity_input.channel,
                            &identity_input.external_id,
                            &provisioned_ilk,
                        );
                        src_ilk = Some(provisioned_ilk);
                    }
                    Ok(None) => {
                        self.stats.identity_fallback_null += 1;
                        tracing::warn!(
                            channel = %identity_input.channel,
                            external_id = %identity_input.external_id,
                            "identity miss with no provisional src_ilk; forwarding with null src_ilk"
                        );
                    }
                    Err(error) => {
                        self.stats.identity_provision_errors += 1;
                        self.stats.identity_fallback_null += 1;
                        tracing::warn!(
                            ?error,
                            channel = %identity_input.channel,
                            external_id = %identity_input.external_id,
                            "identity provision failed; forwarding with null src_ilk"
                        );
                    }
                }
            } else {
                self.stats.identity_fallback_null += 1;
                tracing::warn!(
                    channel = %identity_input.channel,
                    external_id = %identity_input.external_id,
                    "identity miss without provisioner; forwarding with null src_ilk"
                );
            }
        }

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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct AlwaysMiss;
    impl IdentityResolver for AlwaysMiss {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn lookup(&self, _channel: &str, _external_id: &str) -> Result<Option<String>, IdentityError> {
            Ok(None)
        }
    }

    fn assert_no_legacy_context_src_ilk(msg: &fluxbee_sdk::protocol::Message) {
        let legacy = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("src_ilk"));
        assert!(
            legacy.is_none(),
            "legacy meta.context.src_ilk should not be present"
        );
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

        let o1 = p
            .process_inbound(&id, None, input.clone(), io.clone(), payload.clone())
            .await;
        assert!(matches!(o1, InboundOutcome::SendNow(_)));

        let o2 = p.process_inbound(&id, None, input, io, payload).await;
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

        let o = p.process_inbound(&id, None, input, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk, None);
        assert_no_legacy_context_src_ilk(&msg);
        let stats = p.stats();
        assert_eq!(stats.identity_lookup_misses, 1);
        assert_eq!(stats.identity_fallback_null, 1);
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

        let o = p.process_inbound(&id, None, input, io, payload).await;
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

    #[tokio::test]
    async fn preserves_io_reply_target_in_meta_context() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = MockIdentityResolver::new();
        let io = slack_inbound_io_context("T123", "U456", "C789", Some("171234.567"), "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T123:U456".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, None, input, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };

        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:test"));
        assert_no_legacy_context_src_ilk(&msg);

        let rt = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("io"))
            .and_then(|io| io.get("reply_target"))
            .cloned()
            .expect("missing meta.context.io.reply_target");

        assert_eq!(
            rt.get("kind").and_then(|v| v.as_str()),
            Some("slack_post")
        );
        assert_eq!(
            rt.get("address").and_then(|v| v.as_str()),
            Some("C789")
        );
        assert_eq!(
            rt.get("params")
                .and_then(|v| v.get("thread_ts"))
                .and_then(|v| v.as_str()),
            Some("171234.567")
        );
        assert_eq!(
            rt.get("params")
                .and_then(|v| v.get("workspace_id"))
                .and_then(|v| v.as_str()),
            Some("T123")
        );
    }

    struct AlwaysProvision;

    #[async_trait::async_trait]
    impl IdentityProvisioner for AlwaysProvision {
        async fn provision(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
            Ok(Some("ilk:provisional:test".to_string()))
        }
    }

    struct AlwaysProvisionError;

    #[async_trait::async_trait]
    impl IdentityProvisioner for AlwaysProvisionError {
        async fn provision(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
            Err(IdentityError::Unavailable)
        }
    }

    struct HitResolver;

    impl IdentityResolver for HitResolver {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn lookup(&self, _channel: &str, _external_id: &str) -> Result<Option<String>, IdentityError> {
            Ok(Some("ilk:hit:test".to_string()))
        }
    }

    struct CountingProvisioner {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl IdentityProvisioner for CountingProvisioner {
        async fn provision(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some("ilk:should-not-be-used".to_string()))
        }
    }

    #[tokio::test]
    async fn miss_can_provision_src_ilk() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = AlwaysMiss;
        let provisioner = AlwaysProvision;
        let io = slack_inbound_io_context("T", "U", "C", None, "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:provisional:test"));
        assert_no_legacy_context_src_ilk(&msg);
        let stats = p.stats();
        assert_eq!(stats.identity_lookup_misses, 1);
        assert_eq!(stats.identity_provision_success, 1);
        assert_eq!(stats.identity_fallback_null, 0);
    }

    #[tokio::test]
    async fn miss_with_provision_error_forwards_with_null_src_ilk() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = AlwaysMiss;
        let provisioner = AlwaysProvisionError;
        let io = slack_inbound_io_context("T", "U", "C", None, "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk, None);
        assert_no_legacy_context_src_ilk(&msg);
        let stats = p.stats();
        assert_eq!(stats.identity_lookup_misses, 1);
        assert_eq!(stats.identity_provision_errors, 1);
        assert_eq!(stats.identity_fallback_null, 1);
    }

    #[tokio::test]
    async fn lookup_hit_skips_provision_call() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = HitResolver;
        let calls = Arc::new(AtomicUsize::new(0));
        let provisioner = CountingProvisioner {
            calls: Arc::clone(&calls),
        };
        let io = slack_inbound_io_context("T", "U", "C", None, "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            tenant_hint: None,
            attributes: serde_json::json!({}),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:hit:test"));
        assert_no_legacy_context_src_ilk(&msg);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let stats = p.stats();
        assert_eq!(stats.identity_lookup_hits, 1);
        assert_eq!(stats.identity_provision_success, 0);
        assert_eq!(stats.identity_fallback_null, 0);
    }
}
