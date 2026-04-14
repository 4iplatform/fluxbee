#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::identity::ResolveOrCreateInput;
use crate::io_context::{wrap_in_meta_context, IoContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayStorageMode {
    Memory,
    Durable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayRawCaptureMode {
    None,
    Bounded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlushReason {
    WindowElapsed,
    ExplicitFinal,
    AttachmentBoundary,
    MaxFragments,
    MaxBytes,
    Manual,
    Shutdown,
    RehydrateExpiry,
}

impl FlushReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowElapsed => "window_elapsed",
            Self::ExplicitFinal => "explicit_final",
            Self::AttachmentBoundary => "attachment_boundary",
            Self::MaxFragments => "max_fragments",
            Self::MaxBytes => "max_bytes",
            Self::Manual => "manual",
            Self::Shutdown => "shutdown",
            Self::RehydrateExpiry => "rehydrate_expiry",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainMode {
    FlushOpen,
    DropOpen,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayPolicy {
    pub enabled: bool,
    pub relay_window_ms: u64,
    pub stale_session_ttl_ms: u64,
    pub rehydrate_grace_ms: u64,
    pub max_open_sessions: usize,
    pub max_fragments_per_session: usize,
    pub max_bytes_per_session: usize,
    pub flush_on_attachment: bool,
    pub flush_on_explicit_final: bool,
    pub flush_on_max_fragments: bool,
    pub flush_on_max_bytes: bool,
    pub storage_mode: RelayStorageMode,
    pub raw_capture_mode: RelayRawCaptureMode,
}

impl RelayPolicy {
    pub fn passthrough() -> Self {
        Self {
            enabled: false,
            relay_window_ms: 0,
            stale_session_ttl_ms: 0,
            rehydrate_grace_ms: 0,
            max_open_sessions: 1_000,
            max_fragments_per_session: 8,
            max_bytes_per_session: 256 * 1024,
            flush_on_attachment: false,
            flush_on_explicit_final: true,
            flush_on_max_fragments: true,
            flush_on_max_bytes: true,
            storage_mode: RelayStorageMode::Memory,
            raw_capture_mode: RelayRawCaptureMode::Bounded,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.max_open_sessions == 0 {
            return Err("max_open_sessions must be > 0".to_string());
        }
        if self.max_fragments_per_session == 0 {
            return Err("max_fragments_per_session must be > 0".to_string());
        }
        if self.max_bytes_per_session == 0 {
            return Err("max_bytes_per_session must be > 0".to_string());
        }
        if self.stale_session_ttl_ms < self.relay_window_ms {
            return Err("stale_session_ttl_ms must be >= relay_window_ms".to_string());
        }
        Ok(())
    }
}

impl Default for RelayPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            relay_window_ms: 2_500,
            stale_session_ttl_ms: 30_000,
            rehydrate_grace_ms: 10_000,
            max_open_sessions: 1_000,
            max_fragments_per_session: 8,
            max_bytes_per_session: 256 * 1024,
            flush_on_attachment: false,
            flush_on_explicit_final: true,
            flush_on_max_fragments: true,
            flush_on_max_bytes: true,
            storage_mode: RelayStorageMode::Memory,
            raw_capture_mode: RelayRawCaptureMode::Bounded,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelayFlushHints {
    #[serde(default)]
    pub final_fragment: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayFragment {
    pub relay_key: String,
    pub fragment_id: String,
    pub received_at_ms: u64,
    #[serde(default)]
    pub content_text: Option<String>,
    #[serde(default)]
    pub attachments: Vec<Value>,
    #[serde(default)]
    pub raw_payload: Option<Value>,
    pub io_context: IoContext,
    pub identity_input: ResolveOrCreateInput,
    #[serde(default)]
    pub flush_hints: RelayFlushHints,
}

impl RelayFragment {
    pub fn estimated_size_bytes(&self) -> usize {
        let text_bytes = self.content_text.as_ref().map(|v| v.len()).unwrap_or(0);
        let attachments_bytes: usize = self.attachments.iter().map(estimate_json_bytes).sum();
        let raw_bytes = self
            .raw_payload
            .as_ref()
            .map(estimate_json_bytes)
            .unwrap_or(0);
        text_bytes + attachments_bytes + raw_bytes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelaySession {
    pub relay_key: String,
    pub opened_at_ms: u64,
    pub last_fragment_at_ms: u64,
    pub deadline_at_ms: u64,
    pub expires_at_ms: u64,
    pub version: u64,
    pub seen_fragment_ids: HashSet<String>,
    pub fragment_count: usize,
    pub byte_count: usize,
    pub contents: Vec<String>,
    pub attachments: Vec<Value>,
    pub raws: Vec<Value>,
    pub io_context: IoContext,
    pub identity_input: ResolveOrCreateInput,
    pub dropped_duplicates: u64,
    pub dropped_over_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayMetadata {
    pub applied: bool,
    pub reason: String,
    pub parts: usize,
    pub window_ms: u64,
    #[serde(default)]
    pub opened_at_ms: Option<u64>,
    #[serde(default)]
    pub last_fragment_at_ms: Option<u64>,
    #[serde(default)]
    pub dropped_duplicates: Option<u64>,
    #[serde(default)]
    pub dropped_over_limit: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssembledTurn {
    pub relay_key: String,
    pub reason: FlushReason,
    pub io_context: IoContext,
    pub identity_input: ResolveOrCreateInput,
    pub payload: Value,
    pub meta_context: Value,
    pub parts: usize,
    pub opened_at_ms: u64,
    pub last_fragment_at_ms: u64,
}

#[derive(Debug, Clone)]
pub enum RelayDecision {
    Hold,
    FlushNow(AssembledTurn),
    DropDuplicate,
    RejectCapacity,
    DropExpired,
}

pub trait RelayStore {
    fn load_session(&self, relay_key: &str) -> Option<RelaySession>;
    fn upsert_session(&mut self, session: RelaySession);
    fn delete_session(&mut self, relay_key: &str) -> Option<RelaySession>;
    fn list_rehydratable(&self, now_ms: u64) -> Vec<RelaySession>;
    fn count_open_sessions(&self) -> usize;
    fn evict_expired(&mut self, now_ms: u64) -> Vec<RelaySession>;
    fn list_open_keys(&self) -> Vec<String>;
}

#[derive(Debug, Default)]
pub struct InMemoryRelayStore {
    sessions: HashMap<String, RelaySession>,
}

impl InMemoryRelayStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl RelayStore for InMemoryRelayStore {
    fn load_session(&self, relay_key: &str) -> Option<RelaySession> {
        self.sessions.get(relay_key).cloned()
    }

    fn upsert_session(&mut self, session: RelaySession) {
        self.sessions.insert(session.relay_key.clone(), session);
    }

    fn delete_session(&mut self, relay_key: &str) -> Option<RelaySession> {
        self.sessions.remove(relay_key)
    }

    fn list_rehydratable(&self, now_ms: u64) -> Vec<RelaySession> {
        self.sessions
            .values()
            .filter(|session| session.expires_at_ms >= now_ms)
            .cloned()
            .collect()
    }

    fn count_open_sessions(&self) -> usize {
        self.sessions.len()
    }

    fn evict_expired(&mut self, now_ms: u64) -> Vec<RelaySession> {
        let expired_keys = self
            .sessions
            .iter()
            .filter_map(|(relay_key, session)| {
                (session.expires_at_ms < now_ms).then(|| relay_key.clone())
            })
            .collect::<Vec<_>>();

        expired_keys
            .into_iter()
            .filter_map(|relay_key| self.sessions.remove(&relay_key))
            .collect()
    }

    fn list_open_keys(&self) -> Vec<String> {
        self.sessions.keys().cloned().collect()
    }
}

#[derive(Debug)]
pub struct RelayBuffer<S> {
    policy: RelayPolicy,
    store: S,
}

impl<S> RelayBuffer<S>
where
    S: RelayStore,
{
    pub fn new(policy: RelayPolicy, store: S) -> Result<Self, String> {
        policy.validate()?;
        Ok(Self { policy, store })
    }

    pub fn policy(&self) -> &RelayPolicy {
        &self.policy
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub fn replace_policy(&mut self, policy: RelayPolicy) -> Result<(), String> {
        policy.validate()?;
        self.policy = policy;
        Ok(())
    }

    pub fn handle_fragment(&mut self, fragment: RelayFragment) -> RelayDecision {
        if !self.policy.enabled || self.policy.relay_window_ms == 0 {
            tracing::debug!(
                relay_key = %fragment.relay_key,
                fragment_id = %fragment.fragment_id,
                "relay passthrough immediate"
            );
            return RelayDecision::FlushNow(self.assemble_passthrough(fragment));
        }

        let now_ms = fragment.received_at_ms;
        let _expired = self.store.evict_expired(now_ms);
        let relay_key = fragment.relay_key.clone();
        let session_exists = self.store.load_session(&relay_key).is_some();
        if !session_exists && self.store.count_open_sessions() >= self.policy.max_open_sessions {
            tracing::warn!(
                relay_key = %relay_key,
                open_sessions = self.store.count_open_sessions(),
                max_open_sessions = self.policy.max_open_sessions,
                "relay capacity rejected"
            );
            return RelayDecision::RejectCapacity;
        }

        let mut session = self.store.delete_session(&relay_key).unwrap_or_else(|| {
            tracing::debug!(
                relay_key = %relay_key,
                fragment_id = %fragment.fragment_id,
                "relay session opened"
            );
            self.new_session(&fragment)
        });

        if session.expires_at_ms < now_ms {
            tracing::warn!(
                relay_key = %relay_key,
                expires_at_ms = session.expires_at_ms,
                now_ms = now_ms,
                "relay session expired before fragment handling"
            );
            return RelayDecision::DropExpired;
        }

        if !session
            .seen_fragment_ids
            .insert(fragment.fragment_id.clone())
        {
            session.dropped_duplicates += 1;
            tracing::debug!(
                relay_key = %relay_key,
                fragment_id = %fragment.fragment_id,
                dropped_duplicates = session.dropped_duplicates,
                "relay duplicate fragment dropped"
            );
            self.store.upsert_session(session);
            return RelayDecision::DropDuplicate;
        }

        let fragment_size = fragment.estimated_size_bytes();
        let next_bytes = session.byte_count.saturating_add(fragment_size);
        if next_bytes > self.policy.max_bytes_per_session {
            session.dropped_over_limit += 1;
            if self.policy.flush_on_max_bytes && session.fragment_count > 0 {
                tracing::warn!(
                    relay_key = %relay_key,
                    next_bytes = next_bytes,
                    max_bytes_per_session = self.policy.max_bytes_per_session,
                    "relay max bytes reached; flushing current session"
                );
                self.store.upsert_session(session);
                return self
                    .flush_session(&relay_key, FlushReason::MaxBytes)
                    .map(RelayDecision::FlushNow)
                    .unwrap_or(RelayDecision::DropExpired);
            }
            tracing::warn!(
                relay_key = %relay_key,
                next_bytes = next_bytes,
                max_bytes_per_session = self.policy.max_bytes_per_session,
                "relay fragment dropped over byte limit"
            );
            self.store.upsert_session(session);
            return RelayDecision::DropExpired;
        }

        session.last_fragment_at_ms = now_ms;
        session.deadline_at_ms = now_ms.saturating_add(self.policy.relay_window_ms);
        session.expires_at_ms = now_ms.saturating_add(self.policy.stale_session_ttl_ms);
        session.version = session.version.wrapping_add(1);
        session.fragment_count = session.fragment_count.saturating_add(1);
        session.byte_count = next_bytes;

        let has_new_attachments = !fragment.attachments.is_empty();

        if let Some(content) = fragment.content_text {
            session.contents.push(content);
        }
        session.attachments.extend(fragment.attachments);
        if matches!(self.policy.raw_capture_mode, RelayRawCaptureMode::Bounded) {
            if let Some(raw) = fragment.raw_payload {
                session.raws.push(raw);
            }
        }
        tracing::debug!(
            relay_key = %relay_key,
            fragments = session.fragment_count,
            bytes = session.byte_count,
            deadline_at_ms = session.deadline_at_ms,
            "relay fragment buffered"
        );

        let flush_reason =
            if self.policy.flush_on_explicit_final && fragment.flush_hints.final_fragment {
                Some(FlushReason::ExplicitFinal)
            } else if self.policy.flush_on_attachment && has_new_attachments {
                Some(FlushReason::AttachmentBoundary)
            } else if self.policy.flush_on_max_fragments
                && session.fragment_count >= self.policy.max_fragments_per_session
            {
                Some(FlushReason::MaxFragments)
            } else {
                None
            };

        self.store.upsert_session(session);

        if let Some(reason) = flush_reason {
            self.flush_session(&relay_key, reason)
                .map(RelayDecision::FlushNow)
                .unwrap_or(RelayDecision::DropExpired)
        } else {
            RelayDecision::Hold
        }
    }

    pub fn flush_session(&mut self, relay_key: &str, reason: FlushReason) -> Option<AssembledTurn> {
        let session = self.store.delete_session(relay_key)?;
        tracing::debug!(
            relay_key = %relay_key,
            reason = reason.as_str(),
            parts = session.fragment_count,
            bytes = session.byte_count,
            "relay session flushed"
        );
        Some(self.assemble_session(session, reason))
    }

    pub fn flush_expired(&mut self, now_ms: u64) -> Vec<AssembledTurn> {
        let ready_keys = self
            .store
            .list_open_keys()
            .into_iter()
            .filter(|relay_key| {
                self.store
                    .load_session(relay_key)
                    .is_some_and(|session| session.deadline_at_ms <= now_ms)
            })
            .collect::<Vec<_>>();

        ready_keys
            .into_iter()
            .filter_map(|relay_key| self.flush_session(&relay_key, FlushReason::WindowElapsed))
            .collect()
    }

    pub fn rehydrate_on_startup(&mut self, now_ms: u64) -> Vec<RelaySession> {
        if !matches!(self.policy.storage_mode, RelayStorageMode::Durable) {
            return Vec::new();
        }
        self.store
            .list_rehydratable(now_ms)
            .into_iter()
            .filter(|session| {
                session.expires_at_ms >= now_ms
                    && session
                        .last_fragment_at_ms
                        .saturating_add(self.policy.rehydrate_grace_ms)
                        >= now_ms
            })
            .collect()
    }

    pub fn drain_on_shutdown(&mut self, mode: DrainMode) -> Vec<AssembledTurn> {
        match mode {
            DrainMode::DropOpen => {
                let open_keys = self.store.list_open_keys();
                if !open_keys.is_empty() {
                    tracing::warn!(
                        open_sessions = open_keys.len(),
                        "relay shutdown dropping open sessions"
                    );
                }
                for relay_key in open_keys {
                    let _ = self.store.delete_session(&relay_key);
                }
                Vec::new()
            }
            DrainMode::FlushOpen => {
                let open_keys = self.store.list_open_keys();
                if !open_keys.is_empty() {
                    tracing::info!(
                        open_sessions = open_keys.len(),
                        "relay shutdown flushing open sessions"
                    );
                }
                open_keys
                    .into_iter()
                    .filter_map(|relay_key| self.flush_session(&relay_key, FlushReason::Shutdown))
                    .collect()
            }
        }
    }

    fn new_session(&self, fragment: &RelayFragment) -> RelaySession {
        let now_ms = fragment.received_at_ms;
        RelaySession {
            relay_key: fragment.relay_key.clone(),
            opened_at_ms: now_ms,
            last_fragment_at_ms: now_ms,
            deadline_at_ms: now_ms.saturating_add(self.policy.relay_window_ms),
            expires_at_ms: now_ms.saturating_add(self.policy.stale_session_ttl_ms),
            version: 0,
            seen_fragment_ids: HashSet::new(),
            fragment_count: 0,
            byte_count: 0,
            contents: Vec::new(),
            attachments: Vec::new(),
            raws: Vec::new(),
            io_context: fragment.io_context.clone(),
            identity_input: fragment.identity_input.clone(),
            dropped_duplicates: 0,
            dropped_over_limit: 0,
        }
    }

    fn assemble_passthrough(&self, fragment: RelayFragment) -> AssembledTurn {
        let relay_metadata = RelayMetadata {
            applied: true,
            reason: FlushReason::Manual.as_str().to_string(),
            parts: 1,
            window_ms: self.policy.relay_window_ms,
            opened_at_ms: Some(fragment.received_at_ms),
            last_fragment_at_ms: Some(fragment.received_at_ms),
            dropped_duplicates: Some(0),
            dropped_over_limit: Some(0),
        };
        let meta_context = wrap_with_relay(&fragment.io_context, &relay_metadata);
        let payload = assembled_payload(
            fragment.content_text.into_iter().collect(),
            fragment.attachments,
            fragment.raw_payload.map(|v| vec![v]).unwrap_or_default(),
        );

        AssembledTurn {
            relay_key: fragment.relay_key,
            reason: FlushReason::Manual,
            io_context: fragment.io_context,
            identity_input: fragment.identity_input,
            payload,
            meta_context,
            parts: 1,
            opened_at_ms: fragment.received_at_ms,
            last_fragment_at_ms: fragment.received_at_ms,
        }
    }

    fn assemble_session(&self, session: RelaySession, reason: FlushReason) -> AssembledTurn {
        let relay_metadata = RelayMetadata {
            applied: true,
            reason: reason.as_str().to_string(),
            parts: session.fragment_count,
            window_ms: self.policy.relay_window_ms,
            opened_at_ms: Some(session.opened_at_ms),
            last_fragment_at_ms: Some(session.last_fragment_at_ms),
            dropped_duplicates: Some(session.dropped_duplicates),
            dropped_over_limit: Some(session.dropped_over_limit),
        };
        let meta_context = wrap_with_relay(&session.io_context, &relay_metadata);
        let payload = assembled_payload(session.contents, session.attachments, session.raws);

        AssembledTurn {
            relay_key: session.relay_key,
            reason,
            io_context: session.io_context,
            identity_input: session.identity_input,
            payload,
            meta_context,
            parts: relay_metadata.parts,
            opened_at_ms: relay_metadata.opened_at_ms.unwrap_or_default(),
            last_fragment_at_ms: relay_metadata.last_fragment_at_ms.unwrap_or_default(),
        }
    }
}

fn assembled_payload(contents: Vec<String>, attachments: Vec<Value>, raws: Vec<Value>) -> Value {
    let mut payload = serde_json::json!({
        "type": "text",
        "content": contents.join("\n"),
        "attachments": attachments,
    });

    if !raws.is_empty() {
        payload["raw"] = serde_json::json!({ "relay_batch": raws });
    }

    payload
}

fn wrap_with_relay(io_context: &IoContext, relay_metadata: &RelayMetadata) -> Value {
    let mut meta_context = wrap_in_meta_context(io_context);
    meta_context["io"]["relay"] =
        serde_json::to_value(relay_metadata).expect("relay metadata should serialize");
    meta_context
}

fn estimate_json_bytes(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io_context::slack_inbound_io_context;

    fn test_fragment(
        relay_key: &str,
        fragment_id: &str,
        received_at_ms: u64,
        content: &str,
    ) -> RelayFragment {
        RelayFragment {
            relay_key: relay_key.to_string(),
            fragment_id: fragment_id.to_string(),
            received_at_ms,
            content_text: Some(content.to_string()),
            attachments: Vec::new(),
            raw_payload: Some(serde_json::json!({ "id": fragment_id })),
            io_context: slack_inbound_io_context("T1", "U1", "C1", None, fragment_id),
            identity_input: ResolveOrCreateInput {
                channel: "slack".to_string(),
                external_id: "T1:U1".to_string(),
                tenant_id: None,
                tenant_hint: Some("T1".to_string()),
                attributes: serde_json::json!({ "team_id": "T1" }),
            },
            flush_hints: RelayFlushHints::default(),
        }
    }

    fn test_policy() -> RelayPolicy {
        RelayPolicy {
            relay_window_ms: 100,
            stale_session_ttl_ms: 500,
            max_open_sessions: 2,
            max_fragments_per_session: 3,
            max_bytes_per_session: 1_024,
            ..RelayPolicy::default()
        }
    }

    #[test]
    fn passthrough_when_disabled() {
        let mut policy = RelayPolicy::passthrough();
        policy.enabled = false;
        let mut relay = RelayBuffer::new(policy, InMemoryRelayStore::new()).expect("relay");

        let decision = relay.handle_fragment(test_fragment("k1", "f1", 10, "hola"));
        let RelayDecision::FlushNow(turn) = decision else {
            panic!("unexpected decision");
        };
        assert_eq!(turn.parts, 1);
        assert_eq!(turn.payload["content"], "hola");
        assert_eq!(turn.meta_context["io"]["relay"]["applied"], true);
    }

    #[test]
    fn holds_then_flushes_on_window() {
        let mut relay = RelayBuffer::new(test_policy(), InMemoryRelayStore::new()).expect("relay");

        let d1 = relay.handle_fragment(test_fragment("k1", "f1", 10, "hola"));
        assert!(matches!(d1, RelayDecision::Hold));
        let d2 = relay.handle_fragment(test_fragment("k1", "f2", 20, "mundo"));
        assert!(matches!(d2, RelayDecision::Hold));

        let flushed = relay.flush_expired(121);
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].payload["content"], "hola\nmundo");
        assert_eq!(
            flushed[0].meta_context["io"]["relay"]["reason"],
            "window_elapsed"
        );
        assert_eq!(flushed[0].meta_context["io"]["relay"]["parts"], 2);
    }

    #[test]
    fn drops_duplicate_fragment_in_same_session() {
        let mut relay = RelayBuffer::new(test_policy(), InMemoryRelayStore::new()).expect("relay");

        assert!(matches!(
            relay.handle_fragment(test_fragment("k1", "f1", 10, "hola")),
            RelayDecision::Hold
        ));
        assert!(matches!(
            relay.handle_fragment(test_fragment("k1", "f1", 20, "hola otra vez")),
            RelayDecision::DropDuplicate
        ));

        let flushed = relay.flush_expired(111);
        assert_eq!(flushed.len(), 1);
        assert_eq!(flushed[0].payload["content"], "hola");
        assert_eq!(
            flushed[0].meta_context["io"]["relay"]["dropped_duplicates"],
            1
        );
    }

    #[test]
    fn flushes_on_max_fragments() {
        let mut relay = RelayBuffer::new(test_policy(), InMemoryRelayStore::new()).expect("relay");

        assert!(matches!(
            relay.handle_fragment(test_fragment("k1", "f1", 10, "a")),
            RelayDecision::Hold
        ));
        assert!(matches!(
            relay.handle_fragment(test_fragment("k1", "f2", 20, "b")),
            RelayDecision::Hold
        ));

        let decision = relay.handle_fragment(test_fragment("k1", "f3", 30, "c"));
        let RelayDecision::FlushNow(turn) = decision else {
            panic!("expected flush");
        };
        assert_eq!(turn.reason, FlushReason::MaxFragments);
        assert_eq!(turn.payload["content"], "a\nb\nc");
    }

    #[test]
    fn rejects_capacity_for_new_session() {
        let mut policy = test_policy();
        policy.max_open_sessions = 1;
        let mut relay = RelayBuffer::new(policy, InMemoryRelayStore::new()).expect("relay");

        assert!(matches!(
            relay.handle_fragment(test_fragment("k1", "f1", 10, "a")),
            RelayDecision::Hold
        ));
        assert!(matches!(
            relay.handle_fragment(test_fragment("k2", "f2", 20, "b")),
            RelayDecision::RejectCapacity
        ));
    }

    #[test]
    fn replace_policy_updates_runtime_settings() {
        let mut relay = RelayBuffer::new(test_policy(), InMemoryRelayStore::new()).expect("relay");
        let mut next = relay.policy().clone();
        next.relay_window_ms = 250;
        next.max_open_sessions = 4;

        relay.replace_policy(next.clone()).expect("replace policy");

        assert_eq!(relay.policy(), &next);
    }
}
