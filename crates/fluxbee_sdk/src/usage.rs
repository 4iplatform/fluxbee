//! LLM usage records and their query (A-70 stage 3, docs/router-stream-v1.md §7). Every model call
//! leaves one record in `storage.usage` through its router's stream; SY.storage keeps them in
//! `llm_usage` and answers `USAGE_QUERY` for SY.admin, which serves the operator, Archi and Cloud.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// SY.admin's question to SY.storage about the records. The answer is `MSG_USAGE_QUERY_RESPONSE`
/// with a [`UsageQueryResponse`].
pub const MSG_USAGE_QUERY: &str = "USAGE_QUERY";
pub const MSG_USAGE_QUERY_RESPONSE: &str = "USAGE_QUERY_RESPONSE";
/// The longest period a query by `ts` may cover.
pub const USAGE_QUERY_MAX_DAYS: i64 = 92;
/// Rows per page when a query names none, and the most it may ask for; SY.storage also keeps
/// every page under 64 KB.
pub const USAGE_QUERY_DEFAULT_LIMIT: u32 = 500;
pub const USAGE_QUERY_MAX_LIMIT: u32 = 1000;
/// A record's text fields, and each `work` value, are at most this long; longer is refused.
pub const USAGE_FIELD_MAX_CHARS: usize = 256;
/// The provider's raw usage object is kept up to this size; a larger one is left out.
pub const USAGE_RAW_MAX_BYTES: usize = 2 * 1024;

/// Why a record charges its tenant (§7.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TenantSource {
    /// A tenant's node pays its own tenant.
    Node,
    /// SY.cognition pays with the communication's agent's tenant.
    Agent,
    /// SY.frontdesk.gov pays with the case's tenant.
    Case,
    /// SY.architect and SY.admin's executor pay with the root tenant.
    Root,
    /// The tenant a rule names was unknown: the root tenant pays.
    FallbackRoot,
}

impl TenantSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Agent => "agent",
            Self::Case => "case",
            Self::Root => "root",
            Self::FallbackRoot => "fallback_root",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    /// The plain path (`LlmClient::generate`).
    Generate,
    /// One turn of a tool loop (`FunctionCallingModel::run_turn`).
    FunctionTurn,
}

impl UsageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generate => "generate",
            Self::FunctionTurn => "function_turn",
        }
    }
}

/// How a call ended. Only `abandoned` and `unreadable` come without counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageOutcome {
    Ok,
    /// The answer was cut at the output limit.
    Truncated,
    /// The model refused (Anthropic `stop_reason: refusal`).
    Refused,
    /// A 2xx whose body could not be read: billed, counts unknown.
    Unreadable,
    /// The caller dropped the call before it answered: maybe billed, counts unknown.
    Abandoned,
}

impl UsageOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Truncated => "truncated",
            Self::Refused => "refused",
            Self::Unreadable => "unreadable",
            Self::Abandoned => "abandoned",
        }
    }
}

/// What a call's work belongs to, when its node knows (Archi's session, pipeline run, solution and
/// stage; SY.admin's executor run and its caller). Fixed keys, short values.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageWork {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<String>,
}

impl UsageWork {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn values(&self) -> [Option<&String>; 6] {
        [
            self.session_id.as_ref(),
            self.pipeline_run_id.as_ref(),
            self.solution_id.as_ref(),
            self.stage.as_ref(),
            self.execution_id.as_ref(),
            self.caller.as_ref(),
        ]
    }
}

/// One model call (§7.4). The counts are normalized across providers: `uncached_input` +
/// `cache_read` + `cache_write` is every input token, and `output` includes the reasoning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub record_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_response_id: Option<String>,
    /// When the answer arrived (or the call was dropped), RFC 3339 UTC, the producer's clock.
    pub ts: String,
    pub hive: String,
    pub node_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ilk: Option<String>,
    pub tenant_id: String,
    pub tenant_source: TenantSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counterpart_ilk: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub kind: UsageKind,
    pub outcome: UsageOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncached_input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_usage: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<UsageWork>,
}

impl UsageRecord {
    /// Why this record cannot be stored as it is, if it cannot: an empty or overlong field, a raw
    /// usage past its cap. SY.storage refuses such a record (§7.5).
    pub fn defect(&self) -> Option<String> {
        let required = [
            ("record_id", &self.record_id),
            ("ts", &self.ts),
            ("hive", &self.hive),
            ("node_name", &self.node_name),
            ("tenant_id", &self.tenant_id),
            ("provider", &self.provider),
            ("model", &self.model),
        ];
        for (name, value) in required {
            if value.trim().is_empty() {
                return Some(format!("{name} is empty"));
            }
            if value.chars().count() > USAGE_FIELD_MAX_CHARS {
                return Some(format!("{name} is too long"));
            }
        }
        let optional = [
            ("provider_response_id", self.provider_response_id.as_ref()),
            ("node_ilk", self.node_ilk.as_ref()),
            ("counterpart_ilk", self.counterpart_ilk.as_ref()),
            ("served_model", self.served_model.as_ref()),
            ("service_tier", self.service_tier.as_ref()),
            ("trace_id", self.trace_id.as_ref()),
            ("thread_id", self.thread_id.as_ref()),
        ];
        for (name, value) in optional {
            if value.is_some_and(|value| value.chars().count() > USAGE_FIELD_MAX_CHARS) {
                return Some(format!("{name} is too long"));
            }
        }
        if let Some(work) = &self.work {
            if work
                .values()
                .into_iter()
                .flatten()
                .any(|value| value.chars().count() > USAGE_FIELD_MAX_CHARS)
            {
                return Some("a work value is too long".to_string());
            }
        }
        if let Some(raw) = &self.raw_usage {
            if serde_json::to_vec(raw).map_or(true, |bytes| bytes.len() > USAGE_RAW_MAX_BYTES) {
                return Some("raw_usage is too large".to_string());
            }
        }
        None
    }
}

/// How `USAGE_QUERY` groups the records of a period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageGroupBy {
    Day,
    Model,
    Node,
    Tenant,
}

impl UsageGroupBy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "day" => Some(Self::Day),
            "model" => Some(Self::Model),
            "node" => Some(Self::Node),
            "tenant" => Some(Self::Tenant),
            _ => None,
        }
    }
}

/// A question about the records (§7.5). Either a period (`from`, `to`, at most
/// [`USAGE_QUERY_MAX_DAYS`]) grouped by `group_by` or listed record by record, or, with `since`,
/// the records stored after a cursor, for a reader that syncs as they arrive. `after` asks for the
/// next page of the same question.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    /// Every tenant's records. Only SY.admin sets it, for the operator and Archi.
    #[serde(default)]
    pub all_tenants: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_by: Option<UsageGroupBy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// One group of a period: its calls and the five sums.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageGroup {
    pub key: String,
    pub calls: u64,
    pub uncached_input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
    pub reasoning: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageQueryResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<UsageGroup>,
    /// Stored records: a [`UsageRecord`] plus `stored_at` and `verified`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub records: Vec<Value>,
    /// The cursor for the next page (`after`), or for the next sync (`since`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// With `error`: `INVALID_REQUEST` (the question), `FORBIDDEN` (the caller) or
    /// `USAGE_UNAVAILABLE` (no database, a query that failed or another one running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record() -> UsageRecord {
        UsageRecord {
            record_id: "8b6b3c1e-0000-4000-8000-000000000001".to_string(),
            provider_response_id: Some("msg_01".to_string()),
            ts: "2026-10-17T12:00:00Z".to_string(),
            hive: "motherbee".to_string(),
            node_name: "AI.sales@motherbee".to_string(),
            node_ilk: Some("ilk:11111111-1111-4111-8111-111111111111".to_string()),
            tenant_id: "tnt:22222222-2222-4222-8222-222222222222".to_string(),
            tenant_source: TenantSource::Node,
            counterpart_ilk: None,
            provider: "anthropic".to_string(),
            model: "claude-sonnet-5-5".to_string(),
            served_model: Some("claude-sonnet-5-5".to_string()),
            service_tier: None,
            kind: UsageKind::FunctionTurn,
            outcome: UsageOutcome::Ok,
            uncached_input: Some(1200),
            cache_read: Some(0),
            cache_write: Some(0),
            output: Some(80),
            reasoning: None,
            raw_usage: Some(json!({"input_tokens": 1200, "output_tokens": 80})),
            trace_id: None,
            thread_id: None,
            work: None,
        }
    }

    #[test]
    fn a_record_round_trips_with_snake_case_enums() {
        let value = serde_json::to_value(record()).unwrap();
        assert_eq!(value["tenant_source"], json!("node"));
        assert_eq!(value["kind"], json!("function_turn"));
        assert_eq!(value["outcome"], json!("ok"));
        assert!(value.get("reasoning").is_none());
        let back: UsageRecord = serde_json::from_value(value).unwrap();
        assert_eq!(back, record());
        let fallback = serde_json::to_value(TenantSource::FallbackRoot).unwrap();
        assert_eq!(fallback, json!("fallback_root"));
    }

    #[test]
    fn a_defective_record_says_why() {
        assert_eq!(record().defect(), None);
        let mut empty = record();
        empty.tenant_id = " ".to_string();
        assert_eq!(empty.defect().as_deref(), Some("tenant_id is empty"));
        let mut long = record();
        long.work = Some(UsageWork {
            stage: Some("x".repeat(USAGE_FIELD_MAX_CHARS + 1)),
            ..UsageWork::default()
        });
        assert!(long.defect().is_some());
        let mut raw = record();
        raw.raw_usage = Some(json!({"pad": "x".repeat(USAGE_RAW_MAX_BYTES)}));
        assert_eq!(raw.defect().as_deref(), Some("raw_usage is too large"));
    }

    #[test]
    fn grouping_is_a_closed_list() {
        assert_eq!(UsageGroupBy::parse("day"), Some(UsageGroupBy::Day));
        assert_eq!(UsageGroupBy::parse("tenant"), Some(UsageGroupBy::Tenant));
        assert_eq!(UsageGroupBy::parse("model; DROP TABLE x"), None);
        let query: UsageQuery =
            serde_json::from_value(json!({"tenant_id": "tnt:x", "group_by": "model"})).unwrap();
        assert_eq!(query.group_by, Some(UsageGroupBy::Model));
        assert!(!query.all_tenants);
    }
}
