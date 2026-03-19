# IO Slack - Tasks Aligned with IO Identity Baseline

## Goal

Align `io-slack` with the same common IO identity behavior already adopted by `io-sim`:

- shared path in `io-common`: `lookup -> provision_on_miss -> forward`
- fallback-only `src_ilk=null` on provision failure/timeout
- no legacy identity mode toggles in runtime behavior

---

## Scope

- Slack-specific adapter behavior (Socket Mode inbound, Slack Web API outbound)
- Reuse `io-common` identity/inbound pipeline (no duplicated identity logic in `io-slack`)
- Keep `resolve` as production default routing

Out of scope:
- Router/OPA contract changes
- Durable storage backend for IO runtime state

---

## Current Snapshot

- [x] Router connectivity and handshake plumbing are implemented.
- [x] Slack Socket Mode inbound + ACK are implemented.
- [x] Outbound `chat.postMessage` base path is implemented.
- [x] Slack inbound uses shared `io-common` identity pipeline.
- [ ] Operational hardening/tests remain open (retry classification + E2E with router core fixes).

---

## Ordered Task Plan

## P0 - Contract Alignment (Slack must consume common behavior)

- [x] P0.1 Freeze rule: identity resolution/provision lives in `io-common`; `io-slack` only maps Slack events/context.
- [x] P0.2 Remove legacy identity mode wording/toggles from Slack runtime docs/config.
- [x] P0.3 Document degraded fallback explicitly: provision failure/timeout => forward with `src_ilk=null`.

Acceptance:
- Slack adapter has no alternate identity behavior outside `io-common` flow.

---

## P1 - Core Dependencies from `io-common` (must land first)

These are blockers/dependencies for clean Slack adoption:

- [x] P1.1 Replace legacy SHM parser path with SDK-backed lookup in `ShmIdentityResolver`.
- [x] P1.2 Add short-lived local cache `(channel, external_id) -> src_ilk`.
- [x] P1.3 Ensure deterministic `ich_id` input strategy for provision payload.
- [x] P1.4 Add counters/metrics:
  - lookup hit/miss
  - provision success/error
  - fallback-null usage

Acceptance:
- Common identity path is reliable enough for Slack traffic.

---

## P2 - Slack Inbound Integration (using `io-common`)

- [x] P2.1 Route all inbound Slack events through shared `InboundProcessor`.
- [x] P2.2 Ensure envelope parity with `io-sim`:
  - set `meta.src_ilk` (canonical identity carrier)
  - preserve `meta.context.io` fields for Slack
- [x] P2.3 Keep ACK non-blocking (do not block Slack ack on router/provision latency).
- [x] P2.4 Keep dedup active for inbound keys (`event_id` preferred; fallback tuple).
- [x] P2.5 Keep `dst=resolve` as default; fixed destination only as explicit test override.

Acceptance:
- Inbound behavior matches `io-sim` identity semantics with Slack-specific context only.

---

## P3 - Slack Outbound Reliability and Delivery

- [ ] P3.1 Keep current `chat.postMessage` behavior and validate reply target parsing from `meta.context.io.reply_target`.
- [ ] P3.2 Add retry/backoff classification for outbound failures:
  - retryable, non-retryable, rate-limited, auth errors
- [ ] P3.3 Add minimal in-memory outbox state machine for outbound (`PENDING/SENDING/SENT/FAILED/DEAD`) if not fully covered.
- [ ] P3.4 Add delivery metrics/logs:
  - outbound sent/failed
  - latency
  - queue sizes

Acceptance:
- Slack outbound path is observable and operationally predictable under transient failures.

---

## P4 - Config and Ops

- [x] P4.1 Keep config minimal and explicit:
  - Slack tokens (`SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN`, optional signing secret for HTTP mode)
  - router/node config
  - shared identity envs (`ISLAND_ID`, `IDENTITY_TARGET`, `IDENTITY_FALLBACK_TARGET`, `IDENTITY_TIMEOUT_MS`)
- [x] P4.2 Remove any remaining identity-mode env usage from Slack docs/examples.
- [ ] P4.3 Define healthcheck expectations:
  - router connectivity
  - Slack connectivity
  - runtime lifecycle state
- [x] P4.4 Keep optional test-only fixed destination documented as non-production override.

Acceptance:
- Operator-facing config/runbook reflects baseline behavior with no legacy ambiguity.

---

## P5 - Tests and Validation

- [ ] P5.1 Unit/integration tests for identity outcomes:
  - miss + provision success => non-null `src_ilk`
  - miss + provision failure => `src_ilk=null`
  - lookup hit => no provision call
- [ ] P5.2 Inbound/outbound Slack flow validation:
  - app mention inbound normalized correctly
  - outbound reply reaches expected channel/thread
- [ ] P5.3 E2E validation against router behavior (`resolve` path) after core-side fixes for `OPA_NO_TARGET`.

Acceptance:
- Slack behavior is testable and consistent with the shared IO identity baseline.

---

## Open Questions

- [ ] Final `external_id` format for Slack identity mapping (`user` vs `team:user`).
- [ ] Whether any fire-and-forget identity/link signal remains necessary from IO side.
- [ ] Onboarding reinjection constraints: which fields must remain stable (trace/context).

---

## Execution Order

1. P0 contract alignment
2. P1 common-core dependencies
3. P2 Slack inbound integration
4. P3 outbound reliability
5. P4 config/docs/ops
6. P5 tests/E2E validation
