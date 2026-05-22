# Node Teardown Completeness — task list

**Date:** 2026-05-22
**Scope:** make `remove_node_instance` and `kill_node --purge_instance=true` actually tabula-rasa the node's persistent state across orchestrator, identity, vault, and (read-only) routing config. Today the filesystem is wiped but identity, ilk mapping, and dedicated vault secrets leak across recreations, so recreating a node with the same `node_name` inherits stale state and fails.

## 0. Principle

`purge_instance=true` (whether triggered by `kill_node --purge_instance=true` or by `remove_node_instance`) means: every piece of state that the orchestrator owns about this `node_name` is removed. Recreating the same `node_name` afterwards must boot as a fresh first spawn.

State that the orchestrator does NOT own (the runtime package in `dist/`, the operator-managed routes/vpns/taps that mention the node, OPA bundles, workflow definitions) is not removed automatically — but the operator is warned about visible dangling references so they can decide.

## 1. Inventory of stale state today

| What | Where it lives | Removed today on purge? |
|---|---|---|
| Node files directory (`config.json`, local `secrets.json`, runtime state) | `/var/lib/fluxbee/nodes/<KIND>/<node_name>/` | Yes |
| Empty kind dir (`/var/lib/fluxbee/nodes/<KIND>/`) | filesystem | Yes (if empty) |
| Timers owned by the node | SY.timer DB | Yes (`purge_owner_timers_before_teardown`) |
| systemd unit state | systemd | Yes (`stop` + `reset-failed`) |
| Orchestrator `node_ilk_map` entry (`node_name → ilk_id, tenant_id`) | orchestrator state dir | **No** |
| ILK record in SY.identity (entry + definition with personality/role/skill/handbook hashes) | identity DB + identity SHM | **No** |
| ICH mappings + aliases tied to that ILK | identity DB | **No** |
| Vault secrets dedicated to that ILK (`metadata.ilk = "ilk:<old>"`) | vault.db | **No** |
| Routes/VPNs/Taps that mention the node by L2 name | jsr-config SHM (via SY.config.routes) | **No** (depends on operator — and likely should remain operator's call) |

The rows marked **No** are the leak surface. Tasks below close them.

## 2. Tasks

### [x] NTC-1 — Orchestrator removes its own `node_ilk_map` entry

**Why:** when the operator recreates a node with the same `node_name`, `load_persisted_node_identity` returns the old (ilk_id, tenant_id), so the new spawn reuses an ILK that points to potentially invalid prior state.

**Where:** `src/bin/sy_orchestrator.rs`

**Plan:**

- Add a helper `remove_node_ilk_mapping(state: &OrchestratorState, node_name: &str) -> Result<bool, OrchestratorError>` that loads the map, removes `node_name` from both `map.nodes` and `map.tenants`, and saves. Returns true if anything was actually removed.
- Wire it into `remove_node_instance_flow` after `remove_node_instance_dir_with_root` succeeds.
- Wire it into `kill_node_flow` when `purge_instance == true`, after the dir removal block.
- Include the result in the JSON response (`"ilk_mapping_removed": true|false`) so the operator (and archi) see the cleanup.

**Acceptance:**

- After `remove_node_instance`, `load_persisted_node_identity(node_name)` returns `(None, None)`.
- Unit test for `remove_node_ilk_mapping` covering: present → removed; absent → no-op returning false.

### [x] NTC-2 — Expose `delete_ilk` as a SY.identity admin action

**Why:** `IdentityDelta::IlkDelete` exists internally but is not callable through admin. We need a request/response path so the orchestrator can remove an ILK after the node is torn down.

**Where:** `src/bin/sy_identity.rs`, `src/bin/sy_admin.rs`, optionally `src/bin/sy_architect.rs` (allowlist for `list_taps`-style read).

**Plan:**

- Define a `MSG_ILK_DELETE` constant in sy_identity (e.g. `"ILK_DELETE"`).
- In sy_identity handler: accept the new system message, parse `ilk_id`, refuse to delete `well_known_system_ilks` (deterministic SY ilks). Apply locally:
  - Compute the `IdentityDelta::IlkDelete { ilk_id }`.
  - Use the same apply path used by `IlkUpsert` (so the store, DB, and SHM converge).
  - If primary, broadcast the delta to replicas.
- In sy_admin: add new internal action `delete_ilk` wired through `handle_identity_command` (same pattern as `set_ilk_definition`). HTTP path: `DELETE /hives/{hive}/identity/ilks/{ilk_id}`. Mark as `requires_confirmation = true`. Tone-matched admin help summary (one or two sentences, no operator-vocabulary leakage).
- SCMD translator (`translate_scmd` in sy_architect): add `("DELETE", ["hives", hive_id, "identity", "ilks", ilk_id]) => delete_ilk`.

**Acceptance:**

- `DELETE /hives/motherbee/identity/ilks/ilk:<uuid>` returns ok and the ilk is gone from `list_ilks` / `get_ilk`.
- Attempting to delete a well-known SY ilk returns a specific error code (`SYSTEM_ILK_PROTECTED` or similar).
- Unit test for the well-known protection.

### [x] NTC-3 — Vault: list+delete secrets dedicated to a given ILK

**Why:** if a secret was put with `metadata.ilk = "ilk:<old>"`, it lingers in vault after the node is gone. Anyone who later sees the ilk_id reused would inherit access to a credential they should not see.

**Where:** vault already has `vault_list` (with filter) and `vault_delete` (by key). We do not need new vault primitives — only the orchestrator-side helper that uses them.

**Transport decision (2026-05-22):** the orchestrator routes both list and delete through `SY.admin@<hive>` using `admin_command(...)`. Rationale: `vault_delete` requires the caller to be a well-known admin (`SY.admin`/`SY.architect`) or the secret's owner ILK. At teardown time the doomed ILK is already gone (NTC-2 ran first), so the orchestrator cannot impersonate it; routing through admin reuses the well-known admin trust without expanding orchestrator's vault privileges.

**Plan:**

- In `sy_orchestrator.rs`, add an async helper `purge_vault_secrets_for_ilk(state, ilk_id, target_hive)` that:
  - Sends `VAULT_LIST` with filter `{ ilk: "ilk:<…>" }` to `SY.vault@<hive>`.
  - For each returned secret key, sends `VAULT_DELETE`.
  - Returns a summary `{ scanned: N, deleted: M, errors: [...] }`.
- Caller will be NTC-5 (the orchestrator teardown orchestration). Not wired automatically into anything else.

**Acceptance:**

- Helper returns the expected summary when no secrets exist (zero).
- Helper deletes only secrets that match the dedicated ILK; pool secrets (ilk=null) untouched.
- Unit/integration test if possible (likely needs the existing vault test harness).

### [x] NTC-4 — Routing config visibility: enumerate references to the node, do not auto-delete

**Why:** routes/vpns/taps that name the node by L2 may be operator-intentional; auto-deleting them is too aggressive. But the operator should see them after the teardown so they can decide.

**Where:** `src/bin/sy_orchestrator.rs` — read-only enumeration only; the teardown summary includes the list.

**Plan:**

- Helper `enumerate_routing_references_to(state, node_name) -> RoutingReferencesSummary` returning:
  - `routes: Vec<{prefix, action, next_hop_hive}>` where the prefix matches the node_name or its kind
  - `vpns: Vec<{pattern, vpn_id}>` whose pattern matches
  - `taps: Vec<{match_src, match_dst, target}>` where any of the three fields equals the node_name
- Reads the live ConfigSnapshot (via the existing config SHM reader) for the target hive.
- Returns empty lists when there are no references.

**Acceptance:**

- Removing a node that has 0 routes/vpns/taps returns empty lists.
- Removing a node referenced by 2 taps returns those 2 taps; the routes/vpns/taps themselves are NOT removed by orchestrator.

### [x] NTC-5 — Stitch all of the above into `remove_node_instance_flow` and `kill_node_flow` (purge path)

**Why:** none of the cleanup is useful unless the teardown flow actually invokes the new helpers, in a defined order, and reports the result honestly.

**Where:** `src/bin/sy_orchestrator.rs`

**Order of operations within the purge path (after the systemd unit is stopped):**

1. `purge_owner_timers_before_teardown` — already there.
2. `remove_node_instance_dir_with_root` — already there.
3. `(NTC-1) remove_node_ilk_mapping`.
4. `(NTC-2) delete_ilk` against SY.identity (using the ilk_id read **before** step 3, since step 3 erases the mapping). Skip silently if the ilk wasn't registered (idempotent).
5. `(NTC-3) purge_vault_secrets_for_ilk` against SY.vault.
6. `(NTC-4) enumerate_routing_references_to` — read-only summary added to the response.

**Behavior on partial failure:**

- Steps 1–3 must succeed for the response to be `status: "ok"`.
- Step 4 (ILK delete) failure → response `status: "ok"` but include `ilk_delete_error: "..."` so the operator sees the leak. (Identity DB orphan is tolerable; reuse is not.)
- Step 5 (vault delete) similar — partial failure is captured as an `error_list` field, not a top-level failure.
- Step 6 is informational; never fails the response.

**Acceptance:**

- Full happy-path teardown returns a single JSON with: filesystem result, timer purge result, ilk_mapping_removed, ilk_deleted, vault_secrets_purged{scanned, deleted, errors}, routing_references{routes, vpns, taps}.
- Recreating a node with the same `node_name` after `status: ok` teardown spawns with a fresh ILK (`load_persisted_node_identity` returns `(None, None)`, `ILK_REGISTER` triggers).

### [x] NTC-6 — Tests (incremental, attached to each task)

Per-task unit tests are listed in NTC-1..NTC-4. Plus one end-to-end test:

- E2E: `create AI.demo@motherbee` → set definition with personality hash → put dedicated vault secret with `metadata.ilk = ilk_of_demo` → kill_node --purge_instance=true → assert that `node_ilk_map` is clean, ILK is gone in SY.identity, the dedicated vault secret is gone, and the response carries a routing reference summary. Recreate `AI.demo@motherbee` with `run_node` and verify a fresh ILK is assigned.
- Harness: `scripts/node_teardown_completeness_e2e.sh` covers the full flow. Live execution requires a running admin endpoint and a usable `ai.generic` runtime.

### [x] NTC-7 — Docs

- `admin_help_reference.md`: add the new `delete_ilk` action under the identity category, terse.
- `handbook_fluxbee.md`: short bullet under §4 explaining that `purge_instance=true` is a real tabula-rasa (ILK + dedicated vault secrets removed; routes/vpns/taps stay and are surfaced as a summary). No new sub-section unless the operator vocabulary requires it.
- This task file: tick off boxes as work lands.

### [x] NTC-8 — `delete_ilk` also removes ILK aliases everywhere

**Finding:** `delete_ilk` removes the ILK record and ICH mappings, but aliases tied to the deleted ILK can remain in memory/SHM even though the DB cleanup removes them.

**Plan:**

- In `IdentityStore::delete_ilk`, remove aliases where `old_ilk_id == deleted_ilk_id` or `canonical_ilk_id == deleted_ilk_id`.
- In `IdentityStore::apply_delta(IdentityDelta::IlkDelete)`, apply the same alias retention so replicas converge.
- Ensure SHM no longer exposes stale aliases after `IlkDelete` (full snapshot rebuild is acceptable for this infrequent admin/purge path).
- Add unit coverage for old-id and canonical aliases.

### [x] NTC-9 — `kill_node --purge_instance=true` must purge stopped instances

**Finding:** `kill_node` returns `not_found` before purge cleanup when systemd says the unit is inactive. A stopped/crashed node can still have instance files, `node_ilk_map`, identity ILK, and vault secrets.

**Plan:**

- When `purge_instance=true` and a normalized `node_name` is present, allow the purge path to run even if `systemd_unit_is_active` returns false.
- Preserve honest response fields (`state/not_found` or stopped status), but still report filesystem, mapping, ILK, vault, and routing summaries.
- Add unit coverage if the flow can be isolated; otherwise rely on the E2E harness.

### [x] NTC-10 — Format modified Rust bins

**Finding:** `rustfmt --edition 2021 --check src/bin/sy_identity.rs src/bin/sy_admin.rs src/bin/sy_architect.rs src/bin/sy_orchestrator.rs` currently reports formatting diffs in modified files.

**Plan:**

- Run rustfmt on touched Rust files or apply the minimal formatting changes manually.
- Re-run the targeted tests plus `rustfmt --check`.

### [x] NTC-11 — Map new identity errors to operator-friendly HTTP statuses

**Finding:** `ILK_NOT_FOUND` and `SYSTEM_ILK_PROTECTED` are surfaced in payloads, but `sy_admin` falls back to HTTP 500 because they are not mapped in `error_code_to_http_status`.

**Plan:**

- Map `ILK_NOT_FOUND` to 404.
- Map `SYSTEM_ILK_PROTECTED` to a non-500 protected-resource status, preferably 403.
- Add narrow tests if existing admin HTTP status helpers are covered locally.

## 3. Suggested execution order

1. NTC-1 (simplest, immediately removes one source of confusion)
2. NTC-2 (needs the action surface)
3. NTC-3 (depends on having the ilk_id at teardown time, but can be developed in parallel)
4. NTC-4 (read-only, independent)
5. NTC-5 (the integration step that wires the previous four)
6. NTC-6 (E2E sweep)
7. NTC-7 (docs, last)
8. NTC-8..NTC-11 (review follow-ups before final sign-off)

## 4. Out of scope (for now)

- GC of orphan ILKs already in identity from earlier sessions (separate task — could be a later `gc_orphan_ilks` admin action).
- Auto-deleting routes/vpns/taps that reference a removed node (operator-intent question; can be revisited if a real case demands it).
- Cross-hive coordination when an ILK is registered on motherbee but the node lives in a worker — current code already handles cross-hive teardown via `forward_system_action_to_hive`; ensure NTC-5 follows the same pattern.
- Runtime artifacts in `dist/` — they are runtime-scoped, not node-scoped; the existing `remove_runtime_version` action covers that surface separately.
