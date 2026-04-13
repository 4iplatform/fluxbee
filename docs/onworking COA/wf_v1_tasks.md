# WF (Workflow Nodes) v1 — Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-04-13
**Primary spec:** `docs/systemnode-wf-v1_1.md`
**Target module:** `go/nodes/wf/wf-generic/`
**Dependencies:** `go/fluxbee-go-sdk`, `go/sy-timer`, `cel-go`, `modernc.org/sqlite`

---

## 1) Goal

Implement the `WF.*` node family as described in `systemnode-wf-v1_1.md`. The result is a generic Go binary (`wf-generic`) that:

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
- **Restart recovery:** load `wf_instances` WHERE status IN ('running','cancelling') + reconcile timers with TIMER_LIST from SY.timer.
- **Soft abort:** `WF_CANCEL_INSTANCE` sets status = 'cancelling'; next event evaluation for that instance transitions to 'cancelled'.
- **Timer cleanup on termination:** automatic TIMER_CANCEL for all registered timers when an instance reaches a terminal state.
- **Log cap:** 100 entries per instance, FIFO.
- **Retention GC:** periodic sweep of terminated instances older than 7 days (configurable via config.json).

---

## 3) Module setup

### WF-SETUP-1 — go.mod for wf-generic
- [ ] Create `go/nodes/wf/wf-generic/go.mod`
  - module: `github.com/4iplatform/json-router/nodes/wf/wf-generic`
  - go version: 1.25.0
  - require: `fluxbee-go-sdk`, `modernc.org/sqlite`, `github.com/google/cel-go`, `github.com/google/uuid`
  - replace: `github.com/4iplatform/json-router/fluxbee-go-sdk => ../../../fluxbee-go-sdk`
- [ ] Run `go mod tidy` to resolve indirect deps

### WF-SETUP-2 — Directory structure
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
│   ├── store.go        # SQLite schema, CRUD for definitions/instances/logs
│   ├── dispatch.go     # inbound message router (WF_HELP, WF_CANCEL, etc.)
│   ├── timer.go        # timer client helper (wraps SY.timer protocol)
│   └── gc.go           # periodic GC task for terminated instances
```

---

## 4) Workflow definition types and load-time validation

### WF-DEF-1 — Go structs for workflow definition
- [ ] `WorkflowDefinition` struct covering all fields in spec §8.1
- [ ] `StateDefinition` (name, description, entry_actions, exit_actions, transitions)
- [ ] `TransitionDefinition` (event_match, guard, target_state, actions)
- [ ] `ActionDefinition` (type + typed union for each action type)
- [ ] `EventMatch` (msg, optionally type)
- [ ] JSON deserialization with strict unknown-field rejection

### WF-DEF-2 — Load-time validation (spec §8.5)
- [ ] Check 1: JSON structure valid per `wf_schema_version`
- [ ] Check 2: `input_schema` is valid JSON Schema (use `github.com/santhosh-tekuri/jsonschema` or equivalent)
- [ ] Check 3: `initial_state` exists in states
- [ ] Check 4: all `terminal_states` exist in states
- [ ] Check 5: every `target_state` in every transition exists in states
- [ ] Check 6: every guard CEL expression compiles against typed environment
- [ ] Check 7: every action has known type and valid params
- [ ] Check 8: `send_message` targets are syntactically valid L2 names
- [ ] Check 9: `schedule_timer` durations ≥ 60s
- [ ] Check 10: `set_variable` names are valid identifiers
- [ ] Return typed load error with path to offending element on any failure

---

## 5) CEL integration

### WF-CEL-1 — CEL environment
- [ ] Build `cel.Env` with three implicit variables:
  - `input` — typed from `input_schema` (map[string]any in v1, typed registry in future)
  - `state` — `map_type(string, dyn)` (dynamic)
  - `event` — `map_type(string, dyn)` (the incoming message envelope)
- [ ] Register built-in `now()` function returning current UTC unix milliseconds (sourced from Go `time.Now()` for runtime; injectable for tests)
- [ ] Compile all guards at load time; store compiled `cel.Program` per transition

### WF-CEL-2 — Guard evaluation
- [ ] Evaluate guard program with 10ms timeout (spec §9.3)
- [ ] On timeout: log warning, return `false` (transition not taken)
- [ ] On CEL evaluation error: log warning, return `false`
- [ ] On CEL result `true`: transition taken
- [ ] Unit tests: basic boolean guards, `now()` usage, timeout enforcement, missing field → false

---

## 6) SQLite persistence

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

### WF-STORE-5 — GC
- [ ] `DeleteTerminatedBefore(cutoffMs int64) (int, error)` — deletes instances + their log entries
- [ ] Called by periodic GC task (default: every hour, cleans entries older than 7 days)

### WF-STORE-6 — Timer index
- [ ] Table `wf_instance_timers`:
  ```sql
  CREATE TABLE wf_instance_timers (
      instance_id TEXT NOT NULL,
      timer_key   TEXT NOT NULL,
      timer_uuid  TEXT NOT NULL,
      PRIMARY KEY (instance_id, timer_key)
  );
  ```
- [ ] `RegisterTimer(instanceID, timerKey, timerUUID string) error`
- [ ] `GetTimerUUID(instanceID, timerKey string) (string, error)`
- [ ] `DeleteTimer(instanceID, timerKey string) error`
- [ ] `ListTimersForInstance(instanceID string) ([]TimerRow, error)` — used for cleanup on termination

---

## 7) Instance lifecycle

### WF-INST-1 — Instance creation (spec §14.3)
- [ ] Validate incoming payload against `input_schema`
- [ ] Generate `instance_id = "wfi:" + uuid`
- [ ] Insert into `wf_instances` (status=running, current_state=initial_state, state_json={})
- [ ] Acquire per-instance mutex
- [ ] Execute entry_actions of initial_state
- [ ] Persist updated state
- [ ] Release mutex
- [ ] On input schema validation failure: respond with `INVALID_INPUT`

### WF-INST-2 — Event dispatch to instance
- [ ] Resolve instance by `meta.thread_id` matching `instance_id` OR by `user_payload.instance_id` (for TIMER_FIRED)
- [ ] If no correlation: attempt to match payload against input_schema → create new instance
- [ ] Acquire per-instance mutex
- [ ] If status == 'cancelling': transition to 'cancelled' terminal state, run entry_actions of 'cancelled', cleanup
- [ ] Otherwise: evaluate transitions of current_state in declaration order (spec §9.2)
  - For each transition: check event_match on meta.msg (and meta.type if specified)
  - If matched: evaluate CEL guard
  - If guard true: execute actions, run exit_actions of current state, run entry_actions of target state, update state, persist
  - Stop on first matching transition
  - If no match: log "unhandled event", no state change
- [ ] Release mutex

### WF-INST-3 — State transition execution
Action execution order per spec §9.1:
```
[current state: exit_actions]
→ [transition: actions]
→ [target state: entry_actions]
```
- [ ] Each action failure is logged but does NOT halt the transition
- [ ] If target_state is terminal: run terminal cleanup (cancel all timers, set terminated_at, status = completed/cancelled/failed)

### WF-INST-4 — Soft abort
- [ ] On `WF_CANCEL_INSTANCE`:
  - Acquire instance mutex
  - If already terminal: respond with `INSTANCE_ALREADY_TERMINATED`
  - If already cancelling: respond with `INSTANCE_CANCELLING`
  - Set status = 'cancelling', log reason
  - Release mutex (do NOT transition now — happens on next event)
- [ ] When cancelling instance receives any event: immediately transition to 'cancelled' (entry_actions of 'cancelled' run, timers cleaned up)

---

## 8) Action vocabulary

### WF-ACT-1 — `send_message`
- [ ] Resolve payload substitution: `"state.foo"` → lookup in state_json, `"input.foo"` → lookup in input_json, `"event.payload.foo"` → lookup in current event. Unresolved paths: omit field.
- [ ] Build L2 message using fluxbee-go-sdk
- [ ] Set `meta.msg` from action definition
- [ ] Set `meta.type` from action (default: "system")
- [ ] Send via SDK connection. Fire and forget (do not wait for response).
- [ ] Log the action (action_type=send_message, summary="send_message to {target}", ok=true/false)

### WF-ACT-2 — `schedule_timer`
- [ ] Parse `fire_in` (duration string: "30m", "2h", "1d") OR `fire_at` (ISO 8601 UTC) — at least one required
- [ ] Validate `fire_in` ≥ 60s at load time (CHECK WF-DEF-2 check 9); also validate at execution time as safety net
- [ ] Build TIMER_SCHEDULE message to SY.timer:
  - `target_l2_name`: this node's L2 name
  - `payload`: `{ "instance_id": "...", "timer_key": "..." }`
  - `missed_policy` forwarded
- [ ] On TIMER_SCHEDULE_RESPONSE: persist `(instance_id, timer_key, timer_uuid)` in `wf_instance_timers`
- [ ] Note: `schedule_timer` is a send; the response arrives as a separate event in the normal receive loop. The runtime must handle TIMER_SCHEDULE_RESPONSE and write to the timer index — this is outside normal instance transition evaluation.

### WF-ACT-3 — `cancel_timer`
- [ ] Lookup `timer_uuid` from `wf_instance_timers` by (instance_id, timer_key)
- [ ] If not found: no-op, log "timer_key not found"
- [ ] Send TIMER_CANCEL to SY.timer with timer_uuid
- [ ] Delete row from `wf_instance_timers`

### WF-ACT-4 — `reschedule_timer`
- [ ] Lookup `timer_uuid` from `wf_instance_timers`
- [ ] If not found: no-op (timer may have already fired), log
- [ ] Send TIMER_RESCHEDULE to SY.timer

### WF-ACT-5 — `set_variable`
- [ ] Evaluate `value` as CEL expression against current (input, state, event) context
- [ ] Write result into instance state_json[name]
- [ ] Persist state_json immediately (part of the same atomic persist as the transition)

---

## 9) Inbound message dispatch

### WF-DISP-1 — Dispatch table
- [ ] `NODE_STATUS_GET` → respond with standard health response (ok, version, active_instances count)
- [ ] `WF_HELP` → respond with WF_HELP_RESPONSE (spec §15)
- [ ] `WF_CANCEL_INSTANCE` → WF-INST-4
- [ ] `WF_GET_INSTANCE` → load instance + recent log, return WF_GET_INSTANCE_RESPONSE
- [ ] `WF_LIST_INSTANCES` → list with filters, return WF_LIST_INSTANCES_RESPONSE
- [ ] `WF_GET_CODE` → return raw workflow definition JSON
- [ ] `TIMER_FIRED` → route by `user_payload.instance_id` (spec §11.2):
  - present + exists: deliver to instance as event
  - present + terminated: log orphaned timer, discard
  - absent: attempt to match as new trigger
- [ ] `TIMER_SCHEDULE_RESPONSE` → write timer_uuid to wf_instance_timers
- [ ] Anything else → attempt instance correlation, then new-instance creation

### WF-DISP-2 — Error responses
- [ ] All error responses: `{ "ok": false, "error": "<ERROR_CODE>", "detail": "..." }`
- [ ] Error codes as defined in spec §18

---

## 10) Restart recovery

### WF-RECOVER-1 — On node startup (spec §13.4)
- [ ] Open `wf_instances.db`
- [ ] Load all `wf_definitions` into memory, compile CEL environments
- [ ] Load all instances WHERE status IN ('running', 'cancelling') into in-memory map
- [ ] For each recovered instance: call `TIMER_LIST` on SY.timer filtered by `target_l2_name = this node`
  - Match returned timer_uuids to `wf_instance_timers` rows
  - For each expired timer with `missed_policy = "fire"`: inject a synthetic TIMER_FIRED event into that instance
  - For each timer not in the SY.timer response: remove from `wf_instance_timers` (was cancelled or expired-and-dropped)
- [ ] Enter receive loop

---

## 11) Node startup and config

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
- [ ] `node.Run()`: connect SDK → load workflow definition → compile CEL → open SQLite → recover running instances → reconcile timers → start GC goroutine → start receive loop

---

## 12) Tests

### WF-TEST-1 — Definition validation unit tests
- [ ] Valid workflow loads without error
- [ ] Missing initial_state → load error with path
- [ ] Bad target_state reference → load error
- [ ] Guard that fails CEL compile → load error
- [ ] schedule_timer with fire_in < 60s → load error
- [ ] send_message with invalid L2 name → load error

### WF-TEST-2 — CEL guard unit tests
- [ ] `event.payload.complete == true` evaluates correctly
- [ ] Missing field in state → false (not panic)
- [ ] `now()` returns current time
- [ ] Guard with infinite loop / timeout → returns false within 15ms

### WF-TEST-3 — Instance lifecycle unit tests (in-memory, no network)
- [ ] Create instance → initial_state entry_actions execute
- [ ] Event matching first transition → state changes, actions execute
- [ ] Event matching second transition (guard on first is false) → second taken
- [ ] No transition matches → state unchanged, event logged as unhandled
- [ ] WF_CANCEL_INSTANCE sets cancelling; next event → cancelled terminal
- [ ] Terminal state reached → timers cleaned up (mock timer client)
- [ ] Transition action failure (send fails) → transition still completes

### WF-TEST-4 — Store unit tests
- [ ] Insert + retrieve instance roundtrip
- [ ] Log cap: insert 105 entries → only 100 remain (oldest dropped)
- [ ] GC: delete instances older than cutoff, preserve newer
- [ ] Timer index CRUD

### WF-TEST-5 — Restart recovery unit test (in-memory SQLite + mock timer client)
- [ ] Persist running instance → simulate restart → instance recovered
- [ ] Timer with missed_policy=fire and past fire_at → synthetic TIMER_FIRED injected on recovery

---

## 13) Build and install integration

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

## 14) Reference workflow

### WF-REF-1 — Canonical example workflow (WF.invoice)
- [ ] Create `go/nodes/wf/examples/wf.invoice.json` — the full invoice workflow from spec §19
- [ ] Used as the reference for integration testing and as a starting point for operators

---

## Order of implementation

```
WF-SETUP-1 → WF-SETUP-2
  → WF-DEF-1 → WF-DEF-2
    → WF-STORE-1 → WF-STORE-2 → WF-STORE-3 → WF-STORE-4 → WF-STORE-5 → WF-STORE-6
    → WF-CEL-1 → WF-CEL-2
      → WF-INST-1 → WF-INST-2 → WF-INST-3 → WF-INST-4
        → WF-ACT-1 → WF-ACT-2 → WF-ACT-3 → WF-ACT-4 → WF-ACT-5
          → WF-DISP-1 → WF-DISP-2
            → WF-RECOVER-1
              → WF-CONFIG-1 → WF-CONFIG-2
                → WF-TEST-1 ... WF-TEST-5
                  → WF-BUILD-1 → WF-BUILD-2
                    → WF-REF-1
```

---

## Known frictions / notes

1. **`TIMER_SCHEDULE_RESPONSE` is async.** The `schedule_timer` action sends to SY.timer and continues; the timer_uuid arrives later as a separate incoming message. The dispatch table (WF-DISP-1) must handle `TIMER_SCHEDULE_RESPONSE` outside the normal instance transition path and write to the timer index. This is a non-obvious flow.

2. **Per-instance mutex with SQLite.** All state mutations for a given instance go through a per-instance `sync.Mutex` in memory. SQLite write contention is between instances (not within one instance) and SQLite's WAL mode handles it. No global lock needed.

3. **Payload substitution in `send_message` is not CEL.** It is a simple path lookup (`"state.foo"` → state["foo"]). This is different from guard evaluation and `set_variable` which are full CEL. Keep the two paths clearly separated in `actions.go`.

4. **`now()` in CEL is side-effectful.** For deterministic testing, inject a clock function via the environment builder. The default production clock is `time.Now().UnixMilli()`.

5. **No new-instance creation for arbitrary messages.** Only trigger-shaped messages (no instance_id correlation, payload matches input_schema) create new instances. All others that fail correlation return `INSTANCE_NOT_FOUND`.
