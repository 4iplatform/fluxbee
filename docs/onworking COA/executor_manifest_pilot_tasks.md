# Fluxbee Executor Plan Pilot Tasks

**Status:** draft  
**Date:** 2026-04-19  
**Audience:** `SY.admin`, `SY.architect`, executor/runtime developers  
**Parent spec:** `executor_manifest_pilot_spec.md`

---

## 1. Goal

Implement the pilot where:

- `SY.architect` is the host/UI
- `SY.admin` hosts the AI executor mode
- execution uses OpenAI function calling
- executor-visible functions map 1:1 to real admin actions
- the input artifact is an `executor_plan`

This task list is only for the pilot execution layer, not for the full manifest/reconciler architecture.

---

## 2. Phase A — Plan Contract

- [ ] `EXEC-PILOT-A1` Define the `executor_plan` JSON schema in code.
  - Validate:
    - `plan_version`
    - `kind=executor_plan`
    - `metadata`
    - `execution.strict`
    - `execution.stop_on_error`
    - `execution.allow_help_lookup`
    - `execution.steps[]`
  - Reject unknown top-level fields.

- [ ] `EXEC-PILOT-A2` Validate `steps[].action` against the current admin action registry.
  - Fail early if the plan references an unknown admin action.

- [ ] `EXEC-PILOT-A3` Validate `steps[].args` shape against the executor-visible function schema for that action.
  - `args` must use real admin argument names.
  - No executor-only aliases such as `target` if the action contract uses `hive`.

- [ ] `EXEC-PILOT-A4` Define and validate `executor_fill`.
  - Explicit allowlist only.
  - No executor-side architectural choices.

Acceptance:

- invalid plans fail before executor invocation
- plans that drift from real admin contracts are rejected clearly

---

## 3. Phase B — `SY.admin` Executor Runtime

- [ ] `EXEC-PILOT-B1` Add executor mode/runtime entrypoint inside `SY.admin`.
  - Separate it clearly from normal HTTP admin handling.
  - Do not turn `SY.admin` into a chat host.

- [ ] `EXEC-PILOT-B2` Define the internal execution request contract from `SY.architect` to `SY.admin`.
  - Include:
    - execution/session id
    - plan payload
    - executor options
    - optional operator/context labels

- [ ] `EXEC-PILOT-B3` Define the execution response/event contract from `SY.admin` back to `SY.architect`.
  - Include:
    - `queued`
    - `running`
    - `help_lookup`
    - `done`
    - `failed`
    - `stopped`

- [ ] `EXEC-PILOT-B4` Implement stop-on-error behavior in `SY.admin`.
  - Any failing step stops the plan when `stop_on_error=true`.
  - Admin timeout/unavailable must stop execution clearly.

- [ ] `EXEC-PILOT-B5` Implement auxiliary read/help call restrictions.
  - Allowed:
    - `get_admin_action_help`
    - read-only admin actions needed to resolve infra-obvious values
  - Forbidden:
    - undeclared mutating calls
    - hidden destructive side effects

Acceptance:

- `SY.admin` can receive a plan and run it deterministically
- failures stop the flow cleanly

---

## 4. Phase C — OpenAI Function Catalog In `SY.admin`

- [x] `EXEC-PILOT-C1` Build an executor-visible OpenAI function catalog from the admin action registry.
  - Function name must equal admin action name.
  - Description must stay concise and operational.
  - Implemented in `SY.admin` with full-catalog-by-default generation from `INTERNAL_ACTION_REGISTRY`.

- [x] `EXEC-PILOT-C2` Generate strict JSON schemas from admin action metadata whenever possible.
  - `additionalProperties: false`
  - exact field names
  - required fields
  - enums where available

- [ ] `EXEC-PILOT-C3` Define manual overrides for actions whose schemas cannot yet be generated cleanly.
  - Keep the override set explicit and small.
  - Document which actions still need generation improvements.

- [x] `EXEC-PILOT-C4` Expose `get_admin_action_help` as a first-class executor function.

- [x] `EXEC-PILOT-C5` Add regression tests so new admin actions cannot silently drift from executor-visible schemas.

Acceptance:

- executor sees one function per action
- function contracts stay aligned with the real admin surface

---

## 5. Phase D — OpenAI Key / Secret Handling In `SY.admin`

- [x] `EXEC-PILOT-D1` Define how `SY.admin` loads the OpenAI API key for executor mode.
  - Use the same secret-management direction already used for AI-capable system nodes.
  - Do not hardcode or depend on ad hoc environment variables for normal system operation.
  - Implemented through standard node `CONFIG_GET/SET`, not a separate local HTTP secret path.

- [x] `EXEC-PILOT-D2` Add secret record loading for `SY.admin`.
  - Prefer node-local secret storage under the standard node secrets path.
  - Align with existing secret loading helpers if available.

- [x] `EXEC-PILOT-D3` Define executor-specific config shape for model selection.
  - Example:
    - provider
    - model
    - reasoning effort
    - optional timeout / token caps
  - Current shape also includes optional catalog mode/action allowlist.

- [ ] `EXEC-PILOT-D4` Ensure `SY.admin` fails closed when executor mode is requested without a valid OpenAI key.
  - Clear error
  - No partial execution

- [x] `EXEC-PILOT-D5` Ensure logs never print the raw API key or secret payloads.

Acceptance:

- `SY.admin` executor can run with a properly loaded OpenAI key
- missing or invalid secret fails clearly and safely

---

## 6. Phase E — `SY.architect` Host Integration

- [ ] `EXEC-PILOT-E1` Add plan ingestion flow to `SY.architect`.
  - Receive external `executor_plan`
  - validate schema
  - create execution session

- [ ] `EXEC-PILOT-E2` Send the execution request to `SY.admin`.
  - `SY.architect` should not own the executor function catalog.

- [ ] `EXEC-PILOT-E3` Render step-by-step execution events in chat/history.
  - Visible progression per step
  - clear stop point on failure

- [ ] `EXEC-PILOT-E4` Render final execution summary in the chat.
  - total steps
  - completed steps
  - failed step
  - key result/error

- [ ] `EXEC-PILOT-E5` Keep this mode separate from normal SCMD and normal Archi chat.

Acceptance:

- operator can submit a plan through Archi
- Archi shows execution progress and final outcome clearly

---

## 7. Phase F — Prompting

- [ ] `EXEC-PILOT-F1` Add a narrow executor prompt inside `SY.admin`.
  - Execute only declared steps
  - No renamed actions
  - No step reordering
  - No invented required values
  - Use help only as support

- [ ] `EXEC-PILOT-F2` Encode the infrastructure-obvious refinement rule in the executor prompt.
  - Only fill values explicitly allowed by `executor_fill`.

- [ ] `EXEC-PILOT-F3` Keep `SY.architect` prompt focused on host behavior.
  - Archi coordinates and renders
  - Admin executes

Acceptance:

- host and executor roles are sharply separated

---

## 8. Phase G — Errors And Observability

- [ ] `EXEC-PILOT-G1` Add rich structured execution errors from `SY.admin`.
  - action name
  - step id
  - code
  - detail
  - whether failure came from:
    - validation
    - help ambiguity
    - admin action failure
    - timeout/control-plane failure

- [ ] `EXEC-PILOT-G2` Add execution log storage for pilot sessions.
  - request
  - step events
  - final result

- [ ] `EXEC-PILOT-G3` Ensure logs and persisted records are redacted where needed.
  - no secrets
  - no sensitive payload dumps unless intentionally allowed

Acceptance:

- pilot failures are diagnosable after the fact
- executor mode improves error quality, not just execution

---

## 9. Phase H — MVP Scope Decision

- [x] `EXEC-PILOT-H1` Decide whether pilot v1 exposes:
  - all admin actions, or
  - a bounded subset sufficient for testing
  - Decision in code: full catalog by default, optional subset mode via executor local config.

- [ ] `EXEC-PILOT-H2` If using a subset first, define the exact initial list.
  - recommended first slice:
    - `list_admin_actions`
    - `get_admin_action_help`
    - `get_runtime`
    - `list_nodes`
    - `add_route`
    - `delete_route`
    - `add_vpn`
    - `delete_vpn`
    - `run_node`
    - `set_node_config`
    - `get_node_status`

Acceptance:

- the pilot scope is explicit and testable

---

## 10. Phase I — End-To-End Validation

- [ ] `EXEC-PILOT-I1` E2E: read-only execution plan succeeds.
  - example: `get_runtime`, `list_nodes`

- [ ] `EXEC-PILOT-I2` E2E: mutating plan succeeds.
  - example: `add_route`

- [ ] `EXEC-PILOT-I3` E2E: help lookup path is exercised.

- [ ] `EXEC-PILOT-I4` E2E: invalid plan fails before execution.

- [ ] `EXEC-PILOT-I5` E2E: `SY.admin` timeout/failure stops the execution visibly.

- [ ] `EXEC-PILOT-I6` E2E: missing OpenAI secret in `SY.admin` fails cleanly.

Acceptance:

- full host -> admin executor -> chat rendering loop works

---

## 11. Recommended Order

1. `EXEC-PILOT-D1` to `EXEC-PILOT-D4`
2. `EXEC-PILOT-C1` to `EXEC-PILOT-C4`
3. `EXEC-PILOT-A1` to `EXEC-PILOT-A4`
4. `EXEC-PILOT-B1` to `EXEC-PILOT-B5`
5. `EXEC-PILOT-E1` to `EXEC-PILOT-E5`
6. `EXEC-PILOT-F1` to `EXEC-PILOT-F3`
7. `EXEC-PILOT-G1` to `EXEC-PILOT-G3`
8. `EXEC-PILOT-H1` to `EXEC-PILOT-H2`
9. `EXEC-PILOT-I1` to `EXEC-PILOT-I6`

---

## 12. Bottom Line

This pilot is ready to implement when the team agrees on these two things:

- `SY.admin` is the home of the executor AI
- the executor-visible OpenAI function catalog is derived from the real admin action surface

The OpenAI key/secret for executor mode in `SY.admin` is a first-class implementation task, not a detail to defer.
