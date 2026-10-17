//! SY.storage's inbox metrics over the mesh (A-70 stage 2b, docs/router-stream-v1.md §6.3).
//! SY.admin and SY.orchestrator ask `SY.storage@<primary hive>` with a system message; until the
//! embedded NATS broker went, they asked on `storage.metrics.get`.

use serde::{Deserialize, Serialize};

/// Asks SY.storage for its inbox metrics. The answer is `MSG_STORAGE_METRICS_RESPONSE`, with a
/// [`StorageMetricsResponse`] payload.
pub const MSG_STORAGE_METRICS_GET: &str = "STORAGE_METRICS_GET";
pub const MSG_STORAGE_METRICS_RESPONSE: &str = "STORAGE_METRICS_RESPONSE";

/// SY.storage's inbox: what waits to be processed, what failed, and how much it processed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageInboxMetrics {
    pub pending: i64,
    pub pending_with_error: i64,
    pub oldest_pending_age_s: i64,
    pub processed_total: i64,
    pub max_attempts: i32,
}

/// `ok` with the metrics when SY.storage could read its database; otherwise `error` says why.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StorageMetricsResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<StorageInboxMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
