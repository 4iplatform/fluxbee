#![forbid(unsafe_code)]

use std::time::{Duration, Instant};

use serde_json::Value;

use crate::identity::{IdentityProvisioner, IdentityResolver, ResolveOrCreateInput};
use crate::io_context::{wrap_in_meta_context, IoContext};
use crate::person_assignment::{IoSelf, PersonAssignment, PersonAssignmentStatus};
use crate::relay::AssembledTurn;
use crate::reliability::{dedup_key_from_io_context, DedupCache};
use crate::router_message::{build_user_message, new_trace_id};
use crate::text_v1_blob::IoBlobRuntimeConfig;
use fluxbee_sdk::protocol::Message;

#[derive(Debug, Clone)]
pub struct InboundConfig {
    pub ttl: u32,
    pub dedup_ttl: Duration,
    pub dedup_max_entries: usize,
    pub dst_node: Option<String>,
    /// The person this IO carries, its config's `io.person_ilk` (`ilk:<uuid>`, next to
    /// `io.dst_node`; `crate::person_assignment::extract_runtime_person_ilk`): a Digital
    /// Executive's channel, for one (docs/digital-executive-v1-stage-2.md §7). While identity
    /// confirms it (`crate::person_assignment`), each inbound message names it as `dst_ilk`; it
    /// never changes where the message goes. None, or blank, reads nothing and changes nothing.
    pub person_ilk: Option<String>,
    /// The ILK this node runs as (`FLUXBEE_NODE_ILK_ID`, `fluxbee_sdk::read_self_ilk_from_env`):
    /// the entry identity's confirmation of `person_ilk` is read from. Read for nothing else.
    pub self_ilk_id: Option<String>,
    pub provision_on_miss: bool,
    pub blob_runtime: Option<IoBlobRuntimeConfig>,
    /// The tenant this node runs as. Every external user it lets in belongs to it.
    ///
    /// An external channel message carries no tenant of its own: a Slack user id or a phone
    /// number says who wrote, not who pays. The owner is the tenant that launched the node, and
    /// the external identity is part of that tenant's cost.
    ///
    /// It lives HERE, not at each call site, because the call sites got it wrong the same way:
    /// io.slack and io.wapp both passed `tenant_id: None` with a `tenant_hint` that nothing ever
    /// read, so `stable_ich_id` received an empty tenant and identity refused the seed —
    /// provisioning could never succeed and EVERY external user entered the mesh with a null
    /// src_ilk. Filling it in the shared processor makes that impossible to forget in the next
    /// IO node too.
    pub self_tenant_id: Option<String>,
}

impl Default for InboundConfig {
    fn default() -> Self {
        Self {
            ttl: 16,
            dedup_ttl: Duration::from_millis(10 * 60 * 1000),
            dedup_max_entries: 50_000,
            dst_node: None,
            person_ilk: None,
            self_ilk_id: None,
            provision_on_miss: true,
            self_tenant_id: None,
            blob_runtime: Some(IoBlobRuntimeConfig::default()),
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
    self_tenant_id: Option<String>,
    self_ilk_id: Option<String>,
    /// The person the config assigns this IO, and identity's last word on it.
    person: PersonAssignment,
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
        let _ = config.blob_runtime;

        Self {
            node_uuid: node_uuid.into(),
            ttl,
            dst_node,
            provision_on_miss: config.provision_on_miss,
            self_tenant_id: config.self_tenant_id,
            self_ilk_id: config.self_ilk_id,
            person: PersonAssignment::new(config.person_ilk.as_deref()),
            dedup,
            stats: InboundStats::default(),
        }
    }

    pub fn stats(&self) -> InboundStats {
        self.stats
    }

    pub fn set_dst_node(&mut self, dst_node: Option<String>) {
        self.dst_node = dst_node;
    }

    /// Assigns the person this IO carries, from a new config's `io.person_ilk` (None for none), as
    /// `set_dst_node` takes its `io.dst_node`. Another person than the one held is checked against
    /// identity afresh, at the next message or `refresh_person_ilk`.
    pub fn set_person_ilk(&mut self, person_ilk: Option<String>) {
        self.person.set(person_ilk.as_deref());
    }

    /// Checks the person this IO carries against identity now, as at boot or after a config
    /// change, rather than at the next message; nothing with no person. The status after it.
    pub fn refresh_person_ilk(
        &mut self,
        identity: &dyn IdentityResolver,
    ) -> Option<PersonAssignmentStatus> {
        let io = IoSelf {
            node_uuid: &self.node_uuid,
            ilk: self.self_ilk_id.as_deref(),
            tenant: self.self_tenant_id.as_deref(),
        };
        self.person.check(&io, identity, Instant::now());
        self.person.status()
    }

    /// What the IO's status says of the person its config assigns
    /// (`PersonAssignmentStatus::add_to_status`): None with no person.
    pub fn person_ilk_status(&self) -> Option<PersonAssignmentStatus> {
        self.person.status()
    }

    /// The person an inbound message names as its `dst_ilk` at `now`: the one the config assigns,
    /// while identity confirms it, checked again once the last check is
    /// `PERSON_RECHECK_INTERVAL` old. None otherwise.
    fn person_to_stamp(&mut self, identity: &dyn IdentityResolver, now: Instant) -> Option<String> {
        let io = IoSelf {
            node_uuid: &self.node_uuid,
            ilk: self.self_ilk_id.as_deref(),
            tenant: self.self_tenant_id.as_deref(),
        };
        self.person.person_at(&io, identity, now)
    }

    pub async fn process_inbound(
        &mut self,
        identity: &dyn IdentityResolver,
        provisioner: Option<&dyn IdentityProvisioner>,
        identity_input: ResolveOrCreateInput,
        dst_node_override: Option<String>,
        io_context: IoContext,
        payload: Value,
    ) -> InboundOutcome {
        let dedup_key = dedup_key_from_io_context(&io_context);
        if self.dedup.is_duplicate_and_mark(&dedup_key) {
            self.stats.dedup_hits += 1;
            return InboundOutcome::DroppedDuplicate;
        }
        self.stats.dedup_misses += 1;

        self.process_message_parts(
            identity,
            provisioner,
            identity_input,
            dst_node_override,
            wrap_in_meta_context(&io_context),
            payload,
        )
        .await
    }

    pub async fn process_assembled_turn(
        &mut self,
        identity: &dyn IdentityResolver,
        provisioner: Option<&dyn IdentityProvisioner>,
        turn: AssembledTurn,
    ) -> InboundOutcome {
        let dedup_key = dedup_key_from_io_context(&turn.io_context);
        if self.dedup.is_duplicate_and_mark(&dedup_key) {
            self.stats.dedup_hits += 1;
            return InboundOutcome::DroppedDuplicate;
        }
        self.stats.dedup_misses += 1;

        self.process_message_parts(
            identity,
            provisioner,
            turn.identity_input,
            turn.dst_node_override,
            turn.meta_context,
            turn.payload,
        )
        .await
    }

    async fn process_message_parts(
        &mut self,
        identity: &dyn IdentityResolver,
        provisioner: Option<&dyn IdentityProvisioner>,
        identity_input: ResolveOrCreateInput,
        dst_node_override: Option<String>,
        meta_context: Value,
        payload: Value,
    ) -> InboundOutcome {
        // The external identity belongs to the tenant that launched this node (see
        // `InboundConfig::self_tenant_id`). An explicit tenant on the input still wins: io.api
        // authenticates a principal and knows better than we do.
        let mut identity_input = identity_input;
        if identity_input
            .tenant_id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .is_none()
        {
            identity_input.tenant_id = self.self_tenant_id.clone();
        }
        let identity_input = identity_input;

        let trace_id = new_trace_id();
        let mut src_ilk = if let Some(src_ilk_override) = identity_input
            .src_ilk_override
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            tracing::debug!(
                channel = %identity_input.channel,
                external_id = %identity_input.external_id,
                src_ilk = %src_ilk_override,
                "identity src_ilk override used"
            );
            Some(src_ilk_override.clone())
        } else {
            match identity.lookup(&identity_input) {
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
                        identity.remember(&identity_input, &provisioned_ilk);
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

        // The person this IO carries, while identity confirms it (docs/digital-executive-v1-stage-2.md
        // §7): its `dst_ilk`. Where the message goes stays `io.dst_node`.
        let dst_ilk = self.person_to_stamp(identity, Instant::now());

        InboundOutcome::SendNow(build_user_message(
            &self.node_uuid,
            dst_node_override.or_else(|| self.dst_node.clone()),
            self.ttl,
            trace_id,
            src_ilk,
            dst_ilk,
            meta_context,
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

    /// A provisioner that remembers which tenant it was called with.
    struct TenantCapture(std::sync::Mutex<Option<Option<String>>>);
    #[async_trait::async_trait]
    impl IdentityProvisioner for TenantCapture {
        async fn provision(
            &self,
            input: &ResolveOrCreateInput,
        ) -> Result<Option<String>, IdentityError> {
            *self.0.lock().unwrap() = Some(input.tenant_id.clone());
            Ok(Some("ilk:provisionado".to_string()))
        }
    }

    struct AlwaysMiss;
    impl IdentityResolver for AlwaysMiss {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn lookup(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
            Ok(None)
        }
    }

    fn assert_no_legacy_context_src_ilk(msg: &fluxbee_sdk::protocol::Message) {
        let legacy = msg.meta.context.as_ref().and_then(|ctx| ctx.get("src_ilk"));
        assert!(
            legacy.is_none(),
            "legacy meta.context.src_ilk should not be present"
        );
    }

    fn assert_no_legacy_context_ich(msg: &fluxbee_sdk::protocol::Message) {
        let legacy = msg.meta.context.as_ref().and_then(|ctx| ctx.get("ich"));
        assert!(
            legacy.is_none(),
            "legacy meta.context.ich should not be present"
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
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o1 = p
            .process_inbound(&id, None, input.clone(), None, io.clone(), payload.clone())
            .await;
        assert!(matches!(o1, InboundOutcome::SendNow(_)));

        let o2 = p.process_inbound(&id, None, input, None, io, payload).await;
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
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, None, input, None, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk, None);
        assert_no_legacy_context_src_ilk(&msg);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T:C"));
        assert_no_legacy_context_ich(&msg);
        let stats = p.stats();
        assert_eq!(stats.identity_lookup_misses, 1);
        assert_eq!(stats.identity_fallback_null, 1);
    }

    #[tokio::test]
    async fn forwards_thread_id_in_meta_without_legacy_context_thread_id() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = MockIdentityResolver::new();
        let io = slack_inbound_io_context("T", "U", "C", Some("171234.567"), "Ev1");
        let expected_thread_id = io.conversation.thread_id.clone();
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, None, input, None, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.thread_id, expected_thread_id);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T:C"));
        let legacy_context_thread_id = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("thread_id"))
            .and_then(|v| v.as_str());
        assert_eq!(legacy_context_thread_id, None);
        assert_no_legacy_context_ich(&msg);
    }

    #[tokio::test]
    async fn forwards_channel_id_as_thread_id_without_legacy_context_thread_id() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = MockIdentityResolver::new();
        let io = slack_inbound_io_context("T", "U", "C123", None, "Ev1");
        let expected_thread_id = io.conversation.thread_id.clone();
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, None, input, None, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.thread_id, expected_thread_id);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T:C123"));
        let legacy_context_thread_id = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("thread_id"))
            .and_then(|v| v.as_str());
        assert_eq!(legacy_context_thread_id, None);
        assert_no_legacy_context_ich(&msg);
    }

    #[tokio::test]
    async fn preserves_io_reply_target_in_meta_context() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = MockIdentityResolver::new();
        let io = slack_inbound_io_context("T123", "U456", "C789", Some("171234.567"), "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T123:U456".to_string(),
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p.process_inbound(&id, None, input, None, io, payload).await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };

        assert!(msg
            .meta
            .src_ilk
            .as_deref()
            .is_some_and(|value| value.starts_with("ilk:mock:")));
        assert_no_legacy_context_src_ilk(&msg);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T123:C789"));
        assert_no_legacy_context_ich(&msg);

        let rt = msg
            .meta
            .context
            .as_ref()
            .and_then(|ctx| ctx.get("io"))
            .and_then(|io| io.get("reply_target"))
            .cloned()
            .expect("missing meta.context.io.reply_target");

        assert_eq!(rt.get("kind").and_then(|v| v.as_str()), Some("slack_post"));
        assert_eq!(rt.get("address").and_then(|v| v.as_str()), Some("C789"));
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
        async fn provision(
            &self,
            _input: &ResolveOrCreateInput,
        ) -> Result<Option<String>, IdentityError> {
            Ok(Some("ilk:provisional:test".to_string()))
        }
    }

    struct AlwaysProvisionError;

    #[async_trait::async_trait]
    impl IdentityProvisioner for AlwaysProvisionError {
        async fn provision(
            &self,
            _input: &ResolveOrCreateInput,
        ) -> Result<Option<String>, IdentityError> {
            Err(IdentityError::Unavailable)
        }
    }

    struct HitResolver;

    impl IdentityResolver for HitResolver {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }

        fn lookup(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
            Ok(Some("ilk:hit:test".to_string()))
        }
    }

    struct CountingProvisioner {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl IdentityProvisioner for CountingProvisioner {
        async fn provision(
            &self,
            _input: &ResolveOrCreateInput,
        ) -> Result<Option<String>, IdentityError> {
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
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, None, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:provisional:test"));
        assert_no_legacy_context_src_ilk(&msg);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T:C"));
        assert_no_legacy_context_ich(&msg);
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
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, None, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk, None);
        assert_no_legacy_context_src_ilk(&msg);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T:C"));
        assert_no_legacy_context_ich(&msg);
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
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, None, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:hit:test"));
        assert_no_legacy_context_src_ilk(&msg);
        assert_eq!(msg.meta.ich.as_deref(), Some("slack://T:C"));
        assert_no_legacy_context_ich(&msg);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let stats = p.stats();
        assert_eq!(stats.identity_lookup_hits, 1);
        assert_eq!(stats.identity_provision_success, 0);
        assert_eq!(stats.identity_fallback_null, 0);
    }

    #[tokio::test]
    async fn src_ilk_override_bypasses_lookup_and_provision() {
        let mut p = InboundProcessor::new("node", InboundConfig::default());
        let id = AlwaysMiss;
        let calls = Arc::new(AtomicUsize::new(0));
        let provisioner = CountingProvisioner {
            calls: Arc::clone(&calls),
        };
        let io = slack_inbound_io_context("T", "U", "C", None, "Ev1");
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            src_ilk_override: Some("ilk:override:test".to_string()),
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let payload = serde_json::json!({ "type": "text", "content": "hi" });

        let o = p
            .process_inbound(&id, Some(&provisioner), input, None, io, payload)
            .await;
        let InboundOutcome::SendNow(msg) = o else {
            panic!("unexpected outcome: {o:?}");
        };
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:override:test"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// The external user belongs to the tenant that launched the node.
    ///
    /// A message from an external channel brings no tenant of its own: a Slack user id or a phone
    /// number say who wrote, not who pays. io.slack and io.wapp used to send `tenant_id: None`
    /// with a `tenant_hint` nobody read, so the ICH seed arrived with an empty tenant, identity
    /// rejected it, and EVERY external user came in with a null src_ilk.
    #[tokio::test]
    async fn the_external_user_takes_the_nodes_tenant() {
        let capture = TenantCapture(std::sync::Mutex::new(None));
        let mut proc = InboundProcessor::new(
            "uuid-test",
            InboundConfig {
                self_tenant_id: Some("tnt:1111".to_string()),
                ..InboundConfig::default()
            },
        );
        let input = ResolveOrCreateInput {
            channel: "slack".to_string(),
            external_id: "T:U".to_string(),
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: Some("T0WORKSPACE".to_string()),
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let _ = proc
            .process_message_parts(
                &AlwaysMiss,
                Some(&capture),
                input,
                None,
                serde_json::json!({}),
                serde_json::json!({"type":"text","content":"hello"}),
            )
            .await;

        let seen = capture.0.lock().unwrap().clone();
        assert_eq!(
            seen,
            Some(Some("tnt:1111".to_string())),
            "provisioning has to receive the node's tenant, not an empty one"
        );
    }

    /// An explicit tenant in the input WINS: io.api authenticates a principal and knows better.
    #[tokio::test]
    async fn an_explicit_tenant_wins_over_the_nodes() {
        let capture = TenantCapture(std::sync::Mutex::new(None));
        let mut proc = InboundProcessor::new(
            "uuid-test",
            InboundConfig {
                self_tenant_id: Some("tnt:1111".to_string()),
                ..InboundConfig::default()
            },
        );
        let input = ResolveOrCreateInput {
            channel: "api".to_string(),
            external_id: "principal".to_string(),
            src_ilk_override: None,
            tenant_id: Some("tnt:9999".to_string()),
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        };
        let _ = proc
            .process_message_parts(
                &AlwaysMiss,
                Some(&capture),
                input,
                None,
                serde_json::json!({}),
                serde_json::json!({"type":"text","content":"hello"}),
            )
            .await;

        assert_eq!(
            capture.0.lock().unwrap().clone(),
            Some(Some("tnt:9999".to_string()))
        );
    }

    // docs/digital-executive-v1-stage-2.md §7: the person an IO carries.

    use crate::person_assignment::tests::{ilk, Region, ED, IO_ILK, OTHER_TENANT, TENANT};
    use fluxbee_sdk::protocol::Destination;

    const CONSCIOUS: &str = "AI.laura@worker1";

    fn person_config(person_ilk: Option<&str>) -> InboundConfig {
        InboundConfig {
            dst_node: Some(CONSCIOUS.to_string()),
            person_ilk: person_ilk.map(str::to_string),
            self_ilk_id: Some(IO_ILK.to_string()),
            self_tenant_id: Some(TENANT.to_string()),
            ..InboundConfig::default()
        }
    }

    fn wapp_input() -> ResolveOrCreateInput {
        ResolveOrCreateInput {
            channel: "whatsapp".to_string(),
            external_id: "5491100000000".to_string(),
            src_ilk_override: None,
            tenant_id: None,
            tenant_hint: None,
            attributes: serde_json::json!({}),
            ilk_type: Some("human".to_string()),
        }
    }

    async fn inbound_message(
        processor: &mut InboundProcessor,
        identity: &dyn IdentityResolver,
        event: &str,
    ) -> Message {
        let io = slack_inbound_io_context("T", "U", "C", None, event);
        let outcome = processor
            .process_inbound(
                identity,
                None,
                wapp_input(),
                None,
                io,
                serde_json::json!({"type": "text", "content": "hola"}),
            )
            .await;
        let InboundOutcome::SendNow(msg) = outcome else {
            panic!("unexpected outcome: {outcome:?}");
        };
        msg
    }

    fn goes_to_the_conscious(msg: &Message) {
        assert!(matches!(
            msg.routing.dst,
            Destination::Unicast(ref dst) if dst == CONSCIOUS
        ));
        assert_eq!(msg.meta.src_ilk.as_deref(), Some("ilk:writer"));
    }

    /// While identity confirms the person the config assigns, each inbound message names it as
    /// its `dst_ilk`, and still goes to `io.dst_node`.
    #[tokio::test]
    async fn a_confirmed_person_is_each_inbound_messages_dst_ilk() {
        let region = Region::confirming();
        let mut p = InboundProcessor::new("uuid-io", person_config(Some(ED)));

        let msg = inbound_message(&mut p, &region, "Ev1").await;
        goes_to_the_conscious(&msg);
        assert_eq!(msg.meta.dst_ilk.as_deref(), Some(ED));
        assert_eq!(
            msg.meta.context.as_ref().unwrap()["dst_ilk"],
            serde_json::json!(ED)
        );
        assert_eq!(
            p.person_ilk_status(),
            Some(PersonAssignmentStatus {
                ilk: ED.to_string(),
                confirmed: true,
                reason: None
            })
        );

        // The next message within the interval reads no entry again.
        let reads = region.reads();
        let msg = inbound_message(&mut p, &region, "Ev2").await;
        assert_eq!(msg.meta.dst_ilk.as_deref(), Some(ED));
        assert_eq!(region.reads(), reads);
    }

    /// While identity does not confirm it, nothing is stamped, the status says why, and the
    /// message still goes to `io.dst_node`: a config written by hand, a relation of another role,
    /// a person of another tenant, an IO whose own ILK is unknown.
    #[tokio::test]
    async fn a_person_identity_does_not_confirm_is_never_stamped() {
        let cases: [(&str, Box<dyn Fn(&Region)>, InboundConfig); 4] = [
            (
                "names no person",
                Box::new(|region| region.put(ilk(IO_ILK, TENANT, None, None))),
                person_config(Some(ED)),
            ),
            (
                "the role conscious",
                Box::new(|region| region.put(ilk(IO_ILK, TENANT, Some(ED), Some("conscious")))),
                person_config(Some(ED)),
            ),
            (
                "not of this IO's tenant",
                Box::new(|region| region.put(ilk(ED, OTHER_TENANT, None, None))),
                person_config(Some(ED)),
            ),
            (
                "no ILK of its own",
                Box::new(|_| {}),
                InboundConfig {
                    self_ilk_id: None,
                    ..person_config(Some(ED))
                },
            ),
        ];
        for (why, change, config) in cases {
            let region = Region::confirming();
            change(&region);
            let mut p = InboundProcessor::new("uuid-io", config);
            let msg = inbound_message(&mut p, &region, "Ev1").await;
            goes_to_the_conscious(&msg);
            assert_eq!(msg.meta.dst_ilk, None, "{why}");
            assert_eq!(
                msg.meta.context.as_ref().unwrap()["dst_ilk"],
                serde_json::Value::Null
            );
            let status = p.person_ilk_status().expect("a status");
            assert_eq!(status.ilk, ED);
            assert!(!status.confirmed);
            assert!(
                status.reason.as_deref().unwrap().contains(why),
                "{status:?}"
            );
        }
    }

    /// With no person in the config nothing is read, nothing is stamped and the status says
    /// nothing, as before.
    #[tokio::test]
    async fn no_person_in_the_config_changes_nothing() {
        let region = Region::confirming();
        for person in [None, Some("  ")] {
            let mut p = InboundProcessor::new("uuid-io", person_config(person));
            let msg = inbound_message(&mut p, &region, "Ev1").await;
            goes_to_the_conscious(&msg);
            assert_eq!(msg.meta.dst_ilk, None);
            assert_eq!(p.person_ilk_status(), None);
            assert_eq!(p.refresh_person_ilk(&region), None);
        }
        assert_eq!(region.reads(), 0);
    }

    /// A config change assigns a person, or takes it away, at once; `refresh_person_ilk` checks
    /// it without waiting for a message.
    #[tokio::test]
    async fn a_config_change_assigns_the_person_and_the_check_runs_afresh() {
        let region = Region::confirming();
        let mut p = InboundProcessor::new("uuid-io", person_config(None));
        let msg = inbound_message(&mut p, &region, "Ev1").await;
        assert_eq!(msg.meta.dst_ilk, None);

        p.set_person_ilk(Some(ED.to_string()));
        assert_eq!(
            p.refresh_person_ilk(&region).map(|status| status.confirmed),
            Some(true)
        );
        let msg = inbound_message(&mut p, &region, "Ev2").await;
        assert_eq!(msg.meta.dst_ilk.as_deref(), Some(ED));

        p.set_person_ilk(None);
        let msg = inbound_message(&mut p, &region, "Ev3").await;
        assert_eq!(msg.meta.dst_ilk, None);
        assert_eq!(p.person_ilk_status(), None);
    }
}
