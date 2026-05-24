# SY.architect RPC canonicalization tasks

Date: 2026-05-24 (created)
Status: Open
Related: `docs/onworking COA/orchestrator_internal_rpc_multiplexing_tasks.md`

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

Suggested command channels:

- `system` for `NODE_STATUS_GET`, `CONFIG_GET`, `CONFIG_SET`, and `VAULT_SECRET_CHANGED`.
- `incoming` for non-RPC, non-system router messages that currently flow through `router_recv_loop` and may be persisted by session id.

Suggested rules:

- `pre_pending`:
  - `OneOf { msg_type: SYSTEM_KIND, msgs: [NODE_STATUS_GET, CONFIG_GET, CONFIG_SET, VAULT_SECRET_CHANGED] } -> Command("system")`
- `post_pending`:
  - optional drops for response-only transport leftovers that architect never treats as commands;
  - `Any -> Command("incoming")`

The broad `Any` catch-all is acceptable only after the SDK stale/response-only guards, so late `ADMIN_COMMAND_RESPONSE` frames from architect's own RPCs do not become chat/incoming messages.

### Workers

Delete `router_connect_loop` / `router_recv_loop` as bespoke receive owners. Replace them with worker tasks fed by `RpcClient`:

- `run_architect_system_worker(sender_snapshot, state, rx)`:
  - calls `try_handle_default_node_status`;
  - handles `CONFIG_GET`;
  - handles `CONFIG_SET`;
  - handles `VAULT_SECRET_CHANGED`.
- `run_architect_incoming_worker(state, rx)`:
  - preserves today's non-system behavior from `router_recv_loop`;
  - logs message metadata;
  - persists `router_message_session_id` messages via `persist_router_incoming_message`.

Do not await outbound RPCs inside the SDK recv loop. The recv loop lives inside `RpcClient`; operational work happens in workers.

### Outbound admin RPCs

Refactor `execute_admin_action_with_context` so it no longer creates a `NodeConfig` and no longer calls `RpcClient::connect_with_retry` per action.

It should use:

```rust
let client = context.rpc.clone();
client.send_admin_rpc(AdminCommandRequest { ... }).await
```

The following paths should all reuse the same client:

- `fluxbee_system_get` / `tool.read`
- `get_or_refresh_admin_actions` / `plan_compiler.cache_refresh`
- `get_admin_action_help` / `plan_compiler.help_lookup`
- `PlanCompilerLiveQueryTool` / `plan_compiler.live_query`
- `validate_plan_with_admin` / `plan_compiler.pre_validate`
- `execute_executor_plan_with_context` / `executor.validate` and `executor.run`
- pipeline snapshot collection (`snapshot.inventory`, `snapshot.nodes`, `snapshot.routes`, etc.)
- status refresh (`fetch_inventory_status_data`)

### Vault lookup follow-up

Architect still has boot/hot-refresh paths that use `connect()` with `NodeUuidMode::Ephemeral` for vault `resolve_resource`:

- OpenAI resource lookup.
- Postgres/messages-db resource lookup.

These should move to one of:

- a new SDK helper that resolves vault resources through an existing `Arc<RpcClient>`;
- a `VaultClient` wrapper over `Arc<RpcClient>`;
- local architect helpers that build the vault request over `send_system_rpc`.

This is lower risk than the admin RPC issue because it is not creating the `SY.architect.<purpose>.<uuid>` inventory storm, but it still announces unnecessary ephemeral router connections and should be cleaned in the same migration.

## Task list

### A. Inventory and guardrails

- [ ] ARCH-RPC-A1. Add a short regression test or diagnostic assertion that normal status refresh plus one admin action does not produce `SY.architect.*.<uuid>` inventory entries.
- [ ] ARCH-RPC-A2. Add grep-based guard in the task notes or CI-adjacent script for `SY.architect.{purpose}.` and `Uuid::new_v4().simple()` in `sy_architect.rs`.

### B. Shared client in architect

- [ ] ARCH-RPC-B1. Add `rpc` ownership to `ArchitectState` or construct it before `ArchitectState` and store `Arc<RpcClient>` directly.
- [ ] ARCH-RPC-B2. Add `build_architect_rpc_profile()` in `sy_architect.rs`.
- [ ] ARCH-RPC-B3. Replace `router_connect_loop` / `router_recv_loop` with profile-driven workers.
- [ ] ARCH-RPC-B4. Keep `router_connected` semantics by deriving it from `client.sender_snapshot().is_connected()` or by updating the flag from worker/connection-loss observations. Do not keep a second receiver just to update status.

### C. Migrate outbound admin calls

- [ ] ARCH-RPC-C1. Extend `ArchitectAdminToolContext` with `rpc: Arc<RpcClient>`.
- [ ] ARCH-RPC-C2. Refactor `execute_admin_action_with_context` to call `context.rpc.send_admin_rpc(...)`.
- [ ] ARCH-RPC-C3. Remove the per-action `NodeConfig { name: format!("SY.architect.{purpose}.{}"...), uuid_mode: Ephemeral }` block.
- [ ] ARCH-RPC-C4. Refactor `fetch_inventory_status_data` to reuse the shared client instead of creating `SY.architect.status.<uuid>`.
- [ ] ARCH-RPC-C5. Re-run plan compiler, executor validate/run, live query, tool.read, and status refresh paths against the shared client.

### D. Migrate vault lookups

- [ ] ARCH-RPC-D1. Design a small SDK surface for vault-over-`RpcClient` or implement a local helper over `send_system_rpc`.
- [ ] ARCH-RPC-D2. Migrate OpenAI lookup.
- [ ] ARCH-RPC-D3. Migrate messages-db Postgres lookup.
- [ ] ARCH-RPC-D4. Verify `VAULT_SECRET_CHANGED` hot-refresh still rebuilds OpenAI runtime and messages-db runtime without creating ephemeral router nodes.

### E. Tests and runtime validation

- [ ] ARCH-RPC-E1. Unit-test the architect route profile with `RpcTestHarness`: colliding `ADMIN_COMMAND_RESPONSE` completes/drops as RPC response, while system commands route to `system` and chat/incoming messages route to `incoming`.
- [ ] ARCH-RPC-E2. Unit-test that `CONFIG_GET` / `CONFIG_SET` responses are emitted from the canonical `SY.architect@<hive>` sender.
- [ ] ARCH-RPC-E3. Runtime validation: after restart and one status refresh cycle, `inventory` contains `SY.architect@motherbee` but no `SY.architect.status.*`.
- [ ] ARCH-RPC-E4. Runtime validation: after one plan compiler help lookup and one executor validation, `inventory` contains no `SY.architect.plan_compiler.*` and no `SY.architect.executor.*`.

## Acceptance criteria

- `rg "SY\\.architect\\.(status|tool|plan_compiler|executor|snapshot)" src/bin/sy_architect.rs` finds no generated router node names.
- `sy_architect.rs` has exactly one production `NodeConfig` with `name: "SY.architect"` and `NodeUuidMode::Persistent`.
- Normal architect status refresh does not increase router-visible node count.
- Plan compiler and executor paths work through the canonical sender.
- `SY.identity` / `SY.admin` authorization continues to see `src_l2_name = SY.architect@<hive>` for architect-originated requests.
- No private architect RPC dispatcher is introduced; all multiplexing stays in `fluxbee_sdk::rpc::RpcClient`.

## SDK impact

The admin/executor/status part should be simple because the SDK is already ready. The main code work is plumbing `Arc<RpcClient>` through `ArchitectState` / `ArchitectAdminToolContext` and deleting per-call connects.

The only SDK extension likely worth adding is vault-over-`RpcClient`, because current `resolve_resource` helpers still take `NodeSender + NodeReceiver` ownership. That is a follow-up to make every architect router operation share the same canonical connection.
