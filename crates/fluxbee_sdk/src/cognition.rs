use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// v2 (A-70 stage 2): a communication is `(thread_id, agent ILK)`, and every envelope carries the
/// communication's turn number, which orders it (docs/router-stream-v1.md §6.2).
pub const COGNITION_DURABLE_SCHEMA_VERSION: u16 = 2;
pub const COGNITION_DURABLE_ENTITY_VERSION_V1: u16 = 1;

/// SY.cognition to `SY.storage@motherbee`: the state of one communication, for its rebuild.
pub const MSG_COGNITION_STATE_GET: &str = "COGNITION_STATE_GET";
pub const MSG_COGNITION_STATE_RESPONSE: &str = "COGNITION_STATE_RESPONSE";

pub const SUBJECT_STORAGE_COGNITION_THREADS: &str = "storage.cognition.threads";
pub const SUBJECT_STORAGE_COGNITION_CONTEXTS: &str = "storage.cognition.contexts";
pub const SUBJECT_STORAGE_COGNITION_REASONS: &str = "storage.cognition.reasons";
pub const SUBJECT_STORAGE_COGNITION_COOCCURRENCES: &str = "storage.cognition.cooccurrences";
pub const SUBJECT_STORAGE_COGNITION_SCOPES: &str = "storage.cognition.scopes";
pub const SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES: &str = "storage.cognition.scope_instances";
pub const SUBJECT_STORAGE_COGNITION_MEMORIES: &str = "storage.cognition.memories";
pub const SUBJECT_STORAGE_COGNITION_EPISODES: &str = "storage.cognition.episodes";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionDurableEntity {
    Thread,
    Context,
    Reason,
    Cooccurrence,
    Scope,
    ScopeInstance,
    Memory,
    Episode,
}

impl CognitionDurableEntity {
    pub fn subject(self) -> &'static str {
        match self {
            Self::Thread => SUBJECT_STORAGE_COGNITION_THREADS,
            Self::Context => SUBJECT_STORAGE_COGNITION_CONTEXTS,
            Self::Reason => SUBJECT_STORAGE_COGNITION_REASONS,
            Self::Cooccurrence => SUBJECT_STORAGE_COGNITION_COOCCURRENCES,
            Self::Scope => SUBJECT_STORAGE_COGNITION_SCOPES,
            Self::ScopeInstance => SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES,
            Self::Memory => SUBJECT_STORAGE_COGNITION_MEMORIES,
            Self::Episode => SUBJECT_STORAGE_COGNITION_EPISODES,
        }
    }

    pub fn requires_thread_id(self) -> bool {
        matches!(
            self,
            Self::Thread
                | Self::Context
                | Self::Reason
                | Self::Cooccurrence
                | Self::ScopeInstance
                | Self::Memory
                | Self::Episode
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CognitionDurableOp {
    Upsert,
    Close,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitionDurableEnvelope<T> {
    pub schema_version: u16,
    pub entity_version: u16,
    pub entity: CognitionDurableEntity,
    pub op: CognitionDurableOp,
    pub entity_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// The agent that owns the communication: with `thread_id`, the communication's identity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_ilk: Option<String>,
    /// The communication's turn number when this was written: a stored row is replaced only by an
    /// envelope of an equal or later turn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    pub hive: String,
    pub writer: String,
    pub ts: String,
    pub data: T,
}

/// The communication an envelope belongs to: `None` for a scope, which is shared by its instances.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CognitionCommunicationRef {
    pub thread_id: String,
    pub agent_ilk: String,
    pub turn: u64,
}

impl<T> CognitionDurableEnvelope<T> {
    pub fn new(
        entity: CognitionDurableEntity,
        op: CognitionDurableOp,
        entity_id: impl Into<String>,
        communication: Option<&CognitionCommunicationRef>,
        hive: impl Into<String>,
        writer: impl Into<String>,
        ts: impl Into<String>,
        data: T,
    ) -> Self {
        Self {
            schema_version: COGNITION_DURABLE_SCHEMA_VERSION,
            entity_version: COGNITION_DURABLE_ENTITY_VERSION_V1,
            entity,
            op,
            entity_id: entity_id.into(),
            thread_id: communication.map(|c| c.thread_id.clone()),
            agent_ilk: communication.map(|c| c.agent_ilk.clone()),
            turn: communication.map(|c| c.turn),
            hive: hive.into(),
            writer: writer.into(),
            ts: ts.into(),
            data,
        }
    }
}

/// The payload of a `storage.cognition.batch` record: envelopes of one turn, packed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionEnvelopeBatch {
    pub envelopes: Vec<serde_json::Value>,
}

/// The payload of `COGNITION_STATE_GET`: one page of a communication's state. `after` is the last
/// `(entity, entity_id)` of the previous page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CognitionStateGet {
    pub thread_id: String,
    pub agent_ilk: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<(String, String)>,
}

/// The payload of `COGNITION_STATE_RESPONSE`: envelopes in the shape SY.cognition writes them.
/// `next` is set when more pages follow. An empty first page means the communication has no
/// state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionStateResponse {
    pub ok: bool,
    #[serde(default)]
    pub envelopes: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CognitionIlkProfile {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub as_sender: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub as_receiver: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionThreadData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_ilk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_ilk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ich: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_count: Option<u64>,
    /// The communication's last turns, oldest first, as its memory package carries them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<crate::protocol::MemoryTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionContextData {
    pub label: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_avg_cumulative: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_avg_ema: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_samples: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ilk_weights: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ilk_profile: BTreeMap<String, CognitionIlkProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionReasonData {
    pub label: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_avg_cumulative: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_avg_ema: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_samples: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals_canonical: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signals_extra: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ilk_weights: BTreeMap<String, f64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ilk_profile: BTreeMap<String, CognitionIlkProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionCooccurrenceData {
    pub context_id: String,
    pub context_label: String,
    pub reason_id: String,
    pub reason_label: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_avg_cumulative: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_avg_ema: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight_samples: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionScopeData {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binding_energy_ema: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionScopeInstanceData {
    pub scope_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason_id: Option<String>,
    /// The communication's turn numbers it spans (the turn replaced `thread_seq`, §6.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_turn: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_turn: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opened_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionMemoryData {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weight: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dominant_reason_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ilk_weights: BTreeMap<String, f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CognitionEpisodeData {
    pub affect_id: String,
    pub title: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_intensity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_strength: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_context_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_reason_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_signals: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intensity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

const fn is_zero(value: &u64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_entity_subject_mapping_is_stable() {
        assert_eq!(
            CognitionDurableEntity::Thread.subject(),
            SUBJECT_STORAGE_COGNITION_THREADS
        );
        assert_eq!(
            CognitionDurableEntity::ScopeInstance.subject(),
            SUBJECT_STORAGE_COGNITION_SCOPE_INSTANCES
        );
        assert!(CognitionDurableEntity::Episode.requires_thread_id());
        assert!(!CognitionDurableEntity::Scope.requires_thread_id());
    }

    #[test]
    fn durable_envelope_serializes_expected_shape() {
        let envelope = CognitionDurableEnvelope::new(
            CognitionDurableEntity::Context,
            CognitionDurableOp::Upsert,
            "context:123",
            Some(&CognitionCommunicationRef {
                thread_id: "thread:sha256:abc".to_string(),
                agent_ilk: "ilk:agent".to_string(),
                turn: 7,
            }),
            "motherbee",
            "SY.cognition@motherbee",
            "2026-03-31T12:34:56Z",
            CognitionContextData {
                label: "billing dispute".to_string(),
                status: "open".to_string(),
                tags: vec!["billing".to_string(), "refund".to_string()],
                ..CognitionContextData::default()
            },
        );

        let encoded = serde_json::to_value(&envelope).expect("serialize");
        assert_eq!(
            encoded.get("entity").and_then(|v| v.as_str()),
            Some("context")
        );
        assert_eq!(encoded.get("op").and_then(|v| v.as_str()), Some("upsert"));
        assert_eq!(
            encoded.get("thread_id").and_then(|v| v.as_str()),
            Some("thread:sha256:abc")
        );
        assert_eq!(
            encoded.get("agent_ilk").and_then(|v| v.as_str()),
            Some("ilk:agent")
        );
        assert_eq!(encoded.get("turn").and_then(|v| v.as_u64()), Some(7));
        assert_eq!(
            encoded.get("schema_version").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            encoded
                .get("data")
                .and_then(|v| v.get("label"))
                .and_then(|v| v.as_str()),
            Some("billing dispute")
        );
    }
}
