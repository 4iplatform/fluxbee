# Archi Pipeline And Tooling Bug Tasks

**Date:** 2026-05-17  
**Scope:** concrete code/tooling bugs in `SY.architect` before documentation cleanup and before cognitive/strategy tuning.

## 0. Principle

Do not add special-case code for a single E2E scenario.

The reference scenario is only a proving case:

- Existing node: `AI.sales@motherbee`
- Missing nodes/resources: `IO.api`, `IO.slack`, and a new `WF.*` mirror/echo node
- Desired behavior: inbound API messages reach `AI.sales@motherbee`, AI responses return to API, and AI input/output is mirrored to Slack

Archi must be able to design and drive this class of solution through the generic pipeline.

## 1. Current Observed Bugs

### [x] ARCHI-BUG-1 — Designer invalid manifest blocks instead of self-repairing

Observed log:

- `fluxbee_start_pipeline` starts the design loop.
- Designer returns an invalid `solution_manifest`.
- Error: `designer returned invalid solution_manifest: missing field 'solution'`.
- Pipeline becomes blocked with `DESIGN_INCOMPLETE`.

Problem:

- `run_designer_with_context(...)` parses the submitted manifest directly into `SolutionManifestV2`.
- Parse/validation errors return `Err(...)`.
- `run_design_loop(...)` only iterates when it receives a valid manifest and an auditor verdict of `revise`.
- A malformed designer tool payload never reaches the normal design repair loop.

Expected behavior:

- Malformed designer output is treated as repairable design feedback for up to the normal design iteration limit.
- The next designer attempt receives structured feedback such as:
  - missing required top-level field `solution`
  - expected shape: `manifest_version`, `solution`, `desired_state`, `advisory`
- Only block after repeated schema failure or exhausted budget/iterations.

Acceptance:

- A missing top-level `solution` field triggers a retry, not immediate pipeline block.
- The design loop trace records the schema failure and retry attempt.
- If the designer still fails after the retry limit, the operator sees a concrete `DESIGN_SCHEMA_INVALID` or equivalent blocker.

Implementation note 2026-05-17:

- `run_designer_with_context(...)` now returns a `DesignerRunOutcome`.
- Invalid/missing `solution_manifest` payloads become structured design feedback instead of immediate `Err`.
- `run_design_loop(...)` persists `design_schema_validation` trace events and retries until `MAX_DESIGN_ITERATIONS`.
- Final repeated schema failure returns `DESIGN_SCHEMA_INVALID`.

### [x] ARCHI-BUG-2 — Blocked pipeline retry request does not route to `fluxbee_pipeline_action`

Observed log:

- Previous pipeline is blocked.
- Operator says: `reintenta hasta lograr un plan`.
- `fluxbee_start_pipeline` reports `blocked_run_pending` and lists options: `discard`, `restart_from_design`, `retry`.
- Archi response becomes: `The model suggested a confirmation, but no pending action was staged in this chat.`

Problem:

- The session has a valid blocked pipeline action path: `fluxbee_pipeline_action`.
- The conversational layer did not translate the operator's retry intent into `fluxbee_pipeline_action`.
- It fell into the generic "suggested confirmation" guard.

Expected behavior:

- When a blocked pipeline exists and the operator says `reintenta`, `retry`, `reinicia`, or similar, Archi must call `fluxbee_pipeline_action`.
- If the operator intent maps clearly to restart from design, use `restart_from_design`.
- If the intent is ambiguous between `retry` and `restart_from_design`, ask one short disambiguation question.

Acceptance:

- `blocked_run_pending` followed by `reintenta` does not call `fluxbee_start_pipeline` again.
- The next tool call is `fluxbee_pipeline_action`.
- The chat never emits the generic confirmation guard for this case.

Implementation note 2026-05-17:

- Added host-level blocked pipeline control handling before normal AI chat.
- Clear retry/restart/discard text now resolves directly to the pipeline action path.
- For design-stage blockers, `reintenta` maps to `restart_from_design` because there is no useful confirmation checkpoint to re-enter.
- Delegated recovery text such as "hacé lo que tengas que hacer" also maps deterministically: design-stage blockers restart from design; execution-stage blockers retry.

### [x] ARCHI-BUG-3 — Confirmation guard hides the real recovery state

Observed log:

- Host returned: `The model suggested a confirmation, but no pending action was staged in this chat.`

Problem:

- The guard is useful for preventing fake confirmations, but the message is too generic when a blocked pipeline exists.
- It hides the actionable recovery path and looks like stale confirmation code.

Expected behavior:

- If no pending confirmation exists but a blocked pipeline exists, the guard should surface the blocked pipeline options.
- The response should mention:
  - pipeline id
  - blocked reason
  - valid actions: `discard`, `restart_from_design`, `retry`

Acceptance:

- Generic guard message is not used when a blocked pipeline is present.
- Operator receives a deterministic recovery message or Archi invokes `fluxbee_pipeline_action` directly when intent is clear.

Implementation note 2026-05-17:

- The confirmation guard now checks for blocked pipeline state before returning the generic no-pending-action message.
- If a blocked pipeline exists, it returns recovery options and the blocker instead.

### [x] ARCHI-BUG-4 — `fluxbee_start_pipeline` blocked-run response is not strongly actionable enough

Current behavior:

- Tool returns `status: "blocked_run_pending"` and says to call `fluxbee_pipeline_action`.

Problem:

- The main Archi model still mishandled the follow-up.
- The tool contract may be sufficient for humans but not deterministic enough for tool routing.

Expected behavior:

- The returned payload should be easy for Archi to consume mechanically.
- Include a compact `next_tool` hint:
  - `next_tool: "fluxbee_pipeline_action"`
  - `allowed_actions: ["discard", "restart_from_design", "retry"]`

Acceptance:

- The model can reliably select the correct tool after `blocked_run_pending`.
- No prompt-specific case details are required.

Implementation note 2026-05-17:

- `blocked_run_pending` responses now include `next_tool: "fluxbee_pipeline_action"` and `allowed_actions`.

### [x] ARCHI-BUG-5 — Discarded blocked pipeline can reappear from stale run state

Observed log:

- `fluxbee_pipeline_action` returned OK for `discard`.
- A following `fluxbee_start_pipeline` still reported the same `pipeline_run_id` as `blocked_run_pending`.
- The old `blocked_reason` still contained `budget:100000`, proving the response came from stale pipeline state, not the new run.

Problem:

- Pipeline run reads can see duplicate/stale records for the same `pipeline_run_id`.
- The lookup path selected an older `Blocked` record instead of the newest terminal `Failed` record.

Expected behavior:

- Pipeline reads coalesce records by `pipeline_run_id`.
- The newest `(updated_at_ms, created_at_ms)` record wins.
- Once a run is terminal, it must not block a new pipeline.

Implementation note 2026-05-17:

- Added `latest_pipeline_runs_by_id(...)`.
- Applied it to latest non-terminal lookup, latest blocked lookup, and load-by-id fallback reads.
- Added unit coverage that a newer terminal state wins over an older blocked state.

### [x] ARCHI-BUG-6 — Plan compiler missing `submit_executor_plan` escapes as protocol error

Observed log:

- Tenant was resolved correctly to `tnt:00000000-0000-0000-0000-000000000001`.
- `fluxbee_plan_compiler` failed with:
  - `protocol error: plan_compiler agent failed: plan_compiler did not call submit_executor_plan`

Problem:

- `run_plan_compiler_with_context(...)` required a successful `submit_executor_plan` tool result.
- If the model finished without that tool call, the code returned `Err(...)`.
- The tool wrapper surfaced it as a transport/protocol failure instead of a recoverable compiler-output failure.

Expected behavior:

- Missing or malformed `submit_executor_plan` is treated like invalid model output.
- The compiler gets one generic contract-feedback retry:
  - call `submit_executor_plan` exactly once
  - use `status='plan_ready'` with `plan`, or `status='blocked'` with `blocked_reason`
- If the retry still fails, Archi returns a structured blocked result with trace data, not a protocol error.

Implementation note 2026-05-17:

- Added `PlanCompilerRunOutcome` with `Submitted` and `Invalid`.
- Missing submit call, unsupported status, missing plan, and invalid executor-plan shape now become `Invalid`.
- `run_plan_compiler_transaction(...)` retries invalid tool output once with generic feedback.
- Repeated invalid output returns `blocked_code=plan_compiler_invalid_output`.
- Added unit coverage for feedback text and structured blocker conversion.

### [x] ARCHI-BUG-8 — WF node created with base runtime but never appears in inventory

Observed log:

- Archi compiles a `run_node` plan for `WF.sales@motherbee` with `runtime = "wf.engine"` and `runtime_version = "1.0.0"`.
- `run_node` returns `status = "ok"` and creates `/var/lib/fluxbee/nodes/WF/WF.sales@motherbee/config.json`.
- A later inventory/list-nodes read does not show `WF.sales@motherbee`.

Problem:

- `wf.engine` is the base runtime binary package only.
- Real `WF.*` managed nodes must run a concrete workflow package runtime such as `wf.sales`, generated by `SY.wf-rules` with `runtime_base = "wf.engine"`.
- `run_node` accepted the base runtime and returned success after launching the transient systemd unit, even though the process could not become a healthy workflow node without a package-native `flow/definition.json`.

Expected behavior:

- Plans must not spawn `WF.*` directly with `wf.engine`.
- The correct path for creating a workflow from source is `wf_rules_compile_apply` with `auto_spawn=true` and `tenant_id` on first deploy.
- Direct `run_node` for `WF.*` is valid only when the runtime is an already published workflow package runtime.

Implementation note 2026-05-17:

- `sy-orchestrator` now rejects `WF.*` managed spawn/start when the selected runtime manifest entry is not `type = "workflow"`, returning `WF_RUNTIME_PACKAGE_REQUIRED`.
- `SY.admin` action help for `run_node` now states that `wf.engine` is not valid for direct `WF.*` spawn.
- Archi platform facts and handbook now distinguish WF base runtime from concrete workflow package runtime.

### [x] ARCHI-BUG-12 — Storage/identity miss vault bootstrap broadcast due to ephemeral-connect-then-reconnect race

Observed (2026-05-17, clean reinstall + restart cycle):

- After `cleanall + install.sh + restart`, vault arrives last per hive.yaml, emits its bootstrap `VAULT_SECRET_CHANGED { op=put }` for every secret in `vault.db`, the router fans out — but the storage node never reacts and stays in `STORAGE_NOT_READY` indefinitely.
- Router fanout summary shows `delivered_to=7, skipped_self=1` and `SY.storage@motherbee` is NOT in the destination list, even though storage was scheduled to start before vault.

Timing (extracted from router journal):

```text
17:34:45.998 storage HELLO (ephemeral) uuid=644b3b33 → router registers it
17:34:46.004 storage WARN "vault lookup failed (VAULT_UNAVAILABLE)" — vault not up yet
              [storage closes the ephemeral connection]
17:34:46.356 vault HELLO uuid=c625a702 → router registers vault
17:34:46.357 vault emits bootstrap broadcast
17:34:46.358 router fanout: registered_nodes=8, delivered_to=7, SY.storage NOT IN LIST
17:34:47.018 storage HELLO (persistent, second connection) uuid=f8fc63e5 → router registers
              [620ms TOO LATE — bootstrap already gone]
```

Problem (root cause):

- `sy_storage` and `sy_identity` follow a pattern of (a) open an **ephemeral** SDK connection to query vault, (b) close it after the lookup completes or fails, (c) open the real **persistent** connection that stays alive for the lifetime of the node.
- Between (b) and (c) the node is NOT registered in the router. If vault emits its bootstrap broadcast in that window, the router's fanout (`for node in nodes_guard.iter()` in `src/router/mod.rs`) skips the node — broadcasts are NOT buffered for future re-delivery.
- The previous comment in `hive.yaml` claiming "Vault-last guarantees delivery" was wrong: vault-last only guarantees that nodes already persistently registered receive the broadcast. The ephemeral-reconnect window breaks that.

Decision (2026-05-17): option C from design discussion — connect persistent FIRST, then reuse that connection for the vault lookup. No timeouts, no defensive polling — fix the cause.

Implementation note 2026-05-17:

- `sy_storage.rs`: `resolve_database_url(...)` no longer opens its own ephemeral connection. It takes `(&NodeSender, &mut NodeReceiver, ...)` and runs the vault `resolve_resource` over them. The `main` flow now calls `connect_with_retry(...)` FIRST (persistent), logs `sy.storage connected to router`, and only then calls `resolve_database_url(&sender, &mut receiver, ...)`. If vault is not yet up, the lookup returns `Missing` and storage continues degraded — but because the persistent connection is announced, vault's bootstrap broadcast lands in storage's receive loop and `handle_vault_secret_changed` triggers the `exit(0)` rescue.
- `sy_identity.rs`: same refactor. `resolve_database_url(...)` now takes `(&NodeSender, &mut NodeReceiver, ...)`. `main` constructs `node_config` and calls `connect_with_retry(...)` BEFORE the `if is_primary { resolve_database_url(...) }` block. The downstream flow (initialize_identity_database_backend, load_identity_store_from_db, ensure_system_ilks_from_hive, identity_shm setup, sync_listener, delta_event_rx, main select loop) is unchanged in code — only the order at the top of `main` changed.
- Removed the now-redundant `connect_with_retry` call that previously appeared just before the main select loop in identity.
- No new timeouts, no polling fallback. The bootstrap broadcast pathway is the only synchronization mechanism (event-driven), exactly as the architecture intends.

Side observation:

- The 30-second restart cycle the operator observed on the orchestrator was a downstream consequence: `wait_for_storage_db_ready(timeout=30s)` in `sy_orchestrator.rs` fails after 30s because storage never becomes ready, propagates the error with `?`, the orchestrator exits non-zero, systemd restarts it (Restart=always, RestartSec=5), repeat. Once storage receives the broadcast on first try (this fix), the readiness probe succeeds within seconds and the orchestrator finishes its bootstrap cleanly.

Acceptance:

- After `cleanall + install.sh + restart`, the journal shows:
  - vault emits bootstrap broadcast with `emitted=N`
  - router fanout summary lists every SY consumer (including storage and identity) in `delivered_to`
  - storage's `handle_vault_secret_changed` matches and exits(0); systemd restart reconnects to the DB; orchestrator readiness probe succeeds within the 30s window
- No more silent degradation on the first boot cycle.

### [x] ARCHI-BUG-11 — Blocked-run-pending surfaces as text-only; operator must type the recovery word

Observed (2026-05-17, follow-up to ARCHI-BUG-10):

- After ARCHI-BUG-10, design/plan/artifact-class blocked pipelines auto-discard. Execution-class and unknown-class blockers still surface to the operator and require typing `discard` / `restart_from_design` / `retry`.
- The chat already renders inline confirmation buttons for `confirm1` / `confirm2` checkpoints (`buildInlineConfirmBar`), but `blocked_run_pending` responses do not opt into that bar.
- Operator UX: a "decision moment" arrives as a plain text suggestion, with no clickable affordance, even though the answer is almost always "drop it".

Decision (2026-05-17):

- Single warning-colored `Discard blocked pipeline` button on `blocked_run_pending` responses. No `Restart from design` or `Retry` button — those stay available via text input for the rare cases that warrant them. This keeps the UI focused on the dominant decision (drop the stale thing) while preserving the advanced recovery vocabulary.
- Click sends `discard` as the chat message; the existing host-level intent classifier (added in ARCHI-BUG-2) catches it and routes to `fluxbee_pipeline_action(action="discard")` without spending tokens on the AI chat path.

Implementation note 2026-05-17:

- Added `.inline-confirm-btn.warning` CSS variant (orange) for destructive-but-explicit recovery clicks.
- `responseWantsInlineConfirm(data)` now returns true when `output.status === "blocked_run_pending"` regardless of `data.mode`.
- `pendingConfirmationUiMeta(data)` returns a special meta for blocked_run_pending: `{ confirmHidden: true, cancelLabel: "Discard blocked pipeline", cancelClass: "warning", cancelMessage: "discard", lockReason: ..., executionHint: "Discarding blocked pipeline..." }`.
- `buildInlineConfirmBar` honors `meta.confirmHidden` (skip primary button), `meta.cancelClass` (warning color), and `meta.cancelMessage` (override the default `CANCEL` payload with `discard`).
- No backend changes required: the host-level classifier from ARCHI-BUG-2 already routes `discard` to `fluxbee_pipeline_action`.

Acceptance:

- When the start tool returns `blocked_run_pending`, the chat shows a single orange button. Clicking it discards the blocked pipeline and re-enables the composer so the operator can re-issue their original intent.
- Operators who want `retry` or `restart_from_design` can still type those words — no regression of the text-driven path.
- `confirm1` / `confirm2` confirmation flows are unchanged.

### [x] ARCHI-BUG-10 — Stale blocked pipelines force the operator to type recovery words before any new pipeline

Observed log (2026-05-17):

- Operator opens a new chat intent ("create the WF.sales node routing IO.api ↔ AI.sales with Slack echo").
- Archi responds with: "Hay un pipeline bloqueado en esta sesión: `09837fe7-...`. Opciones: discard / restart_from_design / retry."
- The blocked pipeline is unrelated to the new intent and was blocked by a design-stage failure that left no recoverable state.
- Operator must type the recovery word before Archi will even start the new pipeline.

Problem:

- `StartPipelineTool::call` calls `latest_nonterminal_pipeline_run_for_session` and, if a `Blocked` run exists, returns `blocked_run_pending` unconditionally regardless of WHY the previous run is blocked.
- The previous pipeline was blocked by a failure class (e.g. `DesignIncomplete`, `PlanInvalid`, `ArtifactContractInvalid`) for which `retry` semantics do not exist — the run can only be discarded or replaced. There is nothing for the operator to recover.
- The ceremony of typing `restart_from_design` adds friction without enabling any recovery the operator could not also get by simply asking for a new pipeline.

Expected behavior:

- The block-on-new-pipeline guard should only fire when the previous blocker is actually recoverable.
- Failure classes that mean "the produced artifact is unusable, rework from scratch" should be auto-discarded transparently when a new pipeline intent arrives. The auto-discard is recorded in the discarded run's `state_json.auto_discarded` and in the new run's `auto_discarded_predecessor` so postmortems can trace it.
- Execution-class blockers (`ExecutionActionFailed`, `ExecutionTimeout`, `ExecutionEnvironmentMissing`) still surface recovery options — a real `retry` may avoid re-doing work, and the situation may need operator attention.
- Unknown / missing failure class also surfaces options (ambiguous; never auto-discard silently).

Decision (2026-05-17): determinist by failure class, not by TTL. Time-based discard is a workaround; the right model is "blocked = unrecoverable" vs "blocked = recoverable".

| Failure class | Recoverable? | Behavior on new pipeline intent |
|---|---|---|
| `DesignIncomplete`, `DesignConflict` | No | auto-discard, start new |
| `SnapshotPartialBlocking`, `SnapshotSectionUnsupported`, `DeltaUnsupported` | No | auto-discard, start new |
| `ArtifactTaskUnderspecified`, `ArtifactContractInvalid`, `ArtifactLayoutInvalid` | No | auto-discard, start new |
| `PlanInvalid`, `PlanContractInvalid` | No | auto-discard, start new |
| `ExecutionEnvironmentMissing`, `ExecutionActionFailed`, `ExecutionTimeout` | Possibly | surface options (today's behavior) |
| `UnknownResidual` / null | Ambiguous | surface options (today's behavior) |

Implementation note 2026-05-17:

- Added `is_auto_discardable_blocked_failure_class(Option<&str>) -> bool` matching the SCREAMING_SNAKE_CASE serialized forms of `FailureClass`.
- Added `auto_discard_stale_blocked_run(...)`: clones the run, sets `status=Failed`, `current_stage=Failed`, appends `state_json.auto_discarded = {at_ms, reason, failure_class, original_blocked_reason}`, saves, and logs at info level (warn-level reserved for actual operator-visible failures).
- Added `execute_pipeline_start_with_context_after_auto_discard(...)`: wraps the standard start helper and injects `auto_discarded_predecessor = {pipeline_run_id, failure_class, blocked_reason}` into the new pipeline's start payload so the trace is preserved.
- `StartPipelineTool::call` now branches on `is_auto_discardable_blocked_failure_class(...)` before returning `blocked_run_pending`. If auto-discard succeeds, the new pipeline starts; if it fails (DB error etc.), falls through to the surface-options path defensively.
- 3 unit tests cover the failure class matrix: design/plan/artifact/snapshot/delta are discardable; execution classes are not; unknown/null/empty are not.

Acceptance:

- A previous pipeline blocked by `DesignIncomplete` does not block a new operator intent — the new pipeline starts directly and the old one is in history as `Failed` with `auto_discarded.reason = "stale_blocked_pipeline_displaced_by_new_intent"`.
- A previous pipeline blocked by `ExecutionActionFailed` still surfaces the three options exactly as before.
- An operator who genuinely wants to recover a Design-class blocked pipeline can do so by referencing the `pipeline_run_id` explicitly (the record is preserved in history; only the `active` slot is freed).

### [x] ARCHI-BUG-9 — Design auditor reports MANIFEST_UNAVAILABLE on a freshly produced valid manifest

Observed log:

- Pipeline restarted from design after a previous block.
- Design loop iteration 3 produced a valid manifest (`section_count=3`, `validation_result="ok"`, `solution_id="sales-conversation-routing"`).
- Audit on the same iteration came back with `score=1`, `status="revise"`, `blocking_issues=["MANIFEST_UNAVAILABLE"]`.
- Auditor summary: "I could not complete a structural audit because the manifest content is truncated and no current saved manifest is available in session. Please resend the full SolutionManifestV2..."
- Loop hit `max_iterations` and pipeline blocked.

Problem (root cause is two coupled defects):

1. `run_design_auditor_with_context(...)` registers `GetManifestCurrentTool` for the auditor agent. The auditor receives the manifest inline in `current_user_message`, so it does not need a separate tool to fetch it. The tool's presence let the model treat a stale miss as evidence that the inline manifest was incomplete.
2. `run_design_loop(...)` updates `current_solution_id` (a local variable) when designer succeeds and saves the manifest to `manifests/<solution_id>/`, but it does NOT update `PipelineRunRecord.solution_id` until AFTER the auditor returns. `GetManifestCurrentTool` resolves session → solution_id through `latest_solution_id_for_session(...)`, which reads the `pipeline_run.solution_id` struct field. The field is still `None` when the auditor calls the tool, so the lookup fails. The auditor then concludes the manifest is unavailable and contradicts the (perfectly valid) inline payload.

Expected behavior:

- Auditor audits the manifest passed inline; it does not look up alternative manifests.
- Any tool called between designer success and auditor return must see the up-to-date `pipeline_run.solution_id`.

Implementation note 2026-05-17:

- Removed `GetManifestCurrentTool` registration from `run_design_auditor_with_context`. Auditor now has only `submit_design_audit_verdict`.
- Auditor system prompt now explicitly states that the manifest is in `current_user_message` and that missing fields should be reported as `revise` findings, not as `MANIFEST_UNAVAILABLE`.
- `run_design_loop` now clones `pipeline_run` into `pipeline_run_with_solution`, sets `solution_id = current_solution_id`, and saves it via `save_pipeline_run_with_context` BEFORE calling the auditor. The post-audit save uses the same updated record so the propagation is consistent across the iteration.

### [x] ARCHI-BUG-7 — Plan compiler cannot inspect existing workflow definitions

Observed log:

- Operator asks for an API → AI → API flow with Slack mirror through a WF echo workflow.
- Archi resolves:
  - `AI.sales@motherbee` exists
  - root tenant is `tnt:00000000-0000-0000-0000-000000000001`
  - runtimes `io.api`, `io.slack`, `ai.generic`, `wf.engine` exist
- Operator says existing `WF.echo.*` may be reused.
- `fluxbee_plan_compiler` blocks with missing `workflow_definition` and says it cannot determine whether the existing WF route/workflow is sufficient.

Problem:

- `PlanCompilerLiveQueryTool` is the correct read-only path for live planning state.
- Its allowlist did not include read-only WF actions:
  - `wf_rules_list_workflows`
  - `wf_rules_get_workflow`
  - `wf_rules_get_status`
- Therefore the compiler could inspect runtimes/nodes/routes but not the existing workflow catalog or definition before deciding whether reuse is possible.

Expected behavior:

- When workflow reuse is in scope, PlanCompiler can query the WF catalog and a concrete WF definition.
- It should only block after reading the relevant WF state, not before.
- If no reusable workflow exists, it should either synthesize a new workflow through the normal WF definition contract or block with a precise missing behavior reason.

Implementation note 2026-05-17:

- Added `wf_rules_list_workflows`, `wf_rules_get_workflow`, and `wf_rules_get_status` to the PlanCompiler live query allowlist.
- Added unit coverage so these read-only WF actions remain available to `query_hive`.

## 2. Tooling Hardening Tasks

### [x] ARCHI-HARD-1 — Add deterministic designer schema-retry loop

Implementation direction:

- Wrap designer parse/manifest validation errors into a structured design feedback event.
- Re-run designer with that feedback while respecting:
  - `MAX_DESIGN_ITERATIONS`
  - token budget
  - existing design loop trace model

Do not:

- Add special prompt text for the IO/API/Slack scenario.
- Accept partial manifests silently.

### [x] ARCHI-HARD-2 — Add host-level blocked pipeline intent routing

Implementation direction:

- Before normal AI chat execution, detect:
  - session has a blocked pipeline
  - operator text clearly asks to retry/restart/discard
- Call `apply_pipeline_action_with_context(...)` or equivalent path directly.

Rationale:

- Recovery commands are control-plane commands, not open-ended reasoning.
- This avoids spending tokens and avoids fake confirmation messages.

### [x] ARCHI-HARD-3 — Improve blocked pipeline recovery UX

Implementation direction:

- If operator intent is not clear, respond with one short choice prompt:
  - `retry`: return to last confirmation checkpoint
  - `restart_from_design`: discard blocked run and redesign from scratch
  - `discard`: close it without retrying

Acceptance:

- No generic "no pending action" message when recovery state exists.

### [x] ARCHI-HARD-4 — Add tests for blocked pipeline action routing

Required cases:

- Blocked design run + `reintenta` routes to `fluxbee_pipeline_action`.
- Blocked design run + `empeza de cero` routes to `restart_from_design`.
- Blocked design run + ambiguous text returns recovery options, not generic confirmation guard.

Implementation note 2026-05-17:

- Added deterministic unit coverage for retry/restart/discard text mapping.
- Added deterministic unit coverage for recovery-options text detection and recovery message content.
- Host chat now returns recovery options for blocked-pipeline help/option questions instead of falling through to normal AI chat.

### [ ] ARCHI-HARD-5 — Add tests for malformed designer manifest recovery

Required cases:

- First designer output missing `solution`, second output valid.
- First designer output has invalid desired_state section, second output valid.
- Designer remains invalid until max iterations, pipeline blocks with explicit schema error.

Partial implementation note 2026-05-17:

- Added unit coverage for actionable schema feedback.
- Full loop-level tests still need an injectable/mocked designer runner or an integration harness.

### [x] ARCHI-HARD-6 — Convert plan compiler protocol-output failures into recoverable output failures

Implementation direction:

- Do not let missing `submit_executor_plan` escape as `AiSdkError::Protocol`.
- Preserve token usage and tool lookup counts in the plan compile trace.
- Retry once with generic tool-contract feedback.
- Return a structured blocked response after repeated failure.

Do not:

- Add scenario-specific instructions for API/Slack/AI topology.
- Hide the failure as a successful empty plan.

### [x] ARCHI-HARD-7 — Expose WF read-only live state to PlanCompiler

Implementation direction:

- `query_hive` must include read-only WF introspection actions when workflow compile/apply is available.
- This is live-state discovery, not mutation.

Acceptance:

- PlanCompiler can list workflows, get a named workflow definition, and get WF status before planning `wf_rules_compile_apply` or workflow reuse.

## 3. Documentation Tasks After Code Bugs

### [x] ARCHI-DOC-1 — Update `rearchitecture_implementation_tasks.md`

Add a short note that malformed designer output is a retryable design-loop failure, not an immediate pipeline blocker.

### [x] ARCHI-DOC-2 — Update `handbook_fluxbee.md`

Clarify operator-facing recovery semantics for blocked pipelines:

- retry
- restart from design
- discard

Keep this as process/help, not prompt-specific instructions.

### [x] ARCHI-DOC-3 — Update `admin_help_reference.md` only if tool contracts changed

If `next_tool` / `allowed_actions` are added to `fluxbee_start_pipeline` output, document the structured response.

## 4. Later Cognitive / Strategy Work

Do not start these until the tooling bugs above are fixed.

### [~] ARCHI-STRAT-LAYERED — Designer iterates explicitly over platform layers

Observed pattern (from `restart_from_design` debug 2026-05-17):

- Designer free-form pass produces a manifest that is schema-valid but covers only some layers of the solution (e.g. 3 of 7 possible desired_state sections).
- Auditor returns `revise`; designer retries; same pattern.
- Pipeline blocks on `max_iterations` with partial coverage that schema-validation accepts but operationally is unusable.

Root cause:

- The designer must reason about 6 layers (infra, runtime distribution, identity/secrets, nodes, routing, application logic) in a single free pass. Any layer that is implicit can be silently skipped.
- The schema validator only checks structural shape, not layer coverage.
- The auditor catches gaps reactively, costing tokens and iterations.

Direction (option A from design discussion):

- Keep a single designer agent.
- Replace the one-shot `submit_solution_manifest` with a structured iteration: a new `assess_layer(layer)` tool that the designer calls in fixed order, one call per layer. Each call records either a layer fragment (work to do) or `no_work` (explicit skip with reason).
- Final `submit_solution_manifest` is accepted only after every required layer has an `assess_layer` outcome.
- Auditor verifies layer coverage as a structural rule (in addition to current schema/consistency checks).
- See `docs/onworking COA/archi/layered_designer_design.md` for the layer enumeration, tool contracts, budget semantics, and failure semantics.

Open design questions (resolve before implementation):

1. **Per-layer budget**: track per-layer token usage as observability only and keep a single global design budget? Or enforce per-layer hard caps?
2. **Layer-level failure semantics**: if `assess_layer` for one layer blocks (designer can't decide), do we (a) discard the whole manifest, (b) retry just that layer up to a per-layer iteration cap, or (c) emit the manifest with the layer marked `blocked` so the operator sees exactly which layer needs more input?

Acceptance:

- Designer cannot produce a manifest without an explicit per-layer outcome for every layer.
- A request like "create WF.sales routing IO.api ↔ AI.sales with Slack echo" passes all 6 layer assessments in a single design pass (not 3 iterations of partial output).
- A request that is unsatisfiable at one specific layer surfaces a precise per-layer blocker (resolution depends on Open Question 2).

Decisions taken (2026-05-17):

- **Budget** (Open Q 1): Model A — single global design budget, per-layer token usage recorded only for observability/trace.
- **Layer failure** (Open Q 2): Model B+C combined — when one or more layers come back `blocked`, the design loop retries with layer-specific feedback up to `MAX_DESIGN_ITERATIONS`; if still blocked, the manifest is emitted with `LAYER_BLOCKED:<layer>` injected into the audit verdict's `blocking_issues` and a `LAYER_BLOCKED_<code>` finding per layer (plan compile must refuse while these exist — separate follow-up below).

Implementation note 2026-05-17:

- Added `DesignerLayer` enum (`infra`, `runtimes`, `identity_secrets`, `nodes`, `routing`, `logic`), `LayerOutcome` enum (Work / NoWork / Blocked), `LayerAssessmentRecord`, and `LayerAssessmentStore` (`tokio::sync::Mutex<Vec<…>>`) shared between the assess tool and submit tool within a single designer call.
- New `AssessLayerTool` enforces canonical order and uniqueness; rejects work without fragment, no_work without reason, blocked without code/reason.
- `DesignerSubmitManifestTool` now holds the store and rejects submit until all six layers have a recorded outcome, listing missing layers in the error so the model retries correctly.
- `run_designer_with_context` creates a fresh store per call, surfaces a coverage error in `DesignerTrace.layer_outcomes` when the model fails to submit, and propagates the layer records on `DesignerOutput`.
- `run_design_loop` detects blocked layers on a successful designer return and:
  1. on a non-final iteration, builds `design_feedback_from_blocked_layers(...)` and continues without spending tokens on the auditor;
  2. on the final iteration, runs the auditor normally and calls `inject_blocked_layer_findings_into_verdict(...)` so the operator sees a precise per-layer blocker in `confirm1_summary`.
- Designer prompt rewritten with a "Layered iteration (REQUIRED contract)" section that lists the six layers, the per-layer outcome shapes, and the cross-layer dependency rule (Routing requires Nodes, Nodes require Runtime, etc.).
- Auditor prompt rewritten to drop coverage-checking (now enforced structurally by the host) and focus on per-layer CONTENT review.
- 9 unit tests (`layer_store_*`, `assess_layer_tool_*`, `submit_manifest_tool_*`, `blocked_layer_feedback_*`, `inject_blocked_layer_findings_*`) cover order/uniqueness, missing-arg rejection, coverage gating, blocked-layer feedback formatting, and verdict injection.

Still pending under this strat:

- Plan compiler must refuse compilation when `LAYER_BLOCKED:*` appears in the confirm1 verdict (today the layer-blocker is informational; plan compile path does not yet hard-stop on it).
- End-to-end run against the reference WF.sales request to confirm one-pass design with full coverage.
- Handbook section formalizing layered model + plan ordering (cross-references `layered_designer_design.md`).

### [ ] ARCHI-STRAT-1 — Evaluate whether Designer has enough platform knowledge

Use the reference IO/API/Slack/WF/AI scenario after tooling recovery works.

Question:

- Does Designer know how to express a topology that requires IO ingress, AI target, Slack mirror, and WF orchestration without producing executor steps?

### [ ] ARCHI-STRAT-2 — Evaluate whether Reconciler can translate that manifest

Question:

- Can manifest desired state become task packets and plan compiler input without custom code?

### [ ] ARCHI-STRAT-3 — Evaluate whether PlanCompiler can execute the resulting delta

Question:

- Does admin help expose enough action contracts to create missing IO/WF nodes, routes, and deployment bindings?

## 5. First Implementation Order

1. `ARCHI-BUG-1` / `ARCHI-HARD-1`
2. `ARCHI-BUG-2` / `ARCHI-HARD-2`
3. `ARCHI-BUG-3` / `ARCHI-HARD-3`
4. `ARCHI-HARD-4` and `ARCHI-HARD-5`
5. Documentation updates
6. Only then run the reference E2E scenario through Archi
