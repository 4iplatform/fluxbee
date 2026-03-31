use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const COGNITION_DURABLE_SCHEMA_VERSION: u16 = 1;
pub const COGNITION_DURABLE_ENTITY_VERSION_V1: u16 = 1;

pub const SUBJECT_STORAGE_COGNITION_THREADS: &str = "storage.cognition.threads";
pub const SUBJECT_STORAGE_COGNITION_CONTEXTS: &str = "storage.cognition.contexts";
pub const SUBJECT_STORAGE_COGNITION_REASONS: &str = "storage.cognition.reasons";
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
    pub hive: String,
    pub writer: String,
    pub ts: String,
    pub data: T,
}

impl<T> CognitionDurableEnvelope<T> {
    pub fn new(
        entity: CognitionDurableEntity,
        op: CognitionDurableOp,
        entity_id: impl Into<String>,
        thread_id: Option<String>,
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
            thread_id,
            hive: hive.into(),
            writer: writer.into(),
            ts: ts.into(),
            data,
        }
    }
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
    pub latest_thread_seq: Option<u64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_thread_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_thread_seq: Option<u64>,
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
            Some("thread:sha256:abc".to_string()),
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
            encoded.get("schema_version").and_then(|v| v.as_u64()),
            Some(1)
        );
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
            encoded
                .get("data")
                .and_then(|v| v.get("label"))
                .and_then(|v| v.as_str()),
            Some("billing dispute")
        );
    }
}
