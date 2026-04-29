# Archi Implementation Improvement Plan

**Date:** 2026-04-25  
**Related docs:** `rearchitecture_implementation_tasks.md`, `fluxbee_rearchitecture_master_spec_v4.md`, `admin_help_reference.md`, `handbook_fluxbee.md`  
**Scope:** improvements after the first Archi manager-only implementation.

---

## Current Assessment

The current implementation is structurally correct for the target architecture:

- Archi behaves as a coordinator, not as a direct executor.
- Direct mutation tools were removed from the Archi provider.
- Clear mutations are routed through `fluxbee_plan_compiler`.
- Complex solution work is routed through `fluxbee_start_pipeline`.
- Pipeline execution has structured stages: design, audit, reconcile, optional artifact loop, plan compile, confirmation, execute, verify.
- A task-level agent token budget exists and is reported in pipeline completion.

The main remaining risk is not whether Archi can call tools. The risk is whether the plan compiler has enough deterministic context to generate valid executor plans every time. That depends heavily on admin help quality, plan validation, and E2E coverage.

---

## Priority 1 — Prove End-to-End Execution

### AIP-1 — Add E2E smoke cases for real Archi tasks

**Problem:** Unit tests validate pieces, but not the full operator path.

**Proposal:** Add a small E2E test matrix that exercises the complete user intent flow:

| Case | Expected path | Success condition |
| --- | --- | --- |
| Read-only status question | `fluxbee_system_get` | Archi reads state and does not call mutation tools |
| Simple mutation: restart existing node | `fluxbee_plan_compiler` | plan is generated, confirmation is required, execution result is reported |
| Runtime publish + run node | pipeline or plan compiler | package publish step plus `run_node` step are valid and ordered |
| Failure: invalid runtime/package | artifact or execution failure | failure class is reported clearly, no silent success |
| Failure: missing required field | plan validation | plan compiler stops with actionable missing field message |

**Acceptance:**

- E2E logs include tool calls, plan JSON, confirmation state, executor result, and final operator message.
- At least one test intentionally fails and confirms that the operator sees the failure cause.

### AIP-2 — Add a dry-run plan validator before operator confirmation

**Problem:** The operator may receive a plan that looks valid but fails on execution because an admin action contract was misread.

**Proposal:** Before showing Confirm2, run a deterministic validator over the executor plan:

- Every `action` exists in admin action catalog.
- Every path param required by the action is present.
- Every required body field is present.
- No path contains unresolved placeholders like `{hive}` or `{node_name}`.
- Every step has a declared risk class.
- Plan step sequence matches compiler_class translation where the plan came from a delta report.

**Acceptance:**

- Invalid plans block before confirmation.
- The operator sees a concise validation error, not a generic executor failure.

---

## Priority 2 — Improve Admin Help For LLM Planning

The current `admin_help_reference.md` is useful for humans and already includes paths, fields, examples, and critical notes. For LLM planning, it needs a more structured, machine-readable layer. The plan compiler should not infer semantics from prose when the contract can provide them explicitly.

### AIP-3 — Add normalized action contract fields to `get_admin_action_help`

Each admin help response should expose a stable object like:

```json
{
  "action": "run_node",
  "category": "nodes",
  "intent": "create_managed_node_instance",
  "read_only": false,
  "requires_confirm": false,
  "risk_class": "non_destructive",
  "idempotency": {
    "mode": "create_if_missing",
    "conflict_behavior": "error_if_instance_exists"
  },
  "preconditions": [
    "target hive exists",
    "runtime exists or can be derived from node_name",
    "runtime version is materialized or set to current"
  ],
  "postconditions": [
    "managed node instance directory exists",
    "systemd unit is created or reused",
    "node process is started"
  ],
  "path": {
    "method": "POST",
    "template": "/hives/{hive}/nodes",
    "params": {
      "hive": { "type": "string", "required": true }
    }
  },
  "body_schema": {
    "required": {
      "node_name": "string"
    },
    "optional": {
      "runtime": "string",
      "runtime_version": "string",
      "tenant_id": "string",
      "unit": "string",
      "config": "object"
    }
  },
  "planner_hints": [
    "Use run_node only when the instance does not exist.",
    "Use start_node when the instance exists but is stopped.",
    "Pass tenant_id on first spawn for AI multi-tenant nodes."
  ],
  "common_mistakes": [
    "Do not use node_control_config_set to create a node.",
    "Do not omit tenant_id for first spawn of tenant-scoped AI nodes."
  ],
  "success_response": {
    "status_field": "status",
    "success_values": ["ok"]
  },
  "failure_signatures": [
    {
      "match": "runtime not materialized",
      "failure_class": "EXECUTION_ENVIRONMENT_MISSING"
    }
  ],
  "examples": [
    {
      "title": "spawn AI support node",
      "request": {
        "method": "POST",
        "path": "/hives/motherbee/nodes",
        "body": {
          "node_name": "AI.support@motherbee",
          "runtime": "ai.common",
          "runtime_version": "current",
          "tenant_id": "tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
        }
      }
    }
  ]
}
```

**Why:** This lets the plan compiler generate plans from structured facts instead of narrative notes.

### AIP-4 — Add `planner_hints` to critical actions

Start with the actions most likely to produce invalid plans:

- `run_node`
- `start_node`
- `restart_node`
- `kill_node`
- `publish_runtime_package`
- `remove_runtime_version`
- `node_control_config_get`
- `node_control_config_set`
- `wf_rules_compile_apply`
- `opa_compile_apply`
- `update`
- `sync_hint`

Required hints:

- when to use the action
- when not to use the action
- required pre-read actions
- whether the target resource must already exist
- whether the action creates, updates, restarts, deletes, or only validates
- whether failure should block the whole plan

### AIP-5 — Add response contract to admin help

**Problem:** Plan execution currently can report success/failure, but agents need a predictable way to interpret action outputs.

**Proposal:** Each help entry should declare:

- `success_status_path`, e.g. `payload.status` or `status`
- accepted success values, e.g. `["ok", "success"]`
- error fields, e.g. `error_code`, `error_detail`, `payload.error`
- material output fields, e.g. `manifest_hash`, `runtime_version`, `node_name`
- whether the result should be surfaced to operator immediately

**Acceptance:**

- Executor verification can be stricter.
- Operator messages can say exactly which step succeeded or failed.

### AIP-6 — Add destructive/restart metadata directly to admin help

The compiler_class table already has risk classes, but simple plan compiler tasks may not originate from a delta report. Admin help should expose a per-action risk class:

| Action | Suggested risk |
| --- | --- |
| `restart_node` | `restarting` |
| `kill_node` | `destructive` when `purge_instance=true`, otherwise `restarting/destructive` depending policy |
| `remove_node_instance` | `destructive` |
| `remove_runtime_version` | `destructive` |
| `delete_route` | `destructive` |
| `delete_vpn` | `destructive` |
| `wf_rules_delete` | `destructive` |
| `opa_rollback` | `restarting_or_policy_change` |
| `publish_runtime_package` | `non_destructive` unless `set_current=true`, then `promotion` |
| `node_control_config_set` | `non_destructive` or `restarting` depending node response |

**Acceptance:**

- Confirm2 summary highlights destructive/restarting steps even for direct one-step plans.

---

## Priority 3 — Strengthen Plan Compiler Behavior

### AIP-7 — Enforce help lookup per action

**Problem:** The prompt says the plan compiler should use help, but prompts are not guarantees.

**Proposal:** Add a deterministic check in the plan compiler runner:

- Track all `get_admin_action_help(action)` calls made during compilation.
- Extract all actions in the produced plan.
- Reject the plan if any action was not looked up.

**Acceptance:**

- The compiler cannot generate a plan using guessed action contracts.

### AIP-8 — Add action selection disambiguation rules

Some action groups are semantically close:

| Intent | Correct action |
| --- | --- |
| create node instance | `run_node` |
| start stopped existing node | `start_node` |
| restart running node | `restart_node` |
| read persisted config | `get_node_config` |
| read live config contract | `node_control_config_get` |
| live config update | `node_control_config_set` |
| publish runtime only | `publish_runtime_package` |
| publish and redeploy runtime | `publish_runtime_package` + `sync_hint`/`update` or runtime-specific redeploy plan |

These rules should be in admin help and in the plan compiler system prompt.

### AIP-9 — Add plan repair loop with one retry

**Problem:** If the first plan fails validation due to missing fields, the compiler should have one controlled chance to repair itself using the validation error.

**Proposal:**

- Keep the current max attempts small.
- Retry only after deterministic validation failure.
- Include exact validation errors as structured repair input.
- If retry fails, block and report.

**Acceptance:**

- Missing field errors can be fixed automatically.
- Infinite or conversational loops remain impossible.

---

## Priority 4 — Improve Operator Reporting

### AIP-10 — Show immediate result for simple operations

For one-step plans, the operator should see:

- action name
- target hive/resource
- whether it ran
- key output fields
- error code/detail if failed

Example:

```text
Executed restart_node on IO.api.support@motherbee.
Result: ok.
The node was restarted by systemd unit fluxbee-node-IO.api.support-motherbee.
```

**Acceptance:**

- No silent success/failure for simple ops.
- Chat output includes executor result summary, not only final pipeline status.

### AIP-11 — Add token usage to all task reports

Current token budget is useful, but the operator should see it consistently:

- design tokens
- artifact tokens
- plan compile tokens
- total used
- budget
- budget exceeded reason if blocked

**Acceptance:**

- Token cost is visible per task, not only in logs.

---

## Priority 5 — Runtime Package Programming Reliability

### AIP-12 — Add platform validation before publish

**Problem:** A package can be uploaded and only fail when run if the executable is not Linux-compatible.

**Proposal:** During artifact audit and publish flow, detect:

- ELF vs Mach-O vs PE
- architecture: x86_64/aarch64
- dynamic linker path
- executable bit
- required shared libraries if discoverable

**Acceptance:**

- Non-Linux executables are rejected before publish.
- Operator sees a clear error: `ARTIFACT_PLATFORM_INVALID`.

### AIP-13 — Add runtime promotion semantics to plans

**Problem:** Publishing, promoting to current, updating hive dist, and recreating/restarting nodes are distinct operations.

**Proposal:** Represent them explicitly in generated plans:

1. `publish_runtime_package`
2. optional `set_current`
3. optional `sync_hint(channel=dist)`
4. optional `update(category=runtime)`
5. optional node restart/recreate

**Acceptance:**

- The operator can see whether a publish is only staged in dist or actually promoted/redeployed.

---

## Priority 6 — Recovery And Safety

### AIP-14 — Add interrupted-run resume coverage

The code has pipeline interruption state. Add E2E tests for:

- interrupted during artifact loop
- interrupted after plan compile before Confirm2
- interrupted during execute

**Acceptance:**

- Resume/discard works and preserves state.

### AIP-15 — Add execution kill switch

**Problem:** If executor behavior is wrong, there should be a fast operational stop.

**Proposal:** Add config flag:

```json
{
  "archi": {
    "execution_enabled": true
  }
}
```

When false:

- plan compile still works
- Confirm2 refuses execution
- operator receives clear disabled message

**Acceptance:**

- Safe production rollout can run Archi in plan-only mode.

---

## Priority 7 — Documentation Updates

### AIP-16 — Split handbook vs planner contracts

The handbook should remain narrative and operator-facing. Admin help should be the planner contract.

Recommended doc split:

- `handbook_fluxbee.md`: concepts, operator workflows, examples.
- `admin_help_reference.md`: generated action reference.
- new generated/validated JSON contract: `admin_action_contracts_v1.json`.
- plan compiler prompt: short, references the tool contract, not long embedded operational prose.

### AIP-17 — Add canonical examples for high-risk flows

Add examples to `admin_help_reference.md` and to the runtime plan cookbook for:

- publish runtime package without promotion
- publish runtime package with promotion to current
- publish runtime package + recreate matching node
- create AI node with tenant_id
- live config set with CONFIG_GET first
- WF deploy with `auto_spawn=true`
- OPA compile/apply
- remove runtime version when it is not current
- blocked remove runtime version when it is current

---

## Recommended Next Implementation Order

1. Add deterministic plan validation before confirmation.
2. Extend `get_admin_action_help` response shape with planner-focused fields.
3. Enforce that plan compiler looked up help for every generated action.
4. Add E2E smoke tests for simple mutation and runtime publish/run.
5. Improve operator execution reporting for one-step plans.
6. Add platform validation for runtime package executables.
7. Add config-driven execution kill switch.

This order minimizes risk: first prevent bad plans, then improve context, then prove full execution.

