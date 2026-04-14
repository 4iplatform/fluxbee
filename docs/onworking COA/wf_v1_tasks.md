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
- [x] Evaluate guard program with 10ms timeout (spec §9.3)
- [x] On timeout: log warning, return `false` (transition not taken)
- [x] On CEL evaluation error: log warning, return `false`
- [x] On CEL result `true`: transition taken
- [x] Unit tests: basic boolean guards, `now()` usage, timeout enforcement, missing field → false

---

## 7) Payload substitution (`$ref` resolution)

### WF-SUBST-1 — `$ref` resolver
- [x] Implement `substitute.go` with function `Resolve(payload any, input, state, event map[string]any) (any, error)`
- [x] Recursively traverse the JSON object
- [x] For each value that is an object with exactly one key `$ref` (and the value is a string):
  - Parse the path: split on `.`
  - First segment must be `input`, `state`, or `event`
  - Walk the path through the corresponding source
  - Replace the wrapper object with the resolved value (preserving type)
  - If path does not resolve: return error
- [x] All other values pass through as literals
- [x] Unit tests:
  - `{"$ref": "input.customer_id"}` → string
  - `{"$ref": "state.validated_at"}` → int preserved
  - `{"$ref": "event.payload.order_id"}` → nested lookup works
  - `{"$ref": "state.nonexistent"}` → error returned
  - Nil payload → nil returned
  - Slice with mixed $ref and literals → all resolved
  - Mixed object (literals + $ref) → both work correctly

---

## 8) SQLite persistence

### WF-STORE-1 — Schema
- [x] Implement `store.go` with schema:
  - `wf_definitions` table
  - `wf_instances` table (indexes on status, created_at)
  - `wf_instance_log` table (index on instance_id + log_id DESC)
- [x] WAL mode + `PRAGMA synchronous = NORMAL` (same pattern as sy-timer)
- [x] Schema migrations via `user_version` pragma

### WF-STORE-2 — Definitions CRUD
- [x] `UpsertDefinition(ctx, workflowType, definitionJSON, hash string, clock)` — insert or replace
- [x] `LoadAllDefinitions(ctx) []DefinitionRow`

### WF-STORE-3 — Instances CRUD
- [x] `CreateInstance(ctx, inst WFInstanceRow) error`
- [x] `LoadRunningInstances(ctx) ([]WFInstanceRow, error)` — WHERE status IN ('running','cancelling')
- [x] `UpdateInstance(ctx, inst WFInstanceRow) error` — updates current_state, status, state_json, current_trace_id, updated_at, terminated_at
- [x] `GetInstance(ctx, instanceID string) (WFInstanceRow, error)`
- [x] `ListInstances(ctx, statusFilter string, limit int, createdAfterMS int64) ([]WFInstanceRow, error)`

### WF-STORE-4 — Log CRUD
- [x] `AppendLog(ctx, entry WFLogEntry) error` — insert + enforce 100-entry cap (delete oldest if needed)
- [x] `GetRecentLog(ctx, instanceID string, limit int) ([]WFLogEntry, error)`
- [x] Log cap enforcement is safe under per-instance mutex

### WF-STORE-5 — GC
- [x] `DeleteTerminatedBefore(ctx, cutoffMS int64) (int, error)` — deletes instances + their log entries + timer rows
- [x] Called by periodic GC task (default: every hour, cleans entries older than 7 days)

### WF-STORE-6 — Timer key index (no UUIDs needed)
- [x] Table `wf_instance_timers` with PRIMARY KEY (instance_id, timer_key)
- [x] No local UUIDs — operations on SY.timer use `client_ref = "wf:<instance_id>::<timer_key>"`
- [x] `RegisterTimer(ctx, instanceID, timerKey string, scheduledAtMS, fireAtMS int64) error` — upserts
- [x] `DeleteTimer(ctx, instanceID, timerKey string) error`
- [x] `ListTimersForInstance(ctx, instanceID string) ([]TimerRow, error)` — used for cleanup on termination
- [x] `ListAllExpectedTimers(ctx) ([]TimerRow, error)` — used in restart recovery

---

## 9) Instance lifecycle

### WF-INST-1 — Instance creation (spec §14.3)
- [x] Generate `instance_id = "wfi:" + uuid`
- [x] Insert into `wf_instances` (status=running, current_state=initial_state, state_json={})
- [x] Acquire per-instance mutex
- [x] Execute entry_actions of initial_state (act-then-persist model)
- [x] Persist updated state
- [x] Release mutex
- [x] On invalid payload: respond with `INVALID_INPUT`

### WF-INST-2 — Event correlation and dispatch (spec §14.5)
- [x] Implement `correlate.go` with the strict order:
  1. TIMER_FIRED + client_ref/user_payload.instance_id → route to that instance; log "orphaned timer" if not found
  2. meta.thread_id matches a running instance → deliver to it
  3. Otherwise → new instance via WF-INST-1
- [x] Outbound `send_message` sets `meta.thread_id = instance_id`

### WF-INST-3 — State transition execution (act-then-persist)
- [x] Per-instance mutex held for full transition
- [x] status == 'cancelling' → immediate transition to 'cancelled', run entry_actions, cleanup timers
- [x] Evaluate transitions in declaration order: event_match → CEL guard → first match wins
- [x] Unhandled event → log entry, no state change
- [x] Execution order: exit_actions → transition.actions → entry_actions of target → persist
- [x] Each action failure logged, does NOT halt transition
- [x] trace_id persisted before actions execute (crash recovery re-uses same trace_id)
- [x] Terminal state → cancel all timers, set terminated_at, set status

### WF-INST-4 — Soft abort
- [x] On `WF_CANCEL_INSTANCE` (in dispatch.go):
  - Acquire instance mutex
  - If already terminal: respond with `INSTANCE_ALREADY_TERMINATED`
  - If already cancelling: respond with `INSTANCE_CANCELLING`
  - Set status = 'cancelling', log reason
  - Release mutex (transition happens on next event)
- [x] When cancelling instance receives any event: immediately transition to 'cancelled' (handled in RunTransition)

---

## 10) Action vocabulary

### WF-ACT-1 — `send_message`
- [x] Resolve `$ref` wrappers in payload via `Resolve()`
- [x] Build L2 message using fluxbee-go-sdk
- [x] Set `meta.msg` and `meta.type` from action definition (default type: "system")
- [x] Set `meta.thread_id = instance_id` (enables response correlation)
- [x] Set `meta.trace_id` from `current_trace_id` (idempotency on re-execution)
- [x] Fire and forget via `Dispatcher.SendMsg()`

### WF-ACT-2 — `schedule_timer` (uses client_ref, no UUID tracking)
- [x] Parse `fire_in` OR `fire_at` — validated at load time and again at execution
- [x] `client_ref = "wf:<instance_id>::<timer_key>"`
- [x] Payload: `{"instance_id": "...", "timer_key": "..."}`; `missed_policy` forwarded
- [x] Fire-and-forget to SY.timer via `TimerSender.ScheduleIn()` / `.Schedule()`
- [x] Insert row in `wf_instance_timers` with computed fire_at
- [x] `fire_in < 60s` at execution time → error returned (transition still continues)

### WF-ACT-3 — `cancel_timer`
- [x] `client_ref = "wf:<instance_id>::<timer_key>"`
- [x] `CancelByClientRef()` to SY.timer
- [x] Delete row from `wf_instance_timers`
- [x] Timer key not registered → no-op, log warning

### WF-ACT-4 — `reschedule_timer`
- [x] `RescheduleByClientRef()` to SY.timer with new `fire_in` / `fire_at`
- [x] Upsert `fire_at_ms` in `wf_instance_timers`
- [x] Timer key not registered → no-op, log warning

### WF-ACT-5 — `set_variable`
- [x] Evaluate `value` as CEL expression against (input, state, event)
- [x] Write result into `inst.StateVars[name]`
- [x] Persisted as part of the transition's atomic state update

---

## 11) Inbound message dispatch

### WF-DISP-1 — Dispatch table
- [x] `NODE_STATUS_GET` → `BuildDefaultNodeStatusResponse`
- [x] `WF_HELP` → WF_HELP_RESPONSE with workflow_type, description, states, input_schema
- [x] `WF_CANCEL_INSTANCE` → WF-INST-4 (sets status=cancelling, responds with WF_CANCEL_INSTANCE_RESPONSE)
- [x] `WF_GET_INSTANCE` → instance row + recent log in WF_GET_INSTANCE_RESPONSE
- [x] `WF_LIST_INSTANCES` → filtered list with count in WF_LIST_INSTANCES_RESPONSE
- [x] `WF_GET_CODE` → raw definition JSON in WF_GET_CODE_RESPONSE
- [x] `TIMER_FIRED` → CorrelateAndDispatch (TIMER_FIRED client_ref route)
- [x] `TIMER_SCHEDULE_RESPONSE`, `TIMER_CANCEL_RESPONSE`, `TIMER_RESCHEDULE_RESPONSE` → log only
- [x] Anything else → CorrelateAndDispatch (thread_id route or new instance)

### WF-DISP-2 — Error responses
- [x] All error responses: `{ "ok": false, "error": "<ERROR_CODE>", "detail": "..." }`
- [x] Error codes: INVALID_REQUEST, INSTANCE_NOT_FOUND, INSTANCE_ALREADY_TERMINATED, INSTANCE_CANCELLING, STORE_ERROR, INVALID_INPUT

---

## 12) Restart recovery

### WF-RECOVER-1 — Boot reconciliation flow (spec §11.4)
- [x] Load all instances WHERE status IN ('running', 'cancelling') into InstanceRegistry
- [x] Load all rows from `wf_instance_timers`
- [x] Query SY.timer TIMER_LIST filtered by owner_l2_name
- [x] Build `client_ref → TimerInfo` index from SY.timer response
- [x] Case A: timer in SY.timer, fire_at future → nothing to do
- [x] Case B: timer in SY.timer, fire_at past → inject synthetic TIMER_FIRED, then TIMER_CANCEL in SY.timer
- [x] Case C: timer in wf_instance_timers but NOT in SY.timer → log warning, delete local row
- [x] Orphaned SY.timer entries (not in wf_instance_timers) → TIMER_CANCEL + log
- [x] Enter receive loop only after reconciliation completes

### WF-RECOVER-2 — Recovery edge cases
- [x] Instance 'cancelling' at recovery: skip timer reconciliation, leave for next event
- [x] TIMER_LIST fails: retry with backoff (500ms, 2s, 5s), 3 attempts; refuse to start on persistent failure
- [x] Recovery idempotent: running twice produces same end state (cancel of already-cancelled is no-op)

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
- [x] `LoadConfig(path)` parses config.json with strict unknown-field rejection
- [x] Fail fast if workflow_definition_path, db_path, or sy_timer_l2_name missing
- [x] gc_retention_days and gc_interval_seconds default to 7 and 3600 if unset

### WF-CONFIG-2 — Main entrypoint
- [x] `main.go`: parse `--config` flag, signal-aware context, call `node.Run(ctx, opts)`
- [x] `node.Run()`: load config → load definition → open SQLite → connect SDK → recover → start GC → receive loop
- [x] No SDK config (tests/dry-run): validates definition and exits cleanly

---

## 14) Tests

### WF-TEST-1 — Definition validation unit tests
- [x] Valid workflow loads without error
- [x] Missing initial_state → load error with path
- [x] Bad target_state reference → load error
- [x] Guard that fails CEL compile → load error
- [x] schedule_timer with fire_in < 60s → load error
- [x] send_message with invalid L2 name → load error
- [x] Invalid `$ref` path → load error

### WF-TEST-2 — CEL guard unit tests
- [x] `event.payload.complete == true` evaluates correctly
- [x] Missing field in event → false (not panic)
- [x] `now()` matches injected fixed clock value exactly
- [x] guardEvalTimeout ≤ 10ms enforced by constant
- [x] Injected clock works for deterministic testing
- [x] Multi-variable guard (input.amount > state.threshold) works

### WF-TEST-3 — Substitution unit tests
- [x] `{"$ref": "input.customer_id"}` resolves to string
- [x] `{"$ref": "state.validated_at"}` preserves int64
- [x] `{"$ref": "event.payload.order_id"}` walks nested path
- [x] Missing key → error returned
- [x] Unknown root → error returned
- [x] Nil payload → nil returned
- [x] Slice with mixed $ref and literals resolved correctly
- [x] Nested map with $ref inside resolves correctly

### WF-TEST-4 — Instance lifecycle unit tests (in-memory, no network)
- [x] Create instance → initial_state entry_actions execute (send_message + schedule_timer)
- [x] Event matching transition with guard=true → state=completed, status=completed, terminated_at set
- [x] Guard false on correct msg → state unchanged
- [x] No transition matches → state unchanged, unhandled event logged
- [x] status=cancelling on any event → state=cancelled, terminated_at set
- [x] Terminal state → all registered timers cancelled via mock TimerSender
- [x] Dispatcher.SendMsg failure → transition still completes (action failure logged)
- [x] meta.thread_id correlation routes to correct instance
- [x] TIMER_FIRED with client_ref correlates to correct instance
- [x] CurrentTraceID cleared after transition completes (crash recovery support)
- [x] set_variable action writes to StateVars

### WF-TEST-5 — Store unit tests
- [x] Insert + retrieve instance roundtrip
- [x] UpdateInstance persists state + terminated_at
- [x] LoadRunningInstances returns only running/cancelling
- [x] GetInstance returns error for nonexistent
- [x] Log cap: insert 105 entries → only 100 remain (oldest dropped), newest preserved
- [x] GC: delete instances older than cutoff, preserve newer
- [x] Timer index CRUD (register, list, delete)
- [x] Timer upsert updates fire_at on re-registration
- [x] ListAllExpectedTimers across multiple instances

### WF-TEST-6 — Restart recovery unit tests (in-memory SQLite + mock timer client)
- [x] Persist running instance → simulate restart → instance in registry
- [x] Case A: timer in SY.timer, future fire_at → not cancelled, local row preserved
- [x] Case B: timer in SY.timer, past fire_at → synthetic TIMER_FIRED injected, TIMER_CANCEL sent, local row deleted
- [x] Case C: timer in wf_instance_timers but NOT in SY.timer → local row deleted, warning logged
- [x] Orphaned SY.timer timer (not in wf_instance_timers) → TIMER_CANCEL sent
- [x] Cancelling instance: timers skipped in reconciliation, not treated as orphans
- [x] Recovery idempotent: run twice, same end state, no spurious cancels
- [x] TIMER_LIST failure → error returned (hard dependency enforced)

### WF-TEST-7 — Integration test against real SY.timer v1.1
- [ ] End-to-end: WF.invoice instance through complete flow with real SY.timer
- [ ] End-to-end: cancel mid-flow
- [ ] End-to-end: simulate WF crash mid-transition, restart, verify recovery

---

## 15) Build and install integration

### WF-BUILD-1 — scripts/install.sh
- [x] Add build block for `go/nodes/wf/wf-generic/`:
  ```bash
  if [[ -d "$ROOT_DIR/go/nodes/wf/wf-generic" ]]; then
    ...
    (cd "$ROOT_DIR/go/nodes/wf/wf-generic" && go build -o wf-generic .)
  fi
  ```
- [x] Add binary check: look for `$ROOT_DIR/go/nodes/wf/wf-generic/wf-generic`
- [x] Install binary to `/usr/bin/wf-generic`
- [x] Install to `$STATE_DIR/dist/core/bin/wf-generic`

### WF-BUILD-2 — Orchestrator awareness
- [ ] Verify orchestrator can spawn `WF.*` nodes using `wf-generic` binary + `--config` pointing to the per-node workflow definition
- [x] Node family prefix `WF.` recognized in orchestrator config (review `sy_orchestrator_v2_tasks.md` for spawn patterns)
- [x] Added `scripts/publish-wf-runtime.sh` to publish the WF base runtime as `wf.engine`

---
w

## 16) Reference workflow

### WF-REF-1 — Canonical example workflow (WF.invoice)
- [x] Created `go/nodes/wf/examples/wf.invoice.json`
- [x] Uses `{"$ref": "..."}` syntax for payload substitution
- [x] Covers: collecting_data → validated → invoice_sent → completed (+ failed/cancelled terminals)
- [x] Includes schedule_timer, cancel_timer, send_message, set_variable actions

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
