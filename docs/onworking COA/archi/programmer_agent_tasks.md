# Programmer Agent — Implementation Tasks

**Status:** phases 1–5 complete — ready for E2E testing  
**Date:** 2026-04-21  
**Audience:** `SY.architect` developers  
**Parent spec:** `docs/future_implementations/solution-manifest-spec_1.md`

---

## Goal

Add a `fluxbee_programmer` tool to `SY.architect` that translates a natural language task description into a validated `executor_plan`, using the full live admin action catalog as its available toolbox. Archi calls it as a sub-AI (like `fluxbee_infrastructure_specialist`), receives the plan in memory, presents a human-readable summary to the user for confirmation, and executes on approval.

The programmer maintains a cookbook (JSON file) of successful patterns. It reads the cookbook at invocation time and writes a new entry only when the plan executes without error.

---

## Architecture Summary

```
humano → Archi → fluxbee_programmer (tool call)
                     ├── reads admin_actions_cache from ArchitectState
                     │   (refreshes via list_admin_actions ADMIN_COMMAND if stale)
                     ├── reads cookbook from filesystem (if exists)
                     ├── OpenAI call with function calling
                     │   └── submit_executor_plan(plan, human_summary, cookbook_entry?)
                     └── returns plan + human_summary + optional cookbook_entry

Archi → shows human_summary to user → waits for CONFIRM
Archi → execute_executor_plan_with_context
       └── ok  → writes cookbook_entry to filesystem
       └── err → discards cookbook_entry candidate
```

---

## Phase 1 — Admin Actions Cache

- [x] `PROG-T1` Add `admin_actions_cache` field to `ArchitectState`:
  - `actions: Vec<serde_json::Value>` — raw action descriptors from `list_admin_actions`
  - `fetched_at_ms: u64` — epoch ms of last fetch
  - TTL: 5 minutes (300_000 ms)
  - Wrapped in `Arc<Mutex<Option<AdminActionsCache>>>`

- [x] `PROG-T2` Implement `get_or_refresh_admin_actions(context) -> Result<Vec<Value>>`:
  - If cache is present and age < TTL: return cached value
  - Otherwise: send `list_admin_actions` via ADMIN_COMMAND socket, parse response, update cache
  - On fetch error: return cached value if available, else propagate error

- [ ] `PROG-T3` Unit test: cache hit skips HTTP call; stale cache triggers refresh.

---

## Phase 2 — Cookbook

- [x] `PROG-T4` Define `ProgrammerCookbookEntry` struct:
  ```rust
  struct ProgrammerCookbookEntry {
      task_pattern: String,
      trigger: String,
      steps_pattern: Vec<String>,
      notes: String,
      recorded_at_ms: u64,
  }
  ```

- [x] `PROG-T5` Implement `read_programmer_cookbook() -> Vec<ProgrammerCookbookEntry>`:
  - path: `BlobConfig::default().blob_root / cookbook/programmer-v1.json`
  - Returns empty vec if file not found (first run)
  - Silently ignores parse errors (never block on bad cookbook)

- [x] `PROG-T6` Implement `append_programmer_cookbook_entry(entry)`:
  - Read current cookbook
  - Append new entry
  - Keep last 50 entries (drop oldest if over limit)
  - Write back via `std::fs::write`

- [ ] `PROG-T7` Unit test: read empty → append → read back gets the entry.

**Storage note:** The cookbook is a plain JSON file at `{blob_root}/cookbook/programmer-v1.json`. It uses `BlobConfig::default().blob_root` (the same root as all blob data) as its base directory but bypasses the BlobToolkit staging/ref system — it doesn't need a BlobRef because it's a long-lived internal file, not a message attachment. Direct `std::fs` read/write is intentional here.

---

## Phase 3 — Programmer System Prompt

- [x] `PROG-T8` Write `PROGRAMMER_SYSTEM_PROMPT_BASE` constant:
  - Role, executor_plan format, step ordering rules, no-invent rule
  - Must call `submit_executor_plan` — no free text output
  - Cookbook context usage instructions

- [x] `PROG-T9` Define `submit_executor_plan` function schema:
  - `plan`: full executor_plan object
  - `human_summary`: plain language paragraph, shown verbatim to operator
  - `cookbook_entry`: optional — only if pattern is genuinely reusable

- [x] `PROG-T10` Build dynamic system prompt at invocation time via `build_programmer_prompt(actions, cookbook)`:
  - Starts with `PROGRAMMER_SYSTEM_PROMPT_BASE`
  - Appends toolbox section: each admin action with name, description, args
  - Appends cookbook section: last N entries as examples

---

## Phase 4 — ProgrammerTool Implementation

- [x] `PROG-T11` Implement `ArchitectProgrammerTool` as `FunctionTool`:
  - Tool name: `fluxbee_programmer`
  - Input args: `task` (string), `hive` (string), `context` (optional string)
  - Stores `ProgrammerPendingPlan` in `ArchitectState.programmer_pending` keyed by session_id

- [x] `PROG-T12` Implement `run_programmer_with_context(context, task, hive, user_context)`:
  1. `get_or_refresh_admin_actions` for current toolbox
  2. `read_programmer_cookbook` for examples
  3. `build_programmer_prompt`
  4. OpenAI call with `ProgrammerSubmitTool` as only registered function (force_call)
  5. Extract plan from `FunctionLoopItem::ToolResult` for `submit_executor_plan`
  6. Validate via `validate_architect_executor_plan_shape`
  7. Return `ProgrammerOutput { plan, human_summary, cookbook_entry }`

- [x] `PROG-T13` Register `ArchitectProgrammerTool` in the Archi tool registry (alongside infra specialist).

---

## Phase 5 — Archi Integration

- [x] `PROG-T14` Updated Archi system prompt:
  - When to call `fluxbee_programmer` (deploy / spawn / route / configure intent)
  - When NOT to call it (questions, status checks, exploration)
  - After result: show `human_summary`, ask for confirmation — never show raw JSON

- [x] `PROG-T15` CONFIRM handler in `handle_chat_message`:
  - Checks `programmer_pending` BEFORE the SCMD pending flow
  - On CONFIRM: executes via `execute_executor_plan_with_context`
  - On success: calls `append_programmer_cookbook_entry` (sync)
  - On error: returns error, no cookbook write

- [x] `PROG-T16` CANCEL handler:
  - Checks `programmer_pending` before SCMD cancel flow
  - Clears the pending plan and returns discard confirmation

---

## Phase 6 — Test

- [ ] `PROG-T17` Manual E2E test — simple case (runtimes already published):
  - Input: "Quiero un nodo WF que reciba la salida de AI.support y la routee a IO.slack y IO.email en worker-220. Los runtimes están publicados."
  - Expected plan: run_node WF + run_node IO.slack + run_node IO.email + add_route×3
  - Expected flow: Archi shows summary → user confirms → plan executes → cookbook entry written

- [ ] `PROG-T18` Manual E2E test — full case (runtimes not published):
  - Expected plan: publish_runtime_package×N + run_node×3 + add_route×3
  - Verify ordering: all publish steps before run_node steps

- [ ] `PROG-T19` Error case: programmer returns plan with unknown action name
  - `validate_architect_executor_plan_shape` must catch it before execution
  - Archi reports validation error, no cookbook write

- [ ] `PROG-T20` Cookbook accumulation: run PROG-T17 twice
  - After second run: cookbook has 2 entries
  - Third invocation: cookbook context appears in programmer prompt

---

## Phase 7 — Refinements (post first test)

- [ ] `PROG-T21` Tune programmer prompt based on observed plan quality from PROG-T17/T18
- [ ] `PROG-T22` Add cookbook relevance filter: only inject entries matching task_pattern similarity (simple keyword match)
- [ ] `PROG-T23` Tighten step args schema per action type (v2 — after basic flow works)

---

## Phase 8 — Agent Handshake Visibility (2026-04-23)

- [x] `PROG-T24` Add `ProgrammerTrace` / `ProgrammerTraceStep` structs to capture task, hive, steps, validation status, and first_validation_error.
- [x] `PROG-T25` Extend `ProgrammerPendingPlan` with `trace: Option<ProgrammerTrace>`.
- [x] `PROG-T26` In `ArchitectProgrammerTool::run()`: build `ProgrammerTrace` from final plan steps and validation outcome; store in pending; enrich tool result JSON with `plan_steps`, `validation`, `first_validation_error`.
- [x] `PROG-T27` In `handle_run_ai_chat()`: after AI run completes, if `programmer_pending` was set this turn, inject `programmer_trace` into the chat output JSON.
- [x] `PROG-T28` Frontend CSS: add `.agent-trace*` classes for collapsible handshake panel.
- [x] `PROG-T29` Frontend JS: `renderProgrammerTrace(trace)` — renders programmer call, plan steps (id / action / args_preview), pre-validation result, retry error if applicable.
- [x] `PROG-T30` Frontend: `renderResponsePayload()` calls `renderProgrammerTrace()` when `output.programmer_trace` is present, rendering it between the Archi message and the tool_results section.

**Visible result:** when Archi calls `fluxbee_programmer`, the chat shows a collapsible "Agent activity · programmer → N steps" panel with: task sent, each step (id / action / args), validation status (ok / retried). If first plan was rejected, the rejection error is shown inline.

---

## Open / Deferred

- **Auditor agent**: deferred. See `solution-manifest-spec_1.md` section 6.
- **Full reconciliation** (diff manifest vs actual state): deferred.
- **`fluxbee_infrastructure_specialist` action cache**: still static. Migrate to same TTL cache pattern as PROG-T1/T2 in a future pass.
- **Unit tests PROG-T3, T7**: written as manual for now; automate when test harness for ADMIN_COMMAND socket calls is available.
- **Executor trace**: the post-CONFIRM executor steps already render via `renderExecutorResult`. Consider adding a similar "executor activity" toggle that persists with the pre-CONFIRM programmer trace in the same bubble.
