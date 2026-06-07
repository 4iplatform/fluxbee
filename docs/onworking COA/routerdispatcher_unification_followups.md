# RouterDispatcher unification — follow-ups

Date: 2026-06-06
Status: Open tracker
Parent: [routerdispatcher_unification_plan.md](routerdispatcher_unification_plan.md) (Closed 2026-06-06)
Architect parent: [sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md](sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md) (Closed 2026-06-06)

## Why this doc

The unification plan and the architect refactor were both closed on 2026-06-06: all 8 inline dispatchers gone, all 9 Vault sites on `VaultClient` over the canonical `Arc<RouterDispatcher>`, `connect()` private, 6 CI guards strict-clean, workspace + SDK tests verde.

What lives here is the **leftover work that is real, was identified honestly, but is explicitly out of scope of the unification PR**. Either it's testing/audit work that complements the live path, or it's a deeper divergence (Go SDK vs Rust SDK) that nobody is hitting today and will need its own scoped change when load demands it.

This file is the single source of truth for "things to do next on the dispatcher surface". Open the items below in priority order; close them by editing this file and ticking the box.

## P0 — Test gaps for invariants we already rely on

### H4. End-to-end regression test for `VAULT_SECRET_CHANGED` hot-refresh

- [ ] **H4.1** Architect: integration test that broadcasts `VAULT_SECRET_CHANGED { resource_type: "openai" }`, then issues a follow-up RPC that exercises the refreshed `ArchitectAiRuntime`, and asserts the new OpenAI key materialized via `state.rpc` (not via the deleted ephemeral helper).
- [ ] **H4.2** Architect: same test for `resource_type: "postgres"` exercising `refresh_architect_messages_db_url` and observing the new `messages_db_url` materialized in `ArchitectState`.
- [ ] **H4.3** Admin: same test for `refresh_admin_executor_ai_runtime` via `AdminContext.rpc`.
- [ ] **H4.4** Negative path: broadcast with a `resource_type` outside the architect's `VaultSecretInterest`, assert NO refresh happens (current code path already filters; add the assert).

**Why P0:** the audit on 2026-06-06 found that an earlier bug had been making every `VAULT_SECRET_CHANGED` reach the architect get rejected with FORBIDDEN. The bug was fixed but it slipped past human review and past the SDK tests because there is no end-to-end test that asserts the broadcast → re-resolve → cached-runtime-replaced chain. If this regresses again we will not notice until a customer rotates a key.

### H5. Admin inbound origin-authorization audit (parity with architect Section E)

- [ ] **H5.1** Enumerate SY.admin's inbound SYSTEM_KIND handlers (start at `handle_admin_system_message` or equivalent; the architect refactor introduced the pattern of catching protected actions before `try_handle_default_node_status`).
- [ ] **H5.2** For each handler, decide: is this action open to anyone in the hive, or does it have an implicit "only orchestrator / only architect / only config-routes" assumption that is enforced today only by convention?
- [ ] **H5.3** For each protected action, port the architect's pattern from [src/bin/sy_architect.rs:6664-6715](src/bin/sy_architect.rs#L6664-L6715): an allowlist constant, `*_authorized()` predicate, and `build_admin_forbidden_response()` helper that returns the canonical `*_RESPONSE` with `error_code: "FORBIDDEN"`.
- [ ] **H5.4** Verify NO broadcast event (anything emitted with `src_l2_name: None` or `Destination::Broadcast`) ends up in the protected-action set — same trap that Bug #1 in the audit was. Add a comment near the allowlist explaining why broadcasts must stay out.
- [ ] **H5.5** Update the relevant CI guard (`architect_no_ephemeral_guard.sh` does NOT cover admin's protected actions today; either extend it or add a new `admin_origin_auth_guard.sh`).

**Why P0:** before this plan, admin and architect both had the same exposure: anyone in the hive could send protected SYSTEM messages. We closed it for architect. Admin still has it.

## P1 — Go SDK divergences from Rust SDK

The Go dispatcher was built as a mirror of Rust but with two corner cases that matter under load. Diagnostic and SY-side traffic don't hit them today; nothing is broken **right now**.

### GO-1. Late-response classification (`Stale` / `UnknownResponse`)

- [ ] **GO-1.1** In [go/fluxbee-go-sdk/dispatcher.go:667-714](go/fluxbee-go-sdk/dispatcher.go#L667-L714), the `deliver()` switch handles only `outcomeSuccess`, `outcomeTerminalError`, `outcomeInvalidResponse`, and `outcomeUnrelated`. Rust handles 6 actions: the missing two are `Stale` (recently-completed trace_id; drop with metric) and `UnknownResponse` (response-only shape with no matching trace; drop with metric).
- [ ] **GO-1.2** Without those, a response that arrives **after** the caller's timeout falls through to `postPendingRules` and gets routed to whatever channel `RouteAny` points at. For `sy-opa-rules` / `wf-generic` (both use `RouteAny → "incoming"`), that means stale RPC responses appear in the main loop's inbox as if they were operational messages. The main loop drops them but does not report them, so we lose a signal that timeouts are happening.
- [ ] **GO-1.3** Port the Rust `stale` and `response_only` registries to Go (`d.stale`, `d.responseOnly`, both keyed by trace_id with a TTL eviction or bounded LRU). Add counters: `metricStaleResponses`, `metricUnknownResponses`.
- [ ] **GO-1.4** Expose the counters via `IsConnected`-style getters so the SY.opa / wf-generic / sy-timer processes can surface them in NODE_STATUS responses.
- [ ] **GO-1.5** Test: send an RPC with a 1ms timeout, sleep 100ms, have the test harness reply on the same trace_id — expect drop with `metricStaleResponses == 1`, no delivery to any subscribed channel.

**Why P1:** there is no production hot-path that wedges on this today. The risk is silent loss of operability: an operator reading SY.opa-rules's diagnostics will not know that 2% of its RPCs are timing out because the late responses look like legitimate routed messages.

### GO-2. Bounded command channels with silent drop

- [ ] **GO-2.1** In [go/fluxbee-go-sdk/dispatcher.go:401](go/fluxbee-go-sdk/dispatcher.go#L401) (and similarly for broadcasts at line 439), command channels are created with `make(chan Message, 64)`. In `routeToTarget` ([dispatcher.go:735-738](go/fluxbee-go-sdk/dispatcher.go#L735-L738)), the send uses `select { case ch <- msg: default: }` — if the channel is full, the message is **silently dropped**.
- [ ] **GO-2.2** Rust uses `mpsc::unbounded_channel`, and tracks depth with a `RPC_COMMAND_DEPTH_WARN_THRESHOLD` to log when consumers are falling behind. Replicate one of two strategies in Go:
  - **Strategy A (preferred)**: keep bounded but add a per-channel `metricCommandChannelDrops` counter; log a warning on first drop and every Nth drop. Operators see drops, can decide to raise the bound or fix the consumer.
  - **Strategy B**: switch to an unbounded ring (e.g. a `container/list`-backed queue behind a `sync.Mutex` + `sync.Cond`) — closer to Rust semantics but more code.
- [ ] **GO-2.3** Audit current Go consumers (`sy-wf-rules`, `sy-opa-rules`, `wf-generic`, `sy-timer`) and decide per-channel: what is the realistic peak burst? If any is plausibly >64, raise the bound for that channel before shipping the strategy.
- [ ] **GO-2.4** Test: produce 1000 messages to a single command channel without a consumer; assert that either all 1000 are buffered (Strategy B) or that exactly the expected drop count is reported via the new metric (Strategy A).

**Why P1:** the silent-drop semantics matter when a consumer momentarily falls behind. With 64-deep queues and `default:` discard, a burst of 65 admin commands while the handler is doing a slow operation just loses the 65th, no log, no metric. None of the current SY services has been observed to burst >64 messages at a single channel; this becomes relevant the moment we add a busier producer.

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
