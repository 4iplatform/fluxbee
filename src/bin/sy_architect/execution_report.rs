//! What the chat says after a plan the operator confirmed ran (FINDINGS A-322, since 0.1.137).
//!
//! SY.admin's executor answers a run with each step's events and one fixed line, "Execution
//! completed successfully" or "Execution stopped at step 's1'", and that line was all the chat
//! showed: a refusal's code and whether asking again may pass went unsaid, and a workflow's delete
//! that answered ok with two versions left read as a full success. Now Archi's model gets one
//! turn, with no tool, with each step's answer, and tells the operator what happened, in the
//! operator's language and as its handbook says (`explain_run`). When there is no turn (Archi has
//! no AI runtime, or the session's turns cannot be read) or it fails (the provider fails, no answer
//! within `EXPLANATION_TURN_TIMEOUT`, no text), the chat keeps the fixed line with each step's code
//! and detail under it (`fallback_text`), with no model. One model call per plan run, recorded with
//! the stage `EXPLANATION_STAGE`.
//!
//! No secret and no capability reaches that turn (FINDINGS A-124). Each step's view is built from
//! an allowlist of fields (`step_view`), never by copying its answer, so a `publish_cloud_endpoint`
//! token, a page's link, a vault value, an API key or a password is in none of them. Each text a
//! view keeps is cut, and as a backstop loses the links, the credentials written with their value
//! and the runs that read as a key (`scrub`); a short secret in a free text reads as none, which is
//! why the allowlist keeps no field that holds one.

use std::future::Future;
use std::time::Duration;

use fluxbee_ai_sdk::{
    FunctionCallingConfig, FunctionCallingModel, FunctionCallingRunner, FunctionRunInput,
    FunctionToolRegistry, ImmediateConversationMemory,
};
use serde_json::{json, Map, Value};

/// The stage of the turn's usage records (`work.stage` in `llm_usage`).
pub const EXPLANATION_STAGE: &str = "execution_report";

/// The longest the turn may take before the chat keeps the fixed line instead: the operator waits
/// for it once the plan ran.
pub const EXPLANATION_TURN_TIMEOUT: Duration = Duration::from_secs(90);

/// The most characters a step's detail, `next` or a hive's reason keeps.
const DETAIL_MAX_CHARS: usize = 300;
/// The most characters a warning keeps, and the run's own line.
const WARNING_MAX_CHARS: usize = 400;
const LINE_MAX_CHARS: usize = 600;
/// The most characters a name keeps: a step id, an action, a hive, a node, a runtime, a version.
const NAME_MAX_CHARS: usize = 128;
/// The most steps a report names; the rest are counted.
const MAX_STEPS: usize = 40;
/// The most items a list keeps (the versions left, the pages, the hives not read); the rest are
/// counted.
const MAX_ITEMS: usize = 10;
/// The most hive names a list of hives keeps (`pending`, `held`).
const MAX_HIVE_NAMES: usize = 20;
/// The most counts a step keeps, and how deep in its payload they are read.
const MAX_COUNTS: usize = 16;
const COUNT_DEPTH: usize = 3;
/// The shortest run of a key's characters that reads as a key or a token in a free text (`scrub`).
const KEY_RUN_MIN_CHARS: usize = 32;

/// The args that say what a step acted on. No other arg is read: a step's args may carry a secret
/// (`vault_put`'s value, `add_hive`'s password) or a page's link (`unpublish_artifact`).
const TARGET_ARGS: &[&str] = &[
    "hive",
    "hive_id",
    "target",
    "node_name",
    "runtime",
    "runtime_version",
    "version",
    "workflow_name",
];

/// The payload fields a step's view keeps on their own terms (`payload_view`); every other one is
/// read only for its counts. `message` is the detail of a refusal SY.admin passed on.
const PAYLOAD_FIELDS: &[&str] = &[
    "status",
    "error_code",
    "message",
    "retryable",
    "unanswered_hives",
    "next",
    "warning",
    "packages_left",
    "pages_left_up",
    "wf_node",
    "pending",
    "held",
    "update",
];

/// What the model is asked to do with the report, ahead of it.
const EXPLANATION_REQUEST: &str = "[Execution report. SY.architect wrote this message, not the \
operator.] The operator confirmed the plan you staged, and SY.admin's executor ran it. The JSON \
below is the run and each step's answer, with only the fields you may report: no token, link, key \
or secret is in it, and none is to be asked for.

Tell the operator what happened, in the language the operator writes in, following your handbook. \
For each step, say what it did: what is done; what stayed and why (a workflow delete's \
packages_left: each version, its error_code and detail, whether asking again may remove it, and \
the hives not read); a refusal's error_code, its detail and whether asking again may pass \
(retryable, unanswered_hives, next); the pages that stay public and until when (pages_left_up); \
the warning. A step's status is done, failed, not_run (the run stopped before it) or not_reported \
(the run's answer does not say what it did: say so, never that it ran or that it did not, and \
offer to read SY.admin's command log, which has each step's outcome). An installation update is \
as you followed it after the run (its state then, after followed_s seconds), not as its step first \
answered; run.verified says whether the run checked out as done. When pages_left_up has no count \
(its error says why) or more above 0, or a removal failed, the pages that stay up are not all \
named here: say so, and offer to read /artifacts. Never call the run a success when a step failed \
or something stayed. Offer what your handbook says comes next, for the operator to ask for. This \
turn has no tools: answer from this report alone, briefly, and do not say how to confirm.";

/// Explains the run in `output`, SY.admin's executor answer as Archi keeps it (its `message`, its
/// `payload.events` and `payload.summary`, its `verification_verdict`, the installation updates it
/// followed), with `plan_steps`, the confirmed plan's `execution.steps`. With `model`, one turn of
/// it, the session's turns (`memory`, read only then) before the report; with none (`Err`, why
/// there is none), when the session's turns cannot be read, or when the turn fails, the fixed line
/// with each step under it. The run's `message` becomes what the chat says, and `explanation` says
/// how it was made: its `source` (`model` or `fallback`), its `model_calls`, the `steps` the turn
/// was given, and the turn's `tokens_used` (counted in the answer's `token_usage` too) or why it
/// gave nothing (`error`).
pub async fn explain_run<M>(
    model: Result<&dyn FunctionCallingModel, String>,
    memory: M,
    plan_steps: &[Value],
    output: &mut Value,
    timeout: Duration,
) where
    M: Future<Output = Result<ImmediateConversationMemory, String>>,
{
    let steps = step_views(output, plan_steps);
    let line = fixed_line(output);
    let (turn, model_calls) = match model {
        Err(why) => (Err(why), 0),
        Ok(model) => match memory.await {
            // Without the session's turns the turn would answer with none of them, and not
            // knowing the operator's language: the chat keeps the fixed line instead.
            Err(why) => (
                Err(format!("the session's turns could not be read: {why}")),
                0,
            ),
            Ok(memory) => {
                let input = model_input(&run_view(output, &line), &steps);
                (
                    explanation_turn(model, Some(memory), input, timeout).await,
                    1,
                )
            }
        },
    };
    let (message, explanation) = match turn {
        Ok((text, tokens_used)) => {
            count_turn_tokens(output, tokens_used);
            (
                text,
                json!({
                    "source": "model",
                    "model_calls": model_calls,
                    "tokens_used": tokens_used,
                    "steps": steps,
                }),
            )
        }
        Err(why) => {
            let why = scrub(&why);
            tracing::warn!(
                reason = %why,
                model_calls,
                "sy.architect: no explanation of the plan's run from the model; the chat keeps \
                 the executor's line with each step's code and detail"
            );
            (
                fallback_text(&line, &steps),
                json!({
                    "source": "fallback",
                    "model_calls": model_calls,
                    "error": cut(&why, DETAIL_MAX_CHARS),
                    "steps": steps,
                }),
            )
        }
    };
    output["message"] = json!(message);
    output["explanation"] = explanation;
}

/// The turn: one call of `model`, with no tool, the report as its message and `memory` (the
/// session's summary and recent turns) before it, within `timeout`. Its text and the tokens it
/// used, or why it gave no text.
async fn explanation_turn(
    model: &dyn FunctionCallingModel,
    memory: Option<ImmediateConversationMemory>,
    input: String,
    timeout: Duration,
) -> Result<(String, u32), String> {
    let runner = FunctionCallingRunner::new(FunctionCallingConfig {
        max_turns: 1,
        output_schema: None,
    });
    let tools = FunctionToolRegistry::new();
    let turn = runner.run_with_input(
        model,
        &tools,
        FunctionRunInput {
            current_user_message: input,
            current_user_parts: None,
            immediate_memory: memory,
        },
    );
    match tokio::time::timeout(timeout, turn).await {
        Err(_) => Err(format!(
            "the model gave no answer within {} s",
            timeout.as_secs()
        )),
        Ok(Err(err)) => Err(format!("the model's call failed: {err}")),
        Ok(Ok(run)) => run
            .final_assistant_text
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
            .map(|text| (text, run.tokens_used))
            .ok_or_else(|| "the model gave no text".to_string()),
    }
}

/// The turn's tokens in the answer's `token_usage`, which the chat's executor card shows: as
/// `execution_report`, and in its `total` with the plan compile's.
fn count_turn_tokens(output: &mut Value, tokens_used: u32) {
    let Some(answer) = output.as_object_mut() else {
        return;
    };
    let usage = answer.entry("token_usage").or_insert_with(|| json!({}));
    if !usage.is_object() {
        *usage = json!({});
    }
    let total = usage.get("total").and_then(Value::as_u64).unwrap_or(0) + u64::from(tokens_used);
    usage["execution_report"] = json!(tokens_used);
    usage["total"] = json!(total);
}

/// The turn's message: what it is asked, then the report, `{run, steps}`.
fn model_input(run: &Value, steps: &[Value]) -> String {
    let report = serde_json::to_string_pretty(&json!({ "run": run, "steps": steps }))
        .unwrap_or_else(|_| "{}".to_string());
    format!("{EXPLANATION_REQUEST}\n\n{report}")
}

/// The run's line as the chat had it: Archi's `message` (the executor's final line, or that the
/// plan ran but is not done, with any note), else the executor's own.
fn fixed_line(output: &Value) -> String {
    text_of(output, "message")
        .or_else(|| {
            output
                .pointer("/payload/summary")
                .and_then(|summary| text_of(summary, "final_message"))
        })
        .unwrap_or("The plan ran.")
        .to_string()
}

/// The run as the turn sees it: its status, its steps done of all, the step it stopped at, its
/// answer's code (`EXECUTION_FAILED`, `EXECUTOR_UNAVAILABLE`...), whether Archi verified it, and
/// its line.
fn run_view(output: &Value, line: &str) -> Value {
    let mut view = Map::new();
    let summary = output.pointer("/payload/summary");
    if let Some(status) = summary.and_then(|summary| text_of(summary, "status")) {
        view.insert("status".into(), json!(clean_name(status)));
    }
    for key in ["completed_steps", "total_steps"] {
        if let Some(count) = summary
            .and_then(|summary| summary.get(key))
            .and_then(Value::as_u64)
        {
            view.insert(key.into(), json!(count));
        }
    }
    if let Some(step) = summary.and_then(|summary| text_of(summary, "failed_step_id")) {
        view.insert("failed_step_id".into(), json!(clean_name(step)));
    }
    if let Some(code) = text_of(output, "error_code") {
        view.insert("error_code".into(), json!(clean_name(code)));
    }
    if let Some(verified) = output
        .pointer("/verification_verdict/verified_success")
        .and_then(Value::as_bool)
    {
        view.insert("verified".into(), json!(verified));
    }
    if output
        .pointer("/payload/answer_too_large")
        .is_some_and(|value| !value.is_null())
    {
        view.insert("answer_too_large".into(), json!(true));
    }
    view.insert("line".into(), json!(clean(line, LINE_MAX_CHARS)));
    Value::Object(view)
}

/// Each step of the run, in the plan's order, as the turn sees it (`step_view`); a step the run
/// names that the plan does not comes after them. At most `MAX_STEPS`, the rest counted.
fn step_views(output: &Value, plan_steps: &[Value]) -> Vec<Value> {
    let events: &[Value] = output
        .pointer("/payload/events")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    // A run whose answer says nothing of its steps: one too large for one mesh frame, which came
    // back with no events (FINDINGS A-198), or an executor that failed before its report (no
    // summary, no events: `EXECUTOR_UNAVAILABLE`, a step's model run that failed after the steps
    // before it ran). Its steps may have run, and their outcome is in SY.admin's command log.
    let unreported = output
        .pointer("/payload/answer_too_large")
        .is_some_and(|value| !value.is_null())
        || !output
            .pointer("/payload/summary")
            .is_some_and(Value::is_object);
    // The installation updates Archi followed after the run (`follow_installation_updates`).
    let followed = output
        .get("installation_updates")
        .filter(|updates| updates.is_object());
    let mut ids: Vec<&str> = Vec::new();
    let planned = plan_steps.iter().filter_map(|step| text_of(step, "id"));
    let ran = events.iter().filter_map(|event| text_of(event, "step_id"));
    for id in planned.chain(ran) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let mut views: Vec<Value> = ids
        .iter()
        .take(MAX_STEPS)
        .map(|id| {
            let step_events: Vec<&Value> = events
                .iter()
                .filter(|event| text_of(event, "step_id") == Some(*id))
                .collect();
            let plan_step = plan_steps
                .iter()
                .find(|step| text_of(step, "id") == Some(*id));
            step_view(id, plan_step, &step_events, unreported, followed)
        })
        .collect();
    if ids.len() > MAX_STEPS {
        views.push(json!({ "more_steps": ids.len() - MAX_STEPS }));
    }
    views
}

/// One step as the turn sees it, from an allowlist of fields: its `id` and `action`; its `status`
/// (`done`, `failed`, `not_run`, or `not_reported` when the run's answer says nothing of its
/// steps); what it acted on (`target`, from `TARGET_ARGS` only); its answer's `answer_status`;
/// its `error_code` and, for a step that failed or was refused, its `error_detail` cut (its
/// answer's, else those of the event that failed it); and of its answer's payload, what
/// `payload_view` keeps.
fn step_view(
    id: &str,
    plan_step: Option<&Value>,
    events: &[&Value],
    unreported: bool,
    followed: Option<&Value>,
) -> Value {
    let mut view = Map::new();
    view.insert("id".into(), json!(clean_name(id)));
    let action = events
        .iter()
        .rev()
        .find_map(|event| text_of(event, "step_action"))
        .or_else(|| plan_step.and_then(|step| text_of(step, "action")));
    if let Some(action) = action {
        view.insert("action".into(), json!(clean_name(action)));
    }
    // The help the executor's model read on the way (`help_lookup`, its `get_admin_action_help`)
    // is none of the step's. Its args and its answer are those of its own action's events (one
    // with no tool, or the action's); an add_hive's join, which the executor follows with
    // get_hive, counts for its status and its failure only.
    let events: Vec<&Value> = events
        .iter()
        .copied()
        .filter(|event| text_of(event, "status").is_none_or(|status| status != "help_lookup"))
        .collect();
    let own: Vec<&Value> = events
        .iter()
        .copied()
        .filter(|event| text_of(event, "tool_name").is_none_or(|tool| Some(tool) == action))
        .collect();
    let reached = |wanted: &str| {
        events.iter().any(|event| {
            text_of(event, "status").is_some_and(|status| status.eq_ignore_ascii_case(wanted))
        })
    };
    let status = if events.is_empty() {
        if unreported {
            "not_reported"
        } else {
            "not_run"
        }
    } else if reached("failed") || reached("stopped") {
        "failed"
    } else if reached("done") {
        "done"
    } else {
        "unknown"
    };
    view.insert("status".into(), json!(status));
    // What it acted on: its args as it ran, else as the plan wrote them.
    let args = own
        .iter()
        .rev()
        .find_map(|event| {
            event
                .get("tool_args_preview")
                .filter(|args| args.is_object())
        })
        .or_else(|| {
            plan_step
                .and_then(|step| step.get("args"))
                .filter(|args| args.is_object())
        });
    if let Some(args) = args {
        let target: Map<String, Value> = TARGET_ARGS
            .iter()
            .filter_map(|key| Some(((*key).to_string(), json!(name_of(args.get(*key)?)?))))
            .collect();
        if !target.is_empty() {
            view.insert("target".into(), Value::Object(target));
        }
    }
    // Its answer: SY.admin's, in the last of its own events that carries one.
    let answer = own.iter().rev().find_map(|event| {
        event
            .get("result_preview")
            .filter(|answer| answer.is_object())
    });
    let payload = answer
        .and_then(|answer| answer.get("payload"))
        .filter(|payload| payload.is_object());
    if let Some(answer_status) = answer.and_then(|answer| text_of(answer, "status")) {
        view.insert("answer_status".into(), json!(clean_name(answer_status)));
    }
    // Its code and detail: its answer's, else those of the last event that failed it (a step that
    // could not be dispatched, a join that failed).
    let answer_code = answer
        .and_then(|answer| text_of(answer, "error_code"))
        .or_else(|| payload.and_then(|payload| text_of(payload, "error_code")));
    let failing = events
        .iter()
        .rev()
        .find(|event| text_of(event, "error_code").is_some());
    let error_code = answer_code.or_else(|| failing.and_then(|event| text_of(event, "error_code")));
    if let Some(code) = error_code {
        view.insert("error_code".into(), json!(clean_name(code)));
    }
    if error_code.is_some() || status == "failed" {
        let answer_detail = || {
            answer
                .and_then(|answer| answer.get("error_detail"))
                .and_then(detail_of)
                .or_else(|| {
                    payload
                        .and_then(|payload| payload.get("message"))
                        .and_then(detail_of)
                })
        };
        let event_detail = || {
            failing
                .into_iter()
                .chain(events.iter().rev())
                .find_map(|event| event.get("error_message").and_then(detail_of))
        };
        let detail = if answer_code.is_some() {
            answer_detail().or_else(event_detail)
        } else {
            event_detail().or_else(answer_detail)
        };
        if let Some(detail) = detail {
            view.insert(
                "error_detail".into(),
                json!(clean(&detail, DETAIL_MAX_CHARS)),
            );
        }
    }
    if let Some(payload) = payload {
        payload_view(payload, followed, &mut view);
    }
    Value::Object(view)
}

/// What a step keeps of its answer's payload: whether asking again may pass (`retryable`), the
/// hives not read (`unanswered_hives`) and what ends it (`next`); the `warning`; the versions a
/// workflow's delete left (`packages_left`); the pages a removal left up (`pages_left_up`, each by
/// its publication id and expiry only); a deploy's or a delete's WF node (`wf_node`); an
/// installation change's hives (`pending`, `held`) and update (`update`), as Archi followed it
/// when it did (`followed`); and its counts.
fn payload_view(payload: &Value, followed: Option<&Value>, view: &mut Map<String, Value>) {
    if let Some(retryable) = payload.get("retryable").and_then(Value::as_bool) {
        view.insert("retryable".into(), json!(retryable));
    }
    if let Some(hives) = payload.get("unanswered_hives").and_then(Value::as_array) {
        view.insert(
            "unanswered_hives".into(),
            listed(hives, unanswered_hive_view),
        );
    }
    if let Some(next) = payload.get("next").and_then(detail_of) {
        view.insert("next".into(), json!(clean(&next, DETAIL_MAX_CHARS)));
    }
    if let Some(warning) = payload.get("warning").and_then(detail_of) {
        view.insert("warning".into(), json!(clean(&warning, WARNING_MAX_CHARS)));
    }
    if let Some(left) = payload.get("packages_left").and_then(Value::as_array) {
        view.insert("packages_left".into(), listed(left, package_left_view));
    }
    if let Some(pages) = payload
        .get("pages_left_up")
        .filter(|pages| pages.is_object())
    {
        view.insert("pages_left_up".into(), pages_left_up_view(pages));
    }
    if let Some(node) = payload.get("wf_node").filter(|node| node.is_object()) {
        view.insert(
            "wf_node".into(),
            names(node, &["node_name", "action", "status"]),
        );
    }
    for key in ["pending", "held"] {
        if let Some(hives) = payload.get(key).and_then(Value::as_array) {
            view.insert(key.into(), hive_names(hives));
        }
    }
    if let Some(update) = payload.get("update").filter(|update| update.is_object()) {
        let follow = text_of(update, "id")
            .and_then(|id| followed?.get(id))
            .filter(|follow| follow.get("update").is_some_and(Value::is_object));
        match follow {
            Some(follow) => followed_update_view(follow, view),
            None => {
                view.insert("update".into(), names(update, &["id", "state"]));
            }
        }
    }
    let mut counts = Map::new();
    collect_counts(payload, "", 1, &mut counts);
    if !counts.is_empty() {
        view.insert("counts".into(), Value::Object(counts));
    }
}

/// An installation update as Archi followed it after the run (`follow_installation_updates`, up to
/// 10 minutes while its hives only move), never as its step first answered, which SY.admin sends
/// while hives still move (`open`, its hives `pending`): its `id`, its `state` then,
/// `superseded_by`, how long it was followed (`followed_s`), and the hives it then left off the
/// target (`pending`) and held (`held`), in place of the step's.
fn followed_update_view(follow: &Value, view: &mut Map<String, Value>) {
    let update = &follow["update"];
    let mut update_view = names(update, &["id", "state", "superseded_by"]);
    if let Some(secs) = follow.get("followed_s").and_then(Value::as_u64) {
        update_view["followed_s"] = json!(secs);
    }
    view.insert("update".into(), update_view);
    for key in ["pending", "held"] {
        view.remove(key);
        if let Some(hives) = update.get(key).and_then(Value::as_array) {
            view.insert(key.into(), hive_names(hives));
        }
    }
}

/// A list of hives as names (each hive, or each hive with its state), at most `MAX_HIVE_NAMES`.
fn hive_names(hives: &[Value]) -> Value {
    Value::Array(
        hives
            .iter()
            .filter_map(name_of)
            .take(MAX_HIVE_NAMES)
            .map(Value::String)
            .collect(),
    )
}

/// A version a workflow's delete left: its `runtime`, `version`, `error_code`, its `detail` cut
/// (but a `PURGE_STOPPED`'s, the same for each after the first, as SY.admin keeps it when it cuts
/// its answer; a `DIST_UNREADABLE`'s says why the folder could not be listed), whether asking
/// again may remove it (`retryable`) and the hives not read; an entry SY.admin cut to fit the mesh
/// frame keeps its `truncated_items`.
fn package_left_view(item: &Value) -> Option<Value> {
    let item = item.as_object()?;
    let mut view = Map::new();
    for key in ["runtime", "version", "error_code"] {
        if let Some(name) = item.get(key).and_then(name_of) {
            view.insert(key.into(), json!(name));
        }
    }
    if item.get("error_code").and_then(Value::as_str) != Some("PURGE_STOPPED") {
        if let Some(detail) = item.get("detail").and_then(detail_of) {
            view.insert("detail".into(), json!(clean(&detail, DETAIL_MAX_CHARS)));
        }
    }
    if let Some(retryable) = item.get("retryable").and_then(Value::as_bool) {
        view.insert("retryable".into(), json!(retryable));
    }
    if let Some(hives) = item.get("unanswered_hives").and_then(Value::as_array) {
        view.insert(
            "unanswered_hives".into(),
            listed(hives, unanswered_hive_view),
        );
    }
    if let Some(cut_items) = item.get("truncated_items").and_then(Value::as_u64) {
        view.insert("truncated_items".into(), json!(cut_items));
    }
    (!view.is_empty()).then_some(Value::Object(view))
}

/// A hive SY.admin did not read: its `hive`, `reason` and, for a join not done, `next`. The hives
/// SY.admin left out of a list it cut to fit the mesh frame (its `{"truncated_items": n}`, FINDINGS
/// A-328) are counted as `{"more": n}`, which `hive_list` says as "<n> more": the report dropped
/// them, and with them that more hives went unread.
fn unanswered_hive_view(item: &Value) -> Option<Value> {
    if let Some(hive) = name_of(item) {
        return Some(json!({ "hive": hive }));
    }
    let Some(hive) = item.get("hive").and_then(name_of) else {
        let more = item
            .get("truncated_items")
            .or_else(|| item.get("more"))
            .and_then(Value::as_u64)?;
        return (more > 0).then(|| json!({ "more": more }));
    };
    let mut view = Map::new();
    view.insert("hive".into(), json!(hive));
    if let Some(reason) = item.get("reason").and_then(detail_of) {
        view.insert("reason".into(), json!(clean(&reason, DETAIL_MAX_CHARS)));
    }
    if let Some(next) = item.get("next").and_then(detail_of) {
        view.insert("next".into(), json!(clean(&next, DETAIL_MAX_CHARS)));
    }
    Some(Value::Object(view))
}

/// The pages a removal left up: their `count` (null when SY.admin could not read its ledger, and
/// its `error` says why), how many it did not name (`more`), and each named one by its
/// `publication_id` and expiry only (`expires_at`, epoch seconds, and `expires`, the same in UTC).
/// Never a link, its key, nor anything else of a page.
fn pages_left_up_view(pages: &Value) -> Value {
    let mut view = Map::new();
    for key in ["count", "more"] {
        if let Some(count) = pages
            .get(key)
            .filter(|count| count.is_u64() || count.is_null())
        {
            view.insert(key.into(), count.clone());
        }
    }
    if let Some(error) = pages.get("error").and_then(detail_of) {
        view.insert("error".into(), json!(clean(&error, DETAIL_MAX_CHARS)));
    }
    if let Some(list) = pages.get("pages").and_then(Value::as_array) {
        let named = listed(list, |page| {
            let mut entry = Map::new();
            entry.insert(
                "publication_id".into(),
                json!(name_of(page.get("publication_id")?)?),
            );
            if let Some(at) = page.get("expires_at").and_then(Value::as_i64) {
                entry.insert("expires_at".into(), json!(at));
                if let Some(date) = utc_date(at) {
                    entry.insert("expires".into(), json!(date));
                }
            }
            Some(Value::Object(entry))
        });
        view.insert("pages".into(), named);
    }
    Value::Object(view)
}

/// The numbers, flags and list lengths of a payload, by their path (`removed.file.episodes`; a
/// list's length as `<path> (items)`), down to `COUNT_DEPTH`, at most `MAX_COUNTS`: no text is
/// read, no field whose name reads as a secret's or is not a plain field name, and nothing under a
/// field named `value`: a vault value is any JSON (`vault_get` answers it as `value`), so a number
/// or a flag in it is a secret too, as SY.admin's run log redacts it.
fn collect_counts(value: &Value, prefix: &str, depth: usize, out: &mut Map<String, Value>) {
    let Some(fields) = value.as_object() else {
        return;
    };
    for (key, field) in fields {
        if out.len() >= MAX_COUNTS {
            return;
        }
        if (depth == 1 && PAYLOAD_FIELDS.contains(&key.as_str()))
            || !is_field_name(key)
            || super::key_looks_secret(key)
            || key.eq_ignore_ascii_case("value")
        {
            continue;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match field {
            Value::Number(_) | Value::Bool(_) => {
                out.insert(path, field.clone());
            }
            Value::Array(items) => {
                out.insert(format!("{path} (items)"), json!(items.len()));
            }
            Value::Object(_) if depth < COUNT_DEPTH => {
                collect_counts(field, &path, depth + 1, out);
            }
            _ => {}
        }
    }
}

/// A plain field name: a letter, then letters, digits or `_`, at most 40 characters and 4 digits.
/// A map keyed by anything else (an id, a hash, a key) is not read.
fn is_field_name(key: &str) -> bool {
    key.len() <= 40
        && key.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && key.chars().filter(char::is_ascii_digit).count() <= 4
}

/// The chat's text when the turn fails: the run's line as it was, then one line per step with
/// its code, its detail and what it left, from the same views the turn gets.
fn fallback_text(line: &str, steps: &[Value]) -> String {
    let mut text = line.trim().to_string();
    for step in steps {
        text.push_str("\n- ");
        match step.get("more_steps").and_then(Value::as_u64) {
            Some(more) => text.push_str(&format!("{more} more steps")),
            None => text.push_str(&step_line(step)),
        }
    }
    text
}

/// One step of `fallback_text`: `<id> <action> (<target>): <status>, <code>: <detail>; ...`.
fn step_line(step: &Value) -> String {
    let field = |key: &str| step.get(key).and_then(Value::as_str);
    let mut line = format!(
        "{} {}",
        field("id").unwrap_or("?"),
        field("action").unwrap_or("?")
    );
    if let Some(target) = step.get("target").and_then(Value::as_object) {
        let named: Vec<String> = target
            .iter()
            .map(|(key, value)| format!("{key} {}", plain(value)))
            .collect();
        line.push_str(&format!(" ({})", named.join(", ")));
    }
    line.push_str(": ");
    line.push_str(match field("status").unwrap_or("unknown") {
        "not_run" => "did not run",
        "not_reported" => "its outcome is in SY.admin's command log",
        status => status,
    });
    if let Some(code) = field("error_code") {
        line.push_str(&format!(", {code}"));
    }
    if let Some(detail) = field("error_detail") {
        line.push_str(&format!(": {detail}"));
    }
    if let Some(hives) = step.get("unanswered_hives").and_then(Value::as_array) {
        line.push_str(&format!("; hives not read: {}", hive_list(hives)));
    }
    if let Some(retryable) = step.get("retryable").and_then(Value::as_bool) {
        line.push_str(if retryable {
            "; asking again may pass"
        } else {
            "; asking again will not pass as it stands"
        });
    }
    if let Some(next) = field("next") {
        line.push_str(&format!("; next: {next}"));
    }
    if let Some(left) = step.get("packages_left").and_then(Value::as_array) {
        let versions: Vec<String> = left.iter().filter_map(package_line).collect();
        if !versions.is_empty() {
            line.push_str(&format!("; versions left: {}", versions.join(", ")));
        }
    }
    if let Some(pages) = step.get("pages_left_up").and_then(pages_line) {
        line.push_str(&format!("; {pages}"));
    }
    if let Some(update) = step.get("update").and_then(Value::as_object) {
        let text = |key: &str| update.get(key).and_then(Value::as_str);
        if let (Some(id), Some(state)) = (text("id"), text("state")) {
            line.push_str(&format!("; installation update {id}: {state}"));
        }
    }
    if let Some(warning) = field("warning") {
        line.push_str(&format!("; warning: {warning}"));
    }
    line
}

/// A version left, in `step_line`: `wf.x 0.0.1 (RUNTIME_USAGE_UNKNOWN, retryable, hives not
/// read: worker1 (TIMEOUT))`; one with no version says why with its detail (`DIST_UNREADABLE`);
/// the entries SY.admin did not name, `<n> more`.
fn package_line(entry: &Value) -> Option<String> {
    if let Some(more) = entry
        .get("more")
        .or_else(|| entry.get("truncated_items"))
        .and_then(Value::as_u64)
    {
        return Some(format!("{more} more"));
    }
    let text = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
    };
    let mut why: Vec<String> = text("error_code").map(str::to_string).into_iter().collect();
    if text("version").is_none() {
        if let Some(detail) = text("detail") {
            why.push(detail.to_string());
        }
    }
    if entry.get("retryable").and_then(Value::as_bool) == Some(true) {
        why.push("retryable".to_string());
    }
    if let Some(hives) = entry
        .get("unanswered_hives")
        .and_then(Value::as_array)
        .filter(|hives| !hives.is_empty())
    {
        why.push(format!("hives not read: {}", hive_list(hives)));
    }
    let name: Vec<&str> = [text("runtime"), text("version")]
        .into_iter()
        .flatten()
        .collect();
    match (name.is_empty(), why.is_empty()) {
        (true, true) => None,
        (false, true) => Some(name.join(" ")),
        (true, false) => Some(why.join(", ")),
        (false, false) => Some(format!("{} ({})", name.join(" "), why.join(", "))),
    }
}

/// The pages a removal left up, in `step_line`: `pages left up: 2 (pub:… until <date>, 1 more)`;
/// when SY.admin could not count them, that /artifacts lists the pages that are up. None when it
/// left none.
fn pages_line(pages: &Value) -> Option<String> {
    if pages.get("count").is_some_and(Value::is_null) {
        let why = pages
            .get("error")
            .and_then(Value::as_str)
            .map(|error| format!(" ({error})"))
            .unwrap_or_default();
        return Some(format!(
            "pages left up: not counted{why}; /artifacts lists the pages that are up"
        ));
    }
    let count = pages.get("count").and_then(Value::as_u64).unwrap_or(0);
    if count == 0 {
        return None;
    }
    let mut named: Vec<String> = pages
        .get("pages")
        .and_then(Value::as_array)
        .map(|pages| {
            pages
                .iter()
                .filter_map(|page| {
                    let id = page.get("publication_id")?.as_str()?;
                    Some(match page.get("expires").and_then(Value::as_str) {
                        Some(date) => format!("{id} until {date}"),
                        None => id.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if let Some(more) = pages
        .get("more")
        .and_then(Value::as_u64)
        .filter(|more| *more > 0)
    {
        named.push(format!("{more} more"));
    }
    let mut line = format!("pages left up: {count}");
    if !named.is_empty() {
        line.push_str(&format!(" ({})", named.join(", ")));
    }
    Some(line)
}

/// The hives of a list, each with its reason: `worker1 (TIMEOUT), worker2`.
fn hive_list(hives: &[Value]) -> String {
    hives
        .iter()
        .filter_map(|hive| {
            if let Some(more) = hive.get("more").and_then(Value::as_u64) {
                return Some(format!("{more} more"));
            }
            let name = hive.get("hive")?.as_str()?;
            Some(match hive.get("reason").and_then(Value::as_str) {
                Some(reason) => format!("{name} ({reason})"),
                None => name.to_string(),
            })
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first `MAX_ITEMS` of `items` that `view` keeps, then how many more there were.
fn listed(items: &[Value], view: impl Fn(&Value) -> Option<Value>) -> Value {
    let mut kept: Vec<Value> = items.iter().take(MAX_ITEMS).filter_map(view).collect();
    if items.len() > MAX_ITEMS {
        kept.push(json!({ "more": items.len() - MAX_ITEMS }));
    }
    Value::Array(kept)
}

/// The fields of `value` named in `keys` that are names (`name_of`).
fn names(value: &Value, keys: &[&str]) -> Value {
    Value::Object(
        keys.iter()
            .filter_map(|key| Some(((*key).to_string(), json!(name_of(value.get(*key)?)?))))
            .collect(),
    )
}

/// A name or a short code: a string, cut and without links (`clean_name`), or a number.
fn name_of(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| clean_name(text))
        }
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// A detail as text: a string as it is, anything else but null as its JSON, pretty, so that
/// `scrub` reads each of its values as a word of its own.
fn detail_of(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => {
            let text = text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        other => serde_json::to_string_pretty(other).ok(),
    }
}

fn text_of<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// A value of `step_line`: a string as it is, anything else as its JSON.
fn plain(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// An epoch-seconds time in UTC, RFC 3339.
fn utc_date(epoch_secs: i64) -> Option<String> {
    chrono::DateTime::from_timestamp(epoch_secs, 0)
        .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// A free text the turn may carry: scrubbed (`scrub`), then cut to `max_chars`.
fn clean(text: &str, max_chars: usize) -> String {
    cut(&scrub(text), max_chars)
}

/// A name the turn may carry: its links, channel ids and credentials taken out (`scrub_text`,
/// without the rule for a long run of a key's characters, which a name such as a version or an id
/// may be), cut to `NAME_MAX_CHARS`.
fn clean_name(text: &str) -> String {
    cut(&scrub_text(text, false), NAME_MAX_CHARS)
}

/// `text`, cut to `max_chars` characters, with `…` when cut.
fn cut(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut kept: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    kept.push('…');
    kept
}

/// What stands in for a link, and for a credential, a channel id or a key.
const LINK_REMOVED: &str = "[link removed]";
const REMOVED: &str = "[removed]";

/// The names a credential's value follows: `token=…`, `"api_key": "…"`, `password: …`,
/// `Authorization: Bearer …`.
const CREDENTIALS: &[&str] = &[
    "token",
    "password",
    "passwd",
    "passphrase",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "credential",
    "credentials",
    "private_key",
    "ssh_key",
];

/// What joins a credential's name to its value within a word.
const JOINTS: &[&str] = &["=", "\":", "':", ":"];

/// A free text with what reads as a link, a channel id, a credential's value or a key taken out
/// (`scrub_text`, with the rule for a long run of a key's characters); its whitespace becomes
/// single spaces.
fn scrub(text: &str) -> String {
    scrub_text(text, true)
}

/// `text`, word by word (`word_kind`): a link becomes `[link removed]`; a channel id, a credential
/// written with its value in one word (`token=abc`, `"api_key":"k"`) and, with `long_runs`, a word
/// holding a run of a key's characters, `[removed]`. A word that names a credential and leaves its
/// value to the next (`password:`, `"token":`, `Bearer`), or a credential's name followed by `=`
/// or `:` as a word of its own, stays, and that value becomes `[removed]`: after
/// `Authorization:`, its scheme (`Bearer`, `Basic`) stays and the word after it goes. A short
/// secret written any other way reads as a plain word.
fn scrub_text(text: &str, long_runs: bool) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut kept: Vec<String> = Vec::with_capacity(words.len());
    // The word that is a credential's value, removed whatever it reads as.
    let mut value_at: Option<usize> = None;
    for (at, word) in words.iter().copied().enumerate() {
        if value_at == Some(at) {
            if is_auth_scheme(word) {
                kept.push(word.to_string());
                value_at = Some(at + 1);
            } else {
                kept.push(REMOVED.to_string());
            }
            continue;
        }
        match word_kind(word, long_runs) {
            WordKind::Removed(with) => kept.push(with.to_string()),
            WordKind::NamesItsValue => {
                kept.push(word.to_string());
                value_at = Some(at + 1);
            }
            WordKind::Kept => {
                kept.push(word.to_string());
                if is_credential_name(word)
                    && words
                        .get(at + 1)
                        .is_some_and(|joint| matches!(*joint, "=" | ":"))
                {
                    value_at = Some(at + 2);
                }
            }
        }
    }
    kept.join(" ")
}

/// What a word of a text is to `scrub_text`.
enum WordKind {
    /// It may reach the turn as it is.
    Kept,
    /// It must not, and this stands in for it.
    Removed(&'static str),
    /// It names a credential whose value is the next word; it stays.
    NamesItsValue,
}

/// A link (`://`, a page's `/public/` key, a channel's `/e/` path) is `[link removed]`; a channel
/// id (`ich:<uuid>`, its `/e/` path without the prefix) and a credential with its value in the
/// word are `[removed]`; a credential's name with its joint at the word's end, and `Bearer`, name
/// the next word; with `long_runs`, a word holding a run of a key's characters (`holds_a_key_run`)
/// is `[removed]`.
fn word_kind(word: &str, long_runs: bool) -> WordKind {
    let lower = word.to_ascii_lowercase();
    if lower.contains("://") || lower.contains("/public/") || lower.contains("/e/") {
        return WordKind::Removed(LINK_REMOVED);
    }
    if names_a_channel(&lower) {
        return WordKind::Removed(REMOVED);
    }
    match credential_value_in(&lower) {
        Some(true) => return WordKind::Removed(REMOVED),
        Some(false) => return WordKind::NamesItsValue,
        None => {}
    }
    if word
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .eq_ignore_ascii_case("bearer")
    {
        return WordKind::NamesItsValue;
    }
    if long_runs && holds_a_key_run(word) {
        return WordKind::Removed(REMOVED);
    }
    WordKind::Kept
}

/// Whether `lower` names a channel by its id: `ich:` at its start or after a character that is
/// not a letter or a digit (`'ich:…'`, `channel=ich:…`; not `which:`).
fn names_a_channel(lower: &str) -> bool {
    lower.match_indices("ich:").any(|(at, _)| {
        lower[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_ascii_alphanumeric())
    })
}

/// Whether `lower` names a credential with its joint (`token=`, `"api_key":`, `password:`):
/// `Some(true)` when its value is in the word too, `Some(false)` when it is the next word, `None`
/// when it names none.
fn credential_value_in(lower: &str) -> Option<bool> {
    CREDENTIALS.iter().find_map(|name| {
        JOINTS.iter().find_map(|joint| {
            let named = format!("{name}{joint}");
            let at = lower.find(&named)?;
            let value =
                lower[at + named.len()..].trim_matches(|c: char| !c.is_ascii_alphanumeric());
            Some(!value.is_empty())
        })
    })
}

/// Whether `word` is a credential's name and nothing else (`password`, `"token"`).
fn is_credential_name(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    CREDENTIALS
        .iter()
        .any(|name| core.eq_ignore_ascii_case(name))
}

/// Whether `word` is an authorization's scheme, which its credential follows.
fn is_auth_scheme(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    core.eq_ignore_ascii_case("bearer") || core.eq_ignore_ascii_case("basic")
}

/// Whether `word` holds a run of `KEY_RUN_MIN_CHARS` or more of a key's characters (letters,
/// digits, `-`, `_`, `+`, `/`, `=`: base64 and base64url), a letter and a digit among them, as a
/// key or a token reads: a JWT's part, a key in a JSON detail, an id after its prefix
/// (`ilk:<uuid>`). A dotted name or a version breaks at its dots.
fn holds_a_key_run(word: &str) -> bool {
    word.split(|c: char| !(c.is_ascii_alphanumeric() || "-_+/=".contains(c)))
        .any(|run| {
            run.len() >= KEY_RUN_MIN_CHARS
                && run.chars().any(|c| c.is_ascii_alphabetic())
                && run.chars().any(|c| c.is_ascii_digit())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use fluxbee_ai_sdk::{AiSdkError, FunctionModelTurnRequest, FunctionModelTurnResponse};
    use std::future::{ready, Ready};
    use std::sync::Mutex;

    /// What the model was sent, turn by turn, and what it answers: its text, an error, or nothing
    /// for longer than any turn waits.
    struct RecordingModel {
        requests: Mutex<Vec<FunctionModelTurnRequest>>,
        answer: Answer,
    }

    enum Answer {
        Text(&'static str),
        Fails,
        Hangs,
    }

    impl RecordingModel {
        fn new(answer: Answer) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                answer,
            }
        }

        /// Everything the model was sent, its items as text.
        fn sent(&self) -> String {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .map(|request| serde_json::to_string(&request.items).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    #[async_trait]
    impl FunctionCallingModel for RecordingModel {
        async fn run_turn(
            &self,
            request: FunctionModelTurnRequest,
        ) -> fluxbee_ai_sdk::Result<FunctionModelTurnResponse> {
            self.requests.lock().unwrap().push(request);
            match self.answer {
                Answer::Text(text) => Ok(FunctionModelTurnResponse {
                    assistant_text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    tokens_used: 321,
                }),
                Answer::Fails => Err(AiSdkError::Protocol(
                    "provider answered 529 overloaded".to_string(),
                )),
                Answer::Hangs => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    unreachable!("no turn waits an hour")
                }
            }
        }
    }

    /// A session with no turns yet, read at once.
    fn no_turns() -> Ready<Result<ImmediateConversationMemory, String>> {
        ready(Ok(ImmediateConversationMemory::default()))
    }

    const TOKEN: &str = "fbct_9f2c4e7a1b3d5f60718293a4b5c6d7e8f9a0b1c2";
    const PAGE_KEY: &str = "4b1d9e0c7a5f3e2d1c0b9a8f7e6d5c4b3a2f1e0d9c8b7a6f5e4d3c2b1a0f9e8d";
    const VAULT_VALUE: &str = "vault-value-q7Rk2mXz";
    const API_KEY: &str = "sk-ant-api03-Zx9Yw8Vu7Ts6Rq5Po4Nm3Lk2";
    const PASSWORD: &str = "Hunter2-pass";

    /// The plan the operator confirmed: a workflow's delete, then a removal of a version, then a
    /// step that did not run.
    fn plan_steps() -> Vec<Value> {
        vec![
            json!({ "id": "s1", "action": "wf_rules_delete",
                    "args": { "hive": "motherbee", "workflow_name": "gate0236a", "force": true } }),
            json!({ "id": "s2", "action": "remove_runtime_version",
                    "args": { "hive": "motherbee", "runtime": "wf.gate0236a",
                              "runtime_version": "0.0.1" } }),
            json!({ "id": "s3", "action": "kill_node",
                    "args": { "hive": "worker1", "node_name": "AI.sales@worker1" } }),
        ]
    }

    /// SY.admin's executor answer, as Archi keeps it, of a run that stopped at s2: s1's delete
    /// answered ok with two versions left and a page left up, and with a Cloud token, a page's
    /// link, a vault value and credentials in fields no view keeps; s2 was refused
    /// RUNTIME_USAGE_UNKNOWN, worker1 not read, after the executor's model read its help. Each
    /// secret also sits in its args and in a text.
    fn run_output() -> Value {
        let link = format!("https://edge.example.com/public/{PAGE_KEY}");
        let mut s1_payload = json!({
            "status": "ok",
            "deleted": true,
            "token": TOKEN,
            "url": link,
            "value": VAULT_VALUE,
            "secret_ref": "vault://tnt:x/openai_key",
            "edge_url": format!("https://edge.example.com/e/ich-{PAGE_KEY}"),
            "wf_node": { "node_name": "WF.gate0236a@motherbee", "action": "killed",
                         "status": "ok", "error": format!("see {link}") },
            "warning": format!("workflow gate0236a is deleted, but its purge did not remove 2 \
                versions of wf.gate0236a; the page {link} stays, Authorization: Bearer {TOKEN}, \
                api_key={API_KEY}"),
            "removed": { "file": { "communications": 3, "episodes": 1 } }
        });
        s1_payload["packages_left"] = json!([
            { "runtime": "wf.gate0236a", "version": "0.0.1", "error_code": "RUNTIME_USAGE_UNKNOWN",
              "detail": format!("not read; token={TOKEN}"), "retryable": true,
              "unanswered_hives": [ { "hive": "worker1", "reason": "TIMEOUT",
                                      "detail": VAULT_VALUE } ] },
            { "runtime": "wf.gate0236a", "version": "0.0.2", "error_code": "PURGE_STOPPED",
              "detail": format!("the purge stopped at 0.0.1; password: {PASSWORD}"),
              "retryable": false }
        ]);
        s1_payload["pages_left_up"] = json!({
            "count": 1, "more": 0,
            "pages": [ { "publication_id": "pub:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d",
                         "expires_at": 1_794_528_000_i64, "status": "up", "url": link,
                         "key": PAGE_KEY, "filename_original": "report.html" } ]
        });
        let s1_done = json!({
            "step_id": "s1", "step_action": "wf_rules_delete", "status": "done",
            "tool_name": "wf_rules_delete",
            "tool_args_preview": { "hive": "motherbee", "workflow_name": "gate0236a",
                                   "password": PASSWORD },
            "result_preview": { "status": "ok", "action": "wf_rules_delete", "error_code": null,
                                "error_detail": null, "payload": s1_payload }
        });
        let refused = "cannot tell whether runtime 'wf.gate0236a' version '0.0.1' is in use";
        let s2_answer = json!({
            "status": "error", "action": "remove_runtime_version",
            "error_code": "RUNTIME_USAGE_UNKNOWN",
            "error_detail": format!("{refused}: the nodes of worker1 (TIMEOUT) were not read, and \
                nothing was removed. Ask again once each hive answers ({link} {TOKEN})"),
            "payload": {
                "usage_global_visible": { "running_nodes": [] },
                "unanswered_hives": [ { "hive": "worker1", "reason": "TIMEOUT",
                                        "detail": format!("token={TOKEN}") } ],
                "retryable": true
            }
        });
        let s2_failed = json!({
            "step_id": "s2", "step_action": "remove_runtime_version", "status": "failed",
            "tool_name": "remove_runtime_version",
            "tool_args_preview": { "hive": "motherbee", "runtime": "wf.gate0236a",
                                   "runtime_version": "0.0.1", "secret": VAULT_VALUE },
            "error_code": "RUNTIME_USAGE_UNKNOWN", "error_message": refused,
            "result_preview": s2_answer
        });
        // The help the executor's model read on the way: not the step's answer, nor its args.
        let help = json!({
            "step_id": "s2", "step_action": "remove_runtime_version", "status": "help_lookup",
            "tool_name": "get_admin_action_help",
            "tool_args_preview": { "action_name": "remove_runtime_version", "hive": "worker9" },
            "result_preview": { "status": "ok", "error_code": "HELP_NOTE",
                                "payload": { "warning": "the help's own words", "retryable": false } }
        });
        let events = json!([
            { "step_id": "s1", "step_action": "wf_rules_delete", "status": "running",
              "tool_args_preview": { "hive": "motherbee", "workflow_name": "gate0236a",
                                     "api_key": API_KEY } },
            s1_done,
            s2_failed,
            help,
            { "step_id": "s2", "step_action": "remove_runtime_version", "status": "stopped",
              "error_code": "RUNTIME_USAGE_UNKNOWN", "error_message": refused }
        ]);
        let summary = json!({
            "status": "failed", "total_steps": 3, "completed_steps": 1, "failed_step_id": "s2",
            "failed_step_action": "remove_runtime_version",
            "final_message": "Execution stopped at step 's2'",
            "failure": { "code": "RUNTIME_USAGE_UNKNOWN", "detail": refused, "step_id": "s2",
                         "step_action": "remove_runtime_version" }
        });
        json!({
            "status": "error",
            "action": "executor_execute_plan",
            "message": "Execution stopped at step 's2'",
            "payload": { "status": "failed", "events": events, "summary": summary },
            "error_code": "EXECUTION_FAILED",
            "verification_verdict": {
                "verified_success": false, "eligible_for_cookbook": false,
                "reason": "execution report is not verified successful: summary.status=failed"
            }
        })
    }

    /// The secrets of `run_output`, none of which may reach the model.
    fn secrets() -> Vec<String> {
        vec![
            TOKEN.to_string(),
            PAGE_KEY.to_string(),
            VAULT_VALUE.to_string(),
            API_KEY.to_string(),
            PASSWORD.to_string(),
            "edge.example.com".to_string(),
            "/public/".to_string(),
            "/e/".to_string(),
            "vault://".to_string(),
            "openai_key".to_string(),
        ]
    }

    /// FINDINGS A-322 and A-124: after a run, the model gets one turn, with no tool, with each
    /// step's answer, and no token, link, vault value, key or password of the answers or of the
    /// steps' args reaches it, though every one of them sits in the run: in fields no view keeps
    /// (`token`, `url`, `value`, `secret_ref`, a page's `key`, the args), and in the texts a view
    /// keeps cut (`warning`, `error_detail`, a version's `detail`, `wf_node.error`, a hive's
    /// `detail`). What the operator is told is there: each step's status, code, detail cut, the
    /// versions left with their codes and whether asking again may remove them, the hive not
    /// read, the page left up by its id and expiry, the warning, the counts, and the step that did
    /// not run. Its text becomes the run's message, and its tokens count in the answer's
    /// `token_usage` with the plan compile's.
    #[tokio::test]
    async fn the_explanation_turn_gets_each_steps_answer_and_no_secret() {
        let model = RecordingModel::new(Answer::Text(
            "The workflow is deleted; two of its versions stayed.",
        ));
        let mut output = run_output();
        output["token_usage"] = json!({ "plan_compile": 1000, "total": 1000, "budget": 60000 });
        explain_run(
            Ok(&model),
            no_turns(),
            &plan_steps(),
            &mut output,
            EXPLANATION_TURN_TIMEOUT,
        )
        .await;

        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "one model call per run");
        assert!(requests[0].tools.is_empty(), "the turn offers no tool");
        drop(requests);
        let sent = model.sent();
        for secret in secrets() {
            assert!(
                !sent.contains(&secret),
                "{secret} reached the model: {sent}"
            );
        }
        for help in ["the help's own words", "worker9", "HELP_NOTE"] {
            assert!(!sent.contains(help), "{help} reached the model: {sent}");
        }
        for reported in [
            "wf_rules_delete",
            "gate0236a",
            "RUNTIME_USAGE_UNKNOWN",
            "PURGE_STOPPED",
            "0.0.2",
            "worker1",
            "TIMEOUT",
            "\\\"retryable\\\": true",
            "pub:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d",
            "2026-11-13T00:00:00Z",
            "its purge did not remove 2 versions",
            "nothing was removed",
            "not read; [removed]",
            "removed.file.communications",
            "\\\"not_run\\\"",
            "EXECUTION_FAILED",
            "Execution stopped at step 's2'",
            "[link removed]",
        ] {
            assert!(
                sent.contains(reported),
                "{reported} did not reach the model: {sent}"
            );
        }

        assert_eq!(
            output["message"],
            json!("The workflow is deleted; two of its versions stayed.")
        );
        assert_eq!(output["explanation"]["source"], json!("model"));
        assert_eq!(output["explanation"]["model_calls"], json!(1));
        assert_eq!(output["explanation"]["tokens_used"], json!(321));
        assert_eq!(
            output["token_usage"],
            json!({ "plan_compile": 1000, "execution_report": 321, "total": 1321,
                    "budget": 60000 })
        );
        let steps = output["explanation"]["steps"].to_string();
        for secret in secrets() {
            assert!(
                !steps.contains(&secret),
                "{secret} kept in the steps: {steps}"
            );
        }
    }

    /// Each step's view: what it acted on from its args alone, its status, its answer's code and
    /// detail, and of its payload only the allowlisted fields, never from the help its executor
    /// read on the way; a step the plan names and the run did not reach is `not_run`.
    #[test]
    fn a_steps_view_keeps_only_the_allowlisted_fields() {
        let views = step_views(&run_output(), &plan_steps());
        assert_eq!(views.len(), 3, "{views:?}");

        let s1 = &views[0];
        assert_eq!(s1["status"], json!("done"));
        assert_eq!(
            s1["target"],
            json!({ "hive": "motherbee", "workflow_name": "gate0236a" })
        );
        assert_eq!(s1.get("error_code"), None);
        assert_eq!(s1.get("error_detail"), None);
        assert_eq!(
            s1["packages_left"],
            json!([
                { "runtime": "wf.gate0236a", "version": "0.0.1",
                  "error_code": "RUNTIME_USAGE_UNKNOWN", "detail": "not read; [removed]",
                  "retryable": true,
                  "unanswered_hives": [ { "hive": "worker1", "reason": "TIMEOUT" } ] },
                { "runtime": "wf.gate0236a", "version": "0.0.2",
                  "error_code": "PURGE_STOPPED", "retryable": false }
            ])
        );
        assert_eq!(
            s1["pages_left_up"],
            json!({ "count": 1, "more": 0, "pages": [
                { "publication_id": "pub:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d",
                  "expires_at": 1_794_528_000_i64, "expires": "2026-11-13T00:00:00Z" } ] })
        );
        assert_eq!(
            s1["wf_node"],
            json!({ "node_name": "WF.gate0236a@motherbee", "action": "killed", "status": "ok" })
        );
        assert_eq!(
            s1["counts"],
            json!({ "deleted": true, "removed.file.communications": 3,
                    "removed.file.episodes": 1 })
        );
        let warning = s1["warning"].as_str().unwrap();
        assert!(
            warning.starts_with("workflow gate0236a is deleted"),
            "{warning}"
        );
        assert!(warning.contains("[link removed]"), "{warning}");
        assert!(
            warning.contains("Authorization: Bearer [removed]"),
            "{warning}"
        );
        assert!(warning.ends_with("[removed]"), "{warning}");

        let s2 = &views[1];
        assert_eq!(s2["status"], json!("failed"));
        assert_eq!(s2["answer_status"], json!("error"));
        assert_eq!(s2["error_code"], json!("RUNTIME_USAGE_UNKNOWN"));
        assert_eq!(s2["retryable"], json!(true));
        assert_eq!(
            s2["unanswered_hives"],
            json!([ { "hive": "worker1", "reason": "TIMEOUT" } ])
        );
        assert_eq!(
            s2["target"],
            json!({ "hive": "motherbee", "runtime": "wf.gate0236a", "runtime_version": "0.0.1" })
        );
        let detail = s2["error_detail"].as_str().unwrap();
        assert!(
            detail.starts_with("cannot tell whether runtime"),
            "{detail}"
        );
        assert!(detail.chars().count() <= DETAIL_MAX_CHARS, "{detail}");
        assert_eq!(
            s2["counts"],
            json!({ "usage_global_visible.running_nodes (items)": 0 })
        );

        assert_eq!(
            views[2],
            json!({ "id": "s3", "action": "kill_node", "status": "not_run",
                    "target": { "hive": "worker1", "node_name": "AI.sales@worker1" } })
        );
    }

    /// When the turn fails, the chat keeps the run's line with each step's code and detail under
    /// it, and says so, with no model text: the provider's error, no answer in time, no AI runtime.
    #[tokio::test(start_paused = true)]
    async fn a_failed_turn_keeps_the_fixed_line_with_each_steps_code() {
        for (answer, why) in [
            (Answer::Fails, "provider answered 529 overloaded"),
            (Answer::Hangs, "no answer within 90 s"),
        ] {
            let model = RecordingModel::new(answer);
            let mut output = run_output();
            explain_run(
                Ok(&model),
                no_turns(),
                &plan_steps(),
                &mut output,
                EXPLANATION_TURN_TIMEOUT,
            )
            .await;
            assert_eq!(model.requests.lock().unwrap().len(), 1);
            assert_eq!(output["explanation"]["source"], json!("fallback"));
            assert_eq!(output["explanation"]["model_calls"], json!(1));
            assert_eq!(output.get("token_usage"), None, "no tokens to count");
            let error = output["explanation"]["error"].as_str().unwrap();
            assert!(error.contains(why), "{error}");
            let message = output["message"].as_str().unwrap();
            assert!(message.starts_with("Execution stopped at step 's2'\n- s1 wf_rules_delete (hive motherbee, workflow_name gate0236a): done"), "{message}");
            for line in [
                "versions left: wf.gate0236a 0.0.1 (RUNTIME_USAGE_UNKNOWN, retryable, hives not read: worker1 (TIMEOUT)), wf.gate0236a 0.0.2 (PURGE_STOPPED)",
                "pages left up: 1 (pub:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d until 2026-11-13T00:00:00Z)",
                "- s2 remove_runtime_version (hive motherbee, runtime wf.gate0236a, runtime_version 0.0.1): failed, RUNTIME_USAGE_UNKNOWN: cannot tell whether runtime",
                "hives not read: worker1 (TIMEOUT); asking again may pass",
                "- s3 kill_node (hive worker1, node_name AI.sales@worker1): did not run",
            ] {
                assert!(message.contains(line), "{line} missing: {message}");
            }
            for secret in secrets() {
                assert!(
                    !message.contains(&secret),
                    "{secret} in the line: {message}"
                );
            }
        }

        // Without an AI runtime there is no call at all.
        let mut output = run_output();
        explain_run(
            Err("AI provider not configured.".to_string()),
            no_turns(),
            &plan_steps(),
            &mut output,
            EXPLANATION_TURN_TIMEOUT,
        )
        .await;
        assert_eq!(output["explanation"]["source"], json!("fallback"));
        assert_eq!(output["explanation"]["model_calls"], json!(0));
        assert!(output["message"]
            .as_str()
            .unwrap()
            .starts_with("Execution stopped at step 's2'\n- s1 wf_rules_delete"));
    }

    /// When the session's turns cannot be read, no turn runs: it would answer with none of them,
    /// not knowing the operator's language. The chat keeps the fixed line, with no model call.
    #[tokio::test]
    async fn a_session_whose_turns_cannot_be_read_gets_the_fixed_line_with_no_call() {
        let model = RecordingModel::new(Answer::Text("The run, in English."));
        let mut output = run_output();
        explain_run(
            Ok(&model),
            ready(Err::<ImmediateConversationMemory, String>(
                "messages table: not found".to_string(),
            )),
            &plan_steps(),
            &mut output,
            EXPLANATION_TURN_TIMEOUT,
        )
        .await;
        assert!(model.requests.lock().unwrap().is_empty(), "no model call");
        assert_eq!(output["explanation"]["source"], json!("fallback"));
        assert_eq!(output["explanation"]["model_calls"], json!(0));
        let error = output["explanation"]["error"].as_str().unwrap();
        assert!(
            error.starts_with("the session's turns could not be read: messages table"),
            "{error}"
        );
        assert!(output["message"]
            .as_str()
            .unwrap()
            .starts_with("Execution stopped at step 's2'\n- s1 wf_rules_delete"));
    }

    /// A model that answers no text is a failed turn too.
    #[tokio::test]
    async fn a_turn_with_no_text_falls_back() {
        let model = RecordingModel::new(Answer::Text("  "));
        let turn =
            explanation_turn(&model, None, "report".to_string(), EXPLANATION_TURN_TIMEOUT).await;
        assert_eq!(turn, Err("the model gave no text".to_string()));
    }

    /// When SY.admin's executor fails before its report (`EXECUTOR_UNAVAILABLE`: a step's model
    /// run that failed, a reference that did not resolve), its answer carries no step, and the
    /// steps before the one it failed at ran. Each is `not_reported`, never `not_run`: the turn
    /// and the fallback say its outcome is in SY.admin's command log, and the turn gets the code.
    #[tokio::test]
    async fn an_executor_that_failed_before_its_report_leaves_each_step_not_reported() {
        let mut output = json!({
            "status": "error", "action": "executor_execute_plan", "payload": null,
            "error_code": "EXECUTOR_UNAVAILABLE",
            "error_detail": "executor model run failed: provider answered 529 overloaded",
            "message": "Executor plan failed: executor model run failed: provider answered 529 \
                        overloaded"
        });
        let views = step_views(&output, &plan_steps());
        assert_eq!(
            views
                .iter()
                .map(|view| view["status"].clone())
                .collect::<Vec<_>>(),
            vec![json!("not_reported"); 3]
        );
        assert_eq!(
            run_view(&output, &fixed_line(&output))["error_code"],
            json!("EXECUTOR_UNAVAILABLE")
        );

        explain_run(
            Err("AI provider not configured.".to_string()),
            no_turns(),
            &plan_steps(),
            &mut output,
            EXPLANATION_TURN_TIMEOUT,
        )
        .await;
        let message = output["message"].as_str().unwrap();
        assert!(
            message.starts_with("Executor plan failed: executor model run failed"),
            "{message}"
        );
        assert!(!message.contains("did not run"), "{message}");
        assert_eq!(
            message
                .matches("its outcome is in SY.admin's command log")
                .count(),
            3,
            "{message}"
        );
    }

    /// An installation update's step is told as Archi followed it after the run, not as SY.admin
    /// answered it while its hives moved (`open`, worker1 pending): done after 42 s, no hive
    /// pending. An update Archi did not follow keeps its answer's.
    #[test]
    fn an_installation_update_is_told_as_archi_followed_it() {
        let answered = json!({
            "status": "partial", "action": "update_installation",
            "payload": {
                "update": { "id": "upd-1731160000000", "state": "open",
                            "pending": ["worker1 (core in_flight, config pending)"], "held": [] },
                "pending": ["worker1 (core in_flight, config pending)"],
                "opening": "opened", "elapsed_ms": 4100,
                "message": "update upd-1731160000000 is open: not every hive is on the target yet"
            }
        });
        let mut output = json!({
            "status": "ok", "message": "Execution completed successfully",
            "payload": {
                "events": [ { "step_id": "s1", "step_action": "update_installation",
                              "status": "done", "tool_name": "update_installation",
                              "result_preview": answered } ],
                "summary": { "status": "done", "total_steps": 1, "completed_steps": 1,
                             "final_message": "Execution completed successfully" }
            }
        });
        let plan = vec![json!({ "id": "s1", "action": "update_installation",
                                "args": { "reason": "release 0.1.137" } })];
        let answered_view = &step_views(&output, &plan)[0];
        assert_eq!(
            answered_view["update"],
            json!({ "id": "upd-1731160000000", "state": "open" })
        );
        assert_eq!(
            answered_view["pending"],
            json!(["worker1 (core in_flight, config pending)"])
        );

        output["installation_updates"] = json!({ "upd-1731160000000": {
            "update": { "id": "upd-1731160000000", "state": "done", "superseded_by": null,
                        "pending": [], "held": [],
                        "hives": { "worker1": { "core": { "state": "on_target" } } } },
            "followed_s": 42 } });
        let views = step_views(&output, &plan);
        let followed = &views[0];
        assert_eq!(followed["status"], json!("done"));
        assert_eq!(
            followed["update"],
            json!({ "id": "upd-1731160000000", "state": "done", "followed_s": 42 })
        );
        assert_eq!(followed["pending"], json!([]));
        assert_eq!(followed["held"], json!([]));
        let text = fallback_text("Execution completed successfully", &views);
        assert!(
            text.contains("installation update upd-1731160000000: done"),
            "{text}"
        );
    }

    /// A vault value is any JSON: `vault_get` answers it as `value`, a number or an object of
    /// numbers and flags as well as a text, and no count of it reaches the turn (FINDINGS A-124);
    /// the answer's other numbers do.
    #[tokio::test]
    async fn no_number_or_flag_of_a_vault_value_reaches_the_turn() {
        for value in [
            json!(482193),
            json!({ "pin": 4821, "enabled": true, "codes": [11, 12] }),
        ] {
            let mut output = json!({
                "status": "ok", "message": "Execution completed successfully",
                "payload": {
                    "events": [ { "step_id": "s1", "step_action": "vault_get", "status": "done",
                        "tool_name": "vault_get",
                        "tool_args_preview": { "hive": "motherbee", "key": "door_pin" },
                        "result_preview": { "status": "ok", "action": "vault_get",
                            "payload": { "status": "ok", "key": "door_pin", "version": 3,
                                         "access_count": 2,
                                         "metadata": { "tenant_id": "tnt:x",
                                                       "created_at": 1_794_500_000_i64 },
                                         "value": value } } } ],
                    "summary": { "status": "done", "total_steps": 1, "completed_steps": 1,
                                 "final_message": "Execution completed successfully" }
                }
            });
            let views = step_views(&output, &[]);
            let counts = views[0]["counts"].as_object().expect("its counts");
            assert_eq!(counts.get("version"), Some(&json!(3)), "{counts:?}");
            assert!(
                counts.keys().all(|key| !key.starts_with("value")),
                "{counts:?}"
            );

            let model = RecordingModel::new(Answer::Text("The value was read."));
            explain_run(
                Ok(&model),
                no_turns(),
                &[],
                &mut output,
                EXPLANATION_TURN_TIMEOUT,
            )
            .await;
            let sent = model.sent();
            let steps = output["explanation"]["steps"].to_string();
            for secret in ["482193", "4821", "value.enabled", "value.codes"] {
                assert!(!sent.contains(secret), "{secret} reached the model: {sent}");
                assert!(
                    !steps.contains(secret),
                    "{secret} kept in the steps: {steps}"
                );
            }
        }
    }

    /// What says why a version stayed or a page may stay up reaches the turn and the fallback: a
    /// `DIST_UNREADABLE`'s detail, a page ledger that could not be read (`count` null, with its
    /// error, and /artifacts lists the pages), and an entry SY.admin cut as "<n> more", never as
    /// "()".
    #[test]
    fn what_says_why_a_version_or_a_page_stayed_is_kept() {
        let detail = "dist/runtimes/wf.gate0237a cannot be listed: Permission denied (os error 13)";
        let ledger = "the page ledger could not be read: database is locked";
        let payload = json!({
            "status": "ok",
            "packages_left": [
                { "runtime": "wf.gate0237a", "error_code": "DIST_UNREADABLE", "retryable": false,
                  "detail": detail },
                { "truncated_items": 3 }
            ],
            "pages_left_up": { "count": null, "error": ledger }
        });
        let output = json!({ "message": "Execution completed successfully", "payload": {
            "events": [ { "step_id": "s1", "step_action": "wf_rules_delete", "status": "done",
                          "tool_name": "wf_rules_delete",
                          "result_preview": { "status": "ok", "payload": payload } } ],
            "summary": { "status": "done", "total_steps": 1, "completed_steps": 1 } } });
        let views = step_views(&output, &[]);
        assert_eq!(
            views[0]["packages_left"],
            json!([
                { "runtime": "wf.gate0237a", "error_code": "DIST_UNREADABLE",
                  "detail": detail, "retryable": false },
                { "truncated_items": 3 }
            ])
        );
        assert_eq!(
            views[0]["pages_left_up"],
            json!({ "count": null, "error": ledger })
        );
        let text = fallback_text("Execution completed successfully", &views);
        for part in [
            format!("versions left: wf.gate0237a (DIST_UNREADABLE, {detail}), 3 more"),
            format!(
                "pages left up: not counted ({ledger}); /artifacts lists the pages that are up"
            ),
        ] {
            assert!(text.contains(&part), "{part} missing: {text}");
        }
        assert!(!text.contains("()"), "{text}");
    }

    /// FINDINGS A-328's review: a refusal whose `unanswered_hives` SY.admin cut to fit the mesh
    /// frame keeps the count of the hives it left out (`{"truncated_items": n}`): the report says
    /// "<n> more", in the turn as in the fallback, where it dropped the count and with it that
    /// more hives went unread. A version left keeps its own the same way.
    #[test]
    fn the_hives_sy_admin_cut_from_a_refusal_are_counted() {
        let answer = json!({
            "status": "error", "action": "remove_runtime_version",
            "error_code": "RUNTIME_USAGE_UNKNOWN",
            "error_detail": "the nodes of worker1 (TIMEOUT) and 4 more hives were not read",
            "payload": {
                "retryable": true,
                "unanswered_hives": [
                    { "hive": "worker1", "reason": "TIMEOUT" },
                    { "truncated_items": 4 }
                ]
            }
        });
        let delete = json!({
            "status": "ok",
            "packages_left": [
                { "runtime": "wf.gate0239w", "version": "0.0.1",
                  "error_code": "RUNTIME_USAGE_UNKNOWN", "retryable": true,
                  "unanswered_hives": [ { "hive": "worker2", "reason": "TIMEOUT" },
                                        { "truncated_items": 2 } ] }
            ]
        });
        let output = json!({ "payload": { "events": [
            { "step_id": "s1", "step_action": "remove_runtime_version", "status": "failed",
              "tool_name": "remove_runtime_version", "error_code": "RUNTIME_USAGE_UNKNOWN",
              "result_preview": answer },
            { "step_id": "s2", "step_action": "wf_rules_delete", "status": "done",
              "tool_name": "wf_rules_delete", "result_preview": { "status": "ok", "payload": delete } }
        ], "summary": { "status": "failed", "total_steps": 2, "completed_steps": 1 } } });
        let views = step_views(&output, &[]);
        assert_eq!(
            views[0]["unanswered_hives"],
            json!([ { "hive": "worker1", "reason": "TIMEOUT" }, { "more": 4 } ])
        );
        assert_eq!(
            views[1]["packages_left"][0]["unanswered_hives"],
            json!([ { "hive": "worker2", "reason": "TIMEOUT" }, { "more": 2 } ])
        );
        let text = fallback_text("Execution failed", &views);
        for part in [
            "hives not read: worker1 (TIMEOUT), 4 more",
            "hives not read: worker2 (TIMEOUT), 2 more",
        ] {
            assert!(text.contains(part), "{part} missing: {text}");
        }
        // A cut of nothing counts nothing.
        assert_eq!(unanswered_hive_view(&json!({ "truncated_items": 0 })), None);
    }

    /// An add_hive that was accepted and whose join failed: the step failed with the join's code
    /// and detail, which the executor followed with get_hive, its answer the add_hive's own
    /// (`accepted`), and of its args only the hive it adds, never its SSH password.
    #[test]
    fn a_failed_join_fails_its_add_hive_step() {
        let output = json!({ "payload": { "events": [
            { "step_id": "s1", "step_action": "add_hive", "status": "running",
              "tool_args_preview": { "hive_id": "worker2", "address": "10.0.0.7",
                                     "ssh_user": "root", "ssh_password": PASSWORD } },
            { "step_id": "s1", "step_action": "add_hive", "status": "done", "tool_name": "add_hive",
              "tool_args_preview": { "hive_id": "worker2", "ssh_password": PASSWORD },
              "result_preview": { "status": "accepted", "action": "add_hive",
                                  "payload": { "status": "accepted", "hive_id": "worker2",
                                               "message": "the join runs in the background" } } },
            { "step_id": "s1", "step_action": "add_hive", "status": "failed", "tool_name": "get_hive",
              "tool_args_preview": { "hive_id": "worker2" },
              "result_preview": { "status": "ok", "payload": { "hive": { "status": "failed" } } },
              "error_code": "JOIN_FAILED", "error_message": "ssh: permission denied" },
            { "step_id": "s1", "step_action": "add_hive", "status": "stopped", "tool_name": "add_hive",
              "error_code": "JOIN_FAILED", "error_message": "ssh: permission denied" }
        ] } });
        let views = step_views(&output, &[]);
        assert_eq!(
            views,
            vec![
                json!({ "id": "s1", "action": "add_hive", "status": "failed",
                         "target": { "hive_id": "worker2" }, "answer_status": "accepted",
                         "error_code": "JOIN_FAILED", "error_detail": "ssh: permission denied" })
            ]
        );
    }

    /// The links, credentials and long keys a text may carry, taken out word by word; the rest of
    /// it stays, and a name keeps a long id.
    #[test]
    fn scrub_takes_out_links_credentials_and_keys() {
        assert_eq!(
            scrub("see https://edge.x/public/abc and /e/ich-1, token=abc \"api_key\":\"k\" Bearer xyz ok"),
            "see [link removed] and [link removed] [removed] [removed] Bearer [removed] ok"
        );
        assert_eq!(scrub(&format!("key {PAGE_KEY}.")), "key [removed]");
        assert_eq!(
            scrub("the token this call minted is handed to no one"),
            "the token this call minted is handed to no one"
        );
        assert_eq!(
            scrub("vault://tnt:x/openai_key stays"),
            "[link removed] stays"
        );
        assert_eq!(
            clean_name("pub:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d"),
            "pub:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d"
        );
        assert_eq!(cut("abcdef", 4), "abc…");
    }

    /// A credential's value goes however it is written (`name: value`, `name = value`, pretty
    /// JSON, after an authorization's scheme), and a key whatever its alphabet (a JWT, base64 with
    /// `/`, a key quoted in a JSON detail); a channel id goes, in a text and in a name; a version,
    /// a path and an English word stay.
    #[test]
    fn scrub_takes_out_a_credentials_value_and_a_key_written_any_way() {
        assert_eq!(
            scrub(&format!("password: {PASSWORD} ok")),
            "password: [removed] ok"
        );
        assert_eq!(
            scrub(&format!("password = {PASSWORD} ok")),
            "password = [removed] ok"
        );
        assert_eq!(
            clean(
                &detail_of(&json!({ "code": "X", "password": PASSWORD })).unwrap(),
                DETAIL_MAX_CHARS
            ),
            "{ \"code\": \"X\", \"password\": [removed] }"
        );
        assert_eq!(
            scrub("Authorization: Basic dXNlcjpwYXNzd29yZA== sent"),
            "Authorization: Basic [removed] sent"
        );
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.\
                   dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U";
        assert_eq!(scrub(&format!("signed {jwt}")), "signed [removed]");
        assert_eq!(
            scrub("key dGhpcyBpcyBhIHNlY3JldCBr/ZXkgZm9yIHRlc3RzIDEy+OTQ= stays"),
            "key [removed] stays"
        );
        let detail = detail_of(&json!({ "outcome": { "key": PAGE_KEY } })).unwrap();
        let cleaned = clean(&detail, DETAIL_MAX_CHARS);
        assert!(!cleaned.contains(PAGE_KEY), "{cleaned}");
        assert!(cleaned.contains("\"key\": [removed]"), "{cleaned}");

        let channel = "ich:7d1f0b6e-2c4a-4e8b-9a3d-5f6e7a8b9c0d";
        assert_eq!(
            scrub(&format!("edge did not open URL '{channel}': refused")),
            "edge did not open URL [removed] refused"
        );
        assert_eq!(clean_name(channel), "[removed]");
        assert_eq!(scrub("which: one"), "which: one");
        assert_eq!(
            scrub("dist/runtimes/wf.gate0237a/0.1.137 cannot be listed"),
            "dist/runtimes/wf.gate0237a/0.1.137 cannot be listed"
        );
    }
}
