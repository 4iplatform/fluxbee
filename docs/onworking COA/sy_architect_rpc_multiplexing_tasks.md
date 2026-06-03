# SY.architect RPC canonicalization tasks

Date: 2026-05-24 (created)
Updated: 2026-05-24 (review feedback applied: Scope explicit, Section F auth added, profile rules tightened, "no legacy / no shims" stance pinned)
Status: Open
Related: `docs/onworking COA/orchestrator_internal_rpc_multiplexing_tasks.md`

## Scope — what changes and what does NOT

**In scope:**

- Eliminate `SY.architect.<purpose>.<uuid>` ephemeral router-visible nodes (Section C).
- Eliminate ephemeral vault-lookup connections that announce/withdraw per call (Section D).
- Reassign ownership of the `NodeReceiver` for the canonical `SY.architect@<hive>` connection from the bespoke `router_recv_loop` to the SDK's `RpcClient` (Section B). This is a **prerequisite** for Section C — the same deadlock pattern fixed in ORPC Block C applies here verbatim. If we kept the manual recv loop and added a shared `Arc<RpcClient>` for outbound, we would either (a) create a second router connection with the same canonical name (inventory storm with a different shape), or (b) have two consumers reading the same `NodeReceiver` (race, lost responses, lost inbound). Only Section B avoids both.
- Add origin authorization for inbound system actions on architect (Section F), mirroring the AF-P1 fix applied to orchestrator.

**NOT in scope:**

- **Wire transport.** Socket router (`/var/run/fluxbee/routers/*.sock`) stays. `RpcClient` uses it internally.
- **Message protocol.** `Message` / `Routing` / `Meta` over socket stays.
- **HTTP admin API on architect.** `/admin/...` endpoints stay.
- **Inter-node communication semantics.** Other nodes still talk to architect via `SYSTEM_KIND` / `ADMIN_KIND` messages over the router. The only "comm" change is internal to architect: who owns the receiver loop.

## Implementation stance — no legacy, no shims, no intermediate workarounds

Per `feedback_no_legacy_in_dev`: this migration lands as **a single coherent change**. Explicitly forbidden:

- Compat path that keeps the bespoke `router_recv_loop` alongside the new `Arc<RpcClient>`.
- Transitional flag, `#[cfg(...)]`, or env var to toggle between old and new ephemeral patterns.
- "Migration helper" that wraps `RpcClient::connect_with_retry` per outbound call (would just rename the inventory storm).
- Keeping `execute_admin_action_with_context` building its own `NodeConfig` "for now" and migrating call sites one by one across multiple PRs.
- Keeping `fluxbee_sdk::resolve_resource` ephemeral connect path "until VaultClient lands" — vault migration is part of the same PR (Section D), not a follow-up.

The bespoke recv loop and every per-purpose `NodeConfig { name: format!("SY.architect.{purpose}.{}", Uuid::new_v4().simple()), uuid_mode: Ephemeral, ... }` block are **deleted** in the same PR that introduces the shared `Arc<RpcClient>`. Acceptance gates (Sections A and E) fail if any `SY.architect.<purpose>.<uuid>` pattern remains in source. CI grep guard (ARCH-RPC-A2) keeps the line drawn.

Rationale: this avoids the exact failure mode the ORPC v1 implementation hit. v1 partially migrated, introducing `OrchestratorRouterClient` while leaving handlers awaiting RPCs inline on the bespoke receive loop. That produced a structural deadlock that took two more review passes to detect and a full v2 rewrite to fix. We will not pay that cost twice.

## Goal

Make `SY.architect` use one canonical router identity for all internal RPC work:

```text
SY.architect@<hive>
```

After this change, normal architect activity must not create router-visible helper nodes such as:

```text
SY.architect.status.<uuid>@<hive>
SY.architect.tool.read.<uuid>@<hive>
SY.architect.plan_compiler.help_lookup.<uuid>@<hive>
SY.architect.plan_compiler.cache_refresh.<uuid>@<hive>
SY.architect.plan_compiler.pre_validate.<uuid>@<hive>
SY.architect.executor.validate.<uuid>@<hive>
SY.architect.executor.run.<uuid>@<hive>
```

The target is the same operational stance already reached by `SY.orchestrator` and `SY.admin`: one long-lived `fluxbee_sdk::rpc::RpcClient`, response multiplexing by `trace_id`, and profile-declared operational routing.

## Why this is separate from ORPC

`orchestrator_internal_rpc_multiplexing_tasks.md` solved the SDK and the first two consumers (`SY.orchestrator`, `SY.admin`). `SY.architect` is a separate consumer migration.

The ORPC document previously classified architect one-shot `RpcClient` calls as acceptable because they used the canonical SDK abstraction and were expected to be short-lived. Runtime observation changed the risk model: those one-shot clients are visible in inventory as many `SY.architect.*.<uuid>` nodes. That is operational noise at minimum, and it makes incident review harder because real nodes and transient helper clients share the same inventory surface.

## Current behavior

`SY.architect` currently has two router interaction patterns.

1. A canonical persistent connection:
   - `main` builds `NodeConfig { name: "SY.architect", uuid_mode: Persistent }`.
   - `router_connect_loop` stores a `NodeSender` in `ArchitectState.router_sender`.
   - `router_recv_loop` consumes inbound messages and calls `handle_architect_system_message`.

2. Per-operation outbound RPC clients:
   - `execute_admin_action_with_context` builds `NodeConfig { name: format!("SY.architect.{purpose}.{}", Uuid::new_v4().simple()), uuid_mode: Ephemeral }`.
   - `fetch_inventory_status_data` does the same for `SY.architect.status.<uuid>`.
   - Vault boot lookups use `connect()` with `NodeUuidMode::Ephemeral` and canonical name `SY.architect`.

Observed generated purposes include:

| Purpose | Source |
| --- | --- |
| `status` | `fetch_inventory_status_data` / status refresh loop |
| `tool.read` | `fluxbee_system_get` read path |
| `plan_compiler.cache_refresh` | admin action catalog refresh |
| `plan_compiler.help_lookup` | admin action schema/help lookup |
| `plan_compiler.live_query` | plan compiler read-only live queries |
| `plan_compiler.pre_validate` | executor plan validation during compile |
| `executor.validate` | executor plan validation before execution |
| `executor.run` | executor plan execution |
| `snapshot.*` | pipeline actual-state snapshot collection |

## Findings

- The generated names are expected from current `sy_architect.rs`; they are not created by the new `SY.orchestrator` RPC path.
- The behavior is still undesirable. Inventory should show durable nodes and intentionally managed runtime instances, not internal client connections created for every admin action.
- The SDK work is already mostly done. `fluxbee_sdk::rpc::RpcClient` provides `connect_with_retry`, `send_admin_rpc`, `send_system_rpc`, `send_with_matcher`, pending response routing, stale response filtering, response-only filtering, command channels, broadcast channels, and `RpcTestHarness`.
- No new per-service RPC implementation should be added to architect. It should consume the SDK `RpcClient`.

## Target design

`ArchitectState` owns one shared `Arc<RpcClient>`:

```rust
rpc: OnceLock<Arc<RpcClient>>
```

or, if startup ordering allows it cleanly:

```rust
rpc: Arc<RpcClient>
```

The canonical `RpcClient` uses:

```rust
NodeConfig {
    name: "SY.architect".to_string(),
    uuid_mode: NodeUuidMode::Persistent,
    ...
}
```

### Route profile

Build an architect-specific `OperationalRouteProfile` in `main`.

**Command channels:**

- `system` — `NODE_STATUS_GET`, `CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED` (all `SYSTEM_KIND`).
- `incoming` — non-RPC user-facing traffic that today flows through `router_recv_loop` and is persisted by session id (chat / impersonation flow). Concretely: `user`, `chat`, `text` `msg_type`s.

**`pre_pending_rules` (operational commands that must never be misclassified by a pending matcher):**

- `OneOf { msg_type: SYSTEM_KIND, msgs: [NODE_STATUS_GET, CONFIG_GET, CONFIG_SET, VAULT_SECRET_CHANGED] } -> Command("system")`

**`post_pending_rules` (observational fan-out / broad operational catch-alls, safe because the SDK stale + response-only guards run first):**

- `AnyMsgOfType("user") -> Command("incoming")`
- `AnyMsgOfType("chat") -> Command("incoming")`
- `AnyMsgOfType("text") -> Command("incoming")`

**Explicitly NOT used:** `RouteMatch::Any`. We do not declare a generic catch-all. Anything that does not match the rules above increments `rpc_route_unmatched_total` and is logged at debug — that visibility is the point. If a new `msg_type` shows up in production we want to know, not silently route it into the incoming worker. Adding a new `msg_type` to architect requires adding an explicit rule.

**Rule ordering**: `OneOf` and `Exact` rules go before `AnyMsgOfType` rules within the same table (`pre_pending` or `post_pending`). `OperationalRouteProfileBuilder::build` rejects a broad rule that makes a later rule in the same table unreachable, so misordering is a build-time failure, not a runtime bug.

**ADMIN_COMMAND inbound to architect**: architect does **not** currently process `ADMIN_COMMAND` from the router — those go to `sy_admin`. The profile above intentionally omits a pre_pending rule for `(ADMIN_KIND, MSG_ADMIN_COMMAND)`. If a future change makes architect a target of `ADMIN_COMMAND` from another node, add:

```text
pre_pending:
  - Exact { msg_type: ADMIN_KIND, msg: MSG_ADMIN_COMMAND } -> Command("admin")
```

plus the corresponding `admin` command channel and worker, mirroring the orchestrator profile. Without this rule, a colliding `trace_id` between an outbound architect admin RPC and an inbound `ADMIN_COMMAND` would surface as `InvalidResponse` on the waiter instead of routing the inbound command properly.

**Response-only / stale ordering**: late `ADMIN_COMMAND_RESPONSE` frames from architect's own outbound RPCs are filtered by the SDK's response-only registry **before** the post_pending rules run. They never reach `incoming`. This is the AF-P7 fix in ORPC Iteration 3 (`register_response_only` is called only after a successful send, so failed sends don't pollute the registry).

### Workers

**Delete** `router_connect_loop` / `router_recv_loop` as bespoke receive owners. Per the no-legacy stance: these are not kept "for fallback" or wrapped behind a flag — they are removed in the same PR.

Replace them with worker tasks fed by `RpcClient`:

- `run_architect_system_worker(sender, state, rx)`:
  - per-message origin authorization gate (Section F);
  - dispatches `NODE_STATUS_GET` → `try_handle_default_node_status`;
  - dispatches `CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED` to their handlers.
- `run_architect_incoming_worker(state, rx)`:
  - preserves today's non-system behavior from `router_recv_loop`;
  - logs message metadata;
  - persists `router_message_session_id` messages via `persist_router_incoming_message`.

**Do not await outbound RPCs inside the SDK recv loop.** The recv loop lives inside `RpcClient`; operational work happens in workers that consume from `take_command_receiver("system" | "incoming")`. This is the same invariant that closed ORPC Block C (orchestrator deadlock fix).

**Worker serialization**: each worker processes one message at a time. `VAULT_SECRET_CHANGED` triggers a hot refresh that rebuilds the OpenAI runtime and the messages-db runtime; while that runs, the next system message waits. This is the same trade-off the orchestrator accepted in Block C5 and is acceptable here for the same reasons (lifecycle ordering preservation > latency under load).

### Connection-state surface

`router_connected: AtomicBool` becomes redundant. Derive the same signal from `client.sender_snapshot().is_connected()` (exposed publicly by ORPC Iteration 2 AF-P2a). If a caller needs to wait for reconnect, `client.sender_snapshot().wait_connected().await` (AF-P5 — uses the canonical Tokio `Notify` `enable()` pattern, no lost wakeups).

Per the no-legacy stance: the `router_connected` flag is removed entirely, not kept as a cached field. Any read of "is the router connected" goes through `client.sender_snapshot().is_connected()`.

### Outbound admin RPCs

Refactor `execute_admin_action_with_context` so it **no longer creates a `NodeConfig`** and **no longer calls `RpcClient::connect_with_retry` per action**. The per-call ephemeral block is **deleted**, not refactored into a helper, not wrapped behind a feature flag, not left "for tests" — it is gone.

Replacement:

```rust
let client = Arc::clone(&context.rpc);
client.send_admin_rpc(AdminCommandRequest { ... }).await
```

Every existing call site listed below switches to the shared `Arc<RpcClient>` in the same PR. No phased rollout, no "migrate site by site" — all 8 categories ship together:

- `fluxbee_system_get` / `tool.read`
- `get_or_refresh_admin_actions` / `plan_compiler.cache_refresh`
- `get_admin_action_help` / `plan_compiler.help_lookup`
- `PlanCompilerLiveQueryTool` / `plan_compiler.live_query`
- `validate_plan_with_admin` / `plan_compiler.pre_validate`
- `execute_executor_plan_with_context` / `executor.validate` and `executor.run`
- pipeline snapshot collection (`snapshot.inventory`, `snapshot.nodes`, `snapshot.routes`, etc.)
- status refresh (`fetch_inventory_status_data`)

If any of these call sites resists the migration (e.g., it currently relies on a per-call timeout that does not map cleanly to `send_admin_rpc`), the fix is to update that call site to use the canonical contract — **not** to keep its `connect_with_retry` block as an exception.

### Vault lookups (part of the same PR — not deferred)

Architect today has two paths that use `connect()` with `NodeUuidMode::Ephemeral` for vault `resolve_resource`:

- OpenAI resource lookup ([sy_architect.rs:6174](src/bin/sy_architect.rs#L6174)).
- Postgres/messages-db resource lookup ([sy_architect.rs:6249](src/bin/sy_architect.rs#L6249)).

These are migrated **in the same PR** as Sections B and C. Per the no-legacy stance: the ephemeral `connect()` blocks for vault are deleted alongside the admin-RPC ephemeral blocks. The migration target is one of (decided in design discussion, not deferred):

1. **`VaultClient` over `Arc<RpcClient>`** in `crates/fluxbee_sdk/src/vault.rs`. New struct that wraps `Arc<RpcClient>` and exposes `resolve(...)`. The existing `resolve_resource(&NodeSender, &mut NodeReceiver, ...)` free function is **deleted** from the SDK in the same PR — no parallel API.
2. **Local helper in architect** built over `send_system_rpc` with the canonical `(SYSTEM_KIND, MSG_VAULT_GET)` shape. Acceptable only if the same helper is not needed by other nodes; otherwise (1).

Whichever is chosen, the old `resolve_resource` free function is removed in the same PR. The Pattern 3 inventory table in ORPC Block F is updated to remove the architect rows (and the other 5 sites listed there migrate too, since (1) closes the abstraction for all of them — see ORPC follow-up note "Add `VaultClient` (or `RpcClient::resolve_resource`) to SDK, then migrate the 7 sites").

`VAULT_SECRET_CHANGED` hot-refresh continues to rebuild OpenAI runtime and messages-db runtime, but the rebuild call path no longer announces an ephemeral router connection.

## Task list

### A. Inventory and guardrails

- [ ] ARCH-RPC-A1. Regression test that normal status refresh plus one admin action plus one plan compiler help lookup does not produce any `SY.architect.*` inventory entry other than the canonical `SY.architect@<hive>`.
- [ ] ARCH-RPC-A2. CI guard script `scripts/architect_no_ephemeral_guard.sh` (or equivalent) that runs:
  - `rg "SY\\.architect\\.(status|tool|plan_compiler|executor|snapshot)" src/bin/sy_architect.rs` → must return 0 hits.
  - `rg "Uuid::new_v4\\(\\)\\.simple\\(\\)" src/bin/sy_architect.rs` → must return 0 hits.
  - `rg "NodeUuidMode::Ephemeral" src/bin/sy_architect.rs` → must return 0 hits.
  - `rg "router_connect_loop\\|router_recv_loop\\|router_connected" src/bin/sy_architect.rs` → must return 0 hits.
  - `rg "fluxbee_sdk::resolve_resource\\|resolve_resource\\(" src/bin/sy_architect.rs` → must return 0 hits.

  The script exits non-zero on any hit. Hook it from the same pre-commit / CI job that runs `cargo fmt --check`.

### B. Shared client in architect

- [ ] ARCH-RPC-B1. Store `Arc<RpcClient>` in `ArchitectState` (e.g. `rpc: Arc<RpcClient>`, constructed before `ArchitectState` so no `OnceLock` indirection is needed). The bespoke `router_sender` field is **deleted**.
- [ ] ARCH-RPC-B2. Add `build_architect_rpc_profile()` in `sy_architect.rs` with the explicit `OperationalRouteProfile` from the Route profile section. No `RouteMatch::Any`.
- [ ] ARCH-RPC-B3. **Delete** `router_connect_loop` and `router_recv_loop`. Replace with `run_architect_system_worker` and `run_architect_incoming_worker` consuming from `take_command_receiver("system")` and `take_command_receiver("incoming")` respectively. No bespoke recv loop remains.
- [ ] ARCH-RPC-B4. **Delete** the `router_connected: AtomicBool` field. Any reader (HTTP `/status`, etc.) uses `state.rpc.sender_snapshot().is_connected()` directly. If a reader needs to await reconnect, use `state.rpc.sender_snapshot().wait_connected().await` (AF-P5 wakeup-safe).

### C. Migrate outbound admin calls

- [ ] ARCH-RPC-C1. Extend `ArchitectAdminToolContext` with `rpc: Arc<RpcClient>`. All call sites that construct `ArchitectAdminToolContext` pass the shared client.
- [ ] ARCH-RPC-C2. Refactor `execute_admin_action_with_context` to call `context.rpc.send_admin_rpc(...)`. The function no longer takes `socket_dir` / `state_dir` / `config_dir` for `NodeConfig` construction — those parameters are removed.
- [ ] ARCH-RPC-C3. **Delete** the per-action `NodeConfig { name: format!("SY.architect.{purpose}.{}"...), uuid_mode: Ephemeral }` block. Not refactored into a helper, not behind a flag, gone.
- [ ] ARCH-RPC-C4. Refactor `fetch_inventory_status_data` to reuse the shared client. The `SY.architect.status.<uuid>` `NodeConfig` block is **deleted**.
- [ ] ARCH-RPC-C5. All 8 outbound categories listed in "Outbound admin RPCs" migrate in this PR. No category is deferred. Re-run plan compiler, executor validate/run, live query, tool.read, snapshot collection, and status refresh paths against the shared client.

### D. Migrate vault lookups (no shims left behind)

- [ ] ARCH-RPC-D1. Land `VaultClient` over `Arc<RpcClient>` in `crates/fluxbee_sdk/src/vault.rs` (or local helper over `send_system_rpc` if architect-only — decided in design discussion, not deferred). The choice is made and committed in the same PR.
- [ ] ARCH-RPC-D2. Migrate architect OpenAI lookup. **Delete** the ephemeral `connect()` block at [sy_architect.rs:6174](src/bin/sy_architect.rs#L6174).
- [ ] ARCH-RPC-D3. Migrate architect messages-db Postgres lookup. **Delete** the ephemeral `connect()` block at [sy_architect.rs:6249](src/bin/sy_architect.rs#L6249).
- [ ] ARCH-RPC-D4. **Delete** the free function `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` from the SDK. No parallel API kept.
- [ ] ARCH-RPC-D5. Migrate the other 5 vault-lookup sites listed in ORPC Block F Pattern 3 (`sy_admin:820`, `sy_cognition:1594`, `io-slack:739`, `ai-generic:1682`, `ai-frontdesk-gov:1708`) to the new API in the same PR. Pattern 3 in ORPC closes entirely, not partially.
- [ ] ARCH-RPC-D6. Verify `VAULT_SECRET_CHANGED` hot-refresh in architect still rebuilds OpenAI runtime and messages-db runtime — through the shared `Arc<RpcClient>`, with no ephemeral router connection announced.

### E. Tests and runtime validation

- [ ] ARCH-RPC-E1. Unit-test the architect route profile with `RpcTestHarness`: colliding `ADMIN_COMMAND_RESPONSE` completes/drops as RPC response, while system commands route to `system` and chat/incoming messages route to `incoming`.
- [ ] ARCH-RPC-E2. Unit-test that `CONFIG_GET` / `CONFIG_SET` responses are emitted from the canonical `SY.architect@<hive>` sender.
- [ ] ARCH-RPC-E3. Runtime validation: after restart and one status refresh cycle, `inventory` contains `SY.architect@motherbee` but no `SY.architect.status.*`.
- [ ] ARCH-RPC-E4. Runtime validation: after one plan compiler help lookup and one executor validation, `inventory` contains no `SY.architect.plan_compiler.*` and no `SY.architect.executor.*`.

### F. Origin authorization for inbound system actions (mirrors ORPC AF-P1)

The current architect `router_recv_loop` calls `handle_architect_system_message` without validating the caller. Since architect accepts `CONFIG_SET`, `CONFIG_GET`, `VAULT_SECRET_CHANGED`, and `NODE_STATUS_GET` inbound, any node routable to architect can mutate its config or trigger a vault refresh. This is the same privilege-escalation shape ORPC orchestrator had with `START_NODE` / `RESTART_NODE` before AF-P1. We will not ship the architect migration without closing it.

Single source of truth, modeled on `protected_system_action_response` from `sy_orchestrator.rs`:

```rust
fn protected_architect_system_action_response(action: &str) -> Option<&'static str> {
    match action {
        "NODE_STATUS_GET" => Some("NODE_STATUS_GET_RESPONSE"),
        "CONFIG_GET" => Some("CONFIG_GET_RESPONSE"),
        "CONFIG_SET" => Some("CONFIG_SET_RESPONSE"),
        "VAULT_SECRET_CHANGED" => Some("VAULT_SECRET_CHANGED_RESPONSE"),
        _ => None,
    }
}
```

Allowlist for `is_allowed_architect_source_name(state, src_l2_name)`: at minimum `SY.admin@<hive>`, `SY.config-routes@<hive>`, `SY.vault@<hive>`, plus any `state.architect_allowed_origins` declared in hive config (parallel to `state.system_allowed_origins` in orchestrator). The set is **declared explicitly**, not inferred from prefixes — and any change requires editing the function plus a regression test.

`run_architect_system_worker` runs the gate at the top of the worker per message, before dispatching to the per-action handler. Unauthorized messages emit a `FORBIDDEN` payload via `send_system_action_response(sender, msg, response_name, payload)` and **return** — they do not reach the handler. This is identical in shape to the AF-P1 fix in orchestrator.

- [ ] ARCH-RPC-F1. Implement `protected_architect_system_action_response` covering the 4 actions above.
- [ ] ARCH-RPC-F2. Implement `is_allowed_architect_source_name(state, src_l2_name)` with the explicit allowlist.
- [ ] ARCH-RPC-F3. `run_architect_system_worker` gates each message through `(F1, F2)` before reaching the per-action dispatcher; unauthorized path emits `FORBIDDEN` via the matching `*_RESPONSE` name.
- [ ] ARCH-RPC-F4. Regression tests: one per protected action × `src_l2_name = None` → `FORBIDDEN` response with the matching `*_RESPONSE` name. Mirror `protected_actions_emit_forbidden_response_when_origin_is_unauthorized` from orchestrator's test suite.
- [ ] ARCH-RPC-F5. Unit test that pins the table: `protected_architect_system_action_response_covers_all_4_protected_actions`. Acts as a regression guard if anyone adds a new inbound system action without also adding it to the table.

## Acceptance criteria

**Naming / inventory:**

- `rg "SY\\.architect\\.(status|tool|plan_compiler|executor|snapshot)" src/bin/sy_architect.rs` returns no hits.
- `rg "Uuid::new_v4\\(\\)\\.simple\\(\\)" src/bin/sy_architect.rs` returns no hits.
- `sy_architect.rs` has exactly **one** production `NodeConfig` with `name: "SY.architect"` and `NodeUuidMode::Persistent`.
- Normal architect status refresh does not increase router-visible node count.
- After one plan compiler help lookup + one executor validation, `inventory` shows `SY.architect@<hive>` and nothing matching `SY.architect.*`.

**Routing / RPC:**

- Plan compiler, executor, and status refresh paths all work through the shared `Arc<RpcClient>::send_admin_rpc`.
- `SY.identity` / `SY.admin` authorization continues to see `src_l2_name = SY.architect@<hive>` for architect-originated requests.
- No private architect RPC dispatcher is introduced; all multiplexing stays in `fluxbee_sdk::rpc::RpcClient`.
- The architect `OperationalRouteProfile` builds with explicit `msg_type` rules — no `RouteMatch::Any` in either rule table.

**Authorization (Section F):**

- All 4 inbound protected actions (`NODE_STATUS_GET`, `CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED`) reject unauthorized callers with the matching `*_RESPONSE` `FORBIDDEN` payload.
- `protected_architect_system_action_response_covers_all_4_protected_actions` pins the table.

**No-legacy invariants (must be true at merge):**

- The bespoke `router_connect_loop` and `router_recv_loop` are **deleted** (not commented out, not behind a flag).
- The `router_connected: AtomicBool` field is **deleted**. Connection-state queries go through `client.sender_snapshot().is_connected()`.
- Every `NodeConfig { name: format!("SY.architect.{purpose}.{}", ...), uuid_mode: Ephemeral }` block is **deleted**.
- Every `fluxbee_sdk::resolve_resource` call inside architect is **deleted**; replaced by `VaultClient` (Section D) or `send_system_rpc` shape.
- The free function `fluxbee_sdk::resolve_resource` is **deleted** from the SDK (Section D, option 1) — no parallel API kept "for other callers".
- No transitional flag, env var, or `#[cfg]` selects between bespoke recv loop and `RpcClient`.

**CI guard:**

- `scripts/architect_no_ephemeral_guard.sh` (or equivalent CI check) enforces the grep invariants above on every PR.

## SDK impact

The admin/executor/status migrations consume APIs the SDK already ships (`RpcClient::connect_with_retry`, `send_admin_rpc`, `send_system_rpc`, `send_with_matcher`, `RpcTestHarness`, `NodeSender::is_connected`, `NodeSender::wait_connected`). No new SDK surface is required for Sections A, B, C, E, F.

Section D requires one new SDK addition: a `VaultClient` (or equivalent) that wraps `Arc<RpcClient>` and exposes the vault resolve / get / list path. The current free function `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` is **deleted in the same PR** — no parallel APIs left behind. The 5 other vault-lookup sites listed in ORPC Block F Pattern 3 (`sy_admin:820`, `sy_cognition:1594`, `io-slack:739`, `ai-generic:1682`, `ai-frontdesk-gov:1708`) migrate to the new API in the same PR. That clears Pattern 3 entirely, not partially.

This is a wider blast radius than strictly necessary for architect alone, but the no-legacy stance applies at the SDK boundary too: keeping `resolve_resource` for the other 5 sites while architect uses `VaultClient` would leave us with two APIs for the same operation, drifting independently. We pay the migration cost once.
