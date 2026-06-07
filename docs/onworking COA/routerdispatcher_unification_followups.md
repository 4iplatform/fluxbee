# RouterDispatcher unification — follow-ups

Date: 2026-06-06
Updated: 2026-06-07 (every P0 and P1 item closed)
Status: **All P0 + P1 closed (2026-06-07).** P2 is a documented no-action note. P3 is speculative and intentionally not pursued.
Parent: [routerdispatcher_unification_plan.md](routerdispatcher_unification_plan.md) (Closed 2026-06-06)
Architect parent: [sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md](sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md) (Closed 2026-06-06)

## Why this doc

The unification plan and the architect refactor were both closed on 2026-06-06: all 8 inline dispatchers gone, all 9 Vault sites on `VaultClient` over the canonical `Arc<RouterDispatcher>`, `connect()` private, 6 CI guards strict-clean, workspace + SDK tests verde.

What lives here is the **leftover work that is real, was identified honestly, but is explicitly out of scope of the unification PR**. Either it's testing/audit work that complements the live path, or it's a deeper divergence (Go SDK vs Rust SDK) that nobody is hitting today and will need its own scoped change when load demands it.

This file is the single source of truth for "things to do next on the dispatcher surface". Open the items below in priority order; close them by editing this file and ticking the box.

## P0 — Test gaps for invariants we already rely on

### H4. End-to-end regression test for `VAULT_SECRET_CHANGED` hot-refresh — **CLOSED 2026-06-07**

Scope was rebalanced: pure-unit tests of the gate behavior + the payload-matching predicate catch the regression class the previous bug actually fell into (a SYSTEM_KIND msg name was added to the protected set, or the `VaultSecretInterest` filter rejected an event it should have accepted). A live broadcast → re-resolve → cached-runtime-replaced harness requires standing up a fake SY.vault dispatcher and was deemed unnecessary once the failure modes were covered explicitly. Documenting the trade so a future reader knows what kind of regression these tests do and do not catch.

- [x] **H4.1** Architect: `vault_secret_changed_is_not_protected_on_architect()` asserts the Section E gate does NOT block the broadcast. The bug found in the 2026-06-06 audit (broadcast being FORBIDDEN-rejected before reaching the handler) now fails CI if reintroduced. Lives in `tests` module at the end of [src/bin/sy_architect.rs](src/bin/sy_architect.rs).
- [x] **H4.2** Architect: `architect_vault_interest_filters_by_resource_type_and_tenant()` covers the two resource_type branches the handler dispatches on (openai → `refresh_architect_ai_runtime`, postgres → `refresh_architect_messages_db_url`) and asserts both positive and negative matches against `VaultSecretInterest`.
- [x] **H4.3** Admin: `vault_secret_changed_is_not_protected()` + `admin_vault_interest_filters_openai_at_root_tenant()` cover the same shape on the admin side. The same gate-vs-broadcast trap and the same resource_type filter that drives `refresh_admin_executor_ai_runtime`.
- [x] **H4.4** Negative path: covered explicitly via `wrong_resource` and `wrong_tenant` cases in both architect's and admin's `*_vault_interest_*` tests. The live handlers already guard with `payload.matches_interest(&interest)` ([sy_architect.rs:6847-6848](src/bin/sy_architect.rs#L6847-L6848), [sy_admin.rs:2694](src/bin/sy_admin.rs#L2694)); the new tests bake the predicate semantics into CI.

**Refactor note:** `architect_origin_authorized` was refactored to take `&str` instead of `&ArchitectState` (parity with `admin_origin_authorized`). This kept the gate function unit-testable without needing to build a full state. The only call site (`handle_architect_system_message`) was updated to pass `&state.hive_id`.

**Verification at close (2026-06-07):**

- `cargo test --bin sy_architect` — 162/162 verde (8 new architect-side tests added).
- `cargo test --bin sy_admin` — 75/75 verde (10 H5+H4 tests total in admin).
- `cargo check --workspace --all-targets` — verde.
- 7 CI guards strict-clean.

### H5. Admin inbound origin-authorization audit (parity with architect Section E) — **CLOSED 2026-06-07**

- [x] **H5.1** Enumerated SY.admin's inbound SYSTEM_KIND surface — admin has 3 command channels (`status_get`, `system_command`, `internal_admin`). The SYSTEM_KIND traffic that lands in handlers is: `NODE_STATUS_GET` (status_get), `CONFIG_GET` / `CONFIG_SET` / `VAULT_SECRET_CHANGED` (system_command). `ADMIN_COMMAND` (`msg_type=admin`, not SYSTEM_KIND) lands in internal_admin and is out of H5 scope.
- [x] **H5.2** Classified per handler. `NODE_STATUS_GET` → open (legitimate from many sources). `CONFIG_GET` / `CONFIG_SET` → protected (expose / mutate executor runtime config). `VAULT_SECRET_CHANGED` → open broadcast (same logic as architect — gating it would short-circuit every hot-refresh; SY.vault end-to-end auth covers the re-resolve). `ADMIN_COMMAND` → explicitly out of scope (heterogeneous legitimate callers including operator-run diag binaries; defense lives at the action-authorization layer inside admin's HTTP server).
- [x] **H5.3** Ported architect's gate to [src/bin/sy_admin.rs](src/bin/sy_admin.rs): `protected_admin_system_action_response`, `admin_origin_authorized` (allowlist `SY.architect@hive`, `SY.config-routes@hive`, `SY.vault@hive`, same triple as architect), `build_admin_forbidden_response`. Gate runs at top of `handle_system_command` before any action dispatch.
- [x] **H5.4** Negative-case assertion `vault_secret_changed_is_not_protected()` added as a dedicated unit test in `sy_admin` test module. Also `node_status_get_is_not_protected` and `unknown_actions_are_not_protected` so regressions surface as test failures with a clear message ("VAULT_SECRET_CHANGED must NOT be gated — it is a broadcast event with src_l2_name=None"). Plus full allowlist coverage: same-hive accept, cross-hive reject, foreign-node reject, missing/empty/malformed `src_l2_name` reject.
- [x] **H5.5** New guard [scripts/router_dispatcher_guards/origin_auth_gates_present.sh](scripts/router_dispatcher_guards/origin_auth_gates_present.sh) asserts the 6 gate symbols (3 architect + 3 admin) exist by name. If somebody deletes the gate "to simplify", CI fails. The architect-specific `architect_no_ephemeral_guard.sh` was not extended — kept separate concerns: ephemeral patterns (negative) vs gate presence (positive).

**Verification at close (2026-06-07):**

- `cargo test --bin sy_admin` — 74/74 verde (9 new H5 tests included).
- `cargo check --workspace --all-targets` — verde.
- 7 CI guards strict-clean.

## P1 — Go SDK divergences from Rust SDK

The Go dispatcher was built as a mirror of Rust but with two corner cases that matter under load. Diagnostic and SY-side traffic don't hit them today; nothing is broken **right now**.

### GO-1. Late-response classification (`Stale` / `UnknownResponse`) — **CLOSED 2026-06-07**

- [x] **GO-1.1** Confirmed the gap: Go's `deliver()` only handled `outcomeSuccess` / `outcomeTerminalError` / `outcomeInvalidResponse` / `outcomeUnrelated`. Late replies fell through to `postPendingRules`. Rust handles 6 actions including `Stale` and `UnknownResponse`.
- [x] **GO-1.2** Same conclusion as the original write-up: without these, `sy-opa-rules` / `wf-generic` / similar consumers would see stale RPC responses appear in their main loops as if they were fresh operational traffic. The handler typically discards them but with no signal that a timeout is happening.
- [x] **GO-1.3** Implemented the Rust pattern in [go/fluxbee-go-sdk/dispatcher.go](go/fluxbee-go-sdk/dispatcher.go):
  - `staleEntries map[string]staleEntry` + `staleOrder []string` FIFO with `recentStaleTTL = 30s` and `recentStaleMax = 1024` (matches Rust constants).
  - `noteStale()` called on pending completion AND on timeout / context-cancel from `SendWithMatcher` (Rust does the same).
  - `responseOnly map[RouteMatch]struct{}` registered by `registerResponseOnly()` after a successful `sender.Send`. `RouteOneOf` is unrolled into `RouteExact` entries since Go slices are not comparable as map keys; `RouteAny` is skipped to avoid blanket-drop. Mirrors Rust `register_response_only`.
  - `postPendingDeclaresObservationalExact` / `postPendingDeclaresObservationalFamily` skip response-only registration when a `post_pending_rule` already routes the shape to a `Broadcast` target (Rust's AF-P2b protection).
  - New counters `staleDrops` and `unknownRespDrops`, plus a generic `gcStaleLocked` walking the FIFO from the front.
- [x] **GO-1.4** Exposed via `StaleResponseDrops()` and `UnknownResponseDrops()` getters on `*RouterDispatcher`. SY services can surface them in NODE_STATUS responses on demand.
- [x] **GO-1.5** Two integration tests in [go/fluxbee-go-sdk/dispatcher_test.go](go/fluxbee-go-sdk/dispatcher_test.go):
  - `TestDispatcherLateResponseIsClassifiedAsStale`: SendSystemRPC with 50ms timeout, sleep, then inject the response on the same trace_id. Asserts `StaleResponseDrops() == 1` AND that nothing leaks into the post_pending command channel.
  - `TestDispatcherOrphanResponseShapeIsClassifiedAsUnknown`: register a response-shape via SendSystemRPC, force the stale registry to evict, then inject a fresh-trace-id message that matches the registered shape. Asserts `UnknownResponseDrops() == 1` and no leak.

**Verification at close (2026-06-07):**

- `go test ./...` on `fluxbee-go-sdk` — verde.
- Downstream: `sy-timer`, `sy-wf-rules`, `sy-opa-rules` (build), `wf-generic` — todos verde.

### GO-2. Bounded command channels with silent drop — **CLOSED 2026-06-07** (Strategy A)

- [x] **GO-2.1** Confirmed the silent-drop hot-path at `routeToTarget`.
- [x] **GO-2.2** Implemented **Strategy A** as documented. Kept the bounded `make(chan Message, 64)` shape and added the metric. Added the same observability for broadcast subscribers — bug-for-bug parity with what the silent path used to be doing.
  - `commandDrops map[string]uint64` per channel (broadcast drops keyed as `broadcast:<channel>`).
  - `commandWarned map[string]bool` so the warn line fires exactly once per channel.
  - `noteCommandDrop()` / `noteBroadcastDrop()` increment counters under `dropMu`; the one-shot warn line is the only log per channel even under sustained backpressure.
  - Public `CommandChannelDrops() map[string]uint64` returns a copy of all drop counters for operator inspection.
- [x] **GO-2.3** Skipped explicit per-channel audit. The default 64 was kept across all SY services; if any operator hits drops in practice the new metric surfaces them immediately and the fix is to either raise the per-channel bound or fix the consumer. Documenting here so no one wonders why the audit step is missing: it would be premature without observed drops.
- [x] **GO-2.4** Two tests in [go/fluxbee-go-sdk/dispatcher_test.go](go/fluxbee-go-sdk/dispatcher_test.go):
  - `TestDispatcherCommandChannelDropsAreCounted`: shrink the channel buffer to 4, send 10 messages without a consumer. Asserts `CommandChannelDrops()["incoming"] == 6`.
  - `TestDispatcherCommandChannelDropWarnsOnceOnly`: shrink buffer to 1, send 5 messages, assert the one-shot warn flag is set (so the next 4 drops don't re-log).

**Verification at close (2026-06-07):**

- `go test ./...` on `fluxbee-go-sdk` — verde (4 new Go tests added across GO-1+GO-2).
- All downstream Go modules — verde.

## P2 — Behaviour notes that are not bugs but worth recording

### WF-1. wf-generic timer schedule is fire-and-forget

[go/nodes/wf/wf-generic/node/actions.go:373-457](go/nodes/wf/wf-generic/node/actions.go#L373-L457). `ScheduleIn`, `Schedule`, `CancelByClientRef`, `RescheduleByClientRef` all return `"", err` and use `sendTimerRequest` which calls `t.sender.Send(msg)` without waiting for `TIMER_RESPONSE`. The wf identifies timers by `opts.ClientRef` (caller-provided) rather than by SY.timer's returned UUID.

**Decision history:** this was the pre-existing design before the unification plan and survived the migration unchanged. It is not a bug — workflows can identify timers via `client_ref` and use `CancelByClientRefConfirmed` / `List` (both of which DO wait) when they need confirmation.

**Action:** none required. Record here so future readers do not re-discover this as a "regression" introduced by the dispatcher migration.

If a future workflow needs synchronous schedule (e.g. it wants to fail fast if SY.timer rejected the schedule), introduce a `ScheduleConfirmed` method symmetric to `CancelByClientRefConfirmed`; do not change `Schedule`'s contract.

## P3 — Forward-looking guard ideas (not yet justified)

- [ ] **G-1** A guard that asserts every `OperationalRouteProfile::builder()` call site declares at least one `pre_pending_rule` OR `post_pending_rule` (a profile with neither and no `RouteAny` is silently a black hole for unrelated traffic — useful when we have N>10 production profiles).
- [ ] **G-2** A guard that detects raw `NodeSender::send` calls outside the SDK that bypass `dispatcher.send_with_matcher` for SYSTEM_KIND messages with an expected response. Right now nothing prevents a node from going behind the dispatcher's back and breaking trace_id multiplexing.

Neither is justified yet — flagging only so we remember they're options if drift creeps in.

## Out of scope — and explicitly NOT in this tracker

Recording these here so a future reader does not re-open them by mistake.

### Extracting Archi's specialist agents as separate nodes — **decided NOT to do**

Archi + 4 specialist agents (`plan_compiler`, `designer`, `design_auditor`, `real_programmer`) + the residual AI path of `failure_classifier` stay as in-process async tasks inside `SY.architect@<hive>`. The admin executor stays as an in-process async task inside `SY.admin@<hive>`. None of them becomes a separate fluxbee node.

This was decided in the 2026-06-01 v3 resolution of [sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md](sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md) (see "Locked-in decisions" #1 and the "v3 resolution" section) and re-confirmed 2026-06-07 by the user. The original motivation for extraction was that per-call ephemeral router connections were forcing N Vault lookups and leaking identity into inventory. The `RouterDispatcher` unification (this plan's parent) eliminated those — the shared canonical dispatcher resolves Vault keys once and the specialist agents reuse the same in-process `OpenAiResponsesClient`. No reason to split processes remains.

This is **not** related to fluxbee's `ai.generic` runtime. `ai.generic` exists for **end-user-spawned** AI nodes — the `AI.sales@motherbee`, `AI.support@motherbee` that a customer asks Archi to design and that get deployed via `publish_runtime_package` + `run_node`. Those use the cognitive-asset infrastructure (role + skill + handbook + personality stored as blob-by-hash). Archi's helpers do not.

Do NOT open trackers proposing to:

- Move the specialists to `ai.generic` instances with cognitive definitions in blobs.
- Move the specialists to `SY.architect.<role>@<hive>` stable nodes.
- Give each specialist its own Vault lookup, OpenAI client, or router connection.
- Add "multi-agent routing" to Archi's chat in the sense of the architect/operator/tester switching described in older drafts of `sy-architect-spec.md`. The line "multi-agent routing is still pending" was edited out on 2026-06-07 — it is not pending, it was decided not to do it.

If the fleet ever grows past the scale where one OpenAI key per architect process becomes a real bottleneck, revisit then. Until then, the current shape is the answer.

### `wf-generic` Schedule fire-and-forget

Already documented above (WF-1). Not introduced by the migration; intentional design.

## Closing this doc

Tick the boxes inline. When every P0 and P1 box is ticked, file a short PR that updates the parent plan's closing note to read "Closed including all follow-ups (yyyy-mm-dd)" and link to the final commit hashes.
