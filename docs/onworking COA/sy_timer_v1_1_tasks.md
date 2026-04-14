# SY.timer v1.1 — Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-04-14
**Type:** extension of existing SY.timer v1.0 (already implemented and in production)
**Primary spec:** `docs/sy-timer.md` (existing) — to be updated to v1.1
**Target module:** `go/sy-timer/` (existing)
**Required by:** WF v1 (`wf_v1_tasks.md`)

---

## 1) Goal

Extend the existing SY.timer v1.0 implementation with **client_ref-based lookup** for `TIMER_CANCEL`, `TIMER_RESCHEDULE`, and `TIMER_GET` operations.

This is a small, additive extension. The v1.0 API remains unchanged; v1.1 adds an alternative key for these three operations. Existing clients of v1.0 are unaffected.

The motivation is to enable WF v1 to operate timers without needing to wait for the asynchronous `TIMER_SCHEDULE_RESPONSE` to obtain the timer's UUID. With v1.1, a client can construct a deterministic `client_ref` at scheduling time and use it immediately for any subsequent operation.

---

## 2) Background: why this is needed

In SY.timer v1.0, `client_ref` is an optional metadata field provided by the client at schedule time, intended as a "friendly name" for human/debug purposes. It is stored in the database but is not used as a lookup key. All operations on a timer require the SY.timer-generated `timer_uuid`, which is only available after `TIMER_SCHEDULE_RESPONSE` arrives.

This creates a problem for asynchronous clients like WF, which want to schedule a timer and then potentially cancel or reschedule it before the response has been processed. The WF runtime cannot block waiting for the response (it would freeze the event loop), and tracking pending UUIDs locally adds significant complexity and a race window.

The fix is to make `client_ref` a usable lookup key, alongside `timer_uuid`. The client supplies a `client_ref` that is guaranteed unique within the scope of `(owner_l2_name, client_ref)`, and any subsequent operation can use either the UUID or the client_ref to identify the timer.

For full context, see `wf_v1_tasks.md` Resolved 1 and `docs/wf-v1.md` §10.2.

---

## 3) Frozen decisions

- **Backward compatibility:** v1.1 is fully backward compatible with v1.0. All existing clients continue to work unchanged. The new behavior is opt-in by passing `client_ref` instead of `timer_uuid`.
- **Uniqueness scope:** `client_ref` is unique per `(owner_l2_name, client_ref)` tuple. Two different owners can use the same `client_ref` independently. The combination of owner + client_ref is enforced at insert time.
- **Lookup priority:** when both `timer_uuid` and `client_ref` are provided in a request, `timer_uuid` wins (consistent with v1.0 semantics). When only `client_ref` is provided, lookup uses `(owner_l2_name from routing.src_l2_name, client_ref)`.
- **Idempotent schedule:** if a `TIMER_SCHEDULE` arrives with a `client_ref` that already exists for the requesting owner, the existing timer is returned (with its existing UUID and current state) instead of creating a duplicate. This makes scheduling naturally idempotent for retries and crash recovery.
- **`client_ref` format:** any string up to 255 characters. SY.timer does not enforce structure. WF will use `wf:<instance_id>::<timer_key>` but that is a WF convention, not a SY.timer requirement.
- **No new verbs:** the existing verbs (`TIMER_SCHEDULE`, `TIMER_CANCEL`, `TIMER_RESCHEDULE`, `TIMER_GET`) gain optional `client_ref` parameters. No new `TIMER_*` messages introduced.

---

## 4) Schema changes

### SYT11-DB-1 — Add unique index on (owner_l2_name, client_ref)
- [x] Migration: add unique index `idx_timers_owner_clientref` on `timers(owner_l2_name, client_ref)` WHERE `client_ref IS NOT NULL` AND `status = 'pending'`
- [x] Note: `client_ref` already exists as a column in v1.0; only the unique index is new
- [x] Active-only constraint (`status = 'pending'`) means a fired or canceled timer's `client_ref` can be reused for a new timer
- [x] Bump `user_version` pragma for migration tracking

### SYT11-DB-2 — Migration handling
- [x] On startup, check `user_version`; if v1.0, run the migration to add the index, then update `user_version`
- [x] Existing rows with NULL `client_ref` are excluded from the unique constraint by the WHERE clause
- [x] Existing rows with a non-NULL `client_ref` may already have duplicates (since v1.0 didn't enforce uniqueness). Test against this: if duplicates exist, the migration must handle them by either failing loudly with a clear error, or marking older duplicates as canceled. **Recommended: fail loudly**, since v1.0 client_refs were not load-bearing and any duplicates indicate an unexpected use pattern that should be reviewed.

---

## 5) Protocol extensions

### SYT11-PROTO-1 — `TIMER_SCHEDULE` idempotent on client_ref
- [x] When a `TIMER_SCHEDULE` request arrives with `client_ref` set:
  - Look up `(owner_l2_name, client_ref)` in DB
  - If found AND status is `pending`: return the existing `timer_uuid` and current state, do NOT create a new timer. Response includes a flag `already_existed: true` for the client to know.
  - If found AND status is `fired` or `canceled`: create a new timer (the old one is no longer active, so the `client_ref` is reusable per the unique index)
  - If not found: create new timer normally
- [x] When `client_ref` is NOT set: behavior unchanged from v1.0

### SYT11-PROTO-2 — `TIMER_CANCEL` accepts client_ref
- [x] Request payload accepts EITHER `timer_uuid` OR `client_ref` (not both required)
- [x] If both provided: `timer_uuid` wins, `client_ref` ignored
- [x] If only `client_ref`: look up by `(owner_l2_name, client_ref)` AND status active
- [x] If lookup finds nothing: respond ok with `found: false` (cancelling something that doesn't exist is a no-op, not an error — consistent with v1.0)
- [x] If lookup finds a timer: cancel as in v1.0

### SYT11-PROTO-3 — `TIMER_RESCHEDULE` accepts client_ref
- [x] Same lookup pattern as TIMER_CANCEL
- [x] Validates that the resolved timer is in `pending` status
- [x] Updates fire_at as in v1.0

### SYT11-PROTO-4 — `TIMER_GET` accepts client_ref
- [x] `TIMER_GET` by `timer_uuid` remains open-read within the local hive, unchanged from v1.0
- [x] `TIMER_GET` by `client_ref` is owner-scoped and uses the same owner validation as TIMER_CANCEL / TIMER_RESCHEDULE
- [x] Returns full timer record (or not_found)

### SYT11-PROTO-5 — owner_l2_name validation
- [x] All client_ref-based operations resolve `owner_l2_name` from `routing.src_l2_name`, stamped authoritatively by the router on delivery
- [x] Reject if the resolved owner_l2_name does not match the original `owner_l2_name` of the timer being targeted
- [ ] This prevents one node from using `client_ref` to reach another node's timers (since client_refs may collide across owners by design)
- [x] UUID-based operations keep current v1.0 semantics: cancel/reschedule remain owner-scoped, `TIMER_GET` by UUID remains open-read, and `SY.orchestrator` retains its existing purge exception

---

## 6) Code changes

### SYT11-CODE-1 — Lookup helper
- [x] Add internal function `findTimerByClientRef(ownerL2Name, clientRef string) (*Timer, error)`
- [x] Returns timer if found and active, nil if not found
- [x] Used by TIMER_CANCEL, TIMER_RESCHEDULE, TIMER_GET handlers

### SYT11-CODE-2 — TIMER_SCHEDULE handler update
- [x] Before insert, if `client_ref` is set, call `findTimerByClientRef`
- [x] If existing active timer found: return it with `already_existed: true`
- [x] Otherwise insert normally
- [x] Insert path must handle the unique constraint failure gracefully (race between two concurrent schedules with same client_ref): on constraint violation, retry the lookup and return the existing timer

### SYT11-CODE-3 — TIMER_CANCEL handler update
- [x] Accept `client_ref` as alternative to `timer_uuid` in payload
- [x] If only `client_ref`: resolve via `findTimerByClientRef`
- [x] Continue with existing cancel logic

### SYT11-CODE-4 — TIMER_RESCHEDULE handler update
- [x] Same pattern as TIMER_CANCEL

### SYT11-CODE-5 — TIMER_GET handler update
- [x] Same pattern as TIMER_CANCEL

### SYT11-CODE-6 — Response shape changes
- [x] `TIMER_SCHEDULE_RESPONSE` payload gains optional field `already_existed: bool` (default false)
- [x] Other responses unchanged

---

## 7) SDK changes

### SYT11-SDK-1 — Add client_ref parameter to timer client helpers
- [x] `timerClient.Schedule(...)` accepts optional `clientRef` parameter (or via options struct)
- [x] `timerClient.Cancel(...)` accepts EITHER `timerUUID` OR `clientRef`
- [x] `timerClient.Reschedule(...)` accepts EITHER `timerUUID` OR `clientRef`
- [x] `timerClient.Get(...)` accepts EITHER `timerUUID` OR `clientRef`
- [x] Validation: at least one of (timerUUID, clientRef) must be provided

### SYT11-SDK-2 — Rust SDK parity
- [x] Extend `crates/fluxbee_sdk/src/timer.rs` with the same `client_ref`-based lookup mode
- [x] `TimerClient::schedule*` accepts optional `client_ref`
- [x] `TimerClient::cancel`, `reschedule`, and `get` accept EITHER `timer_uuid` OR `client_ref`
- [x] Validation matches Go SDK and protocol semantics exactly

### SYT11-SDK-3 — Document the new pattern
- [ ] Update SDK README with example: schedule with client_ref, cancel by client_ref without waiting for response
- [ ] Note that this is the recommended pattern for any client that needs to operate on timers asynchronously (WF, future systems with similar needs)

---

## 8) Tests

### SYT11-TEST-1 — Unit tests for lookup
- [x] `findTimerByClientRef` returns active timer when match exists
- [ ] Returns nil when no match
- [ ] Returns nil when match exists but is canceled
- [ ] Honors owner scoping (different owner with same client_ref → not found)

### SYT11-TEST-2 — Schedule idempotency
- [x] Schedule with new client_ref → creates timer, `already_existed=false`
- [x] Schedule with existing active client_ref → returns existing timer, `already_existed=true`, no new row inserted
- [ ] Schedule after cancel of same client_ref → creates new timer (because old one is canceled)
- [ ] Schedule after fire of same client_ref → creates new timer (because old one is no longer active)
- [ ] Concurrent schedules with same client_ref → exactly one timer created, both clients see the same timer in their response

### SYT11-TEST-3 — Cancel/Reschedule/Get by client_ref
- [x] Cancel by client_ref → cancels the right timer
- [x] Cancel non-existent client_ref → ok with `found=false`
- [x] Reschedule by client_ref → updates the right timer
- [x] Get by client_ref → returns the right timer
- [x] Get by UUID remains open-read and unchanged from v1.0

### SYT11-TEST-4 — Authorization
- [ ] Cancel by client_ref from a different owner → `UNAUTHORIZED`
- [ ] Same client_ref used by two different owners → operations on owner A's timer don't affect owner B's

### SYT11-TEST-5 — Migration
- [x] Database with v1.0 schema and no client_refs → migration runs, index created
- [ ] Database with v1.0 schema and unique client_refs → migration runs successfully
- [x] Database with v1.0 schema and duplicate client_refs → migration fails with clear error

### SYT11-TEST-6 — End-to-end against running SY.timer
- [ ] Full schedule → cancel by client_ref flow without waiting for schedule response
- [ ] Full schedule → reschedule by client_ref flow
- [ ] Idempotent schedule (call twice with same client_ref, verify only one timer)

---

## 9) Documentation

### SYT11-DOC-1 — Update sy-timer.md to v1.1
- [ ] Bump status to v1.1
- [ ] Add section "Client-supplied identifiers (client_ref)" explaining the new lookup mode
- [ ] Update each verb's request/response section to document the optional `client_ref` parameter
- [ ] Add note in the schedule section about idempotency on client_ref
- [ ] Document the migration from v1.0 (additive, no breaking changes)

### SYT11-DOC-2 — Update SDK docs
- [ ] Update timer client documentation in fluxbee-go-sdk README
- [ ] Update timer client documentation in Rust SDK module docs / examples
- [ ] Add example showing async pattern: schedule + cancel by client_ref without waiting

---

## Order of implementation

```
SYT11-DB-1 → SYT11-DB-2
  → SYT11-CODE-1
    → SYT11-PROTO-1 / SYT11-CODE-2 (schedule idempotency)
    → SYT11-PROTO-2 / SYT11-CODE-3 (cancel)
    → SYT11-PROTO-3 / SYT11-CODE-4 (reschedule)
    → SYT11-PROTO-4 / SYT11-CODE-5 (get)
    → SYT11-PROTO-5 (authorization, applies to all of the above)
    → SYT11-CODE-6 (response shape)
      → SYT11-SDK-1 → SYT11-SDK-2 → SYT11-SDK-3
        → SYT11-TEST-1 ... SYT11-TEST-6
          → SYT11-DOC-1 → SYT11-DOC-2
```

This is intended to be a small, self-contained extension that should take significantly less work than the v1.0 implementation. Estimated: 2-3 days of focused work.

---

## Notes for the implementer

1. **Don't break v1.0.** Every existing test should still pass. The new behavior is purely additive.

2. **Migration is the riskiest part.** If the production database has v1.0 client_refs that happen to collide, the migration will fail. Coordinate with the operator before deployment to verify there are no duplicates, or run a pre-migration check.

3. **The `already_existed` flag in TIMER_SCHEDULE_RESPONSE is informational.** Clients that don't care can ignore it. Clients that care (like WF on recovery) can use it to avoid double-counting.

4. **Idempotency at insert time has a race.** Two concurrent inserts with the same client_ref can both pass the pre-check, then one fails on the unique constraint. Handle this by catching the constraint violation and re-querying. The client should always see a coherent result.

5. **Authorization is critical.** Without it, any node could cancel any other node's timers by guessing client_refs. The owner check at lookup time is non-negotiable. Use `routing.src_l2_name`, not legacy UUID→L2 resolution.

6. **No `TIMER_LIST` changes needed.** The existing `TIMER_LIST` filtering by owner_l2_name already returns enough information. Clients that want to list by client_ref pattern can do so client-side after fetching the full list. (If this becomes inconvenient later, server-side filtering can be added in v1.2.)

7. **Keep state names aligned with the real implementation.** The current timer statuses are `pending`, `fired`, and `canceled`. Do not introduce `scheduled` or British spelling variants into code or migrations.
