# IO Nodes - Identity Provision Baseline (Ordered Task List)

## Goal

Converge all IO nodes to one shared identity path in `io-common`:

- `lookup -> provision_on_miss -> forward`
- Fallback only on failure/timeout: forward with `src_ilk=null`
- No legacy identity modes in runtime behavior

---

## Current Status Snapshot

- [x] `io-sim` uses provision-on-miss as canonical flow (no mode toggle).
- [x] `io-sim` removed legacy identity modes.
- [x] `io-common` has shared `FluxbeeIdentityProvisioner` + `RouterInbox`.
- [x] Baseline fallback behavior exists (`src_ilk=null` on provision failure).
- [x] `ShmIdentityResolver` uses SDK-backed lookup.
- [x] Short cache `(channel, external_id) -> src_ilk` exists in common inbound path.
- [x] `io-slack` adopted the same common identity flow.
- [x] IO envelope identity carrier aligned to `meta.src_ilk` (no `meta.context.src_ilk` use in active adapters).
- [ ] Some docs still need cleanup for text encoding/legacy imported wording.

---

## Ordered Work Plan

## P0 - Contract Freeze (io-common owns identity behavior)

- [x] P0.1 Define provision-on-miss as canonical strategy in IO core.
- [x] P0.2 Remove legacy identity modes from `io-sim`.
- [x] P0.3 Document explicitly that `src_ilk=null` is degraded fallback, not normal path.
- [x] P0.4 Document that identity behavior is common (`io-common`) and channel adapters must not reimplement it.

Acceptance:
- One canonical behavior is documented and enforced from common code.

---

## P1 - Core Implementation in `io-common` (highest priority)

- [x] P1.1 Shared inbound flow: lookup first, then provision on miss.
- [x] P1.2 Shared provision call via router system messages (`ILK_PROVISION`).
- [x] P1.3 Shared non-blocking fallback behavior (`src_ilk=null`) on provision failure/timeout.
- [x] P1.4 Structured logs for lookup/provision/final `src_ilk`.
- [x] P1.5 Replace legacy SHM parser with SDK-backed lookup:
  - `fluxbee_sdk::resolve_ilk_from_hive_id` (or equivalent helper path)
  - ensure alignment with current identity SHM layout
- [x] P1.6 Add short-lived cache `(channel, external_id) -> src_ilk` to reduce repeated provision calls while SHM converges.
- [x] P1.7 Ensure stable `ich_id` input strategy for provision payload (deterministic from channel + external_id).
- [x] P1.8 Identity metrics counters:
  - lookup hit/miss
  - provision success/error
  - fallback null count

Acceptance:
- Common code alone can drive identity resolution for all IO adapters.

---

## P2 - `io-sim` as Reference Adapter

- [x] P2.1 Consume shared identity flow from `io-common` (no app-local duplicate logic).
- [x] P2.2 Keep routing default as `dst=resolve` (fixed dst only explicit test override).
- [x] P2.3 Keep wire debug visibility for diagnosis.
- [x] P2.4 Add/adjust tests:
  - miss + provision success => forwarded with non-null `src_ilk`
  - miss + provision failure => forwarded with `src_ilk=null`
  - lookup hit => no provision call
- [ ] P2.5 Validate behavior after P1.5/P1.6 (SHM-backed hit + cache path in repeated sends).

Acceptance:
- `io-sim` remains the E2E reference for identity behavior before Slack rollout.

---

## P3 - `io-slack` Adoption (same common core)

- [x] P3.1 Remove/avoid duplicated identity logic in `io-slack`; reuse `io-common` pipeline.
- [x] P3.2 Remove legacy identity mode toggles from `io-slack` runtime config.
- [x] P3.3 Keep Slack ACK path non-blocking while identity/provision runs in shared inbound path.
- [x] P3.4 Ensure envelope parity with `io-sim`:
  - `meta.src_ilk` must be the field used
  - preserve Slack io_context fields
- [x] P3.5 Keep `resolve` as default routing behavior for production.
- [x] P3.6 Keep fixed dst only as explicit test override and document it as non-baseline.

Acceptance:
- Slack adapter behaves like sim adapter regarding identity, with only channel-specific mapping differences.

---

## P4 - Documentation Cleanup (legacy wording removal)

- [x] P4.1 `io/README.md`: remove `IDENTITY_MODE` usage and legacy mode examples.
- [x] P4.2 `docs/io/io-common.md`: update to single provision-on-miss baseline.
- [x] P4.3 `docs/io/io-slack.md`: align env/config and behavior with common baseline.
- [x] P4.4 `docs/io/io-slack-tasks.md`: ensure legacy-mode references are fully historical-only or removed from active tasks.
- [x] P4.5 Keep onworking docs aligned with no-mode baseline wording.

Acceptance:
- No active docs suggest legacy identity modes as valid runtime options.

---

## P5 - Open Cross-Team Diagnosis (Router/Core)

- [ ] P5.1 Track `UNREACHABLE/OPA_NO_TARGET` in `dst=resolve` flows with provisional ILK.
- [ ] P5.2 Keep evidence bundle for core team:
  - wire message from IO shows `meta.src_ilk`
  - provision response and resulting ILK
  - router UNREACHABLE payload (`OPA_NO_TARGET`)
- [ ] P5.3 Re-test after core fixes with unchanged IO payloads.

Note:
- This is separate from P1 cache/lookup hardening; both can coexist.

---

## Execution Order

1. P0 (contract freeze)
2. P1 (complete common core)
3. P2 (re-validate on sim)
4. P3 (roll into Slack)
5. P4 (docs close)
6. P5 (cross-team follow-up until resolve path is fixed)

---

## Risks

- Coupling to `SY.identity` availability.
- Potential SHM convergence lag causing repeated provision calls.
- Router/OPA unresolved target path still blocking some E2E scenarios.

Mitigations:
- Keep non-blocking fallback.
- Add cache + metrics.
- Keep reproducible evidence for core-side issues.

---

## Compliance Snapshot (Orchestrator Path)

Implemented now:
- `IO.slack` supports spawn config file input (`/var/lib/fluxbee/nodes/IO/<node_name>/config.json`)
  with env override precedence.
- Token fields supported from spawn config:
  - `slack.app_token` / `slack.bot_token`
  - `slack.app_token_ref` / `slack.bot_token_ref` (`env:VAR`)
- Identity baseline in `io-common` remains canonical (`lookup -> provision_on_miss -> forward`).

Still pending for full compliance:
- Explicit `CONFIG_CHANGED` handling/reload policy for `io-slack` runtime.
- E2E validation in orchestrator path for `publish -> update -> spawn -> status/kill`.
- Final decision on long-term secrets policy (`config.json` plaintext vs managed refs), per core roadmap.
- Remove temporary forced config path workaround in `io-slack` once orchestrator spawn contract reliably injects runtime env (`NODE_CONFIG_PATH`/`NODE_NAME`/`ISLAND_ID`).
