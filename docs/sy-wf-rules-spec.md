# Fluxbee — SY.wf-rules Specification

**Status:** v1.0 draft
**Date:** 2026-04-15
**Audience:** Platform developers, Archi designers, operators
**Reference implementation model:** `go/sy-opa-rules/main.go` (sy.opa-rules — sy.wf-rules follows the same API shape)
**Related specs:** `wf-v1.md`, `sy-timer.md`, runtime-packaging-cli-spec.md

---

## 1. Purpose

`SY.wf-rules` is a system node that manages workflow definitions for the `WF.*` node family, following the same operational model as `SY.opa-rules` manages OPA policies.

The operator (human, Archi, or any authorized node) sends workflow definition code (JSON with CEL guards) to `SY.wf-rules`. The node validates the definition, compiles CEL guards, stages it, and on apply promotes it to current and coordinates with `SY.orchestrator` to restart the corresponding WF node.

The operator never creates packages, never writes shell scripts, never touches the filesystem. The workflow definition is treated as **code that the system compiles and runs**, exactly like Rego policies in `SY.opa-rules`.

---

## 2. Design principle: same shape as OPA

`SY.wf-rules` deliberately mirrors `SY.opa-rules` in:

- **Message types:** system (NODE_STATUS_GET, CONFIG_GET, CONFIG_SET), command (compile_workflow, apply_workflow, rollback_workflow), query (get_workflow, get_status).
- **State management:** three directories per workflow — `current/`, `staged/`, `backup/`. Always-latest with staged → current → backup rotation.
- **Operations:** compile, compile_apply, apply, rollback — same semantics as OPA (with `check` accepted as a silent synonym for `compile`).
- **Response shapes:** same envelope structure, same error codes pattern, same status reporting.

The differences are structural, not stylistic:

| Aspect | SY.opa-rules | SY.wf-rules |
|---|---|---|
| Input code | Rego text | Workflow definition JSON |
| Compilation output | WASM binary | Validated definition + compiled CEL programs (persisted as the original JSON; CEL is re-compiled at WF node boot) |
| Distribution mechanism | SHM region read by routers | Filesystem + orchestrator restart of WF node |
| Scope | One policy per hive | Multiple workflows per hive (one per workflow_name) |
| Consumer | Router (reads SHM directly) | wf-generic node (reads definition from config at boot) |
| Post-apply notification | Broadcast OPA_RELOAD | Command to SY.orchestrator to restart the specific WF node |

---

## 3. Core concepts

### 3.1 Workflow naming

Each workflow managed by `SY.wf-rules` has a **workflow name** (e.g. `invoice`, `onboarding`, `escalation`). This name:

- Maps to the WF node name: workflow name `invoice` → node `WF.invoice@<hive>`.
- Is used as the directory key in state storage.
- Must match `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$` (lowercase, alphanumeric with dashes and dots).

### 3.2 Always-latest with frozen instances

`SY.wf-rules` maintains **one active definition per workflow** (always-latest). There is no version history, no explicit versioning by the user, no rollback to arbitrary past versions.

When a new definition is applied:

1. The current definition moves to backup.
2. The new definition becomes current.
3. `SY.orchestrator` restarts the WF node.
4. The WF node boots, loads the new definition as "current" for new instances.
5. Existing instances in SQLite continue using the definition they were born with (frozen by hash — this is handled by wf-generic internally, not by sy.wf-rules).

The user perceives always-latest. The system preserves running instances transparently. This is the decision documented in the conversation of 2026-04-15.

### 3.3 State directories

```
/var/lib/fluxbee/wf-rules/<workflow_name>/
├── current/
│   ├── definition.json      # The active workflow definition
│   └── metadata.json        # Version, hash, compiled_at, etc.
├── staged/
│   ├── definition.json
│   └── metadata.json
└── backup/
    ├── definition.json
    └── metadata.json
```

One directory tree per workflow_name. Created on first `compile` for that workflow.

---

## 4. Metadata

```json
{
  "version": 3,
  "hash": "sha256:abc123...",
  "workflow_name": "invoice",
  "workflow_type": "invoice",
  "wf_schema_version": "1",
  "compiled_at": "2026-04-15T14:30:00Z",
  "guard_count": 12,
  "state_count": 7,
  "action_count": 15,
  "error_detail": ""
}
```

The `version` is an auto-incrementing integer managed by `SY.wf-rules`, not by the user. Each successful compile bumps the version. The user can optionally pass a version in the request (for idempotency checks), but if omitted, the system increments automatically.

---

## 5. Protocol

### 5.1 Node identity

- L2 name: `SY.wf-rules@<hive>` (e.g. `SY.wf-rules@motherbee`)
- Node base name: `SY.wf-rules`
- Binary: `sy-wf-rules` (installed via install.sh, same pattern as sy-opa-rules)

### 5.2 Message handling (dispatch)

Mirrors sy.opa-rules exactly:

```
system:
  NODE_STATUS_GET    → handleNodeStatusGet
  CONFIG_GET         → handleNodeConfigGet
  CONFIG_SET         → handleNodeConfigSet  (routes to handleWfAction by operation)
  CONFIG_CHANGED     → handleWfAction       (from admin broadcast)

command:
  compile_workflow   → handleWfAction("compile", ...)
  apply_workflow     → handleWfAction("apply", ...)
  rollback_workflow  → handleWfAction("rollback", ...)
  delete_workflow    → handleDeleteWorkflow

query:
  get_workflow       → return current definition JSON + metadata
  get_status         → return current/staged versions, workflow list, WF node status
  list_workflows     → return all managed workflow names with status
```

### 5.3 Operations (via CONFIG_SET or command)

**`compile`** — Validate the workflow definition JSON, compile all CEL guards, write to staged. Do NOT apply. Returns validation result (pass/fail with details).

**`compile_apply`** — Validate + immediately promote to current + notify orchestrator to restart the WF node.

**`apply`** — Promote staged to current. Rotate current → backup. Notify orchestrator.

**`rollback`** — Restore backup to current. Notify orchestrator.

Note: if a request arrives with `operation: "check"`, it is treated as a synonym for `compile` silently. The documentation uses `compile` as the canonical name.

### 5.4 CONFIG_SET payload

```json
{
  "operation": "compile_apply",
  "workflow_name": "invoice",
  "definition": {
    "wf_schema_version": "1",
    "workflow_type": "invoice",
    "description": "Issues an invoice...",
    "input_schema": { ... },
    "initial_state": "validating_data",
    "terminal_states": ["completed", "failed", "cancelled"],
    "states": [ ... ]
  },
  "auto_spawn": true,
  "version": 0
}
```

| Field | Type | Required | Description |
|---|---|---|---|
| `operation` | string | No (default: "compile") | One of: compile, compile_apply, apply, rollback |
| `workflow_name` | string | **Yes, always** | Name of the workflow. Determines node name `WF.<workflow_name>@<hive>`. Required for all operations including apply and rollback. |
| `definition` | object | Yes for compile/compile_apply | The full workflow definition JSON per wf-v1.md §8. |
| `auto_spawn` | bool | No (default: false) | If true and the WF node doesn't exist yet, ask orchestrator to spawn it after apply. |
| `version` | uint64 | No | If provided, used for idempotency check. If 0, auto-increment. |

### 5.5 CONFIG_SET response

**Success (compile):**
```json
{
  "ok": true,
  "node_name": "SY.wf-rules@motherbee",
  "state": "configured",
  "schema_version": 1,
  "config_version": 4,
  "effective_config": {
    "staged": {
      "version": 4,
      "hash": "sha256:abc...",
      "workflow_name": "invoice",
      "compiled_at": "2026-04-15T14:30:00Z",
      "guard_count": 12,
      "state_count": 7,
      "action_count": 15
    }
  }
}
```

**Success (compile_apply/apply):**
```json
{
  "ok": true,
  "node_name": "SY.wf-rules@motherbee",
  "state": "configured",
  "config_version": 4,
  "effective_config": {
    "current": {
      "version": 4,
      "hash": "sha256:abc...",
      "workflow_name": "invoice",
      "compiled_at": "2026-04-15T14:30:00Z",
      "guard_count": 12,
      "state_count": 7,
      "action_count": 15
    }
  },
  "wf_node": {
    "node_name": "WF.invoice@motherbee",
    "action": "restarted",
    "status": "ok"
  }
}
```

**Partial success (apply rotated but node restart failed):**
```json
{
  "ok": true,
  "node_name": "SY.wf-rules@motherbee",
  "state": "configured",
  "config_version": 4,
  "effective_config": {
    "current": {
      "version": 4,
      "hash": "sha256:abc...",
      "workflow_name": "invoice",
      "compiled_at": "2026-04-15T14:30:00Z"
    }
  },
  "wf_node": {
    "node_name": "WF.invoice@motherbee",
    "action": "restart_failed",
    "error": "run_node failed after kill_node: <detail from orchestrator>"
  },
  "warning": "Definition applied to current/. WF node failed to restart. Retry apply or rollback."
}
```

Note that `ok: true` in this case reflects that sy.wf-rules successfully persisted the new definition. The node-level failure is reported in `wf_node.action`, not in the top-level error. This distinguishes "sy.wf-rules did its job but the downstream restart failed" from "sy.wf-rules itself failed". The operator typically reacts to `wf_node.action != "restarted"` with a retry or rollback.

**Error:**
```json
{
  "ok": false,
  "node_name": "SY.wf-rules@motherbee",
  "state": "error",
  "error": {
    "code": "COMPILE_ERROR",
    "detail": "Guard compile failed at states[1].transitions[0].guard: undeclared reference to 'event.payload.complette' (did you mean 'event.payload.complete'?)"
  }
}
```

### 5.6 Command messages

Same shape as sy.opa-rules commands. The action name changes:

**`compile_workflow`:**
```json
{
  "workflow_name": "invoice",
  "definition": { ... },
  "version": 0
}
```

Response:
```json
{
  "status": "ok",
  "version": 4,
  "staged": true,
  "guard_count": 12,
  "state_count": 7
}
```

**`apply_workflow`:**
```json
{
  "workflow_name": "invoice",
  "version": 4
}
```

**`rollback_workflow`:**
```json
{
  "workflow_name": "invoice"
}
```

**`delete_workflow`:**
```json
{
  "workflow_name": "invoice",
  "force": false
}
```

If `force: false` (default): refuses if the WF node has running instances. Returns `INSTANCES_ACTIVE` error with count.

If `force: true`: kills the WF node (via orchestrator), deletes all state directories for that workflow. Instances are lost. This is the "nuclear option" for dev/testing.

### 5.7 Query messages

**`get_workflow`:**

Request payload:
```json
{
  "workflow_name": "invoice"
}
```

Response:
```json
{
  "status": "ok",
  "hive": "motherbee",
  "workflow_name": "invoice",
  "version": 4,
  "hash": "sha256:abc...",
  "compiled_at": "2026-04-15T14:30:00Z",
  "definition": { ... full JSON ... }
}
```

**`get_status`:**

Request payload:
```json
{
  "workflow_name": "invoice"
}
```

Response:
```json
{
  "status": "ok",
  "hive": "motherbee",
  "workflow_name": "invoice",
  "current_version": 4,
  "current_hash": "sha256:abc...",
  "staged_version": 5,
  "last_error": "",
  "wf_node": {
    "node_name": "WF.invoice@motherbee",
    "running": true,
    "active_instances": 12
  }
}
```

**How `active_instances` is obtained:** sy.wf-rules sends a `WF_LIST_INSTANCES` query (wf-v1.md §16) to `WF.<workflow_name>@<hive>` with `status_filter: "running"` and `limit: 0`. The response includes `count`. Timeout: 2 seconds. If the WF node does not respond within the timeout (node down, overloaded, or not spawned), sy.wf-rules returns `"running": false, "active_instances": null, "wf_node_timeout": true`. This does not fail the get_status query — the workflow metadata from sy.wf-rules' own state directories is always returned regardless of node availability.

For `delete_workflow` with `force: false`, the same mechanism is used to check for active instances. If the node does not respond within timeout, the delete is refused with error `INSTANCES_UNKNOWN` — sy.wf-rules does not assume zero instances when it cannot confirm.
```

**`list_workflows`:**

Request payload: `{}` (empty)

Response:
```json
{
  "status": "ok",
  "hive": "motherbee",
  "workflows": [
    {
      "workflow_name": "invoice",
      "current_version": 4,
      "current_hash": "sha256:abc...",
      "wf_node_running": true,
      "active_instances": 12
    },
    {
      "workflow_name": "onboarding",
      "current_version": 1,
      "current_hash": "sha256:def...",
      "wf_node_running": true,
      "active_instances": 3
    }
  ]
}
```

---

## 6. Validation (the "compile" step)

When `SY.wf-rules` receives a workflow definition for compile or compile_apply, it runs the same 11 checks from wf-v1.md §8.5:

1. JSON structure valid per `wf_schema_version`.
2. `input_schema` is valid JSON Schema.
3. `initial_state` exists in `states`.
4. All `terminal_states` exist in `states`.
5. Every `target_state` referenced in transitions exists in `states`.
6. Every CEL guard expression compiles against the typed environment (`input`, `state`, `event`, `now()`).
7. Every action has a known type (from the WF action registry).
8. Every `send_message` target is a syntactically valid L2 name.
9. Every `schedule_timer` duration ≥ 60 seconds.
10. Every `set_variable` name is a valid identifier.
11. Every `$ref` path uses a valid root (`input`, `state`, or `event`).

For item 6 (CEL compilation), `SY.wf-rules` includes `cel-go` as a dependency and compiles guards in-process. The compiled programs are NOT persisted — they serve only to validate. The WF node (`wf-generic`) recompiles CEL from the JSON at boot. This avoids cross-binary compatibility issues.

If any check fails, the definition is NOT staged. The response includes the error code `COMPILE_ERROR` and a detail string with the path to the failing element (same pattern as wf-generic's load-time validation).

---

## 7. Apply and orchestrator coordination

When a definition is applied (compile_apply or apply operations):

1. Rotate sy.wf-rules state directories: `current/ → backup/`, `staged/ → current/`.
2. Determine the target WF node name: `WF.<workflow_name>@<hive>`.
3. Read the current `config.json` of the WF node (if it exists, via orchestrator query).
4. Build the new `config.json` preserving operational fields (`sy_timer_l2_name`, `gc_retention_days`, `gc_interval_seconds`) and replacing the `workflow_definition` field with the new definition.
5. Send the config + node operations to `SY.orchestrator@<hive>`:
   - **If the WF node is already running:**
     a. `set_node_config` (or equivalent) with the new `config.json` contents.
     b. `kill_node` for `WF.<workflow_name>@<hive>`.
     c. `run_node` for `WF.<workflow_name>@<hive>` with runtime `wf.engine`.
   - **If the WF node does not exist and `auto_spawn: true`:**
     a. `run_node` for `WF.<workflow_name>@<hive>` with runtime `wf.engine` and the new config inline (the orchestrator creates the managed directory and the config.json as part of spawn).
   - **If the WF node does not exist and `auto_spawn: false`:**
     a. No orchestrator calls. Definition is stored only in sy.wf-rules `current/`.
     b. Response includes `"wf_node": { "action": "none", "reason": "auto_spawn disabled" }`.

**Important properties of this design:**

- sy.wf-rules **never writes to the filesystem under `/var/lib/fluxbee/nodes/`**. The orchestrator owns that directory tree and is the only system node that writes there. sy.wf-rules operates exclusively via L2 messages to the orchestrator.
- The `workflow_definition` lives as a field inside `config.json`, not as a separate file. This is the contract between sy.wf-rules, the orchestrator, and wf-generic. The wf-generic binary reads its `config.json` at boot and finds the definition as a top-level field.
- On updates to an existing workflow, operational fields (gc, timer, etc.) are preserved from the existing `config.json`. Only `workflow_definition` is replaced. If the operator had customized operational settings via direct CONFIG_SET to the WF node, those customizations persist through workflow definition updates.

### 7.1 config.json format for WF nodes

The `config.json` that the orchestrator writes for a WF node follows this shape:

```json
{
  "sy_timer_l2_name": "SY.timer@motherbee",
  "gc_retention_days": 7,
  "gc_interval_seconds": 3600,
  "workflow_definition": {
    "wf_schema_version": "1",
    "workflow_type": "invoice",
    "description": "Issues an invoice...",
    "input_schema": { ... },
    "initial_state": "validating_data",
    "terminal_states": ["completed", "failed", "cancelled"],
    "states": [ ... ]
  }
}
```

The `workflow_definition` is the complete JSON as defined in wf-v1.md §8. The wf-generic binary, at boot, reads `config.json` and uses this field directly — no separate file, no path indirection.

### 7.2 Kill + run semantics (no atomic restart)

`SY.orchestrator` in its current state does not expose an atomic `restart_node` verb. The restart is implemented by sy.wf-rules as a two-step sequence: `kill_node` followed by `run_node`. Each step is a separate L2 request with its own response.

**Failure handling:**

- If `kill_node` fails (node was already dead, or orchestrator error): log warning, proceed to `run_node` anyway. This tolerates the case where the node died on its own between sy.wf-rules' last observation and the kill attempt.
- If `kill_node` succeeds but `run_node` fails: sy.wf-rules retries `run_node` once after a 1-second delay. If the retry also fails, sy.wf-rules gives up and returns the apply result with `"wf_node": { "action": "restart_failed", "error": "<detail>" }`. The node remains dead until manual intervention.
- If both `kill_node` and `run_node` succeed: `"wf_node": { "action": "restarted", "status": "ok" }`.

**State consistency on restart failure:**

Even if the node restart fails, sy.wf-rules internal state is consistent:
- The new definition is in `current/`.
- The previous definition is in `backup/`.
- A subsequent `rollback` operation works normally (swaps current ↔ backup and attempts another restart).
- A subsequent `apply` retry attempts the restart again with the new definition already in current.

The operator sees a clear failure in the response and can decide next steps: investigate why the node won't spawn, rollback to the previous definition, or intervene manually.

**Future improvement:** if sy.orchestrator later gains an atomic `restart_node` verb, sy.wf-rules can adopt it and eliminate the two-step race. This is a backward-compatible improvement; the current two-step approach continues to work until replaced.

### 7.1 Dependency: wf.engine runtime must be installed

`SY.wf-rules` does NOT install the `wf.engine` runtime (the `wf-generic` binary). That binary must already be available in the dist as a pre-installed runtime (installed via `install.sh` or `publish-wf-runtime.sh`). `SY.wf-rules` only manages the workflow definitions that parameterize that runtime.

If `SY.orchestrator` reports that `wf.engine` is not available when asked to spawn, `SY.wf-rules` returns error `RUNTIME_NOT_AVAILABLE`.

---

## 8. Delete workflow

The `delete_workflow` command removes a workflow entirely:

1. If `force: false`: query `SY.orchestrator` for the node status. If the WF node has active instances (`status = running` with `active_instances > 0`), refuse with `INSTANCES_ACTIVE` error.
2. If `force: true` or no active instances:
   - Send `NODE_KILL` to `SY.orchestrator` for `WF.<workflow_name>@<hive>`.
   - Remove the state directories for that workflow: `/var/lib/fluxbee/wf-rules/<workflow_name>/`.
   - Remove the managed node directory: `/var/lib/fluxbee/nodes/WF/WF.<workflow_name>@<hive>/`.
   - Respond with `"status": "ok", "deleted": true`.

This is a destructive operation. Active instances are lost if `force: true`. There is no undo.

---

## 9. OPA rules for WF authorization

`SY.wf-rules` does NOT manage OPA rules for WF authorization. Those are managed separately via `SY.opa-rules` as part of the hive's routing policy.

When a new workflow is deployed, the operator must separately ensure that appropriate OPA rules exist to authorize messages to `WF.<workflow_name>@<hive>`. `SY.wf-rules` can include a suggested OPA rule snippet in the `compile` response as a convenience hint, but does not apply it.

Suggested snippet in compile response (informational, not applied):
```json
{
  "suggested_opa_rule": "allow {\n    input.routing.dst == \"WF.invoice@motherbee\"\n    input.meta.msg == \"INVOICE_REQUEST\"\n    startswith(input.routing.src_l2_name, \"AI.billing\")\n}"
}
```

---

## 10. Admin surface

`SY.admin` exposes the following endpoints that proxy to `SY.wf-rules` via L2 unicast:

| Admin action | Maps to | Purpose |
|---|---|---|
| `POST /admin/wf-rules/compile` | CONFIG_SET with operation=compile | Validate + stage |
| `POST /admin/wf-rules/compile-apply` | CONFIG_SET with operation=compile_apply | Validate + stage + apply + restart |
| `POST /admin/wf-rules/apply` | CONFIG_SET with operation=apply | Promote staged |
| `POST /admin/wf-rules/rollback` | CONFIG_SET with operation=rollback | Restore backup |
| `POST /admin/wf-rules/delete` | command delete_workflow | Remove workflow |
| `GET /admin/wf-rules/{name}` | query get_workflow | Get current definition |
| `GET /admin/wf-rules/{name}/status` | query get_status | Get status + WF node info |
| `GET /admin/wf-rules` | query list_workflows | List all workflows |

All endpoints use the standard admin authentication. The body for POST endpoints matches the CONFIG_SET payload (§5.4). `workflow_name` is always required — in URL-based endpoints it can be taken from the path parameter `{name}`.

---

## 11. Implementation notes

### 11.1 Language and dependencies

- **Language:** Go
- **CEL:** `github.com/google/cel-go` (same dependency as wf-generic)
- **JSON Schema validation:** `github.com/santhosh-tekuri/jsonschema` or equivalent
- **SDK:** `fluxbee-go-sdk` for router connection
- **Filesystem:** direct file I/O for state directories (no SQLite — definitions are small JSON files, same pattern as sy.opa-rules)

### 11.2 Binary and installation

- Binary name: `sy-wf-rules`
- Installed by `scripts/install.sh` alongside other SY binaries
- State directory: `/var/lib/fluxbee/wf-rules/`
- Lock path: `/var/run/fluxbee/sy-wf-rules.lock`
- Node name pattern: `SY.wf-rules@<hive>`

### 11.3 Boot sequence

1. Load hive.yaml for hive_id.
2. Ensure state directories exist.
3. Acquire file lock (single instance per host).
4. Load/create node UUID.
5. Connect to router via SDK.
6. Enter message receive loop.

No SHM region (unlike sy.opa-rules). WF definitions are not read directly by the router; they are consumed by wf-generic nodes via filesystem.

### 11.4 CEL validation helper

The CEL compilation for validation can be extracted into a shared Go package used by both `sy-wf-rules` and `wf-generic`. This avoids duplicating the CEL environment setup logic:

```
go/pkg/wfcel/
├── environment.go   # Build cel.Env with input/state/event/now()
├── compile.go       # Compile a guard string, return error or program
└── validate.go      # Validate all guards in a WorkflowDefinition
```

Both `sy-wf-rules` and `wf-generic` import this package.

---

## 12. Error codes

| Code | Description |
|---|---|
| `COMPILE_ERROR` | Definition validation or CEL guard compilation failed. Detail includes path to failing element. |
| `NOTHING_STAGED` | Apply requested but no staged definition exists. |
| `NO_BACKUP` | Rollback requested but no backup exists. |
| `VERSION_MISMATCH` | Requested version does not match staged version. |
| `WORKFLOW_NOT_FOUND` | Query or operation on a workflow_name that doesn't exist. |
| `INSTANCES_ACTIVE` | Delete refused because WF node has active instances (and force=false). |
| `INSTANCES_UNKNOWN` | Delete refused because WF node did not respond to instance count query within timeout (and force=false). Cannot confirm zero instances. |
| `RUNTIME_NOT_AVAILABLE` | wf.engine runtime not installed in dist; cannot spawn. |
| `ORCHESTRATOR_ERROR` | Communication with SY.orchestrator failed. |
| `RESTART_FAILED` | Node kill succeeded but subsequent run_node failed twice. Node is dead. Operator intervention required. |
| `KILL_FAILED` | kill_node failed. Unusual — typically means the orchestrator is itself in error. |
| `INVALID_WORKFLOW_NAME` | workflow_name does not match naming regex. |
| `INVALID_CONFIG_SET` | CONFIG_SET payload malformed. |
| `UNSUPPORTED_OPERATION` | Unknown operation string. |

---

## 13. Decisions

| Decision | Rationale |
|---|---|
| Same API shape as sy.opa-rules | Consistency for operators and Archi; reduces learning curve |
| Always-latest, no explicit versioning | Simple model; frozen-by-hash in wf-generic handles instances transparently |
| CEL compiled in sy.wf-rules but NOT persisted | Avoids cross-binary compatibility; wf-generic recompiles at boot |
| Restart WF node on apply (no hot-reload) | Consistent with WF v1 decision; atomic and simple; recovery handles instances |
| kill_node + run_node instead of atomic restart | sy.orchestrator does not expose restart verb; two-step is acceptable; future improvement if orchestrator gains atomic restart |
| sy.wf-rules never writes filesystem under /var/lib/fluxbee/nodes | Orchestrator owns that directory tree; clean ownership boundary |
| workflow_definition embedded in config.json of WF node | Single source of truth for wf-generic at boot; no separate file to coordinate |
| delete_workflow with force flag | Explicit destructive operation; not hidden in normal flow |
| No SHM region | WF definitions consumed via filesystem, not real-time memory-mapped |
| OPA rules managed separately | Separation of concerns; sy.opa-rules owns routing policy |
| auto_spawn default false | Operator explicitly chooses when to start a node; avoids accidental spawns |
| Shared CEL validation package | Single source of truth for guard compilation logic |
| Multiple workflows per sy.wf-rules instance | One sys node per hive manages all workflows; simpler than one sy per workflow |

---

## 14. What is NOT in v1

- Hot-reload of workflow definitions (restart only).
- Version history beyond current + backup.
- Diff between versions.
- Automated OPA rule generation for new workflows.
- Multi-hive distribution from sy.wf-rules (each hive has its own sy.wf-rules managing its own workflows).
- Web UI for workflow editing.
- Visual statechart rendering.
- Workflow testing/simulation within sy.wf-rules.

---

## 15. References

| Topic | Document |
|---|---|
| WF node spec | `wf-v1.md` |
| WF implementation tasks | `wf_v1_tasks.md` |
| OPA rules (reference implementation model) | `go/sy-opa-rules/main.go` |
| Identity (for OPA authorization context) | `10-identity-v2.md` |
| Architecture | `01-arquitectura.md` |
| Runtime packaging (for wf.engine base runtime) | `runtime-packaging-cli-spec.md` |
