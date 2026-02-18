# Router LSA/SLA Full Review (Motherbee/Worker)

Date: 2026-02-18  
Scope: router WAN+LSA path, SHM `jsr-lsa-*`, and admin/orchestrator remote projections.

## Goal

Assess whether the observed remote router UUID (`00000000-0000-0000-0000-000000000000`) is an isolated issue or part of a broader LSA gap, and define a complete worklist.

## Executive conclusion

The UUID issue is **not isolated**.  
It is one symptom of a broader gap in the remote-topology model:

- remote router identity is missing from current LSA contract/SHM projection.
- remote node/router projection in orchestrator is lossy and partially ignores LSA status bits.
- LSA trust and sequence handling need hardening for restart/security scenarios.

## Evidence from current code

1) Remote router UUID is synthetic (nil)
- `src/bin/sy_orchestrator.rs:1000` uses `Uuid::nil()` in `remote_routers_for_hive`.
- This is because `LsaSnapshot` remote hive entries do not carry remote router UUID.

2) LSA protocol payload has no remote gateway identity fields
- `crates/jsr_client/src/protocol.rs:162` (`LsaPayload`) contains `hive/seq/timestamp/nodes/routes/vpns`.
- No `router_id` / `router_name` in payload.

3) SHM LSA remote hive entry has no remote gateway identity
- `src/shm/mod.rs:241` (`RemoteHiveEntry`) stores hive id, seq/time, counters, flags only.
- No remote router UUID/name field.

4) Orchestrator remote-node projection ignores stale/deleted flags
- `src/bin/sy_orchestrator.rs:941-959` includes all nodes for a hive index.
- `src/bin/sy_orchestrator.rs:962-975` hardcodes `"status":"active"` and uses hive `last_updated` as `connected_at`.

5) LSA auth/trust binding gap
- `src/router/mod.rs:1776-1785` accepts `MSG_LSA` and applies payload by `payload.hive`.
- No strict binding between authenticated WAN peer (`peer_hello.hive_id`) and `payload.hive`.

6) Sequence restart risk
- `src/router/mod.rs:1429-1433` rejects if `payload.seq <= last_seq`.
- On remote restart, sequence can reset to small values and be dropped until surpassing previous high watermark.

7) WAN readiness check is presence-only
- `src/bin/sy_orchestrator.rs:2433-2470` (`wait_for_wan`) checks hive presence in LSA.
- It does not enforce freshness/non-stale for success criteria.

## Impact

- Admin `/hives/{id}/routers` cannot expose a real remote router UUID today.
- Remote node/router status from admin API can look healthier than real LSA state.
- Potential stale or spoofed LSA edge cases can pollute remote topology.
- Restart behavior can produce long-lived partial visibility if seq handling is not reset-aware.

## Worklist (onworking)

## Status update (after P2 implementation)

- Remote router identity is now propagated end-to-end:
  - WAN LSA payload includes `router_id` / `router_name`.
  - SHM LSA remote hive entry includes router UUID/name.
  - Admin/orchestrator remote routers projection now reads real UUID/name from LSA state.
- SHM compatibility was handled with `LSA_VERSION` bump (`1 -> 2`) and region recreation on header mismatch.
- Remaining closure is `P3` (tests + operational validation matrix).

## P0 - Data correctness for admin/orchestrator view

- [x] Make orchestrator remote node projection LSA-flag aware:
  - Skip `FLAG_STALE` hives and stale/deleted nodes.
  - Return status based on flags (`active/stale/deleted`) instead of hardcoded `active`.
- [x] Make `wait_for_wan` freshness-aware:
  - Require matching hive not stale and within `HEARTBEAT_STALE_MS` window.
- [x] Keep current nil UUID behavior explicitly documented until protocol/model extension lands.
  - Nota operativa agregada en `docs/onworking/sy_admin_e2e_curl_checklist.md` (sección routers).

Acceptance:
- `/hives/{id}/nodes` and `/hives/{id}/routers` reflect stale states correctly.
- `add_hive` WAN success depends on fresh LSA, not only presence.

## P1 - LSA trust and sequence hardening

- [x] Bind incoming LSA to authenticated WAN session:
  - Track peer hive per TCP session and reject payload hive mismatch.
- [x] Add sequence reset policy:
  - [x] Option A: per-peer/session epoch to allow restart reset.
  - Option B: reset `last_seq` when peer reconnect is detected.
- [x] Add explicit logs/counters for rejected LSA causes:
  - [x] `hive_mismatch`, `stale_seq`, `parse_error`, `unauthorized`.

Acceptance:
- Remote restart does not leave LSA permanently frozen.
- LSA from mismatched hive is rejected deterministically.

## P2 - Remote router identity model completion

- [x] Extend LSA model to carry remote gateway identity:
  - Protocol: added `router_id` / `router_name` to `LsaPayload`.
  - SHM: extended `RemoteHiveEntry` with remote router UUID/name (versioned layout).
- [x] Update orchestrator `/hives/{id}/routers` to return real UUID from LSA state.
- [x] Add migration/compatibility strategy for SHM version bump.
  - `LSA_VERSION` bumped `1 -> 2`.
  - Existing `/jsr-lsa-*` regions with old header are treated as invalid and recreated.
  - Compatibility mode for old peers: if LSA payload omits router identity, router falls back to authenticated WAN HELLO identity for projection.

Acceptance:
- `/hives/{id}/routers` no longer returns nil UUID.
- Remote router identity remains stable across refresh cycles.

## P3 - Tests and operational validation

- [ ] Unit tests:
  - LSA apply with stale seq/restart seq.
  - Hive mismatch rejection.
  - Flag projection for orchestrator API.
- [ ] Integration tests:
  - two-hive WAN bootstrap, disconnect/reconnect, stale transition.
  - add/remove hive with LSA freshness gate.
- [ ] E2E checks:
  - admin endpoints return real remote router UUID after P2.
  - stale transitions visible through API.

## Suggested delivery order

1. P0 (fast correctness wins in API + bootstrap behavior).  
2. P1 (safety and resilience).  
3. P2 (model/protocol/SHM extension).  
4. P3 (test matrix closure).

## Notes

- Existing docs still reference older naming in some sections (`json-router` paths); keep this review focused on LSA behavior first.
- Current UUID `0000...` is expected with current model and should be treated as temporary technical debt, not as a random runtime glitch.
