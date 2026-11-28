//! The runner (docs/digital-executive-v1-stage-2.md §3, §5.2, §9): the steps one by one, each
//! journaled before it is sent and after its answer. A try reads what runs first and sends only
//! what is missing (`build_steps`), so a retry, a restart or a resume converges and never replays.
//! A step that fails for good leaves the deployment `failed`, with the step and its error; nothing
//! is undone by itself.
//!
//! A WF node's ILK is the orchestrator's (stage 3 §4): after its `wf_rules_compile_apply` the run
//! reads the nodes again, takes the ILK from the WF's status into the record, and goes on with its
//! relation. A WF is removed with `wf_rules_delete` in its retire mode, the rest with
//! `kill_node`'s retire: every node's ILK is marked, never purged.

use std::time::Instant;

use serde_json::{json, Value};

use json_router::mesh_reply::bounded_value;

use crate::desired::DesiredState;
use crate::service::{secrets_of, vault_list_call, Deployer, Job};
use crate::steps::{
    build_steps, NodeObserved, NodePresence, Observed, Step, StepKind, ACTION_WAIT, KILL_WAIT,
    MOTHERBEE, WF_WAIT,
};
use crate::store::{DeploymentState, Failure, RecordNode};
use crate::world::{AdminCall, AdminOutcome};

/// The codes of an answer that changed nothing and may pass when asked again, beside the answers
/// that say so (`payload.retryable`).
const RETRYABLE_CODES: &[&str] = &[
    "BUSY",
    "NOT_TAKEN",
    "TIMEOUT",
    "UNREACHABLE",
    "TRANSPORT_ERROR",
    "DB_NOT_READY",
    "DOCUMENT_WRITE_FAILED",
    "IDENTITY_UNAVAILABLE",
    "INVENTORY_UNAVAILABLE",
    "RUNTIME_NOT_PRESENT",
    "RUNTIME_MANIFEST_BUSY",
    "NODE_STATUS_GET_FAILED",
    // SY.wf-rules made the definition current and its node does not run it yet.
    "WF_NODE_NOT_DEPLOYED",
];

/// How much of an answer the journal keeps.
const JOURNAL_ANSWER_MAX_BYTES: usize = 2048;

/// How a step ended.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    Done(Value),
    /// It may pass on the next try, which reads the state first.
    Retry {
        code: String,
        detail: String,
    },
    Fail {
        code: String,
        detail: String,
    },
}

fn outcome_of(outcome: AdminOutcome) -> StepOutcome {
    match outcome {
        AdminOutcome::Ok(payload) => StepOutcome::Done(payload),
        AdminOutcome::Refused {
            code,
            detail,
            retryable,
            ..
        } => {
            if retryable || RETRYABLE_CODES.contains(&code.as_str()) {
                StepOutcome::Retry { code, detail }
            } else {
                StepOutcome::Fail { code, detail }
            }
        }
        AdminOutcome::Unanswered(why) => StepOutcome::Retry {
            code: "NO_ANSWER".to_string(),
            detail: why,
        },
    }
}

impl Deployer {
    /// Runs the runner's jobs, one at a time, in the order they came.
    pub async fn run_jobs(
        self: std::sync::Arc<Self>,
        mut jobs: tokio::sync::mpsc::UnboundedReceiver<Job>,
    ) {
        while let Some(job) = jobs.recv().await {
            self.run_job(job).await;
        }
    }

    pub async fn run_job(&self, job: Job) {
        match job {
            Job::Deploy(deployment_id) => {
                let key = self
                    .record(&deployment_id)
                    .map(|record| record.grouping_key().to_string());
                self.run_deploy(&deployment_id).await;
                if let Some(key) = key {
                    self.release(&key);
                }
            }
            Job::Remove {
                grouping_key,
                deployment_ids,
                person,
                tenant_id,
            } => {
                self.run_remove(&deployment_ids, person.as_deref(), &tenant_id)
                    .await;
                self.release(&grouping_key);
                if let Err(err) = self.refresh_catalog() {
                    tracing::error!(error = %err, "the catalog after a removal");
                }
            }
        }
    }

    async fn pause(&self, attempt: usize) {
        if let Some(wait) = self.timings.retry_backoff.get(attempt.saturating_sub(1)) {
            if !wait.is_zero() {
                tokio::time::sleep(*wait).await;
            }
        }
    }

    fn tries(&self) -> usize {
        self.timings.retry_backoff.len() + 1
    }

    fn fail(&self, deployment_id: &str, operation: &str, step: &str, code: &str, detail: &str) {
        let at_ms = self.world.now_ms();
        let failure = Failure {
            operation: operation.to_string(),
            step: step.to_string(),
            error_code: code.to_string(),
            error_detail: detail.to_string(),
            at_ms,
        };
        tracing::warn!(deployment_id = %deployment_id, operation, step, code, detail, "a deployment failed");
        self.update_record(deployment_id, |record| {
            record.state = DeploymentState::Failed;
            record.failure = Some(failure.clone());
            record.note(
                at_ms,
                "failed",
                "SY.deploy",
                Some(format!("{step}: {code}")),
            );
        });
        self.journal(
            deployment_id,
            json!({"event": "state", "state": "failed", "operation": operation, "step": step, "error_code": code, "error_detail": detail}),
        );
    }

    /// One step, journaled before and after.
    async fn journaled<F>(
        &self,
        deployment_id: &str,
        label: &str,
        call: Option<&AdminCall>,
        run: F,
    ) -> StepOutcome
    where
        F: std::future::Future<Output = StepOutcome>,
    {
        let mut start = json!({"event": "step", "phase": "start", "step": label});
        if let Some(call) = call {
            start["action"] = json!(call.action);
            start["target"] = json!(call.target);
            start["params"] = bounded_value(&call.params, JOURNAL_ANSWER_MAX_BYTES);
        }
        self.journal(deployment_id, start);
        let started = Instant::now();
        let outcome = run.await;
        let duration_ms = started.elapsed().as_millis() as u64;
        let end = match &outcome {
            StepOutcome::Done(answer) => json!({
                "event": "step", "phase": "done", "step": label, "duration_ms": duration_ms,
                "answer": bounded_value(answer, JOURNAL_ANSWER_MAX_BYTES),
            }),
            StepOutcome::Retry { code, detail } => json!({
                "event": "step", "phase": "retry", "step": label, "duration_ms": duration_ms,
                "error_code": code, "error_detail": detail,
            }),
            StepOutcome::Fail { code, detail } => json!({
                "event": "step", "phase": "failed", "step": label, "duration_ms": duration_ms,
                "error_code": code, "error_detail": detail,
            }),
        };
        self.journal(deployment_id, end);
        outcome
    }

    // --- a deployment -----------------------------------------------------------------------------

    async fn run_deploy(&self, deployment_id: &str) {
        let Some(record) = self.record(deployment_id) else {
            return;
        };
        if record.state != DeploymentState::Deploying {
            return;
        }
        let mut desired = match self.desired_for(&record) {
            Ok(desired) => desired,
            Err(err) => {
                self.fail(
                    deployment_id,
                    "deploy",
                    "desired_state",
                    "SOURCE_NOT_FOUND",
                    &err,
                );
                return;
            }
        };
        let tries = self.tries();
        let mut attempt = 0;
        // The reads again a plan asks for (after a WF's compile), within one try.
        let mut reads_again = 0;
        let mut continues = false;
        loop {
            attempt += 1;
            let observed = match self.observe(&desired).await {
                Ok(observed) => observed,
                Err((code, detail)) => {
                    self.journal(
                        deployment_id,
                        json!({"event": "observe", "attempt": attempt, "error_code": code, "error_detail": detail}),
                    );
                    if attempt >= tries {
                        self.fail(deployment_id, "deploy", "observe", &code, &detail);
                        return;
                    }
                    self.pause(attempt).await;
                    continue;
                }
            };
            match self.learn_wf_ilks(deployment_id, &observed) {
                Ok(Some(learned)) => desired = learned,
                Ok(None) => {}
                Err(err) => {
                    self.fail(
                        deployment_id,
                        "deploy",
                        "desired_state",
                        "SOURCE_NOT_FOUND",
                        &err,
                    );
                    return;
                }
            }
            let steps = build_steps(&desired, &observed);
            let mut run = json!({
                "event": "run",
                "operation": "deploy",
                "attempt": attempt,
                "plan": steps.iter().map(|step| step.label.clone()).collect::<Vec<_>>(),
            });
            if continues {
                run["continues"] = json!(true);
            }
            self.journal(deployment_id, run);
            continues = false;
            let mut retry = None;
            let mut read_again = None;
            for step in &steps {
                if let StepKind::Reobserve { .. } = step.kind {
                    self.journal(
                        deployment_id,
                        json!({"event": "step", "phase": "done", "step": step.label, "duration_ms": 0}),
                    );
                    read_again = Some(step.label.clone());
                    break;
                }
                if step.kind == StepKind::RecordActive {
                    let at_ms = self.world.now_ms();
                    self.journal(
                        deployment_id,
                        json!({"event": "step", "phase": "start", "step": step.label}),
                    );
                    self.update_record(deployment_id, |record| {
                        let by = record.requested_by.clone();
                        record.state = DeploymentState::Active;
                        record.failure = None;
                        record.note(at_ms, "deployed", &by, None);
                    });
                    self.journal(
                        deployment_id,
                        json!({"event": "step", "phase": "done", "step": step.label, "duration_ms": 0}),
                    );
                    self.journal(deployment_id, json!({"event": "state", "state": "active"}));
                    tracing::info!(deployment_id = %deployment_id, attempt, "a deployment is active");
                    return;
                }
                match self
                    .journaled(
                        deployment_id,
                        &step.label,
                        step.call.as_ref(),
                        self.execute(&desired, step),
                    )
                    .await
                {
                    StepOutcome::Done(_) => {}
                    StepOutcome::Retry { code, detail } => {
                        retry = Some((step.label.clone(), code, detail));
                        break;
                    }
                    StepOutcome::Fail { code, detail } => {
                        self.fail(deployment_id, "deploy", &step.label, &code, &detail);
                        return;
                    }
                }
            }
            if let Some(label) = read_again {
                reads_again += 1;
                if reads_again <= desired.nodes.len() + 1 {
                    // A read again is part of this try, not another one.
                    attempt -= 1;
                    continues = true;
                    continue;
                }
                retry = Some((
                    label,
                    "NODE_NOT_READY".to_string(),
                    "a WF's node is not read as running after its compile".to_string(),
                ));
            }
            let Some((label, code, detail)) = retry else {
                // The steps always end with the record's or a read again; this is never reached.
                return;
            };
            if attempt >= tries {
                self.fail(deployment_id, "deploy", &label, &code, &detail);
                return;
            }
            self.pause(attempt).await;
        }
    }

    /// A WF's ILK, which its status names once its node runs, written into its record (stage 3
    /// §4); a node made again (compiled again after it was gone) names its new one. The desired
    /// state again when one was learned.
    fn learn_wf_ilks(
        &self,
        deployment_id: &str,
        observed: &Observed,
    ) -> Result<Option<DesiredState>, String> {
        let Some(record) = self.record(deployment_id) else {
            return Ok(None);
        };
        let learned: Vec<(String, String)> = record
            .nodes
            .iter()
            .filter(|node| node.is_wf())
            .filter_map(|node| {
                let ilk = observed.nodes.get(&node.role)?.status_ilk.clone()?;
                (!ilk.eq_ignore_ascii_case(&node.ilk_id)).then(|| (node.role.clone(), ilk))
            })
            .collect();
        if learned.is_empty() {
            return Ok(None);
        }
        let at_ms = self.world.now_ms();
        let updated = self.update_record(deployment_id, |record| {
            for (role, ilk) in &learned {
                if let Some(node) = record.nodes.iter_mut().find(|node| node.role == *role) {
                    node.ilk_id = ilk.clone();
                }
            }
            record.updated_at_ms = at_ms;
        });
        for (role, ilk) in &learned {
            self.journal(
                deployment_id,
                json!({"event": "learned", "role": role, "ilk_id": ilk}),
            );
        }
        match updated {
            Some(record) => self.desired_for(&record).map(Some),
            None => Ok(None),
        }
    }

    /// What runs of `desired` (§5.2): the ILKs, the document's hash, the relations and the
    /// definitions in the SHM; the assets on disk; each node through `get_node_status` (a WF's
    /// ILK from it); each key's assignment through `vault_list`. An error is a code and its detail,
    /// for a retry.
    pub async fn observe(&self, desired: &DesiredState) -> Result<Observed, (String, String)> {
        let ilks = self
            .world
            .identity()
            .map_err(|err| ("IDENTITY_UNAVAILABLE".to_string(), err))?;
        let find = |ilk_id: &str| {
            ilks.iter()
                .find(|ilk| ilk.ilk_id.eq_ignore_ascii_case(ilk_id))
                .cloned()
        };
        let mut observed = Observed {
            person: desired
                .person
                .as_ref()
                .and_then(|person| find(&person.ilk_id)),
            ..Observed::default()
        };
        for node in &desired.nodes {
            let call = AdminCall {
                action: "get_node_status".to_string(),
                target: node.hive().to_string(),
                params: json!({"node_name": node.node_name}),
                timeout: ACTION_WAIT,
            };
            let (presence, status_ilk) = match self.admin.call(&call).await {
                AdminOutcome::Ok(payload) => (
                    NodePresence::Present(
                        payload["node_status"]["lifecycle_state"]
                            .as_str()
                            .unwrap_or("UNKNOWN")
                            .to_string(),
                    ),
                    payload["node_status"]["identity"]["ilk_id"]
                        .as_str()
                        .filter(|ilk| crate::ids::is_ilk_id(ilk))
                        .map(str::to_string),
                ),
                AdminOutcome::Refused { code, .. } if code == "NODE_NOT_FOUND" => {
                    (NodePresence::Absent, None)
                }
                AdminOutcome::Refused { code, detail, .. } => return Err((code, detail)),
                AdminOutcome::Unanswered(why) => return Err(("NO_ANSWER".to_string(), why)),
            };
            // A WF's ILK is the one its status names: the orchestrator's.
            let ilk_id = match (&status_ilk, node.is_wf()) {
                (Some(status_ilk), true) => status_ilk.as_str(),
                _ => node.ilk_id.as_str(),
            };
            observed.nodes.insert(
                node.role.clone(),
                NodeObserved {
                    presence,
                    ilk: find(ilk_id),
                    status_ilk,
                },
            );
            if let Some(definition) = &node.definition {
                for asset in definition.assets() {
                    if self.world.asset_present(&asset.hash) {
                        observed.assets_present.insert(asset.hash.clone());
                    }
                }
            }
        }
        for entry in &desired.vault {
            let call = vault_list_call(
                &desired.tenant_id,
                &entry.resource_type,
                Some(&entry.key),
                ACTION_WAIT,
            );
            match self.admin.call(&call).await {
                AdminOutcome::Ok(payload) => {
                    if let Some(secret) = secrets_of(&payload)
                        .into_iter()
                        .find(|secret| secret.key == entry.key)
                    {
                        observed
                            .vault_assigned
                            .insert(entry.key.clone(), secret.assigned_ilks);
                    }
                }
                AdminOutcome::Refused { code, detail, .. } => return Err((code, detail)),
                AdminOutcome::Unanswered(why) => return Err(("NO_ANSWER".to_string(), why)),
            }
        }
        Ok(observed)
    }

    async fn execute(&self, desired: &DesiredState, step: &Step) -> StepOutcome {
        if let StepKind::AwaitIlk { role } = &step.kind {
            return StepOutcome::Retry {
                code: "NODE_ILK_UNKNOWN".to_string(),
                detail: format!("the WF of the role {role} runs and its status names no ILK yet"),
            };
        }
        if let StepKind::WriteAssets { role, hashes } = &step.kind {
            let Some(definition) = desired
                .nodes
                .iter()
                .find(|node| node.role == *role)
                .and_then(|node| node.definition.as_ref())
            else {
                return StepOutcome::Fail {
                    code: "ASSET_WRITE_FAILED".to_string(),
                    detail: format!("the role {role} has no definition"),
                };
            };
            let mut written = Vec::new();
            for asset in definition.assets() {
                if !hashes.contains(&asset.hash) || written.contains(&asset.hash) {
                    continue;
                }
                match self.world.write_asset(&asset.value) {
                    Ok(hash) if hash == asset.hash => written.push(hash),
                    Ok(hash) => {
                        return StepOutcome::Fail {
                            code: "ASSET_WRITE_FAILED".to_string(),
                            detail: format!("an asset was written as {hash}, not {}", asset.hash),
                        }
                    }
                    Err(err) => {
                        return StepOutcome::Retry {
                            code: "ASSET_WRITE_FAILED".to_string(),
                            detail: err,
                        }
                    }
                }
            }
            return StepOutcome::Done(json!({"written": written}));
        }
        let Some(call) = &step.call else {
            return StepOutcome::Done(Value::Null);
        };
        match (self.admin.call(call).await, &step.kind) {
            // A node launched by an earlier try that read as absent: it is there.
            (AdminOutcome::Refused { code, .. }, StepKind::RunNode { .. })
                if code == "NODE_ALREADY_EXISTS" =>
            {
                StepOutcome::Done(json!({"already_exists": true}))
            }
            (outcome, _) => outcome_of(outcome),
        }
    }

    // --- a removal (§9) ---------------------------------------------------------------------------

    /// Retires each deployment's nodes, newest deployment first and each one's nodes in the
    /// source's reverse order (for the plain ED the IO first, so nothing more comes in, then the
    /// conscious); marks the person when the whole grouping goes; then the records go `removed`.
    async fn run_remove(&self, deployment_ids: &[String], person: Option<&str>, tenant_id: &str) {
        let tries = self.tries();
        // The person's mark goes in the founding deployment's journal: the oldest, the list's last.
        let journal_id = deployment_ids.last().cloned().unwrap_or_default();
        let mut attempt = 0;
        'tries: loop {
            attempt += 1;
            for deployment_id in deployment_ids {
                let Some(record) = self.record(deployment_id) else {
                    continue;
                };
                if record.state != DeploymentState::Removing {
                    continue;
                }
                self.journal(
                    deployment_id,
                    json!({"event": "run", "operation": "remove", "attempt": attempt}),
                );
                for node in record.nodes.iter().rev() {
                    let (label, call) = removal_call(&record.hive, node);
                    let gone = if node.is_wf() {
                        "WORKFLOW_NOT_FOUND"
                    } else {
                        "NODE_NOT_FOUND"
                    };
                    let outcome = self
                        .journaled(deployment_id, &label, Some(&call), async {
                            match self.admin.call(&call).await {
                                AdminOutcome::Refused { code, .. } if code == gone => {
                                    StepOutcome::Done(json!({"not_found": true}))
                                }
                                other => outcome_of(other),
                            }
                        })
                        .await;
                    // A WF's ILK is the one its record learned, else the one the SHM holds for its
                    // node.
                    let ilk_id = if node.ilk_id.is_empty() {
                        self.ilk_of_node(&node.node_name).unwrap_or_default()
                    } else {
                        node.ilk_id.clone()
                    };
                    let mark = match outcome {
                        // A workflow's delete retires its node, as a kill_node's retire does: an
                        // ILK the SHM still holds as active, its mark failed or unknown, is
                        // SY.deploy's to mark.
                        StepOutcome::Done(_) if node.is_wf() => {
                            !ilk_id.is_empty() && self.ilk_present(&ilk_id)
                        }
                        // The orchestrator drops the node's map entry before it marks its ILK, so
                        // a mark it reports failed or unknown is SY.deploy's to make (0.1.159).
                        StepOutcome::Done(answer) if answer["not_found"] == json!(true) => {
                            self.ilk_present(&ilk_id)
                        }
                        StepOutcome::Done(answer) => answer["ilk_retired"] != json!(true),
                        StepOutcome::Retry { code, detail } => {
                            if attempt >= tries {
                                self.fail_removal(deployment_ids, &label, &code, &detail);
                                return;
                            }
                            self.pause(attempt).await;
                            continue 'tries;
                        }
                        StepOutcome::Fail { code, detail } => {
                            self.fail_removal(deployment_ids, &label, &code, &detail);
                            return;
                        }
                    };
                    if mark {
                        let label = format!("delete_ilk:{}", node.role);
                        if let Err((code, detail, retry)) = self
                            .mark_ilk(deployment_id, &label, &ilk_id, tenant_id)
                            .await
                        {
                            if retry && attempt < tries {
                                self.pause(attempt).await;
                                continue 'tries;
                            }
                            self.fail_removal(deployment_ids, &label, &code, &detail);
                            return;
                        }
                    }
                }
            }
            if let Some(person) = person {
                let present = self
                    .world
                    .identity()
                    .map(|ilks| {
                        ilks.iter()
                            .any(|ilk| ilk.ilk_id.eq_ignore_ascii_case(person))
                    })
                    .unwrap_or(true);
                if present {
                    if let Err((code, detail, retry)) = self
                        .mark_ilk(&journal_id, "delete_ilk:person", person, tenant_id)
                        .await
                    {
                        if retry && attempt < tries {
                            self.pause(attempt).await;
                            continue 'tries;
                        }
                        self.fail_removal(deployment_ids, "delete_ilk:person", &code, &detail);
                        return;
                    }
                }
            }
            break;
        }
        for deployment_id in deployment_ids {
            let at_ms = self.world.now_ms();
            self.update_record(deployment_id, |record| {
                if record.state == DeploymentState::Removing {
                    let by = record
                        .removal
                        .as_ref()
                        .map(|removal| removal.by.clone())
                        .unwrap_or_else(|| "SY.deploy".to_string());
                    record.state = DeploymentState::Removed;
                    record.note(at_ms, "removed", &by, None);
                }
            });
            self.journal(deployment_id, json!({"event": "state", "state": "removed"}));
        }
        tracing::info!(deployments = ?deployment_ids, "a removal is done");
    }

    /// Whether the SHM holds an ILK; when it cannot be read, taken as held, so the mark is sent.
    fn ilk_present(&self, ilk_id: &str) -> bool {
        self.world
            .identity()
            .map(|ilks| {
                ilks.iter()
                    .any(|ilk| ilk.ilk_id.eq_ignore_ascii_case(ilk_id))
            })
            .unwrap_or(true)
    }

    /// The ILK the SHM holds for a node, by its name.
    fn ilk_of_node(&self, node_name: &str) -> Option<String> {
        self.world.identity().ok().and_then(|ilks| {
            ilks.into_iter()
                .find(|ilk| ilk.handler_node.as_deref() == Some(node_name))
                .map(|ilk| ilk.ilk_id)
        })
    }

    /// Marks an ILK with `delete_ilk` (never purged): an ILK already marked or gone is done.
    async fn mark_ilk(
        &self,
        deployment_id: &str,
        label: &str,
        ilk_id: &str,
        tenant_id: &str,
    ) -> Result<(), (String, String, bool)> {
        let call = AdminCall {
            action: "delete_ilk".to_string(),
            target: MOTHERBEE.to_string(),
            params: json!({"ilk_id": ilk_id, "tenant_id": tenant_id}),
            timeout: ACTION_WAIT,
        };
        let outcome = self
            .journaled(deployment_id, label, Some(&call), async {
                match self.admin.call(&call).await {
                    AdminOutcome::Refused { code, .. } if code == "ILK_NOT_FOUND" => {
                        StepOutcome::Done(json!({"not_found": true}))
                    }
                    other => outcome_of(other),
                }
            })
            .await;
        match outcome {
            StepOutcome::Done(_) => Ok(()),
            StepOutcome::Retry { code, detail } => Err((code, detail, true)),
            StepOutcome::Fail { code, detail } => Err((code, detail, false)),
        }
    }

    fn fail_removal(&self, deployment_ids: &[String], step: &str, code: &str, detail: &str) {
        for deployment_id in deployment_ids {
            if self
                .record(deployment_id)
                .is_some_and(|record| record.state == DeploymentState::Removing)
            {
                self.fail(deployment_id, "remove", step, code, detail);
            }
        }
    }
}

/// How a node goes (stage 3 §4): a WF with its workflow's delete, forced, which takes its running
/// instances with it, in its retire mode (SY.wf-rules removes the node with the orchestrator's
/// retire, its ILK marked, never purged); any other node with `kill_node`'s retire.
fn removal_call(hive: &str, node: &RecordNode) -> (String, AdminCall) {
    match crate::ids::workflow_name(&node.node_name) {
        Some(workflow) => (
            format!("wf_rules_delete:{}", node.role),
            AdminCall {
                action: "wf_rules_delete".to_string(),
                target: hive.to_string(),
                params: json!({"workflow_name": workflow, "force": true, "retire": true}),
                timeout: WF_WAIT,
            },
        ),
        None => (
            format!("kill_node:{}", node.role),
            AdminCall {
                action: "kill_node".to_string(),
                target: hive.to_string(),
                params: json!({"node_name": node.node_name, "retire_instance": true}),
                timeout: KILL_WAIT,
            },
        ),
    }
}
