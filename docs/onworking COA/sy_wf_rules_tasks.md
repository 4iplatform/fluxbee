# SY.wf-rules — Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-04-15
**Primary spec:** `docs/sy-wf-rules-spec.md`
**Target module:** `go/sy-wf-rules/`
**Dependencies:** `go/fluxbee-go-sdk`, `go/pkg/wfcel/` (shared CEL package, built here), `github.com/google/cel-go`, `github.com/santhosh-tekuri/jsonschema`
**Reference model:** `go/sy-opa-rules/main.go`

---

## 1) Goal

Implement `SY.wf-rules`, the system node that manages workflow definitions for the `WF.*` node family. The node:

- Receives workflow definition JSON via CONFIG_SET or command messages
- Validates the definition (structural + CEL guard compilation)
- Manages staged / current / backup state per workflow
- Coordinates with `SY.orchestrator` (kill_node + run_node) to apply definitions to live WF nodes
- Exposes query operations for definition inspection and status
- Never touches `/var/lib/fluxbee/nodes/` directly — all WF node filesystem ops go through the orchestrator

One binary, one instance per hive, managing all workflows for that hive.

---

## 2) Frozen decisions

- **Language:** Go. Module `github.com/4iplatform/json-router/sy-wf-rules`.
- **CEL validation:** in-process via `github.com/google/cel-go`. Compiled programs NOT persisted.
- **wf-generic reads:** `workflow_definition` field embedded in `config.json`. No separate file.
- **Orchestrator protocol:** `kill_node` + `run_node` in sequence (no atomic restart verb today).
- **Kill failure tolerance:** if `kill_node` fails, proceed to `run_node` anyway (node may already be dead).
- **run_node failure:** retry once after 1s. If still fails, return `restart_failed` partial-success response.
- **active_instances source:** `WF_LIST_INSTANCES` query to WF node with 2s timeout. On timeout: `running: false, active_instances: null, wf_node_timeout: true`. Never assume zero on no response.
- **delete with force=false + unreachable node:** return `INSTANCES_UNKNOWN`, refuse delete.
- **workflow_name:** always required for all operations including apply and rollback.
- **`check` operation:** silent synonym for `compile`. Canonical name is `compile`.
- **Shared CEL package:** `go/pkg/wfcel/` built first, used by both sy-wf-rules and wf-generic.
- **State storage:** filesystem only (no SQLite). Small JSON files, same pattern as sy.opa-rules.
- **No SHM region.** WF definitions are not memory-mapped; they flow through config.json at WF boot.
- **auto_spawn default:** `false`. Operator explicitly spawns.

---

## 3) Critical dependency: shared CEL package

### WFRULES-DEP-1 — go/pkg/wfcel/ must be built before WFRULES-VAL-1

The CEL environment setup and guard validation logic will be shared between `sy-wf-rules` and `wf-generic`. This avoids drift between the validation sy.wf-rules performs at compile time and the validation wf-generic performs at boot.

**Implementation order:**

1. Build `go/pkg/wfcel/` first (tasks WFRULES-CEL-*).
2. Implement sy-wf-rules validation using that package.
3. When wf-generic is implemented, it imports the same package.

If `go/pkg/wfcel/` does not yet exist, it is created as part of this task set.

---

## 4) Module setup

### WFRULES-SETUP-1 — go.mod
- [ ] Create `go/sy-wf-rules/go.mod`
  - module: `github.com/4iplatform/json-router/sy-wf-rules`
  - go version: 1.25.0
  - require: `fluxbee-go-sdk`, `github.com/google/cel-go`, `github.com/santhosh-tekuri/jsonschema`, `github.com/google/uuid`
  - replace: `github.com/4iplatform/json-router/fluxbee-go-sdk => ../fluxbee-go-sdk`
  - replace: `github.com/4iplatform/json-router/pkg/wfcel => ../pkg/wfcel`
- [ ] Run `go mod tidy`

### WFRULES-SETUP-2 — Directory structure
- [ ] Create initial scaffold:
```
go/sy-wf-rules/
├── go.mod
├── main.go              # entry point: load config, call node.Run()
├── node/
│   ├── node.go          # Run(): connect SDK, acquire lock, enter message loop
│   ├── dispatch.go      # route inbound messages by meta.type + meta.msg
│   ├── config.go        # node config (hive_id, state_dir, orchestrator target)
│   ├── store.go         # filesystem state: staged/current/backup r/w helpers
│   ├── validate.go      # compile step: call wfcel.ValidateDefinition, write staged
│   ├── apply.go         # apply step: rotate dirs, call orchestrator
│   ├── orchestrator.go  # L2 helpers: get_node_status, get_node_config, set_node_config, kill_node, run_node
│   ├── wfnode.go        # WF_LIST_INSTANCES query helper (active_instances lookup)
│   ├── handlers.go      # handleConfigSet, handleCompileWorkflow, handleApplyWorkflow, etc.
│   └── query.go         # handleGetWorkflow, handleGetStatus, handleListWorkflows
```

### WFRULES-SETUP-3 — Shared CEL package
- [ ] Create `go/pkg/wfcel/go.mod`
  - module: `github.com/4iplatform/json-router/pkg/wfcel`
- [ ] Directory structure:
```
go/pkg/wfcel/
├── go.mod
├── environment.go   # Build cel.Env with input/state/event/now()
├── compile.go       # CompileGuard(env, expr string) → error
└── validate.go      # ValidateDefinition(def WorkflowDefinition) → []ValidationError
```

---

## 5) Shared CEL package (go/pkg/wfcel)

### WFRULES-CEL-1 — WorkflowDefinition types in wfcel
- [ ] Define minimal `WorkflowDefinition` Go struct covering fields needed for validation:
  - `WfSchemaVersion`, `WorkflowType`, `InitialState`, `TerminalStates`, `States`
  - `StateDefinition` with `Transitions []TransitionDefinition`, `EntryActions`, `ExitActions`
  - `TransitionDefinition` with `Guard string`, `TargetState`, `Actions []ActionDefinition`
  - `ActionDefinition` with `Type string` + per-type fields (send_message, schedule_timer, etc.)
- [ ] JSON deserialization for the above structs

### WFRULES-CEL-2 — CEL environment builder
- [ ] Build `cel.Env` with three implicit variables:
  - `input` — `map_type(string, dyn)` (from input_schema, typed as dynamic in v1)
  - `state` — `map_type(string, dyn)`
  - `event` — `map_type(string, dyn)` (incoming message envelope)
- [ ] Register `now()` function returning unix milliseconds (injectable clock for tests)

### WFRULES-CEL-3 — Guard compilation
- [ ] `CompileGuard(env *cel.Env, expr string) error` — compile single guard, return descriptive error
- [ ] `CompileAllGuards(def WorkflowDefinition) []ValidationError` — walk all transitions, return errors with path (e.g. `states[1].transitions[0].guard`)

### WFRULES-CEL-4 — Full definition validation (11 checks from spec §6)
- [ ] Check 1: `wf_schema_version` present and recognized
- [ ] Check 2: `input_schema` is valid JSON Schema
- [ ] Check 3: `initial_state` exists in states
- [ ] Check 4: all `terminal_states` exist in states
- [ ] Check 5: every `target_state` in transitions exists in states
- [ ] Check 6: every CEL guard compiles (use WFRULES-CEL-3)
- [ ] Check 7: every action type is known (send_message, schedule_timer, cancel_timer, reschedule_timer, set_variable)
- [ ] Check 8: `send_message` targets are syntactically valid L2 names (`<name>@<hive>` or bare name)
- [ ] Check 9: `schedule_timer` durations ≥ 60 seconds
- [ ] Check 10: `set_variable` names are valid identifiers (`[a-zA-Z_][a-zA-Z0-9_]*`)
- [ ] Check 11: `$ref` paths use valid root (`input`, `state`, or `event`)
- [ ] Return `[]ValidationError{Path, Message}`. Empty slice = valid.

### WFRULES-CEL-5 — Tests for wfcel
- [ ] Valid definition passes all 11 checks
- [ ] Each check independently catches a crafted invalid definition
- [ ] CEL guard with typo returns error with correct path
- [ ] `now()` is available in guards (returns a number)

---

## 6) Filesystem state management

### WFRULES-STORE-1 — State directory layout
- [ ] Ensure `/var/lib/fluxbee/wf-rules/<workflow_name>/` on first compile for that workflow
- [ ] `staged/`, `current/`, `backup/` subdirectories created lazily
- [ ] `definition.json` + `metadata.json` written atomically (write to tmp, rename)

### WFRULES-STORE-2 — Metadata struct
- [ ] `WfRulesMetadata` struct: `Version uint64`, `Hash string`, `WorkflowName`, `WorkflowType`, `WfSchemaVersion`, `CompiledAt time.Time`, `GuardCount`, `StateCount`, `ActionCount`
- [ ] `Hash` = `sha256:` prefix + hex of `definition.json` bytes
- [ ] `Version` auto-increments per workflow: read current `version` from `staged/metadata.json` or `current/metadata.json`, +1. If no prior state, start at 1.
- [ ] JSON marshal/unmarshal for metadata

### WFRULES-STORE-3 — Rotate dirs on apply
- [ ] `RotateToApply(workflowName string) error`:
  1. If `backup/` exists, delete it
  2. Rename `current/` → `backup/` (if current/ exists)
  3. Rename `staged/` → `current/`
- [ ] Atomic enough for v1: on crash mid-rotate, operator can inspect and retry

### WFRULES-STORE-4 — Read helpers
- [ ] `ReadCurrentDefinition(workflowName string) (*WorkflowDefinition, *WfRulesMetadata, error)`
- [ ] `ReadStagedMetadata(workflowName string) (*WfRulesMetadata, error)`
- [ ] `WorkflowExists(workflowName string) bool` (current/ non-empty)
- [ ] `ListWorkflows() []string` (enumerate subdirs of wf-rules/)

### WFRULES-STORE-5 — Delete state dirs
- [ ] `DeleteWorkflowState(workflowName string) error` — remove entire `/var/lib/fluxbee/wf-rules/<workflow_name>/`

---

## 7) Orchestrator client

### WFRULES-ORCH-1 — get_node_status
- [ ] `GetNodeStatus(nodeL2Name string) (*NodeStatus, error)` — send `get_node_status` system action to orchestrator, parse response
- [ ] `NodeStatus`: `Running bool`, `ConfigExists bool`, `UnitActive bool`
- [ ] Timeout: 5 seconds

### WFRULES-ORCH-2 — get_node_config
- [ ] `GetNodeConfig(nodeL2Name string) (map[string]any, error)` — send `get_node_config`, return parsed config JSON
- [ ] Used to read existing operational fields before rewriting workflow_definition

### WFRULES-ORCH-3 — set_node_config
- [ ] `SetNodeConfig(nodeL2Name string, config map[string]any) error` — send `set_node_config` with new config
- [ ] Timeout: 5 seconds

### WFRULES-ORCH-4 — kill_node
- [ ] `KillNode(nodeL2Name string) error` — send `kill_node` system action
- [ ] If orchestrator returns `NODE_NOT_FOUND`: return nil (tolerated — node may already be dead)
- [ ] Other errors: return error

### WFRULES-ORCH-5 — run_node
- [ ] `RunNode(nodeL2Name string, config map[string]any) error` — send `run_node` with `node_name`, `runtime = "wf.engine"`, and config
- [ ] Timeout: 10 seconds

### WFRULES-ORCH-6 — Build restart sequence (spec §7.2)
- [ ] `RestartWFNode(nodeL2Name string, newConfig map[string]any) RestartResult`:
  1. KillNode → if error and not NODE_NOT_FOUND: log warning, continue
  2. RunNode → if error: sleep 1s, retry once
  3. If retry also fails: return `RestartResult{Action: "restart_failed", Error: <detail>}`
  4. Success: return `RestartResult{Action: "restarted"}`

---

## 8) WF node query client

### WFRULES-WF-1 — active_instances query
- [ ] `QueryActiveInstances(wfNodeL2Name string) (count int, reachable bool, err error)`:
  - Send `WF_LIST_INSTANCES` query with `status_filter: "running"`, `limit: 0`
  - Timeout: 2 seconds
  - On timeout or NODE_NOT_FOUND: return `count=0, reachable=false, nil`
  - On response: return `count, true, nil`
- [ ] Used by `get_status`, `list_workflows`, and `delete_workflow` (force=false)

---

## 9) Core operations

### WFRULES-OP-1 — compile operation
- [ ] Parse `workflow_name` from request (validate against `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$`)
- [ ] Parse `definition` JSON from request
- [ ] Call `wfcel.ValidateDefinition(def)` — if errors: return `COMPILE_ERROR` with detail path
- [ ] Build metadata (hash definition, compute guard/state/action counts, bump version)
- [ ] Write `staged/definition.json` and `staged/metadata.json` atomically
- [ ] Return compile success response with staged metadata

### WFRULES-OP-2 — apply operation
- [ ] Require `workflow_name`
- [ ] Check `staged/` exists, else return `NOTHING_STAGED`
- [ ] If `version` in request > 0: verify matches `staged/metadata.json`; else skip check
- [ ] Call `RotateToApply`
- [ ] Build new `config.json` for WF node:
  - If WF node config exists: read via WFRULES-ORCH-2, preserve operational fields, replace `workflow_definition`
  - If not: build minimal config with defaults (`sy_timer_l2_name`, `gc_retention_days: 7`, `gc_interval_seconds: 3600`) + `workflow_definition`
- [ ] Determine node existence via WFRULES-ORCH-1
- [ ] If running: SetNodeConfig, then RestartWFNode (WFRULES-ORCH-6)
- [ ] If not running + auto_spawn=true: RunNode with config inline
- [ ] If not running + auto_spawn=false: no orchestrator calls, return with `wf_node.action = "none"`
- [ ] Build and return apply response (include partial-success shape if restart failed)

### WFRULES-OP-3 — compile_apply operation
- [ ] Execute compile (WFRULES-OP-1) — if compile fails, stop and return error
- [ ] If compile succeeds, immediately execute apply (WFRULES-OP-2)
- [ ] Return apply response (staged metadata is visible in current/ at this point)

### WFRULES-OP-4 — rollback operation
- [ ] Require `workflow_name`
- [ ] Check `backup/` exists, else return `NO_BACKUP`
- [ ] Rotate: delete `staged/` (if exists), rename `current/` → `staged/`, rename `backup/` → `current/`
- [ ] RestartWFNode with current/ definition config (same flow as apply)
- [ ] Return rollback response

### WFRULES-OP-5 — delete_workflow operation
- [ ] Require `workflow_name`, `force bool`
- [ ] If `force=false`:
  - QueryActiveInstances — if unreachable: return `INSTANCES_UNKNOWN`
  - If count > 0: return `INSTANCES_ACTIVE` with count
- [ ] KillNode (tolerate NODE_NOT_FOUND)
- [ ] DeleteWorkflowState (WFRULES-STORE-5)
- [ ] Return `{"status": "ok", "deleted": true}`

---

## 10) Query handlers

### WFRULES-QRY-1 — get_workflow
- [ ] Require `workflow_name`
- [ ] Check `WorkflowExists` — else return `WORKFLOW_NOT_FOUND`
- [ ] Read current definition + metadata
- [ ] Return full definition JSON + metadata fields

### WFRULES-QRY-2 — get_status
- [ ] Require `workflow_name`
- [ ] Read current + staged metadata (staged version may be absent)
- [ ] QueryActiveInstances (WFRULES-WF-1)
- [ ] Return: current_version, current_hash, staged_version (omit if no staged), last_error (from metadata), wf_node object

### WFRULES-QRY-3 — list_workflows
- [ ] ListWorkflows() to enumerate all managed workflow names
- [ ] For each: read current metadata + QueryActiveInstances (parallel with timeout)
- [ ] Return array with per-workflow summary

---

## 11) Message dispatch

### WFRULES-DISP-1 — CONFIG_SET routing
- [ ] Parse `operation` field (default: `"compile"`; `"check"` treated as `"compile"`)
- [ ] Validate operation is one of: compile, compile_apply, apply, rollback
- [ ] Route to appropriate operation handler
- [ ] Return `UNSUPPORTED_OPERATION` for unknown values

### WFRULES-DISP-2 — Command messages
- [ ] `compile_workflow` → WFRULES-OP-1
- [ ] `apply_workflow` → WFRULES-OP-2
- [ ] `rollback_workflow` → WFRULES-OP-4
- [ ] `delete_workflow` → WFRULES-OP-5

### WFRULES-DISP-3 — Query messages
- [ ] `get_workflow` → WFRULES-QRY-1
- [ ] `get_status` → WFRULES-QRY-2
- [ ] `list_workflows` → WFRULES-QRY-3

### WFRULES-DISP-4 — System messages
- [ ] `NODE_STATUS_GET` → standard node status handler (same as all SY nodes)
- [ ] `CONFIG_GET` → return node's own operational config (state_dir, hive_id, orchestrator target)
- [ ] `CONFIG_SET` → WFRULES-DISP-1
- [ ] `CONFIG_CHANGED` → WFRULES-DISP-1 (same path as CONFIG_SET, from admin broadcast)

---

## 12) Boot sequence

### WFRULES-BOOT-1 — Startup
- [ ] Load `hive.yaml` for `hive_id` and `blob_path` (needed if future package install integration)
- [ ] Determine orchestrator target: `SY.orchestrator@<hive_id>`
- [ ] Ensure `/var/lib/fluxbee/wf-rules/` exists (mkdir -p)
- [ ] Acquire file lock at `/var/run/fluxbee/sy-wf-rules.lock` (single instance guard)
- [ ] Load or create node UUID (persistent, same pattern as other SY nodes)
- [ ] Connect to router via fluxbee-go-sdk
- [ ] Enter message receive loop

### WFRULES-BOOT-2 — Graceful shutdown
- [ ] On SIGTERM/SIGINT: close SDK connection, release file lock, exit cleanly
- [ ] In-flight request handlers complete before shutdown (context cancellation)

---

## 13) Admin surface (SY.admin integration)

### WFRULES-ADM-1 — POST /admin/wf-rules/compile
- [ ] Parse body: `workflow_name`, `definition`, optional `version`
- [ ] Forward as CONFIG_SET with `operation: "compile"` to `SY.wf-rules@<hive>` via L2
- [ ] Return response from sy.wf-rules

### WFRULES-ADM-2 — POST /admin/wf-rules/compile-apply
- [ ] Forward as CONFIG_SET with `operation: "compile_apply"`

### WFRULES-ADM-3 — POST /admin/wf-rules/apply
- [ ] Forward as CONFIG_SET with `operation: "apply"`; `workflow_name` from body or `{name}` path param

### WFRULES-ADM-4 — POST /admin/wf-rules/rollback
- [ ] Forward as CONFIG_SET with `operation: "rollback"`

### WFRULES-ADM-5 — POST /admin/wf-rules/delete
- [ ] Forward as command `delete_workflow`; parse `force` from body

### WFRULES-ADM-6 — GET /admin/wf-rules/{name}
- [ ] Forward as query `get_workflow`

### WFRULES-ADM-7 — GET /admin/wf-rules/{name}/status
- [ ] Forward as query `get_status`

### WFRULES-ADM-8 — GET /admin/wf-rules
- [ ] Forward as query `list_workflows`

---

## 14) Tests

### WFRULES-TEST-1 — compile happy path
- [ ] Valid definition → staged/definition.json + staged/metadata.json written
- [ ] Metadata has correct hash, guard/state/action counts, version=1

### WFRULES-TEST-2 — compile failure
- [ ] CEL guard typo → `COMPILE_ERROR` with path to offending guard
- [ ] Unknown action type → `COMPILE_ERROR`
- [ ] Invalid `initial_state` → `COMPILE_ERROR`
- [ ] Definition NOT written to staged on any failure

### WFRULES-TEST-3 — apply with mock orchestrator
- [ ] After compile, apply rotates staged→current, backup is empty
- [ ] Second compile+apply: backup contains previous current
- [ ] orchestrator kill_node + run_node called in order

### WFRULES-TEST-4 — restart failure handling
- [ ] Mock run_node returning error → first retry after 1s → second error → `restart_failed` response
- [ ] sy.wf-rules state is consistent: current/ has new definition regardless of restart failure

### WFRULES-TEST-5 — rollback
- [ ] rollback with no backup → `NO_BACKUP`
- [ ] rollback after one apply → restores previous definition to current/

### WFRULES-TEST-6 — delete
- [ ] force=false + unreachable WF node → `INSTANCES_UNKNOWN`
- [ ] force=false + 0 active instances → delete succeeds
- [ ] force=false + active instances > 0 → `INSTANCES_ACTIVE` with count
- [ ] force=true → delete always succeeds

### WFRULES-TEST-7 — workflow_name validation
- [ ] Valid names pass: `invoice`, `on-boarding`, `wf.v2.test`
- [ ] Invalid names rejected: `Invoice`, `wf rules`, `123start`, `wf_rules`

### WFRULES-TEST-8 — check synonym
- [ ] `operation: "check"` behaves identically to `operation: "compile"`

---

## 15) Installation

### WFRULES-INSTALL-1 — Binary
- [ ] Add `sy-wf-rules` to `scripts/install.sh` alongside other SY binaries
- [ ] Systemd unit file: `sy-wf-rules.service`
- [ ] State directory created by install: `/var/lib/fluxbee/wf-rules/`

### WFRULES-INSTALL-2 — Node registration
- [ ] sy.wf-rules registers with SY.identity at boot (same pattern as other SY nodes)
- [ ] Node type: `system`, node name: `SY.wf-rules`

---

## 16) Implementation order

Suggested sequence respecting dependencies:

1. **WFRULES-SETUP-3** — create `go/pkg/wfcel/` module
2. **WFRULES-CEL-1 → CEL-5** — shared CEL package with full validation + tests
3. **WFRULES-SETUP-1, SETUP-2** — sy-wf-rules module scaffold
4. **WFRULES-STORE-1 → STORE-5** — filesystem state management
5. **WFRULES-BOOT-1, BOOT-2** — boot sequence + message loop skeleton
6. **WFRULES-ORCH-1 → ORCH-6** — orchestrator client
7. **WFRULES-WF-1** — WF node query client (active_instances)
8. **WFRULES-OP-1** — compile operation (uses CEL + store)
9. **WFRULES-OP-2, OP-3** — apply + compile_apply
10. **WFRULES-OP-4, OP-5** — rollback + delete
11. **WFRULES-QRY-1 → QRY-3** — query handlers
12. **WFRULES-DISP-1 → DISP-4** — full dispatch wiring
13. **WFRULES-TEST-1 → TEST-8** — tests
14. **WFRULES-ADM-1 → ADM-8** — admin endpoints in SY.admin
15. **WFRULES-INSTALL-1, INSTALL-2** — install + registration

---

## Open questions (deferred)

| # | Question | Impact |
|---|---|---|
| OQ-1 | Numbering bug in spec: §7.1 appears twice (config.json format and wf.engine dependency). Which becomes §7.2? | Doc only |
| OQ-2 | Does sy.orchestrator's `run_node` accept inline config in the payload, or does it require the config.json to already exist on disk? | Affects WFRULES-ORCH-5 and WFRULES-OP-2 auto_spawn path |
| OQ-3 | `WF_LIST_INSTANCES` message name and payload — confirm exact wire format from wf-v1.md §16 before implementing WFRULES-WF-1 | Affects active_instances query |
| OQ-4 | Does SY.admin already have a generic L2 proxy helper, or does each admin endpoint need its own SDK send? | Affects WFRULES-ADM-* scope |
