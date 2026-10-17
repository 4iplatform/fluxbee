//! Usage records for every model call (A-70 stage 3, docs/router-stream-v1.md §7.2). A client is
//! built with an [`AiAccount`]: who spends, who pays and for what. Each 2xx answer leaves one
//! [`UsageRecord`] in the node's router stream (`storage.usage`) through the process's
//! [`UsageRecorder`], so a record never waits on its caller nor fails it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::SecondsFormat;
use fluxbee_sdk::stream::SUBJECT_STORAGE_USAGE;
use fluxbee_sdk::{
    RouterDispatcher, TenantSource, UsageKind, UsageOutcome, UsageRecord, UsageWork,
    DEFAULT_ROOT_TENANT_ID, USAGE_FIELD_MAX_CHARS, USAGE_RAW_MAX_BYTES,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::llm::AiProvider;

/// Records waiting for the recorder's task; past it a record is dropped and counted.
pub const USAGE_RECORDER_QUEUE: usize = 1_024;

/// What a recorder did since it started.
#[derive(Debug, Default)]
pub struct UsageRecorderStats {
    pub queued: AtomicU64,
    /// Dropped because the queue was full.
    pub dropped: AtomicU64,
    /// Taken from the queue but not appended to the stream (no router, a send error).
    pub append_failures: AtomicU64,
}

/// One per process: a task appends each record to the node's router stream, fed by a bounded
/// channel through a synchronous `try_send`, so a record can be queued from anywhere, `Drop`
/// included.
#[derive(Clone)]
pub struct UsageRecorder {
    tx: Option<mpsc::Sender<UsageRecord>>,
    stats: Arc<UsageRecorderStats>,
}

impl std::fmt::Debug for UsageRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageRecorder")
            .field("enabled", &self.tx.is_some())
            .field("stats", &self.stats)
            .finish()
    }
}

impl UsageRecorder {
    /// Starts the recorder's task on the current Tokio runtime. It takes the dispatcher's current
    /// sender for each record, so it follows the node's reconnections.
    pub fn start(dispatcher: Arc<RouterDispatcher>) -> Self {
        let (tx, mut rx) = mpsc::channel::<UsageRecord>(USAGE_RECORDER_QUEUE);
        let stats = Arc::new(UsageRecorderStats::default());
        let task_stats = Arc::clone(&stats);
        tokio::spawn(async move {
            while let Some(record) = rx.recv().await {
                let appended = match serde_json::to_value(&record) {
                    Ok(payload) => fluxbee_sdk::stream::append(
                        &dispatcher.sender_snapshot(),
                        SUBJECT_STORAGE_USAGE,
                        payload,
                    )
                    .await
                    .map_err(|err| err.to_string()),
                    Err(err) => Err(err.to_string()),
                };
                if let Err(err) = appended {
                    let failures = task_stats.append_failures.fetch_add(1, Ordering::Relaxed) + 1;
                    if failures == 1 || failures.is_power_of_two() {
                        tracing::warn!(failures, error = %err, "usage record not appended to the router stream");
                    }
                }
            }
        });
        Self {
            tx: Some(tx),
            stats,
        }
    }

    /// Records go nowhere: unit tests, and tools that run without a router.
    pub fn disabled() -> Self {
        Self {
            tx: None,
            stats: Arc::new(UsageRecorderStats::default()),
        }
    }

    pub fn stats(&self) -> &UsageRecorderStats {
        &self.stats
    }

    fn record(&self, record: UsageRecord) {
        let Some(tx) = &self.tx else {
            return;
        };
        match tx.try_send(record) {
            Ok(()) => {
                self.stats.queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                let dropped = self.stats.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 || dropped.is_power_of_two() {
                    tracing::warn!(
                        dropped,
                        "usage recorder queue full; a usage record was dropped"
                    );
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> (Self, mpsc::Receiver<UsageRecord>) {
        let (tx, rx) = mpsc::channel(USAGE_RECORDER_QUEUE);
        (
            Self {
                tx: Some(tx),
                stats: Arc::new(UsageRecorderStats::default()),
            },
            rx,
        )
    }
}

/// Who spends, who pays and for what (§7.2, §7.3). Both client factories require one, built per
/// operation: its trace, thread and counterpart are those of the work the call serves.
#[derive(Debug, Clone)]
pub struct AiAccount {
    pub recorder: UsageRecorder,
    /// The spending node's full L2 name (`<name>@<hive>`).
    pub node_name: String,
    pub node_ilk: Option<String>,
    pub tenant_id: String,
    pub tenant_source: TenantSource,
    pub counterpart_ilk: Option<String>,
    pub trace_id: Option<String>,
    pub thread_id: Option<String>,
    pub work: Option<UsageWork>,
}

impl AiAccount {
    /// Charges `tenant` under `source`; when the tenant a rule names is unknown (`None` or empty),
    /// the root tenant pays, as `fallback_root`.
    pub fn charge(
        recorder: UsageRecorder,
        node_name: impl Into<String>,
        tenant: Option<String>,
        source: TenantSource,
    ) -> Self {
        let (tenant_id, tenant_source) = match tenant.filter(|tenant| !tenant.trim().is_empty()) {
            Some(tenant) => (tenant, source),
            None => (
                DEFAULT_ROOT_TENANT_ID.to_string(),
                TenantSource::FallbackRoot,
            ),
        };
        Self {
            recorder,
            node_name: node_name.into(),
            node_ilk: None,
            tenant_id,
            tenant_source,
            counterpart_ilk: None,
            trace_id: None,
            thread_id: None,
            work: None,
        }
    }

    /// The root tenant pays (SY.architect, SY.admin's executor).
    pub fn root(recorder: UsageRecorder, node_name: impl Into<String>) -> Self {
        Self::charge(
            recorder,
            node_name,
            Some(DEFAULT_ROOT_TENANT_ID.to_string()),
            TenantSource::Root,
        )
    }

    pub fn with_node_ilk(mut self, node_ilk: Option<String>) -> Self {
        self.node_ilk = non_empty(node_ilk);
        self
    }

    pub fn with_counterpart(mut self, counterpart_ilk: Option<String>) -> Self {
        self.counterpart_ilk = non_empty(counterpart_ilk);
        self
    }

    pub fn with_trace(mut self, trace_id: Option<String>) -> Self {
        self.trace_id = non_empty(trace_id);
        self
    }

    pub fn with_thread(mut self, thread_id: Option<String>) -> Self {
        self.thread_id = non_empty(thread_id);
        self
    }

    pub fn with_work(mut self, work: UsageWork) -> Self {
        self.work = (!work.is_empty()).then_some(work);
        self
    }

    /// A call is about to go out: until the guard hears how it ended, dropping it records
    /// `abandoned`.
    pub(crate) fn begin(
        &self,
        provider: AiProvider,
        model: &str,
        kind: UsageKind,
    ) -> CallGuard<'_> {
        CallGuard {
            account: self,
            provider,
            model: model.to_string(),
            kind,
            armed: true,
        }
    }

    fn record(
        &self,
        provider: AiProvider,
        model: &str,
        kind: UsageKind,
        outcome: UsageOutcome,
        body: Option<&Value>,
    ) {
        self.recorder
            .record(self.build_record(provider, model, kind, outcome, body));
    }

    fn build_record(
        &self,
        provider: AiProvider,
        model: &str,
        kind: UsageKind,
        outcome: UsageOutcome,
        body: Option<&Value>,
    ) -> UsageRecord {
        let usage = body
            .and_then(|body| body.get("usage"))
            .filter(|usage| usage.is_object());
        let counts = usage.map(|usage| match provider {
            AiProvider::OpenAi => openai_counts(usage),
            AiProvider::Anthropic => anthropic_counts(usage),
        });
        let text = |pointer: &str| {
            body.and_then(|body| body.pointer(pointer))
                .and_then(Value::as_str)
                .map(cut)
        };
        UsageRecord {
            record_id: uuid::Uuid::new_v4().to_string(),
            provider_response_id: text("/id"),
            ts: chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            hive: self
                .node_name
                .rsplit_once('@')
                .map(|(_, hive)| hive.to_string())
                .unwrap_or_default(),
            node_name: self.node_name.clone(),
            node_ilk: self.node_ilk.clone(),
            tenant_id: self.tenant_id.clone(),
            tenant_source: self.tenant_source,
            counterpart_ilk: self.counterpart_ilk.clone(),
            provider: provider.as_str().to_string(),
            model: cut(model),
            served_model: text("/model"),
            service_tier: text("/service_tier"),
            kind,
            outcome,
            uncached_input: counts.as_ref().map(|counts| counts.uncached_input),
            cache_read: counts.as_ref().map(|counts| counts.cache_read),
            cache_write: counts.as_ref().map(|counts| counts.cache_write),
            output: counts.as_ref().map(|counts| counts.output),
            reasoning: counts.as_ref().and_then(|counts| counts.reasoning),
            raw_usage: usage
                .filter(|usage| {
                    serde_json::to_vec(usage).is_ok_and(|bytes| bytes.len() <= USAGE_RAW_MAX_BYTES)
                })
                .cloned(),
            trace_id: self.trace_id.clone(),
            thread_id: self.thread_id.clone(),
            work: self.work.as_ref().map(cut_work),
        }
    }
}

/// One outstanding call. `failed` and `answered` disarm it; dropped while armed (the caller gave
/// up: a timeout, a cancelled request), it records `abandoned`, without tokens.
pub(crate) struct CallGuard<'a> {
    account: &'a AiAccount,
    provider: AiProvider,
    model: String,
    kind: UsageKind,
    armed: bool,
}

impl CallGuard<'_> {
    /// No answer that costs: a transport error, a non-2xx status. No record.
    pub(crate) fn failed(&mut self) {
        self.armed = false;
    }

    /// A 2xx answered: one record from its body, or `unreadable` when the body could not be read.
    /// Recorded before any check of the answer: a truncated or invalid answer still cost.
    pub(crate) fn answered(&mut self, body: Option<&Value>) {
        if !self.armed {
            return;
        }
        self.armed = false;
        let outcome = match body {
            Some(body) => outcome_of(self.provider, body),
            None => UsageOutcome::Unreadable,
        };
        self.account
            .record(self.provider, &self.model, self.kind, outcome, body);
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.account.record(
                self.provider,
                &self.model,
                self.kind,
                UsageOutcome::Abandoned,
                None,
            );
        }
    }
}

/// The counts of one answer, normalized across providers (§7.2): `uncached_input` + `cache_read`
/// + `cache_write` is every input token, and `output` includes the reasoning.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Counts {
    uncached_input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
    reasoning: Option<u64>,
}

/// OpenAI Responses: `input_tokens` includes the cached ones, `output_tokens` the reasoning.
fn openai_counts(usage: &Value) -> Counts {
    let input = count(usage, "/input_tokens");
    let cached = count(usage, "/input_tokens_details/cached_tokens");
    Counts {
        uncached_input: input.saturating_sub(cached),
        cache_read: cached,
        cache_write: 0,
        output: count(usage, "/output_tokens"),
        reasoning: usage
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64),
    }
}

/// Anthropic Messages: `input_tokens` excludes cache reads and writes, which come apart (the
/// writes' 5-minute and 1-hour split stays in the raw usage); thinking is inside `output_tokens`.
fn anthropic_counts(usage: &Value) -> Counts {
    Counts {
        uncached_input: count(usage, "/input_tokens"),
        cache_read: count(usage, "/cache_read_input_tokens"),
        cache_write: count(usage, "/cache_creation_input_tokens"),
        output: count(usage, "/output_tokens"),
        reasoning: None,
    }
}

/// How a 2xx answer ended, from its body.
fn outcome_of(provider: AiProvider, body: &Value) -> UsageOutcome {
    match provider {
        AiProvider::OpenAi => {
            if body.get("status").and_then(Value::as_str) != Some("incomplete") {
                return UsageOutcome::Ok;
            }
            match body
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
            {
                Some("content_filter") => UsageOutcome::Refused,
                _ => UsageOutcome::Truncated,
            }
        }
        AiProvider::Anthropic => match body.get("stop_reason").and_then(Value::as_str) {
            Some("max_tokens") => UsageOutcome::Truncated,
            Some("refusal") => UsageOutcome::Refused,
            _ => UsageOutcome::Ok,
        },
    }
}

/// A count, 0 when null or missing.
fn count(usage: &Value, pointer: &str) -> u64 {
    usage.pointer(pointer).and_then(Value::as_u64).unwrap_or(0)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| cut(&value))
}

/// A text field as SY.storage keeps it: at most `USAGE_FIELD_MAX_CHARS`.
fn cut(value: &str) -> String {
    value.chars().take(USAGE_FIELD_MAX_CHARS).collect()
}

fn cut_work(work: &UsageWork) -> UsageWork {
    let field = |value: &Option<String>| value.as_deref().map(cut);
    UsageWork {
        session_id: field(&work.session_id),
        pipeline_run_id: field(&work.pipeline_run_id),
        solution_id: field(&work.solution_id),
        stage: field(&work.stage),
        execution_id: field(&work.execution_id),
        caller: field(&work.caller),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn account(recorder: UsageRecorder) -> AiAccount {
        AiAccount::charge(
            recorder,
            "AI.sales@worker1",
            Some("tnt:22222222-2222-4222-8222-222222222222".to_string()),
            TenantSource::Node,
        )
        .with_node_ilk(Some("ilk:11111111-1111-4111-8111-111111111111".to_string()))
        .with_trace(Some("trace-1".to_string()))
        .with_thread(Some(" ".to_string()))
    }

    #[test]
    fn openai_counts_split_the_cached_input_and_keep_the_reasoning() {
        let usage = json!({
            "input_tokens": 1500,
            "input_tokens_details": {"cached_tokens": 1000},
            "output_tokens": 300,
            "output_tokens_details": {"reasoning_tokens": 120},
            "total_tokens": 1800
        });
        assert_eq!(
            openai_counts(&usage),
            Counts {
                uncached_input: 500,
                cache_read: 1000,
                cache_write: 0,
                output: 300,
                reasoning: Some(120),
            }
        );
        let bare = json!({"input_tokens": 10, "output_tokens": null});
        assert_eq!(openai_counts(&bare).output, 0);
        assert_eq!(openai_counts(&bare).reasoning, None);
    }

    #[test]
    fn anthropic_counts_take_the_cache_fields_apart() {
        let usage = json!({
            "input_tokens": 40,
            "cache_creation_input_tokens": 2000,
            "cache_read_input_tokens": null,
            "cache_creation": {"ephemeral_5m_input_tokens": 2000, "ephemeral_1h_input_tokens": 0},
            "output_tokens": 90
        });
        assert_eq!(
            anthropic_counts(&usage),
            Counts {
                uncached_input: 40,
                cache_read: 0,
                cache_write: 2000,
                output: 90,
                reasoning: None,
            }
        );
    }

    #[test]
    fn the_outcome_comes_from_the_body() {
        let openai = AiProvider::OpenAi;
        let anthropic = AiProvider::Anthropic;
        assert_eq!(
            outcome_of(openai, &json!({"status": "completed"})),
            UsageOutcome::Ok
        );
        assert_eq!(
            outcome_of(
                openai,
                &json!({"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}})
            ),
            UsageOutcome::Truncated
        );
        assert_eq!(
            outcome_of(
                openai,
                &json!({"status": "incomplete", "incomplete_details": {"reason": "content_filter"}})
            ),
            UsageOutcome::Refused
        );
        assert_eq!(
            outcome_of(anthropic, &json!({"stop_reason": "end_turn"})),
            UsageOutcome::Ok
        );
        assert_eq!(
            outcome_of(anthropic, &json!({"stop_reason": "max_tokens"})),
            UsageOutcome::Truncated
        );
        assert_eq!(
            outcome_of(anthropic, &json!({"stop_reason": "refusal"})),
            UsageOutcome::Refused
        );
    }

    #[tokio::test]
    async fn an_answer_is_recorded_once_with_its_counts() {
        let (recorder, mut rx) = UsageRecorder::for_test();
        let account = account(recorder);
        let body = json!({
            "id": "msg_01",
            "model": "claude-sonnet-5-5",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 12, "output_tokens": 7}
        });
        account
            .begin(
                AiProvider::Anthropic,
                "claude-sonnet-5-5",
                UsageKind::FunctionTurn,
            )
            .answered(Some(&body));
        let record = rx.try_recv().expect("one record");
        assert!(rx.try_recv().is_err(), "exactly one record");
        assert_eq!(record.hive, "worker1");
        assert_eq!(record.node_name, "AI.sales@worker1");
        assert_eq!(record.tenant_source, TenantSource::Node);
        assert_eq!(record.provider, "anthropic");
        assert_eq!(record.provider_response_id.as_deref(), Some("msg_01"));
        assert_eq!(record.served_model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(record.kind, UsageKind::FunctionTurn);
        assert_eq!(record.outcome, UsageOutcome::Ok);
        assert_eq!(record.uncached_input, Some(12));
        assert_eq!(record.output, Some(7));
        assert_eq!(record.trace_id.as_deref(), Some("trace-1"));
        assert_eq!(record.thread_id, None, "a blank thread is left out");
        assert_eq!(record.defect(), None);
    }

    #[tokio::test]
    async fn a_dropped_call_is_abandoned_and_a_failed_one_is_not_recorded() {
        let (recorder, mut rx) = UsageRecorder::for_test();
        let account = account(recorder);
        {
            let _outstanding = account.begin(AiProvider::OpenAi, "gpt-5.5", UsageKind::Generate);
        }
        let record = rx.try_recv().expect("an abandoned record");
        assert_eq!(record.outcome, UsageOutcome::Abandoned);
        assert_eq!(record.uncached_input, None);
        assert_eq!(record.provider_response_id, None);

        account
            .begin(AiProvider::OpenAi, "gpt-5.5", UsageKind::Generate)
            .failed();
        account
            .begin(AiProvider::OpenAi, "gpt-5.5", UsageKind::Generate)
            .answered(None);
        let record = rx.try_recv().expect("an unreadable record");
        assert_eq!(record.outcome, UsageOutcome::Unreadable);
        assert!(rx.try_recv().is_err(), "a failed call leaves no record");
    }

    #[test]
    fn an_unknown_tenant_is_charged_to_root_as_fallback() {
        let account = AiAccount::charge(
            UsageRecorder::disabled(),
            "SY.cognition@motherbee",
            None,
            TenantSource::Agent,
        );
        assert_eq!(account.tenant_id, DEFAULT_ROOT_TENANT_ID);
        assert_eq!(account.tenant_source, TenantSource::FallbackRoot);
        let root = AiAccount::root(UsageRecorder::disabled(), "SY.architect@motherbee");
        assert_eq!(root.tenant_source, TenantSource::Root);
        let work = AiAccount::root(UsageRecorder::disabled(), "SY.architect@motherbee")
            .with_work(UsageWork::default());
        assert!(work.work.is_none(), "empty work is left out");
    }

    #[test]
    fn a_full_queue_counts_its_drops() {
        let (tx, _rx) = mpsc::channel(1);
        let recorder = UsageRecorder {
            tx: Some(tx),
            stats: Arc::new(UsageRecorderStats::default()),
        };
        let account = account(recorder.clone());
        for _ in 0..3 {
            account
                .begin(AiProvider::OpenAi, "gpt-5.5", UsageKind::Generate)
                .answered(Some(
                    &json!({"usage": {"input_tokens": 1, "output_tokens": 1}}),
                ));
        }
        assert_eq!(recorder.stats().queued.load(Ordering::Relaxed), 1);
        assert_eq!(recorder.stats().dropped.load(Ordering::Relaxed), 2);
    }
}
