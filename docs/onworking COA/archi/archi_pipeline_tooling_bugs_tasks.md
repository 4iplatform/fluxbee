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
