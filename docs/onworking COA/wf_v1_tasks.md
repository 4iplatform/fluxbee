# WF (Workflow Nodes) v1 — Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-04-13 (updated 2026-04-14 with frictions resolved)
**Primary spec:** `docs/wf-v1.md`
**Target module:** `go/nodes/wf/wf-generic/`
**Dependencies:** `go/fluxbee-go-sdk`, `go/sy-timer` (**requires v1.1, see WF-DEP-1**), `cel-go`, `modernc.org/sqlite`

---

## 1) Goal

Implement the `WF.*` node family as described in `docs/wf-v1.md`. The result is a generic Go binary (`wf-generic`) that:

- Loads a workflow definition JSON at startup
- Manages many concurrent instances of that workflow in SQLite
- Evaluates CEL guards for transitions
- Executes the fixed action vocabulary (`send_message`, `schedule_timer`, `cancel_timer`, `reschedule_timer`, `set_variable`)
- Responds to introspection messages (`WF_HELP`, `WF_GET_INSTANCE`, `WF_LIST_INSTANCES`, `WF_GET_CODE`, `WF_CANCEL_INSTANCE`)
- Survives restarts cleanly (recovers in-flight instances, reconciles timers with SY.timer)

One binary, parameterized by a workflow definition JSON. Deployed as many instances, each as a distinct L2 node (e.g. `WF.invoice@motherbee`, `WF.onboarding@motherbee`).

---

## 2) Frozen decisions

- **Language:** Go. Module path `github.com/4iplatform/json-router/nodes/wf`.
- **CEL:** `github.com/google/cel-go` for guard evaluation.
- **SQLite:** `modernc.org/sqlite` (same as sy-timer, pure Go, no cgo).
- **SDK:** `go/fluxbee-go-sdk` for router connection and message envelope. Replace directive `../../../fluxbee-go-sdk`.
- **Timers:** all workflow-visible timers go through SY.timer. Internal timeouts use Go primitives.
- **No compensation in v1.** No rollback, no sagas, no subworkflows.
- **Restart recovery:** load `wf_instances` WHERE status IN ('running','cancelling') + reconcile timers with SY.timer (see WF-RECOVER-1 for full reconciliation flow).
- **Soft abort:** `WF_CANCEL_INSTANCE` sets status = 'cancelling'; next event evaluation for that instance transitions to 'cancelled'.
- **Timer cleanup on termination:** automatic TIMER_CANCEL for all registered timers when an instance reaches a terminal state.
- **Log cap:** 100 entries per instance, FIFO.
- **Retention GC:** periodic sweep of terminated instances older than 7 days (configurable via config.json).
- **Execution model:** act-then-persist with at-least-once delivery guarantees on actions. Downstream nodes must be idempotent on `meta.trace_id`.
- **Payload substitution syntax:** `{"$ref": "<path>"}` wrapper objects, NOT magic strings.
- **Timer identification:** deterministic `client_ref = "wf:<instance_id>::<timer_key>"`, used for all subsequent operations on the timer. NO local UUID tracking required.
- **Event correlation order:** TIMER_FIRED instance_id → meta.thread_id → new instance trigger.
- **`now()` in CEL:** local process time (Go `time.Now()`), NOT roundtrip to SY.timer. For tests, injectable via clock function.
- **Recovery is WF's responsibility:** the WF owns reconciliation of expired timers at boot, including cleaning up SY.timer state.

---

## 3) Critical dependency: SY.timer v1.1

### WF-DEP-1 — SY.timer v1.1 must be available before WF-ACT-2

**WF v1 requires SY.timer v1.1**, which extends the v1.0 API to support `client_ref` as an alternative key for `TIMER_CANCEL`, `TIMER_RESCHEDULE`, and `TIMER_GET`. See the separate task list `sy_timer_v1_1_tasks.md`.

This dependency is hard:

- Without `client_ref`-based lookup, the WF runtime would have to wait for `TIMER_SCHEDULE_RESPONSE` synchronously before being able to cancel/reschedule, which creates blocking behavior incompatible with the WF event loop.
- Alternatively, the WF would need to maintain a local `client_ref → uuid` index with pending states, which adds significant complexity and a race window where the timer is scheduled but not yet trackable.

**The decision is to extend SY.timer**, not work around the limitation in WF.

**Implementation order:**

1. Implement `sy_timer_v1_1_tasks.md` first (extends existing SY.timer code).
2. Verify SY.timer v1.1 with its own tests.
3. Then start WF v1 implementation.

---

## 4) Module setup

### WF-SETUP-1 — go.mod for wf-generic
- [x] Create `go/nodes/wf/wf-generic/go.mod`
  - module: `github.com/4iplatform/json-router/nodes/wf/wf-generic`
  - go version: 1.25.0
  - require: `fluxbee-go-sdk`, `modernc.org/sqlite`, `github.com/google/cel-go`, `github.com/google/uuid`
  - replace: `github.com/4iplatform/json-router/fluxbee-go-sdk => ../../../fluxbee-go-sdk`
- [x] Run `go mod tidy` to resolve indirect deps

### WF-SETUP-2 — Directory structure
- [x] Create the initial `wf-generic` scaffold (`main.go`, `node/`, placeholder runtime files)
```
go/nodes/wf/wf-generic/
├── go.mod
├── main.go             # entry point: parse flags, load config, call node.Run()
├── node/
│   ├── node.go         # top-level Run() — connect SDK, load workflow, start loop
│   ├── definition.go   # workflow definition JSON types + load-time validation
│   ├── cel.go          # CEL environment builder, guard compiler, guard evaluator
│   ├── instance.go     # WFInstance struct, per-instance mutex, transition evaluation
│   ├── actions.go      # execute action vocabulary (send_message, timers, set_variable)
│   ├── substitute.go   # $ref wrapper resolution for payload substitution
│   ├── store.go        # SQLite schema, CRUD for definitions/instances/logs
│   ├── dispatch.go     # inbound message router (WF_HELP, WF_CANCEL, etc.)
│   ├── correlate.go    # event correlation order: TIMER_FIRED instance_id → thread_id → new
│   ├── timer.go        # timer client helper (wraps SY.timer protocol with client_ref)
│   ├── recover.go      # restart recovery and SY.timer reconciliation
│   └── gc.go           # periodic GC task for terminated instances
```

---

## 5) Workflow definition types and load-time validation

### WF-DEF-1 — Go structs for workflow definition
- [x] `WorkflowDefinition` struct covering all fields in spec §8.1
- [x] `StateDefinition` (name, description, entry_actions, exit_actions, transitions)
- [x] `TransitionDefinition` (event_match, guard, target_state, actions)
- [x] `ActionDefinition` (type + typed union for each action type)
- [x] `EventMatch` (msg, optionally type)
- [x] JSON deserialization with strict unknown-field rejection

### WF-DEF-2 — Load-time validation (spec §8.5)
- [x] Check 1: JSON structure valid per `wf_schema_version`
- [x] Check 2: `input_schema` is valid JSON Schema (use `github.com/santhosh-tekuri/jsonschema` or equivalent)
- [x] Check 3: `initial_state` exists in states
- [x] Check 4: all `terminal_states` exist in states
- [x] Check 5: every `target_state` in every transition exists in states
- [x] Check 6: every guard CEL expression compiles against typed environment
- [x] Check 7: every action has known type and valid params (action type registry in `actions.go`)
- [x] Check 8: `send_message` targets are syntactically valid L2 names
- [x] Check 9: `schedule_timer` durations ≥ 60s
- [x] Check 10: `set_variable` names are valid identifiers
- [x] Check 11: `$ref` paths in `send_message` payloads are syntactically valid (root is `input`, `state`, or `event`)
- [x] Return typed load error with path to offending element on any failure

---

## 6) CEL integration

### WF-CEL-1 — CEL environment
- [x] Build `cel.Env` with three implicit variables:
  - `input` — typed from `input_schema` (map[string]any in v1, typed registry in future)
  - `state` — `map_type(string, dyn)` (dynamic)
  - `event` — `map_type(string, dyn)` (the incoming message envelope)
- [x] Register built-in `now()` function returning current UTC unix milliseconds
  - **Source:** Go `time.Now().UnixMilli()` (LOCAL process time, NOT SY.timer)
  - **Injectable for tests** via clock function passed to environment builder
  - Document explicitly that this is local time, not authoritative hive time
- [x] Compile all guards at load time; store compiled `cel.Program` per transition

### WF-CEL-2 — Guard evaluation
- [ ] Evaluate guard program with 10ms timeout (spec §9.3)
- [ ] On timeout: log warning, return `false` (transition not taken)
- [ ] On CEL evaluation error: log warning, return `false`
- [ ] On CEL result `true`: transition taken
- [ ] Unit tests: basic boolean guards, `now()` usage, timeout enforcement, missing field → false

---

## 7) Payload substitution (`$ref` resolution)

### WF-SUBST-1 — `$ref` resolver
- [ ] Implement `substitute.go` with function `Resolve(payload any, instance *WFInstance, event *Message) any`
- [ ] Recursively traverse the JSON object
- [ ] For each value that is an object with exactly one key `$ref` (and the value is a string):
  - Parse the path: split on `.`
  - First segment must be `input`, `state`, or `event`
  - Walk the path through the corresponding source
  - Replace the wrapper object with the resolved value (preserving type)
  - If path does not resolve: omit the field from the result, log warning
- [ ] All other values pass through as literals
- [ ] Unit tests:
  - `{"$ref": "input.customer_id"}` → string
  - `{"$ref": "state.items"}` → array preserved
  - `{"$ref": "event.payload.invoice_id"}` → nested lookup works
  - `{"$ref": "state.nonexistent"}` → omitted from result
  - Literal strings like `"hello"` → unchanged
  - Mixed object: `{"name": {"$ref": "state.name"}, "literal": "foo"}` → both work correctly

---

## 8) SQLite persistence

### WF-STORE-1 — Schema
- [ ] Implement `store.go` with schema from spec §13.1:
  - `wf_definitions` table
  - `wf_instances` table (indexes on status, created_at)
  - `wf_instance_log` table (index on instance_id + log_id DESC)
- [ ] WAL mode + `PRAGMA synchronous = NORMAL` (same pattern as sy-timer)
- [ ] Schema migrations via `user_version` pragma

### WF-STORE-2 — Definitions CRUD
- [ ] `UpsertDefinition(def WorkflowDefinition, hash string)` — insert or replace
- [ ] `LoadAllDefinitions() []WorkflowDefinitionRow`

### WF-STORE-3 — Instances CRUD
- [ ] `CreateInstance(inst WFInstance) error`
- [ ] `LoadRunningInstances() ([]WFInstance, error)` — WHERE status IN ('running','cancelling')
- [ ] `UpdateInstance(inst WFInstance) error` — updates current_state, status, state_json, updated_at, terminated_at
- [ ] `GetInstance(instanceID string) (WFInstance, error)`
- [ ] `ListInstances(statusFilter string, limit int, createdAfterMs int64) ([]WFInstanceRow, error)`

### WF-STORE-4 — Log CRUD
- [ ] `AppendLog(entry WFLogEntry) error` — insert + enforce 100-entry cap (delete oldest if needed)
- [ ] `GetRecentLog(instanceID string, limit int) ([]WFLogEntry, error)`
- [ ] Note: log cap enforcement is safe under per-instance mutex (no race between instance-local inserts)

### WF-STORE-5 — GC
- [ ] `DeleteTerminatedBefore(cutoffMs int64) (int, error)` — deletes instances + their log entries
- [ ] Called by periodic GC task (default: every hour, cleans entries older than 7 days)

### WF-STORE-6 — Timer key index (no UUIDs needed)
- [ ] Table `wf_instance_timers`:
  ```sql
  CREATE TABLE wf_instance_timers (
      instance_id TEXT NOT NULL,
      timer_key   TEXT NOT NULL,
      scheduled_at_ms INTEGER NOT NULL,
      fire_at_ms INTEGER NOT NULL,
      PRIMARY KEY (instance_id, timer_key)
  );
  ```
- [ ] Note: this table tracks **what timers the WF expects to exist**, but the actual SY.timer state is the source of truth. No local UUIDs because operations on SY.timer use `client_ref = "wf:<instance_id>::<timer_key>"`.
- [ ] `RegisterTimer(instanceID, timerKey string, scheduledAtMs, fireAtMs int64) error`
- [ ] `DeleteTimer(instanceID, timerKey string) error`
- [ ] `ListTimersForInstance(instanceID string) ([]TimerRow, error)` — used for cleanup on termination
- [ ] `ListAllExpectedTimers() ([]TimerRow, error)` — used in restart recovery

---

## 9) Instance lifecycle

### WF-INST-1 — Instance creation (spec §14.3)
- [ ] Validate incoming payload against `input_schema`
- [ ] Generate `instance_id = "wfi:" + uuid`
- [ ] Insert into `wf_instances` (status=running, current_state=initial_state, state_json={})
- [ ] Acquire per-instance mutex
- [ ] Execute entry_actions of initial_state (act-then-persist model)
- [ ] Persist updated state
- [ ] Release mutex
- [ ] On input schema validation failure: respond with `INVALID_INPUT`

### WF-INST-2 — Event correlation and dispatch (spec §14.5)
- [ ] Implement `correlate.go` with the strict order:
  1. If `meta.msg == "TIMER_FIRED"` AND `event.user_payload.instance_id` is present:
     - Look up instance by that ID
     - If exists: deliver as event to instance
     - If does not exist: log "orphaned timer" and discard
  2. If `meta.thread_id` is present and matches an existing instance:
     - Deliver as event to that instance
  3. Otherwise: attempt new instance creation
     - Validate payload against `input_schema`
     - If valid: create instance via WF-INST-1
     - If invalid: respond with `INVALID_INPUT`
- [ ] When sending outbound `send_message`, **always set `meta.thread_id = instance_id`**. This is how response correlation works.

### WF-INST-3 — State transition execution (act-then-persist)
- [ ] Acquire per-instance mutex
- [ ] If status == 'cancelling': transition immediately to 'cancelled' terminal state, run entry_actions of 'cancelled', cleanup, return
- [ ] Otherwise: evaluate transitions of current_state in declaration order (spec §9.2)
  - For each transition: check event_match on meta.msg (and meta.type if specified)
  - If matched: evaluate CEL guard
  - If guard true: STOP iteration, take this transition
  - If no match: log "unhandled event", no state change
- [ ] Execution order for the chosen transition:
  ```
  exit_actions of current_state
  → transition.actions
  → entry_actions of target_state
  → persist new state
  ```
- [ ] Each action failure is logged but does NOT halt the transition
- [ ] Each action gets a `meta.trace_id` derived from the original event's trace_id (preserving traceability across re-execution after crash)
- [ ] If target_state is terminal: run terminal cleanup (cancel all registered timers via WF-ACT-3, set terminated_at, status = completed/cancelled/failed)
- [ ] Release per-instance mutex

### WF-INST-4 — Soft abort
- [ ] On `WF_CANCEL_INSTANCE`:
  - Acquire instance mutex
  - If already terminal: respond with `INSTANCE_ALREADY_TERMINATED`
  - If already cancelling: respond with `INSTANCE_CANCELLING`
  - Set status = 'cancelling', log reason
  - Release mutex (do NOT transition now — happens on next event)
- [ ] When cancelling instance receives any event: immediately transition to 'cancelled' (entry_actions of 'cancelled' run, timers cleaned up)

---

## 10) Action vocabulary

### WF-ACT-1 — `send_message`
- [ ] Use `WF-SUBST-1` to resolve `$ref` wrappers in payload (NOT magic strings)
- [ ] Build L2 message using fluxbee-go-sdk
- [ ] Set `meta.msg` from action definition
- [ ] Set `meta.type` from action (default: "system")
- [ ] **Set `meta.thread_id = instance_id`** (this enables response correlation)
- [ ] **Set `meta.trace_id` from the current event's trace_id** (enables idempotency on re-execution after crash)
- [ ] Send via SDK connection. Fire and forget (do not wait for response).
- [ ] Log the action (action_type=send_message, summary="send_message to {target}", ok=true/false)

### WF-ACT-2 — `schedule_timer` (uses client_ref, no UUID tracking)
- [ ] Parse `fire_in` (duration string: "30m", "2h", "1d") OR `fire_at` (ISO 8601 UTC) — at least one required
- [ ] Validate `fire_in` ≥ 60s at load time (CHECK WF-DEF-2 check 9); also validate at execution time as safety net
- [ ] Construct `client_ref = "wf:<instance_id>::<timer_key>"`
- [ ] Build TIMER_SCHEDULE message to SY.timer:
  - `target_l2_name`: this node's L2 name
  - `client_ref`: `wf:<instance_id>::<timer_key>`
  - `payload`: `{ "instance_id": "...", "timer_key": "..." }`
  - `missed_policy` forwarded
- [ ] Send the request and **return immediately** — do NOT wait for `TIMER_SCHEDULE_RESPONSE`
- [ ] Insert row in `wf_instance_timers` with computed fire_at
- [ ] Log the action immediately as ok=true (the actual schedule may fail in SY.timer; this is best-effort and tolerable)
- [ ] On execution-time validation failure (`fire_in < 60s`): log error, do not send to SY.timer, action result is failure (but transition continues per WF-INST-3)

### WF-ACT-3 — `cancel_timer`
- [ ] Construct `client_ref = "wf:<instance_id>::<timer_key>"`
- [ ] Send TIMER_CANCEL to SY.timer with `client_ref` (NOT timer_uuid)
- [ ] Delete row from `wf_instance_timers`
- [ ] If the row didn't exist: no-op, log warning "timer_key not registered"

### WF-ACT-4 — `reschedule_timer`
- [ ] Construct `client_ref = "wf:<instance_id>::<timer_key>"`
- [ ] Send TIMER_RESCHEDULE to SY.timer with `client_ref` and new `fire_in` / `fire_at`
- [ ] Update `fire_at_ms` in `wf_instance_timers`
- [ ] If the row didn't exist: no-op, log warning

### WF-ACT-5 — `set_variable`
- [ ] Evaluate `value` as CEL expression against current (input, state, event) context
- [ ] Write result into instance state_json[name]
- [ ] Persist state_json immediately (part of the same atomic persist as the transition)

---

## 11) Inbound message dispatch

### WF-DISP-1 — Dispatch table
- [ ] `NODE_STATUS_GET` → respond with standard health response (ok, version, active_instances count)
- [ ] `WF_HELP` → respond with WF_HELP_RESPONSE (spec §15)
- [ ] `WF_CANCEL_INSTANCE` → WF-INST-4
- [ ] `WF_GET_INSTANCE` → load instance + recent log, return WF_GET_INSTANCE_RESPONSE
- [ ] `WF_LIST_INSTANCES` → list with filters, return WF_LIST_INSTANCES_RESPONSE
- [ ] `WF_GET_CODE` → return raw workflow definition JSON
- [ ] `TIMER_FIRED` → handled by WF-INST-2 correlation (TIMER_FIRED instance_id route)
- [ ] `TIMER_SCHEDULE_RESPONSE`, `TIMER_CANCEL_RESPONSE`, `TIMER_RESCHEDULE_RESPONSE` → log only, no state mutation needed (we use client_ref; the responses just confirm SY.timer's view, which is informational)
- [ ] Anything else → handled by WF-INST-2 correlation (thread_id route or new instance)

### WF-DISP-2 — Error responses
- [ ] All error responses: `{ "ok": false, "error": "<ERROR_CODE>", "detail": "..." }`
- [ ] Error codes as defined in spec §18

---

## 12) Restart recovery

### WF-RECOVER-1 — Boot reconciliation flow (spec §11.4)
- [ ] Open `wf_instances.db`
- [ ] Load all `wf_definitions` into memory, compile CEL environments
- [ ] Load all instances WHERE status IN ('running', 'cancelling') into in-memory map
- [ ] Load all rows from `wf_instance_timers` into memory, indexed by instance_id
- [ ] Send `TIMER_LIST` to SY.timer filtered by `owner_l2_name = this WF node`
- [ ] Build a set `sy_timer_view` of `client_ref → timer_data` from the response
- [ ] **For each row in `wf_instance_timers`** (the WF's view of expected timers):
  - Construct expected `client_ref = "wf:<instance_id>::<timer_key>"`
  - **Case A: timer exists in `sy_timer_view`, fire_at in future**
    - Normal case, nothing to do, timer is valid
  - **Case B: timer exists in `sy_timer_view`, fire_at in past**
    - The WF was down when this timer should have fired
    - Look up the original `missed_policy` (we did not persist it locally; for v1, **always treat as `fire`** at recovery — this is acceptable because this only matters when the WF is recovering and the timer was meant to fire)
    - Inject a synthetic `TIMER_FIRED` event into the corresponding instance
    - After processing the synthetic event, send `TIMER_CANCEL` to SY.timer for that `client_ref` to prevent SY.timer from also firing it later
  - **Case C: timer in `wf_instance_timers` but NOT in `sy_timer_view`**
    - SY.timer doesn't know about this timer (already fired and dropped by missed_policy, or cleaned up, or never scheduled)
    - Log warning "expected timer missing"
    - Delete from `wf_instance_timers`
    - The instance may be stuck waiting; that is a workflow design issue, not a runtime bug
- [ ] **For timers in `sy_timer_view` but NOT in `wf_instance_timers`** (timers in SY.timer that the WF doesn't recognize):
  - Send `TIMER_CANCEL` to clean them up (orphaned from a previous WF state)
  - Log info
- [ ] Enter receive loop only after reconciliation completes

### WF-RECOVER-2 — Recovery edge cases
- [ ] Instance in 'cancelling' status at recovery: do NOT reconcile timers, just leave it; it will transition to 'cancelled' on next event
- [ ] If `TIMER_LIST` to SY.timer fails (SY.timer unreachable): log error, retry with backoff (3 attempts), if still failing: refuse to start the WF node (this is a hard dependency)
- [ ] Recovery should be idempotent: running it twice should produce the same end state

---

## 13) Node startup and config

### WF-CONFIG-1 — Config schema (config.json)
```json
{
  "workflow_definition_path": "/etc/fluxbee/wf/wf.invoice.json",
  "db_path": "/var/lib/fluxbee/nodes/WF/WF.invoice@motherbee/wf_instances.db",
  "sy_timer_l2_name": "SY.timer@motherbee",
  "gc_retention_days": 7,
  "gc_interval_seconds": 3600
}
```
- [ ] Load and parse config.json at startup
- [ ] Fail fast if required fields missing

### WF-CONFIG-2 — Main entrypoint
- [ ] `main.go`: parse `--config` flag, load config, call `node.Run(ctx, cfg)`
- [ ] `node.Run()`: connect SDK → load workflow definition → compile CEL → open SQLite → recover running instances → reconcile timers (WF-RECOVER-1) → start GC goroutine → start receive loop

---

## 14) Tests

### WF-TEST-1 — Definition validation unit tests
- [ ] Valid workflow loads without error
- [ ] Missing initial_state → load error with path
- [ ] Bad target_state reference → load error
- [ ] Guard that fails CEL compile → load error
- [ ] schedule_timer with fire_in < 60s → load error
- [ ] send_message with invalid L2 name → load error
- [ ] Invalid `$ref` path (e.g. `{"$ref": "foobar.x"}` where root is not input/state/event) → load error

### WF-TEST-2 — CEL guard unit tests
- [ ] `event.payload.complete == true` evaluates correctly
- [ ] Missing field in state → false (not panic)
- [ ] `now()` returns current local time (verify it matches `time.Now()` within a few ms)
- [ ] Guard with infinite loop / timeout → returns false within 15ms
- [ ] Injected clock works for deterministic testing

### WF-TEST-3 — Substitution unit tests
- [ ] `{"$ref": "input.foo"}` resolves to value of foo
- [ ] `{"$ref": "state.items"}` preserves array type
- [ ] `{"$ref": "event.payload.nested.field"}` walks the nested path
- [ ] Missing field omits from result
- [ ] Literal strings pass through unchanged
- [ ] Mixed payload (literals + $ref) works

### WF-TEST-4 — Instance lifecycle unit tests (in-memory, no network)
- [ ] Create instance → initial_state entry_actions execute
- [ ] Event matching first transition → state changes, actions execute
- [ ] Event matching second transition (guard on first is false) → second taken
- [ ] No transition matches → state unchanged, event logged as unhandled
- [ ] WF_CANCEL_INSTANCE sets cancelling; next event → cancelled terminal
- [ ] Terminal state reached → timers cleaned up (mock timer client)
- [ ] Transition action failure (send fails) → transition still completes
- [ ] meta.thread_id correlation works for response messages
- [ ] TIMER_FIRED with instance_id correlates to correct instance
- [ ] Re-execution after crash uses same trace_id (mock crash by killing actions mid-transition)

### WF-TEST-5 — Store unit tests
- [ ] Insert + retrieve instance roundtrip
- [ ] Log cap: insert 105 entries → only 100 remain (oldest dropped)
- [ ] GC: delete instances older than cutoff, preserve newer
- [ ] Timer index CRUD without UUIDs

### WF-TEST-6 — Restart recovery unit tests (in-memory SQLite + mock timer client)
- [ ] Persist running instance → simulate restart → instance recovered
- [ ] Timer expected by WF, exists in SY.timer, future fire_at → preserved
- [ ] Timer expected by WF, exists in SY.timer, past fire_at → synthetic TIMER_FIRED injected, then TIMER_CANCEL sent
- [ ] Timer expected by WF, NOT in SY.timer → row deleted, warning logged
- [ ] Timer in SY.timer but NOT expected by WF → TIMER_CANCEL sent
- [ ] Recovery is idempotent: run twice, same end state

### WF-TEST-7 — Integration test against real SY.timer v1.1
- [ ] End-to-end: WF.invoice instance through complete flow with real SY.timer
- [ ] End-to-end: cancel mid-flow
- [ ] End-to-end: simulate WF crash mid-transition, restart, verify recovery

---

## 15) Build and install integration

### WF-BUILD-1 — scripts/install.sh
- [ ] Add build block for `go/nodes/wf/wf-generic/`:
  ```bash
  if [[ -d "$ROOT_DIR/go/nodes/wf/wf-generic" ]]; then
    ...
    (cd "$ROOT_DIR/go/nodes/wf/wf-generic" && go build -o wf-generic .)
  fi
  ```
- [ ] Add binary check: look for `$ROOT_DIR/go/nodes/wf/wf-generic/wf-generic`
- [ ] Install binary to `/usr/bin/wf-generic`
- [ ] Install to `$STATE_DIR/dist/core/bin/wf-generic`

### WF-BUILD-2 — Orchestrator awareness
- [ ] Verify orchestrator can spawn `WF.*` nodes using `wf-generic` binary + `--config` pointing to the per-node workflow definition
- [ ] Node family prefix `WF.` recognized in orchestrator config (review `sy_orchestrator_v2_tasks.md` for spawn patterns)

---

## 16) Reference workflow

### WF-REF-1 — Canonical example workflow (WF.invoice)
- [ ] Create `go/nodes/wf/examples/wf.invoice.json` — the full invoice workflow from spec §19
- [ ] Uses the `{"$ref": "..."}` syntax for payload substitution
- [ ] Used as the reference for integration testing and as a starting point for operators

---

## Order of implementation

```
[PREREQUISITE: complete sy_timer_v1_1_tasks.md before starting WF-ACT-2]

WF-SETUP-1 → WF-SETUP-2
  → WF-DEF-1 → WF-DEF-2
    → WF-STORE-1 → WF-STORE-2 → WF-STORE-3 → WF-STORE-4 → WF-STORE-5 → WF-STORE-6
    → WF-CEL-1 → WF-CEL-2
    → WF-SUBST-1
      → WF-INST-1 → WF-INST-2 → WF-INST-3 → WF-INST-4
        → WF-ACT-1 → WF-ACT-2 → WF-ACT-3 → WF-ACT-4 → WF-ACT-5
          → WF-DISP-1 → WF-DISP-2
            → WF-RECOVER-1 → WF-RECOVER-2
              → WF-CONFIG-1 → WF-CONFIG-2
                → WF-TEST-1 ... WF-TEST-7
                  → WF-BUILD-1 → WF-BUILD-2
                    → WF-REF-1
```

---

## Resolved frictions (formerly section "Known frictions / notes")

The following points were raised during planning and resolved before implementation. They are recorded here for traceability:

### Resolved 1 — `TIMER_SCHEDULE_RESPONSE` async race
**Problem:** The `timer_uuid` from SY.timer arrives asynchronously, creating a race window where the WF can't cancel a just-scheduled timer.

**Resolution:** Use `client_ref = "wf:<instance_id>::<timer_key>"` as the canonical timer identifier, eliminating the need to track UUIDs locally. **Requires SY.timer v1.1**, which extends the API to support `client_ref`-based lookup. See `sy_timer_v1_1_tasks.md`.

### Resolved 2 — Crash semantics during transitions
**Problem:** What happens if the runtime crashes between executing an action and persisting the new state?

**Resolution:** Adopt **act-then-persist with at-least-once delivery**. Actions may execute more than once across crash recovery. Downstream nodes must be idempotent on `meta.trace_id`. The runtime preserves the original event's trace_id when re-executing actions. Documented as explicit contract in spec §14.4.

### Resolved 3 — Payload substitution syntax ambiguity
**Problem:** Magic strings like `"state.foo"` are ambiguous (literal vs path lookup) and don't support non-string values.

**Resolution:** Use `{"$ref": "state.foo"}` wrapper objects. Implemented in `WF-SUBST-1`.

### Resolved 4 — `now()` source in CEL
**Problem:** Should `now()` come from local process time or SY.timer?

**Resolution:** Local process time (`time.Now()`). Roundtrip to SY.timer would violate the 10ms guard timeout. Documented in spec and in `WF-CEL-1`.

### Resolved 5 — Event correlation order
**Problem:** How does the WF decide whether an event is for an existing instance or a new trigger?

**Resolution:** Strict order: TIMER_FIRED instance_id → meta.thread_id → new instance. Documented in spec §14.5 and implemented in `WF-INST-2` / `correlate.go`.

### Resolved 6 — WF vs SY.timer recovery race
**Problem:** When the WF restarts, who is responsible for handling timers that should have fired during the downtime?

**Resolution:** WF takes ownership. At boot, queries SY.timer with TIMER_LIST, processes expired timers locally as synthetic TIMER_FIRED events, then cancels them in SY.timer to prevent double-firing. Documented in spec §11.4 and implemented in `WF-RECOVER-1`.

---

## Implementation notes

1. **Per-instance mutex with SQLite.** All state mutations for a given instance go through a per-instance `sync.Mutex` in memory. SQLite write contention is between instances (not within one instance) and SQLite's WAL mode handles it. No global lock needed.

2. **`now()` in CEL is local time.** Don't accidentally make it call SY.timer. The 10ms guard timeout doesn't allow for a roundtrip. For tests, inject a clock function via the environment builder.

3. **Action type registry.** Maintain a single map of `action_type → handler` in `actions.go`. Both `WF-DEF-2` (load-time validation) and `WF-INST-3` (execution) use the same registry. Adding a new action type means one place to edit.

4. **The WF runtime never blocks waiting for a SY.timer response.** Always fire-and-forget. The use of `client_ref` makes this safe.

5. **`meta.trace_id` is the idempotency key for outbound actions.** When re-executing a transition after a crash, re-use the same trace_id from the original event. The WF runtime needs to persist the trace_id of the currently-processing event so it can re-use it on recovery.
