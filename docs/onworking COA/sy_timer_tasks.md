# SY.timer Implementation Tasks

**Status:** planning / ready for implementation
**Date:** 2026-04-08
**Primary spec:** [sy-timer.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/sy-timer.md)
**Related work:** formal top-level `fluxbee-go-sdk`

---

## 1) Goal

Implement `SY.timer` as a first-class Fluxbee system node that provides:

- persistent one-shot and recurring timers
- authoritative time/timezone operations
- direct node-to-node invocation through the standard Fluxbee envelope
- a reusable formal Go SDK surface for first-party and third-party node authors

This effort is intentionally split into:

1. `SY.timer` node implementation
2. `fluxbee-go-sdk` extraction/extension needed to support it cleanly
3. core integration (`install.sh`, orchestrator, docs, operator/admin visibility)

---

## 2) Frozen decisions

These points are already frozen for implementation.

### 2.1 Canonical runtime/node naming

- canonical runtime name: `SY.timer`
- canonical node instance name: `SY.timer@<hive>`
- docs and examples must use uppercase `SY.*` naming, aligned with the rest of the system

### 2.2 Operational model

`SY.timer` is a new core singleton service, aligned with the existing `SY.*` operational model:

- one instance per hive
- binary in `/usr/bin/sy-timer`
- copy in `/var/lib/fluxbee/dist/core/bin/sy-timer`
- explicit systemd unit
- orchestrator supervising/restarting via systemd

It is **not** a runtime-style managed node spawned like `AI.*` / `IO.*` instances.

### 2.3 Ownership/auth source identity

Ownership remains defined in canonical L2 names, but incoming requests still carry:

- `routing.src = <uuid>`

So v1 requires:

- UUID L1 -> L2 name resolution support in `fluxbee-go-sdk`
- node-side authorization expressed against canonical L2 names

### 2.4 Creation timestamp field

`created_at_linux` is dropped from v1.

The contract keeps a single creation timestamp:

- `created_at_utc`

with semantics:

- unix ms from the Linux host clock
- preserved across reschedules as the original creation timestamp

### 2.5 `HELP` naming in v1

For v1, `SY.timer` keeps:

- `TIMER_HELP`

as its node-specific self-description verb.

The future generalization to a cross-node generic `HELP` contract remains open for a later platform pass and does not block implementation.
### 2.6 Direct invocation outside `SY.admin` is intentional

This is not a conflict, but it is a new pattern for a system node.

`SY.timer` is explicitly designed for:

- direct SDK calls from nodes/workflows/agents

not only:

- operator calls through `SY.admin`

That means:

- the direct-call contract is part of the public platform surface
- `TIMER_HELP` and SDK ergonomics matter more than in current admin-mediated nodes

### 2.7 Go SDK publication model

`fluxbee-go-sdk` is frozen as a formal top-level SDK in the repository.

It must:

- move out of `sy-opa-rules/sdk`
- be clearly identified as the Go SDK counterpart for Fluxbee
- remain usable by first-party nodes and external users
- avoid hidden coupling to the `SY.opa.rules` package layout

---

## 3) Implementation strategy

Recommended order:

1. Formalize top-level `fluxbee-go-sdk`
2. Extend it to the minimum needed by `SY.timer`
3. Build `SY.timer` node with direct SDK-first interface
4. Integrate as a system node in core
5. Add admin/operator visibility and docs

---

## 4) Workstream A — Frozen contract

### SYT-S0 — Freeze the architectural decisions

- [x] SYT-S0-T1. Canonical node/runtime name frozen as `SY.timer` / `SY.timer@<hive>`.
- [x] SYT-S0-T2. Operational model frozen as core singleton service via systemd, aligned with other `SY.*`.
- [x] SYT-S0-T3. UUID->L2 ownership resolution frozen as a `fluxbee-go-sdk` responsibility.
- [x] SYT-S0-T4. `created_at_linux` dropped; v1 keeps a single preserved creation timestamp.
- [x] SYT-S0-T5. `TIMER_HELP` remains node-specific in v1; generic `HELP` stays for a later platform pass.
- [x] SYT-S0-T6. `fluxbee-go-sdk` is frozen as a formal top-level SDK, not an internal subpackage under `SY.opa.rules`.

---

## 5) Workstream B — `fluxbee-go-sdk` extension

### SYT-S1 — Promote the current Go SDK from OPA-specific bootstrap to reusable platform SDK

- [ ] SYT-S1-T1. Create a formal top-level `fluxbee-go-sdk/` module in the repo.
- [ ] SYT-S1-T2. Preserve backward compatibility for `SY.opa.rules` while extending the SDK.
- [ ] SYT-S1-T3. Add peer identity resolution support needed by `SY.timer`.
- [ ] SYT-S1-T4. Add typed request/reply helpers for direct `system` RPC-style node interactions.
- [ ] SYT-S1-T5. Add typed error parsing helpers matching `TIMER_RESPONSE`.
- [ ] SYT-S1-T6. Add reusable `HELP` descriptor helpers if that reduces duplication.
- [ ] SYT-S1-T7. Move the current SDK code from `sy-opa-rules/sdk` into the formal SDK layout.
- [ ] SYT-S1-T8. Define stable package naming and import path for Go consumers.
- [ ] SYT-S1-T9. Add module README and first-party/third-party usage guidance.
- [ ] SYT-S1-T10. Define versioning/compatibility policy for the Go SDK.
- [ ] SYT-S1-T11. Add wire-compatibility tests against the Rust SDK contract.
- [ ] SYT-S1-T12. Leave a compatibility migration path for `SY.opa.rules` while imports move to the formal SDK.

### SYT-S2 — Add `timer` client module to the Go SDK

- [ ] SYT-S2-T1. Define Go types:
  - `TimerID`
  - `TimerInfo`
  - `FiredEvent`
  - `MissedPolicy`
  - `ListFilter`
  - `HelpDescriptor`
- [ ] SYT-S2-T2. Implement typed client calls:
  - `Now`
  - `NowIn`
  - `Convert`
  - `Parse`
  - `Format`
  - `Schedule`
  - `ScheduleIn`
  - `ScheduleRecurring`
  - `Cancel`
  - `Reschedule`
  - `Get`
  - `ListMine`
  - `Help`
- [ ] SYT-S2-T3. Implement client-side validation:
  - minimum 60s
  - mutually exclusive absolute vs relative scheduling fields
  - recurring validation shape
- [ ] SYT-S2-T4. Implement retry policy for time operations as defined by the spec.
- [ ] SYT-S2-T5. Implement `ParseFiredEvent(msg)` helper.
- [ ] SYT-S2-T6. Add tests/golden fixtures for SDK wire compatibility.

---

## 6) Workstream C — `SY.timer` node implementation

### SYT-S3 — Node skeleton and lifecycle

- [ ] SYT-S3-T1. Create Go binary/package for `SY.timer`.
- [ ] SYT-S3-T2. Connect through `fluxbee-go-sdk` lifecycle.
- [ ] SYT-S3-T3. Use canonical node instance dir under `/var/lib/fluxbee/nodes/SY/SY.timer@<hive>/`.
- [ ] SYT-S3-T4. Open/create `timers.db` on startup.
- [ ] SYT-S3-T5. Enable SQLite WAL mode and define safe startup/shutdown semantics.
- [ ] SYT-S3-T6. Add structured logging conventions for timer lifecycle and fire events.

### SYT-S4 — Persistence and schema

- [ ] SYT-S4-T1. Implement SQLite schema creation/migrations for `timers`.
- [ ] SYT-S4-T2. Add indexes exactly as required by the spec or revise them if schema is frozen differently.
- [ ] SYT-S4-T3. Implement timer row serialization/deserialization with JSON payload/metadata blobs.
- [ ] SYT-S4-T4. Decide whether to persist all timestamps as unix ms integers in v1.
- [ ] SYT-S4-T5. Implement GC for historical timers (`fired`, `canceled`) with default retention.

### SYT-S5 — Scheduler engine

- [ ] SYT-S5-T1. Implement in-memory heap ordered by next `fire_at`.
- [ ] SYT-S5-T2. Implement startup replay from pending timers.
- [ ] SYT-S5-T3. Implement missed-policy behavior on restart:
  - `fire`
  - `drop`
  - `fire_if_within`
- [ ] SYT-S5-T4. Implement recurring timer next-fire computation from cron.
- [ ] SYT-S5-T5. Implement per-timer locking around fire/cancel/reschedule races.
- [ ] SYT-S5-T6. Emit `TIMER_FIRED` as fire-and-forget L2 event.
- [ ] SYT-S5-T7. Define whether `actual_fire_at_utc_ms` is taken before send or after successful socket write.

### SYT-S6 — Request handlers

- [ ] SYT-S6-T1. Implement `TIMER_SCHEDULE`.
- [ ] SYT-S6-T2. Implement `TIMER_SCHEDULE_RECURRING`.
- [ ] SYT-S6-T3. Implement `TIMER_CANCEL`.
- [ ] SYT-S6-T4. Implement `TIMER_RESCHEDULE`.
- [ ] SYT-S6-T5. Implement `TIMER_GET`.
- [ ] SYT-S6-T6. Implement `TIMER_LIST`.
- [ ] SYT-S6-T7. Implement `TIMER_NOW`.
- [ ] SYT-S6-T8. Implement `TIMER_NOW_IN`.
- [ ] SYT-S6-T9. Implement `TIMER_CONVERT`.
- [ ] SYT-S6-T10. Implement `TIMER_PARSE`.
- [ ] SYT-S6-T11. Implement `TIMER_FORMAT`.
- [ ] SYT-S6-T12. Implement `TIMER_HELP`.
- [ ] SYT-S6-T13. Implement `TIMER_PURGE_OWNER` with orchestrator-only authorization.

### SYT-S7 — Authorization and identity

- [ ] SYT-S7-T1. Implement source UUID -> source L2 resolution path.
- [ ] SYT-S7-T2. Enforce strict ownership on:
  - `TIMER_GET`
  - `TIMER_LIST`
  - `TIMER_CANCEL`
  - `TIMER_RESCHEDULE`
- [ ] SYT-S7-T3. Enforce orchestrator-only permission for `TIMER_PURGE_OWNER`.
- [ ] SYT-S7-T4. Add negative tests for forged/foreign timer access.

---

## 7) Workstream D — Core integration

### SYT-S8 — Install, orchestrator, and service model

- [ ] SYT-S8-T1. Integrate `sy-timer` build into `scripts/install.sh`.
- [ ] SYT-S8-T2. Install binary to `/usr/bin/sy-timer`.
- [ ] SYT-S8-T3. Publish binary to `/var/lib/fluxbee/dist/core/bin/sy-timer`.
- [ ] SYT-S8-T4. Add core manifest entry for `sy-timer`.
- [ ] SYT-S8-T5. Add systemd unit for `sy-timer`.
- [ ] SYT-S8-T6. Add orchestrator startup/shutdown/watchdog handling for `sy-timer`.
- [ ] SYT-S8-T7. Add node cleanup hook from orchestrator:
  - `TIMER_PURGE_OWNER` before node teardown
- [ ] SYT-S8-T8. Decide whether orchestrator itself should use `SY.timer` for any internal delayed actions in v1 or not.

### SYT-S9 — Admin and operator visibility

- [ ] SYT-S9-T1. Decide whether `SY.admin` needs explicit operator endpoints for `SY.timer` in v1.
- [ ] SYT-S9-T2. If yes, define a narrow operator surface:
  - read/help only
  - or full timer operations
- [ ] SYT-S9-T3. Document that direct node SDK usage remains the primary interaction model even if admin visibility exists.

---

## 8) Workstream E — Documentation

### SYT-S10 — Spec alignment and docs

- [ ] SYT-S10-T1. Update [sy-timer.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/sy-timer.md) after freezing the open decisions.
- [ ] SYT-S10-T2. Keep the operation section aligned with the current core singleton `SY.*` model.
- [ ] SYT-S10-T3. Normalize all examples to `SY.timer@<hive>`.
- [ ] SYT-S10-T4. Add `SY.timer` to operator/system node catalogs if applicable.
- [ ] SYT-S10-T5. Document the direct SDK interaction pattern for workflow/node authors.
- [ ] SYT-S10-T6. Document the exact semantics of `TIMER_FIRED` being fire-and-forget.

---

## 9) Workstream F — Testing

### SYT-S11 — Test matrix

- [ ] SYT-S11-T1. Unit tests for all request validators.
- [ ] SYT-S11-T2. Unit tests for cron parsing and minimum interval enforcement.
- [ ] SYT-S11-T3. Unit tests for missed-policy handling.
- [ ] SYT-S11-T4. Unit tests for ownership and forbidden access.
- [ ] SYT-S11-T5. Unit tests for time conversion/parse/format operations.
- [ ] SYT-S11-T6. Integration tests for schedule -> fire -> recurrent reschedule path.
- [ ] SYT-S11-T7. Restart/replay tests against persisted SQLite.
- [ ] SYT-S11-T8. Orchestrator teardown test for `TIMER_PURGE_OWNER`.
- [ ] SYT-S11-T9. Go SDK tests for all typed client helpers.

---

## 10) Recommended execution order

1. `SYT-S1` formalize top-level `fluxbee-go-sdk`
2. `SYT-S2` add Go timer client module
3. `SYT-S3` / `SYT-S4` / `SYT-S5` node core
4. `SYT-S6` request handlers
5. `SYT-S7` authorization
6. `SYT-S8` core integration
7. `SYT-S10` doc alignment
8. `SYT-S11` hardening and replay tests

---

## 11) Current assessment

The spec is implementable and the main architecture is now frozen.

The remaining work is implementation-oriented:

1. formalize and move `fluxbee-go-sdk` to a top-level SDK module
2. extend it with UUID->L2 identity support and timer client primitives
3. implement `SY.timer` as a core singleton `SY.*` service
4. integrate install/orchestrator/docs
