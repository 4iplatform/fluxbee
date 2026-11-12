//! A secret an answer carries is shown once and kept masked (FINDINGS A-124, the operator's call
//! of 2026-11-12: *"Mostrar una vez, no guardar"*).
//!
//! `publish_cloud_endpoint` answers the Cloud's bearer once (its `token`), `externalize` an edge
//! channel's (`token`, or a fanout's `verify_token`) and `vault_get` a secret's `value`. Through
//! Archi such an answer comes back in a step's `result_preview` (every plan runs in SY.admin's
//! executor), and Archi kept each chat answer whole in its `messages` table, and a pipeline run's
//! report in its `pipeline_runs` table: the token stayed in Archi's history, in every snapshot of
//! its node directory, and in what a later turn could be given of it. Now:
//!
//! - the answer the chat gets carries each such secret once more where the operator sees it, in
//!   `output.shown_once` (`show_once`), and the chat keeps that view on screen until the operator
//!   sends again or leaves the chat;
//! - what Archi keeps of an answer (`kept`) has every field whose name reads as a secret's
//!   (`super::key_looks_secret`) and a vault-shaped object's `value` masked, and the value of each
//!   masked wherever a text repeats it;
//! - a plan step of an action that answers a secret, and a step that takes such a step's answer
//!   (`$steps.<id>...`), go to SY.admin's executor with nothing for its model to fill
//!   (`steps_that_would_give_a_model_a_secret`), so no model gets the answer.

use fluxbee_sdk::NODE_SECRET_REDACTION_TOKEN;
use serde_json::{json, Map, Value};

/// The actions whose ok answer carries a secret for the operator, shown once:
/// `publish_cloud_endpoint`'s and `externalize`'s token, `vault_get`'s value.
pub const ACTIONS_ANSWERING_A_SECRET: &[&str] =
    &["publish_cloud_endpoint", "externalize", "vault_get"];

/// The fields that hold the operator's own input, never a secret an answer brought: a step's args
/// and a plan. What they hold is masked in what Archi keeps, and never shown once.
const INPUT_FIELDS: &[&str] = &[
    "tool_args_preview",
    "args",
    "params",
    "plan",
    "executor_plan",
];

/// The shortest secret masked where a text repeats it: a shorter one could be any word.
const TEXT_MASK_MIN_CHARS: usize = 8;

/// What the chat says with the secrets it shows once, and keeps with them masked.
pub const SHOWN_ONCE_NOTE: &str = "Shown once, in this answer: copy it now. Archi keeps it \
masked in this chat's history, so it is in no snapshot and in nothing its model reads later, and \
it cannot show it again. SY.admin cannot read a minted token back either: if it is lost, run the \
action again, which mints a new one.";

/// Whether the field `key` of `object` holds a secret: its name reads as a secret's, or it is the
/// `value` of a vault-shaped object (`{key, value}`), as SY.admin's run log reads them
/// (`redact_executor_log_value`).
fn secret_field(object: &Map<String, Value>, key: &str) -> bool {
    super::key_looks_secret(key)
        || (key.eq_ignore_ascii_case("value") && object.contains_key("key"))
}

/// Whether a field's value is something to mask: not null, not an empty text, and not masked
/// already.
fn holds_something(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty() && text != NODE_SECRET_REDACTION_TOKEN,
        _ => true,
    }
}

/// Every text a secret field of `value` holds, at any depth, at least `TEXT_MASK_MIN_CHARS` long:
/// what `kept` masks where a text repeats it.
fn secret_texts(value: &Value, out: &mut Vec<String>) {
    fn texts_of(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::String(text) => {
                let text = text.trim();
                if text.chars().count() >= TEXT_MASK_MIN_CHARS
                    && text != NODE_SECRET_REDACTION_TOKEN
                    && !out.iter().any(|known| known == text)
                {
                    out.push(text.to_string());
                }
            }
            Value::Array(items) => items.iter().for_each(|item| texts_of(item, out)),
            Value::Object(fields) => fields.values().for_each(|field| texts_of(field, out)),
            _ => {}
        }
    }
    match value {
        Value::Object(fields) => {
            for (key, field) in fields {
                if secret_field(fields, key) {
                    texts_of(field, out);
                } else {
                    secret_texts(field, out);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| secret_texts(item, out)),
        _ => {}
    }
}

/// What Archi keeps of `value`, an answer or a report (FINDINGS A-124): each secret field masked
/// with `NODE_SECRET_REDACTION_TOKEN` (one that holds nothing stays as it is), and each secret's
/// text masked wherever another text repeats it (a message, a summary, a detail).
pub fn kept(value: &Value) -> Value {
    let mut secrets = Vec::new();
    secret_texts(value, &mut secrets);
    // The longest first, so a secret that holds another is masked whole.
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    masked(value, &secrets)
}

fn masked(value: &Value, secrets: &[String]) -> Value {
    match value {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, field)| {
                    let field = if secret_field(fields, key) && holds_something(field) {
                        json!(NODE_SECRET_REDACTION_TOKEN)
                    } else {
                        masked(field, secrets)
                    };
                    (key.clone(), field)
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.iter().map(|item| masked(item, secrets)).collect())
        }
        Value::String(text) => {
            let mut text = text.clone();
            for secret in secrets {
                if text.contains(secret.as_str()) {
                    text = text.replace(secret.as_str(), NODE_SECRET_REDACTION_TOKEN);
                }
            }
            Value::String(text)
        }
        other => other.clone(),
    }
}

/// The secrets an answer brought for the operator (FINDINGS A-124): each secret field, outside the
/// operator's own input (`INPUT_FIELDS`), that holds something, as `{where, field, secret}`:
/// `where` the step that answered it (`step <id> (<action>)`), or the answer's action; `field` its
/// name; `secret` its value as text.
pub fn secrets_in(output: &Value) -> Vec<Value> {
    let mut found = Vec::new();
    let place = output
        .get("action")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| "the answer".to_string());
    collect_secrets(output, &place, &mut found);
    found
}

fn collect_secrets(value: &Value, place: &str, found: &mut Vec<Value>) {
    match value {
        Value::Object(fields) => {
            let step = match (
                fields.get("step_id").and_then(Value::as_str),
                fields.get("step_action").and_then(Value::as_str),
            ) {
                (Some(id), Some(action)) => Some(format!("step {id} ({action})")),
                _ => None,
            };
            let place = step.as_deref().unwrap_or(place);
            for (key, field) in fields {
                if INPUT_FIELDS.contains(&key.as_str()) || key == "shown_once" {
                    continue;
                }
                if secret_field(fields, key) {
                    if holds_something(field) {
                        let secret = match field {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        found.push(json!({ "where": place, "field": key, "secret": secret }));
                    }
                } else {
                    collect_secrets(field, place, found);
                }
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_secrets(item, place, found)),
        _ => {}
    }
}

/// Adds to `output`, the answer the chat gets, the secrets it brought, shown once
/// (`output.shown_once`: `{note, secrets}`), when it brought any. The chat shows them apart, and
/// what Archi keeps of the answer has each masked (`kept`).
pub fn show_once(output: &mut Value) {
    let secrets = secrets_in(output);
    if secrets.is_empty() {
        return;
    }
    if let Some(fields) = output.as_object_mut() {
        fields.insert(
            "shown_once".to_string(),
            json!({ "note": SHOWN_ONCE_NOTE, "secrets": secrets }),
        );
    }
}

/// The ids of the steps a step's args take the answer of: each text that is a reference
/// `$steps.<id>.<path>`, as SY.admin's executor resolves one (`resolve_executor_value_refs`: the
/// whole text, trimmed, its first segment the step's id), at any depth.
fn steps_taken(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            if let Some(id) = text
                .trim()
                .strip_prefix("$steps.")
                .and_then(|path| path.split('.').next())
                .map(str::trim)
                .filter(|id| !id.is_empty())
            {
                if !out.iter().any(|known| known == id) {
                    out.push(id.to_string());
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| steps_taken(item, out)),
        Value::Object(fields) => fields.values().for_each(|field| steps_taken(field, out)),
        _ => {}
    }
}

/// The steps of `plan` whose `executor_fill` would give SY.admin's executor model a secret an
/// answer brought (FINDINGS A-124). SY.admin runs a step with nothing to fill as the plan wrote
/// it, with no model (since 0.1.95); a step with fields to fill (`executor_fill.allowed`) goes
/// through the model, whose prompt holds the step with its references resolved, and which gets
/// the step's answer back. So two kinds are refused:
/// - a step of an action that answers a secret (`ACTIONS_ANSWERING_A_SECRET`) with fields to
///   fill: `step <id> (<action>) answers a secret`;
/// - a step with fields to fill whose args take the answer of such a step (`$steps.<id>...`,
///   which SY.admin resolves before it chooses between the model and a direct dispatch):
///   `step <id> (<action>) takes the answer of step <id> (<action>)`.
pub fn steps_that_would_give_a_model_a_secret(plan: &Value) -> Vec<String> {
    let steps = plan
        .pointer("/execution/steps")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let action_of = |step: &Value| {
        step.get("action")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    };
    let id_of = |step: &Value| {
        step.get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("?")
            .to_string()
    };
    // The steps whose answer is a secret, by id.
    let answering = steps
        .iter()
        .filter(|step| ACTIONS_ANSWERING_A_SECRET.contains(&action_of(step).as_str()))
        .map(|step| (id_of(step), action_of(step)))
        .collect::<Vec<_>>();
    steps
        .iter()
        .filter(|step| {
            step.pointer("/executor_fill/allowed")
                .and_then(Value::as_array)
                .is_some_and(|allowed| !allowed.is_empty())
        })
        .filter_map(|step| {
            let (id, action) = (id_of(step), action_of(step));
            if ACTIONS_ANSWERING_A_SECRET.contains(&action.as_str()) {
                return Some(format!("step {id} ({action}) answers a secret"));
            }
            let mut taken = Vec::new();
            if let Some(args) = step.get("args") {
                steps_taken(args, &mut taken);
            }
            let sources = answering
                .iter()
                .filter(|(source, _)| taken.contains(source))
                .map(|(source, source_action)| format!("step {source} ({source_action})"))
                .collect::<Vec<_>>();
            (!sources.is_empty()).then(|| {
                format!(
                    "step {id} ({action}) takes the answer of {}",
                    sources.join(" and ")
                )
            })
        })
        .collect()
}

/// The refusal of a plan whose `executor_fill` would give a model a secret an answer brought
/// (`steps_that_would_give_a_model_a_secret`), or None.
pub fn refusal_of_a_plan_whose_secret_would_reach_a_model(plan: &Value) -> Option<String> {
    let steps = steps_that_would_give_a_model_a_secret(plan);
    (!steps.is_empty()).then(|| {
        format!(
            "{}: a step with executor_fill runs through SY.admin's executor model, which gets the \
             step's answer and its args with every $steps reference resolved, so the model would \
             get a secret (a token or a vault value); write those steps' args in full, with no \
             executor_fill (FINDINGS A-124)",
            steps.join("; ")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SY.admin's executor answer of a plan whose step published the Cloud's door, as Archi keeps
    /// it: its events, the step's args and its answer (`result_preview`), with the token.
    fn executor_answer(token: &str) -> Value {
        json!({
            "status": "ok",
            "action": "executor_execute_plan",
            "message": format!("The door is open; its bearer {token} was handed over."),
            "payload": {
                "events": [
                    { "step_id": "s1", "step_action": "publish_cloud_endpoint", "status": "running",
                      "tool_args_preview": { "edge_node": "SY.edge@ingress1" } },
                    { "step_id": "s1", "step_action": "publish_cloud_endpoint", "status": "done",
                      "tool_args_preview": { "edge_node": "SY.edge@ingress1" },
                      "result_preview": { "status": "ok", "action": "externalize", "payload": {
                          "url": "/e/ich:1", "edge_node": "SY.edge@ingress1", "token": token,
                          "edge_applied": true } } },
                    { "step_id": "s2", "step_action": "vault_get", "status": "done",
                      "tool_args_preview": { "key": "slack-tokens", "hive": "motherbee" },
                      "result_preview": { "status": "ok", "action": "vault_get", "payload": {
                          "key": "slack-tokens", "value": { "bot": "xoxb-abcdefgh" },
                          "metadata": { "owner": "IO.slack@motherbee" } } } },
                    { "step_id": "s3", "step_action": "vault_put", "status": "done",
                      "tool_args_preview": { "key": "k", "value": "p4ssword-typed", "ssh_password": "pw" },
                      "result_preview": { "status": "ok", "payload": { "key": "k", "version": 2 } } }
                ],
                "summary": { "final_message": format!("ok ({token})") }
            },
            "token_usage": { "total": 61000 },
            "explanation": { "tokens_used": 61000 }
        })
    }

    const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn what_archi_keeps_has_every_secret_masked_and_nothing_else() {
        let kept = kept(&executor_answer(TOKEN));
        let text = kept.to_string();
        for secret in [TOKEN, "xoxb-abcdefgh", "p4ssword-typed", "\"pw\""] {
            assert!(!text.contains(secret), "{secret} kept: {text}");
        }
        let events = &kept["payload"]["events"];
        assert_eq!(
            events[1]["result_preview"]["payload"]["token"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(
            events[2]["result_preview"]["payload"]["value"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(
            events[3]["tool_args_preview"]["value"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(
            events[3]["tool_args_preview"]["ssh_password"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        // A text that repeats the token keeps the rest of its words.
        assert_eq!(
            kept["message"],
            json!(format!(
                "The door is open; its bearer {NODE_SECRET_REDACTION_TOKEN} was handed over."
            ))
        );
        assert_eq!(
            kept["payload"]["summary"]["final_message"],
            json!(format!("ok ({NODE_SECRET_REDACTION_TOKEN})"))
        );
        // Nothing else changes: names, counts, a key's name, the token counts.
        assert_eq!(
            events[1]["result_preview"]["payload"]["url"],
            json!("/e/ich:1")
        );
        assert_eq!(
            events[1]["result_preview"]["payload"]["edge_applied"],
            json!(true)
        );
        assert_eq!(
            events[2]["result_preview"]["payload"]["key"],
            json!("slack-tokens")
        );
        assert_eq!(
            events[2]["result_preview"]["payload"]["metadata"]["owner"],
            json!("IO.slack@motherbee")
        );
        assert_eq!(events[3]["result_preview"]["payload"]["version"], json!(2));
        assert_eq!(kept["token_usage"]["total"], json!(61000));
        assert_eq!(kept["explanation"]["tokens_used"], json!(61000));
        // A public channel's token is null: nothing to mask, so it stays null.
        let public = super::kept(&json!({ "payload": { "token": null, "secret": "" } }));
        assert_eq!(
            public,
            json!({ "payload": { "token": null, "secret": "" } })
        );
        // Kept twice, the same.
        assert_eq!(super::kept(&kept), kept);
    }

    #[test]
    fn the_secrets_an_answer_brought_are_shown_once_and_kept_masked() {
        let mut output = executor_answer(TOKEN);
        show_once(&mut output);
        let shown = &output["shown_once"];
        assert_eq!(shown["note"], json!(SHOWN_ONCE_NOTE));
        // The answers' secrets, never the operator's own input (the args).
        assert_eq!(
            shown["secrets"],
            json!([
                { "where": "step s1 (publish_cloud_endpoint)", "field": "token", "secret": TOKEN },
                { "where": "step s2 (vault_get)", "field": "value",
                  "secret": "{\"bot\":\"xoxb-abcdefgh\"}" }
            ])
        );
        // The live answer keeps the step's answer as it came.
        assert_eq!(
            output["payload"]["events"][1]["result_preview"]["payload"]["token"],
            json!(TOKEN)
        );
        // What Archi keeps of it has the shown secrets masked too, the note as it was.
        let kept = kept(&output);
        assert!(!kept.to_string().contains(TOKEN));
        assert_eq!(kept["shown_once"]["note"], json!(SHOWN_ONCE_NOTE));
        assert_eq!(
            kept["shown_once"]["secrets"][0]["secret"],
            json!(NODE_SECRET_REDACTION_TOKEN)
        );
        assert_eq!(
            kept["shown_once"]["secrets"][0]["where"],
            json!("step s1 (publish_cloud_endpoint)")
        );
        // An SCMD's answer names its action; one with no secret gets no shown_once.
        let mut answer = json!({ "status": "ok", "action": "vault_get",
                                 "payload": { "key": "k", "value": "s3cret-value" } });
        show_once(&mut answer);
        assert_eq!(
            answer["shown_once"]["secrets"][0]["where"],
            json!("vault_get")
        );
        let mut plain =
            json!({ "status": "ok", "action": "list_nodes", "payload": { "nodes": [] } });
        show_once(&mut plain);
        assert!(plain.get("shown_once").is_none());
    }

    #[test]
    fn a_step_that_answers_a_secret_has_nothing_for_a_model_to_fill() {
        let plan = |action: &str, fill: Value| {
            json!({ "kind": "executor_plan", "plan_version": "0.1", "execution": { "steps": [
                { "id": "s1", "action": "run_node", "args": {},
                  "executor_fill": { "allowed": ["runtime_version"] } },
                { "id": "s2", "action": action, "args": { "edge_node": "SY.edge@ingress1" },
                  "executor_fill": fill }
            ] } })
        };
        for action in ACTIONS_ANSWERING_A_SECRET {
            let refusal = refusal_of_a_plan_whose_secret_would_reach_a_model(&plan(
                action,
                json!({ "allowed": ["edge_node"] }),
            ))
            .expect("refused");
            assert!(
                refusal.contains(&format!("step s2 ({action})")),
                "{refusal}"
            );
            assert!(refusal.contains("no executor_fill"), "{refusal}");
            // Nothing to fill: SY.admin runs it with no model.
            for fill in [json!({ "allowed": [] }), Value::Null] {
                assert_eq!(
                    refusal_of_a_plan_whose_secret_would_reach_a_model(&plan(action, fill)),
                    None
                );
            }
        }
        // Another action may have fields to fill (s1 above), and a plan with no steps refuses none.
        assert_eq!(
            refusal_of_a_plan_whose_secret_would_reach_a_model(&plan(
                "add_route",
                json!({ "allowed": ["prefix"] })
            )),
            None
        );
        assert_eq!(
            refusal_of_a_plan_whose_secret_would_reach_a_model(&json!({})),
            None
        );
    }

    /// The review of FINDINGS A-124: SY.admin resolves a step's `$steps.<id>...` references before
    /// it chooses between its model and a direct dispatch, so a later step with `executor_fill`
    /// that takes a secret step's answer hands the model that answer, resolved in its prompt. Such
    /// a step is refused too, at any depth of its args; with nothing to fill it goes as written.
    #[test]
    fn a_step_that_takes_a_secret_steps_answer_has_nothing_for_a_model_to_fill() {
        let plan = |reference: Value, fill: Value| {
            json!({ "kind": "executor_plan", "plan_version": "0.1", "execution": { "steps": [
                { "id": "s1", "action": "publish_cloud_endpoint",
                  "args": { "edge_node": "SY.edge@ingress1" } },
                { "id": "s2", "action": "vault_put",
                  "args": { "key": "cloud-copy", "value": reference,
                            "metadata": { "resource_type": "bearer_token" } },
                  "executor_fill": fill }
            ] } })
        };
        let fill = json!({ "allowed": ["metadata"] });
        for reference in [
            json!("$steps.s1.payload.token"),
            json!(" $steps.s1 "),
            json!({ "bearer": "$steps.s1.payload" }),
            json!(["x", "$steps. s1 .payload.token"]),
        ] {
            let refusal = refusal_of_a_plan_whose_secret_would_reach_a_model(&plan(
                reference.clone(),
                fill.clone(),
            ))
            .unwrap_or_else(|| panic!("{reference} refused"));
            assert!(
                refusal.starts_with(
                    "step s2 (vault_put) takes the answer of step s1 (publish_cloud_endpoint): "
                ),
                "{refusal}"
            );
            assert!(refusal.contains("no executor_fill"), "{refusal}");
            // Nothing to fill: SY.admin dispatches it as written, its reference resolved, with
            // no model.
            assert_eq!(
                refusal_of_a_plan_whose_secret_would_reach_a_model(&plan(reference, Value::Null)),
                None
            );
        }
        // A reference to a step whose answer is no secret, and a text that only names one, pass.
        for reference in [
            json!("$steps.s3.payload.url"),
            json!("see $steps.s1.payload.token"),
        ] {
            assert_eq!(
                refusal_of_a_plan_whose_secret_would_reach_a_model(&plan(reference, fill.clone())),
                None
            );
        }
        // Both kinds in one plan: each named.
        let mut both = plan(json!("$steps.s1.payload.token"), fill);
        both["execution"]["steps"][0]["executor_fill"] = json!({ "allowed": ["edge_node"] });
        assert_eq!(
            steps_that_would_give_a_model_a_secret(&both),
            vec![
                "step s1 (publish_cloud_endpoint) answers a secret".to_string(),
                "step s2 (vault_put) takes the answer of step s1 (publish_cloud_endpoint)"
                    .to_string(),
            ]
        );
    }
}
