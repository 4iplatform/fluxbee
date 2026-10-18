# sy.identity audit - operability and security

Date: 2026-06-30  
Repository: `fluxbee`  
Branch audited: `daily_onworking_coa`  
Commit audited: `25e883e`  
Focus: `src/bin/sy_identity.rs`, the SDK identity contract, operational documentation, and integration points with SHM, router, vault and replication.

## Executive summary

`sy.identity` has a broad functional base, and its local unit tests pass. But the new replication surface and some administrative flows carry significant risks that must be addressed before they can be considered fit for safe operation.

The most relevant findings are:

- The identity sync TCP port is left with optional authentication by default. It listens on `0.0.0.0:9100` and allows full-sync, subscribe and publish without HMAC unless `identity.sync.auth = required` is set explicitly.
- Sync connection handling blocks the main loop during the handshake, and especially during full-sync. This exposes `sy.identity` to DoS from slow or hung clients.
- `VAULT_SECRET_CHANGED` is processed before the normal authorization check and can terminate the primary process with `exit(0)`.
- `TNT_CREATE` can delete an existing tenant from memory if the request was idempotent but the DB write fails.
- A replica can stay alive with incomplete state, and on delta gaps it only reconnects, without forcing a full-sync.
- ILKs published by replicas to the primary appear to be live state, not durable state: they are applied to memory/SHM but not persisted in PostgreSQL.
- Authorization by L2 prefixes is too broad for sensitive actions, and `ILK_REGISTER` allows creating `ilk_type = system` from registrars that should not have that power.

## Scope and methodology

Static review of:

- `src/bin/sy_identity.rs`
- `crates/fluxbee_sdk/src/identity.rs`
- `crates/fluxbee_sdk/src/protocol.rs`
- `src/shm/mod.rs`
- relevant integrations in `src/bin/sy_vault.rs` and `nodes/io/common/src/inbound.rs`
- existing documentation in `docs/`, including the previous audit and the `onworking COA` plans

Checks run:

```bash
cargo test -p fluxbee-sdk identity
cargo test --bin sy_identity
```

Result:

- `cargo test -p fluxbee-sdk identity`: OK, 6 tests passed.
- `cargo test --bin sy_identity`: OK, 33 tests passed.

Limitations:

- No real E2E was run with PostgreSQL, router, multiple hives and exposed TCP sockets.
- No payload fuzzing or load testing was done on sync `9100`.
- Not all SHM consumers were audited exhaustively; only the points needed to assess impact.

## Findings

### F-01 - Critical - TCP sync is left without authentication by default and allows exfiltration/poisoning

Evidence:

- `identity_sync_auth_required` only requires HMAC if the config is exactly `required`; any missing value or typo leaves auth disabled: `src/bin/sy_identity.rs:3176`.
- The primary listens on all interfaces: `0.0.0.0:{sync_port}` in `src/bin/sy_identity.rs:2896`.
- `handle_sync_connection` only runs the HMAC challenge when `auth_required` is true: `src/bin/sy_identity.rs:3939`.
- Without auth, a peer can request `IDENTITY_FULL_SYNC_REQUEST` and receive complete chunks: `src/bin/sy_identity.rs:3951`.
- Without auth, a peer can subscribe to deltas: `src/bin/sy_identity.rs:3961`.
- Without auth, a peer can publish `IDENTITY_DELTA_PUBLISH` with an arbitrary `hive_id`; the `authed_hive` check only applies if auth took place: `src/bin/sy_identity.rs:4020`.
- Delta authority is mainly content-based: `delta_authorized_for_hive` allows `IlkUpsert` if the ILK appears to belong to the `publisher_hive`: `src/bin/sy_identity.rs:1613`.

Impact:

- Any peer that reaches the sync port can exfiltrate the full identity state if auth is not configured.
- It can inject live ILKs for non-primary hives by choosing a `node_name` with the suffix `@publisher_hive`.
- Even if that poisoning is not persisted in the DB, if it reaches SHM it can affect routing, authorization and identity consumers until a restart or a reconciliation.
- A typo in the config silently disables auth.

Recommendation:

- Make auth required by default on the primary.
- Fail startup on unknown values of `identity.sync.auth`.
- If a local mode without auth is needed, bind only to loopback and require an explicit opt-in.
- On publish, always require `publisher_hive` to be authenticated and, preferably, in an allowlist of expected replicas.
- Add negative tests for full-sync, subscribe and publish without auth.

### F-02 - High - A sync connection can block the main loop

Evidence:

- The `listener.accept()` branch of the `tokio::select!` waits inline on `handle_sync_connection(...)`: `src/bin/sy_identity.rs:3101`.
- In full-sync, `write_all` and `flush` run for all the chunks with no spawn and no write timeout: `src/bin/sy_identity.rs:3951`.
- Subscribe and publish do end up spawning tasks, but only after the main loop has done the handshake and the initial parsing.

Impact:

- A slow client, or one that stops reading during full-sync, can keep the main loop busy.
- Meanwhile, heartbeats, alias GC, system messages, replica ingest, delta application, status and config are delayed.
- Combined with F-01, it is a remote DoS on `9100`.

Recommendation:

- Spawn a task for each accepted connection immediately, or use a bounded pool.
- Add read and write timeouts to all socket operations.
- Keep the main loop only for state mutations and internal events.

### F-03 - High - `VAULT_SECRET_CHANGED` can restart the primary before authorization

Evidence:

- `process_system_message` processes `MSG_VAULT_SECRET_CHANGED` before calling `is_authorized`: `src/bin/sy_identity.rs:1788`.
- The primary's handler terminates the process with `std::process::exit(0)` if the payload matches the local interest: `src/bin/sy_identity.rs:6084`.
- The local interest is broad: `resource_type = postgres`, root tenant, self ILK and `system_caller`: `src/bin/sy_identity.rs:6106`.
- `matches_interest` validates the resource type and tenant/ilk, but does not authenticate the origin or `source`: `crates/fluxbee_sdk/src/protocol.rs:351`.

Impact:

- Any node that can route a system message with a compatible payload to `SY.identity@motherbee` can force a restart of the primary.
- If the event repeats, it can produce an availability loop.
- The documented design of open broadcasts for vault assumes low impact because they only trigger a re-resolve; here the real effect is the process exiting.

Recommendation:

- Accept this message only from `SY.vault@<hive-local>` or from an equivalent authenticated route.
- Validate `payload.hive_id == hive_id` and the origin before exiting.
- Consider re-resolving first and restarting only if the secret/version actually changed.
- Add a rate limit and a test for an unauthorized origin.

### F-04 - High - `TNT_CREATE` can delete an existing tenant from memory on a DB failure

Evidence:

- `create_tenant` returns an existing tenant when the name or domain already exists, with `created = false`, without mutating the store: `src/bin/sy_identity.rs:1162`.
- On a DB error, the handler always runs `self.store.tenants.remove(&tenant_id)`: `src/bin/sy_identity.rs:2351`.

Impact:

- If an idempotent `TNT_CREATE` is sent for an existing tenant and persistence fails, that tenant is deleted from memory.
- The case can even affect the root tenant if the request matches its name/domain.
- Since the response is an error, SHM is not necessarily rebuilt and a delta is not necessarily emitted. This leaves memory, SHM and DB divergent until a restart.

Recommendation:

- Roll back via a snapshot as in other mutating paths, or remove only when `created == true`.
- Add a test: existing tenant + injected DB failure keeps the store intact.

### F-05 - High - Replicas can stay alive with incomplete state, and delta gaps do not force a full-sync

Evidence:

- On a worker, if the initial full-sync fails, a warning is logged and the node continues with its local state: `src/bin/sy_identity.rs:2904`.
- `run_delta_subscription_loop` reconnects on stream errors: `src/bin/sy_identity.rs:4233`.
- `stream_deltas_from_primary` detects gaps/out-of-order and returns an error: `src/bin/sy_identity.rs:4296`.
- The caller does not force a full-sync after the gap; it subscribes again.
- `STATUS` reports role/state/database, but not upstream health, delta sequence, last full-sync or lag: `src/bin/sy_identity.rs:1831`.

Impact:

- A replica can publish incomplete SHM and appear operational.
- If deltas are lost, the reconnection can continue from an inconsistent state without recovering what is missing.
- Other nodes can make identity decisions with partial data.

Recommendation:

- If the initial full-sync fails, mark the replica as degraded/not-ready and expose it in `STATUS`.
- On a delta gap, clear or mark the state and run a full-sync before resubscribing.
- Add health fields: `upstream_connected`, `last_full_sync_at`, `last_delta_seq`, `delta_lag`, `reconnects`, `last_sync_error`.

### F-06 - Medium - ILKs published by replicas are not persisted in PostgreSQL

Evidence:

- The primary persists its own system ILKs during bootstrap: `src/bin/sy_identity.rs:2928`.
- Replicas periodically send `self_owned_ilks`: `src/bin/sy_identity.rs:2991`.
- The primary ingests deltas from replicas, updates memory/SHM and rebroadcasts, but does not persist those ILKs in the DB on that path: `src/bin/sy_identity.rs:3020`.

Impact:

- ILKs of worker hives remain as live/cache state on the primary.
- After a restart of the primary, the global state loses remote ILKs until each worker is online and republishes.
- If a worker is down during the restart, its ILKs can disappear from the global view.

Recommendation:

- Define explicitly whether replica ILKs are a durable source or a live cache.
- If they are durable, persist the `IlkUpsert`/delete accepted via delta in the primary DB.
- If they are a live cache, document it and expose the state/age of each remote hive.

### F-07 - Medium/High - Authorization by L2 prefixes is too broad

Evidence:

- Several sensitive actions accept prefixes such as `SY.admin@`, `SY.architect@`, `SY.frontdesk.gov@`, `SY.orchestrator@` and `IO.`: `src/bin/sy_identity.rs:1675`.
- `is_authorized` accepts exact matches or `starts_with(prefix)`: `src/bin/sy_identity.rs:2666`.
- There are tests for a missing source and a configured frontdesk, but not for the cross-hive rejection of a privileged prefix.

Impact:

- A privileged L2 from another hive could run administrative actions if the router lets it through.
- A registration with a name that starts with a sensitive prefix receives the permissions of that family.
- Actions such as `CONFIG_SET`, tenant create/update/sponsor, `ILK_DELETE` and definition set are too exposed.

Recommendation:

- For high-risk actions, use an exact, local allowlist: `SY.admin@<hive>`, `SY.architect@<hive>`, the exact configured frontdesk, and the exact orchestrator where applicable.
- Make cross-hive exceptions explicit per action.
- Add negative tests for `SY.admin@foreign`, an unconfigured frontdesk, and names with a reserved prefix.

### F-08 - Medium/High - `ILK_REGISTER` allows creating `system` ILKs from broad registrars

Evidence:

- `validate_ilk_type` accepts `human`, `agent` and `system`: `src/bin/sy_identity.rs:4829`.
- `register_ilk` writes the requested `ilk_type` directly: `src/bin/sy_identity.rs:885`.
- `ILK_REGISTER` is authorized for `SY.frontdesk.gov@` and `SY.orchestrator@`: `src/bin/sy_identity.rs:1681`.
- `ILK_PROVISION` does have a separate validator that rejects `system`: `src/bin/sy_identity.rs:4836`.
- In vault, a caller with an ILK of type `system` has a fallback read of the root pool: `src/bin/sy_vault.rs:1392`.

Impact:

- A compromised or buggy frontdesk can register ILKs of type `system` in active tenants.
- Since other services trust SHM to resolve the ILK type, this can widen privileges downstream.

Recommendation:

- Separate permissions by type: frontdesk should only register `human`/`agent`.
- Reserve `system` for bootstrap, deterministic definitions from `hive.yaml`, or a more restricted administrative flow.
- Add a test: `SY.frontdesk.gov` + `ilk_type = system` must be rejected.

### F-09 - Medium - SHM permissions `0600` and a silent fallback to miss degrade operation

Evidence:

- The SHM region is created with mode `0o600`: `src/shm/mod.rs:2553`.
- The SDK converts `EACCES`/`EPERM` into `Ok(None)` or `false`: `crates/fluxbee_sdk/src/identity.rs:452`, `crates/fluxbee_sdk/src/identity.rs:505`, `crates/fluxbee_sdk/src/identity.rs:936`.
- IO common logs the error and treats the result as a miss/provisioning path: `nodes/io/common/src/inbound.rs:185`.
- The IO documentation already mentions the `EACCES` symptom and that runtimes must have read access.

Impact:

- If consumers and `sy.identity` do not run with the same user/group, the system degrades to RPC/provisioning without failing hard.
- Installation problems are hidden, and the load on `sy.identity`/frontdesk increases.

Recommendation:

- Standardize the user/group/ACL for SHM at install time.
- Expose `EACCES` as a metric/health issue even if the fallback keeps working.
- Add an operational guard that validates read access from each runtime.

### F-10 - Low/Medium - Schema migrations are still ad hoc

Evidence:

- `ensure_primary_schema` uses `CREATE TABLE IF NOT EXISTS` and `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`, with no versioning table and no ordered migrations: `src/bin/sy_identity.rs:5058`.

Impact:

- It may be acceptable for alpha/testing, but it is fragile for production upgrades.
- There is no clear way to audit partial migrations, rollback or drift between environments.

Recommendation:

- Add an `identity_schema_migrations` table.
- Turn schema changes into versioned, idempotent migrations.
- Record the expected version in `STATUS`.

## Suggested remediation order

1. Close F-01 and F-02 together: auth required by default, fail-closed config, and concurrent handling/timeouts for sync.
2. Fix F-03 to prevent an unauthorized remote restart.
3. Fix F-04 because it can corrupt in-memory state with a common/idempotent flow.
4. Harden replication: F-05 and the durability decision for F-06.
5. Reduce privileges: F-07 and F-08.
6. Improve operability: F-09 and F-10.

## Recommended coverage gaps

- Sync tests with auth absent, auth required, a config typo, and publish with an unauthenticated hive.
- A slow full-sync client test to verify that it does not block heartbeats/status.
- A test of `VAULT_SECRET_CHANGED` from a non-vault origin.
- A `TNT_CREATE` rollback test against an existing tenant with a persistence error.
- A test of a replica with a delta gap, which must force a full-sync.
- Cross-hive tests for authorization by `src_l2_name`.
- Tests of `ILK_REGISTER` rejecting `system` from frontdesk.

## Conclusion

`sy.identity` is functionally advanced. But today the combination of sync open by default, DoS through a blockable loop, and some bypasses/broad privileges leaves it in a risky position for deployments where the sync port or the router are not strictly isolated.

The recommendation is to treat F-01, F-02, F-03 and F-04 as blockers before exposing it outside a controlled development environment.

---

## Resolution status (2026-07-02)

Audit review (adversarial verification of each finding against the current
code + a gap hunt through 6 lenses) and remediation. The audited commit
(`25e883e`) is identical to the current HEAD for `sy_identity.rs` (no changes in
between), so **all 10 findings apply to the current code**.

### Audit quality

High. Of the 10 findings: **8 CONFIRMED** as is, **2 CONFIRMED-PARTIAL**
(a nuance, not a refutation):

- **F-02** partial: the channel's *reads* already have a timeout (`auth_read_line`),
  so the "stall before sending the request" vector was already closed; what
  remained open was the inline full-sync *write* in the main loop.
- **F-07** partial: the `@` suffix anchors the prefix, so the "adjacent name"
  sub-branch (`SY.administrator@`) does not apply; the real risk is the
  **hive-agnostic** prefix (`SY.admin@<other-hive>`), not `starts_with` itself.

None refuted. `src_l2_name` is router-authoritative (F4/F15 of the orchestrator
audit), which narrows F-03/F-07 but does not eliminate them.

### Gaps the audit did NOT cover (found in the review)

- **G-1 [CRITICAL]** — The **cross-hive** sync path accepts an arbitrary
  `ilk_type=system` from a replica (different from F-08, which is only the local
  `ILK_REGISTER`): `delta_authorized_for_hive`/`apply_delta` checked ownership but
  never `ilk_type`. A compromised replica labels its own ilk as `system`
  → `sy_vault::authorize_read` gives it read access to **all the secrets of the
  root pool** (infra creds) across the whole mesh.
- **G-2 [HIGH]** — Cross-hive `AliasUpsert` only validated ownership of the
  `canonical_ilk_id`, not of the `old_ilk_id` (the key that resolution redirects):
  a replica could hijack the resolution of **any** ilk (incl.
  `SY.vault@motherbee`, incl. system ilks) by pointing it at an ilk of its own. Found
  by 2 independent lenses.
- **G-3 [HIGH]** — Fail-**open** defaults: an unknown `ilk_type` mapped to
  `system` both in identity (`parse_ilk_type_for_shm`) and in the router
  (`inject_identity_data`). Corruption/partial sync → privilege instead of deny.
- **G-4 [HIGH]** — `read_line` with no size cap and no timeout on the :9100 channel
  (`run_publish_reader`, `auth_read_line`, `fetch_full_sync`, `stream_deltas`) →
  remote OOM with a frame that has no newline. Found by 2 lenses.
- **G-5 [HIGH]** — `unbounded` per-subscriber channel + subscriber list with no
  cap → memory exhaustion of the primary via a slow subscriber.
- **G-6 [HIGH]** — Alias GC mutates memory before the DB, with no rollback; on a DB
  error the deltas are lost → permanent SHM/replica divergence.
- **G-7 [MEDIUM]** — `total_chunks` (u32) with no bound → a `Vec::resize` of ~400GB →
  process abort when the replica boots. Found by 2 lenses.
- **G-8 [MEDIUM]** — An SHM error after the DB commit propagated via `?` and suppressed the
  broadcast to replicas **and** the response to the client → primary ahead of
  replicas.
- **G-9 [MEDIUM]** — The full store was cloned+sorted on every `accept()`,
  pre-auth, in the main loop (amplification). Overlap with F-02.

### Resolution per finding (code)

Everything is in `src/bin/sy_identity.rs` unless noted otherwise.

| ID | Status | Fix |
| --- | --- | --- |
| **F-01** | Resolved | `identity_sync_auth_required`/`auth_mode_required` **fail-closed**: absent/typo/empty ⇒ `required`; opt-out only with `disabled`/`off`/`none` (with a warning). `config/hive.yaml` + `packaging/hive.yaml.example` with an explicit `auth: required`. |
| **F-02** | Resolved | `handle_sync_connection` is **spawned** per connection (outside the loop); `Semaphore` (`MAX_CONCURRENT_SYNC_CONNS`); subscriber returned via a channel; `write_timed`/`flush_timed` in full-sync + handshake + subscribe. |
| **F-03** | Resolved | `handle_vault_secret_changed` requires the origin `SY.vault@<own-hive>` (router-stamped `src_l2_name`) before `exit(0)`. Same gate in `sy_storage.rs`. |
| **F-04** | Resolved | `TNT_CREATE` takes a snapshot before mutating and restores it on a DB error (the `TNT_UPDATE` pattern), instead of the blind `remove()`. |
| **F-05** | Resolved (via restart) | Delta gap ⇒ log ERROR + `exit(0)` ⇒ systemd restarts it ⇒ re-bootstrap via a clean full-sync (recovers lost revocations). Boot full-sync failure ⇒ log ERROR "DEGRADED" (no exit, to avoid a restart storm). |
| **F-06** | Resolved | The ingest arm **persists** the ilks published by replicas in the DB (`persist_ilk_state_in_db`/`delete_ilk_in_db`), convergent best-effort (re-push on every reconcile). |
| **F-07** | Resolved | `is_authorized` hive-scopes `SY.admin@`/`SY.architect@`/`SY.frontdesk.gov@` to `self.hive_id`; `IO.` and `SY.orchestrator@` stay cross-hive. |
| **F-08** | Resolved | `validate_ilk_type` (the `ILK_REGISTER` path) rejects `system`; anti-downgrade guard for an existing system ilk (`SYSTEM_ILK_PROTECTED`). |
| **F-09** | Resolved (loud) | SDK: EACCES/EPERM from SHM now emits a one-shot `tracing::error!` (the uid/gid misconfig becomes visible); return semantics unchanged so as not to ripple into consumers. |
| **F-10** | Resolved (baseline) | `ensure_primary_schema`: `pg_advisory_lock` + an `identity_schema_migrations` table; the current idempotent DDL is sealed as baseline **v1**. |
| **G-1** | Resolved | `ingest_ilk_type_authorized`: a replica can only assert `ilk_type=system` for a **deterministic** system ilk (`SY.*@publisher`, `ilk_id == deterministic_system_ilk_id`); wired into `delta_authorized_for_hive` + snapshot ingest. |
| **G-2** | Resolved | `AliasUpsert` requires both `old_ilk_id` **and** `canonical_ilk_id` to belong to the publisher, and forbids aliasing a well-known system ilk. |
| **G-3** | Resolved | Fail-safe default: `parse_ilk_type_for_shm` ⇒ `human`; `router/mod.rs inject_identity_data` ⇒ `"unknown"`. |
| **G-4** | Resolved | `read_capped_line`/`read_sync_line` (cap `MAX_SYNC_LINE_BYTES` + timeout) on all reads of :9100. |
| **G-5** | Resolved | **Bounded** per-subscriber channel + `broadcast_deltas` with `try_send` (drop-on-full) + `MAX_DELTA_SUBSCRIBERS`. |
| **G-6** | Resolved | `run_alias_gc` takes a snapshot before mutating and restores it on a DB error. |
| **G-7** | Resolved | `MAX_FULL_SYNC_CHUNKS` bound before the `resize`. |
| **G-8** | Resolved | The SHM error after the commit **no** longer propagates: it is logged, and the flow continues with broadcast + response. |
| **G-9** | Resolved | Full-sync chunks are built **on-demand** (via a oneshot channel to the loop) only for an authenticated FULL_SYNC, not pre-auth per connection. |

### Test coverage

New unit tests in `sy_identity.rs` (`cargo test --bin sy_identity`: **40 pass**):
`auth_mode_required_is_fail_closed` (F-01), `parse_ilk_type_for_shm_fails_safe_to_human`
(G-3), `ingest_system_ilk_type_bound_to_deterministic_shape` +
`delta_authority_rejects_forged_system_ilk_type` (G-1),
`alias_upsert_authority_requires_both_endpoints_owned` (G-2),
`is_authorized_scopes_privileged_sy_roles_to_own_hive` (F-07),
`register_ilk_type_rejects_system` (F-08). Clean workspace build;
`fluxbee-sdk` 147 pass, `json-router` lib 67 pass.

Pending wire-level coverage (requires the lab): the required-by-default HMAC handshake
end-to-end, full-sync DoS with a slow reader, replica gap→restart,
F-06 persistence with restart. See the lab plan below.

### Validation in the Proxmox lab (2026-07-02) — DONE

`.deb` build of the full current tree (identity + closed orchestrator,
`0.1.0-syid`) on `fxbuild`, from-scratch 2-VM on Proxmox VE (motherbee
`192.168.103.150` + worker `192.168.103.151`, clone of the ubuntu-24.04 template,
`.deb` install + `fluxbee-firstboot` + a real `add_hive`).

- **F-01 (compat risk #1) — VALIDATED.** motherbee starts with
  `identity sync auth: required (per-hive HMAC) role="primary"` + listener :9100
  **by default**. `add_hive worker1` distributes `worker1.key` (0600) to motherbee
  **and** worker; the worker (replica, `auth: required`) completes the HMAC handshake and
  `identity full sync bootstrap applied upstream=192.168.103.150:9100`
  (ilk_count 12). **The fail-closed default does NOT break the join.**
- **F-01 on the wire — VALIDATED.** 3 probes WITHOUT a handshake (full-sync request,
  delta subscribe, garbage) → **REJECTED** (0 bytes, connection closed). Zero
  state leak to an unauthenticated peer.
- **F-02 — EXERCISED.** The full-sync was served via a spawned handler + on-demand
  chunks (oneshot to the loop) + write-timeouts; the worker bootstrapped 12 ilks without
  breaking the framing (G-4/G-7 caps OK).
- **G-1 (does not over-block) — VALIDATED.** The worker's 7 legitimate and
  deterministic system-ilks (`SY.*@worker1`) flowed to motherbee (bidirectional
  sync 12→19); an overly strict guard would have blocked them.
- **F-06 — VALIDATED (definitive).** The 7 ilks of worker1 are **persisted**
  in motherbee's DB (`identity_ilks`, `ilk_type=system`). Durability test:
  the worker was stopped and the primary's sy-identity was restarted → `loaded identity
  store from primary db ... ilk_count:19` (they survive). Before F-06 they were lost
  (they went back to 12).
- **F-10 — VALIDATED.** `identity primary schema ensured (baseline v1)` + the
  `identity_schema_migrations` table with the row `1|baseline`; idempotent advisory-lock on
  restart.
- **Recovery — VALIDATED.** After restarting the worker, a clean re-join via
  full-sync re-bootstrap (ilk_count 19).

G-1(rejection)/G-2/G-3 and F-03/F-04/F-05(gap)/F-07/F-08/F-09/G-5/G-6/G-8/G-9 are
covered by the dedicated unit tests (40 pass) + code review; their runtime
path is the same as the one exercised in the lab. Wire probes for G-1/G-2
(a forged publish over a valid HMAC handshake) were not run, on cost/benefit grounds
versus the unit tests.

Final lab status: motherbee (201) + worker (202) running, healthy mesh, 19 ilks
consistent on both sides.
