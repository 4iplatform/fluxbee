# SY.architect canonicalization — architect-specific scope

Date: 2026-06-01
Updated: 2026-06-07 (audit confirmed Archi + 4 specialist helpers + admin executor operate correctly post-RPC)
Status: **Closed (2026-06-06)** — see "Implementation status (2026-06-06)" below
Supersedes strategy in: `docs/onworking COA/sy_architect_rpc_multiplexing_tasks.md`
Related:

- `docs/onworking COA/orchestrator_internal_rpc_multiplexing_tasks.md` (ORPC — orchestrator and admin already migrated)
- `docs/onworking COA/routerdispatcher_unification_plan.md` (global revamp this work is part of)

## Quick read for anyone arriving here later

The doc is long because it captures both the migration and a back-and-forth design discussion. The bottom line is short:

- **Archi (chat) + 4 specialist agents (`plan_compiler`, `designer`, `design_auditor`, `real_programmer`) + the residual AI path of `failure_classifier` live in-process inside `SY.architect@<hive>`.** The admin executor lives in-process inside `SY.admin@<hive>`. None of them becomes a separate fluxbee node.
- This is **deliberate**. The v3 resolution (2026-06-01) cancelled the earlier "stable nodes" extraction direction once `RouterDispatcher` removed the root cause that had motivated extraction (per-call ephemeral connections + N Vault lookups). Confirmed again 2026-06-07 with the user: "si puedo dejar todo dentro de archi revisemoslo, no me importa que use la misma key de openai".
- **There is no half-finished extraction work.** `ai.generic` + cognitive assets (role/skill/handbook/personality) are infrastructure for **end-user-spawned** AI nodes (the `AI.sales@hive`, `AI.support@hive` your customers want), not for Archi's internal helpers. These two systems are unrelated.
- Post-RPC-migration runtime audit (2026-06-07): single canonical `Arc<RouterDispatcher>`, single shared `OpenAiResponsesClient` resolved once from Vault, hot-refresh via `VAULT_SECRET_CHANGED` works, all 5 agents reach SY.admin through the canonical dispatcher, 154 sy_architect tests verde. Nothing to do here.

## Implementation status (2026-06-06)

Architect transport refactor landed alongside the global revamp.

**Closed in this PR:**

- **Section A — Inventory and guardrails v2.** `scripts/router_dispatcher_guards/architect_no_ephemeral_guard.sh` exists and runs clean on the current tree (no `SY.architect.<purpose>.{}` literals, no `NodeUuidMode::Ephemeral`, no deleted `router_*_loop` / `router_sender` / `router_connected` in `sy_architect.rs` or `sy_admin.rs`). Legitimate non-router uses of `Uuid::new_v4().simple()` (attachments, bundles, staging) stay — the guard targets only the four forbidden patterns named in A2.
- **Section B — Canonical `Arc<RouterDispatcher>`.** `ArchitectState.rpc: Arc<RouterDispatcher>` added; `router_connect_loop`, `router_recv_loop`, `router_sender: Arc<Mutex<Option<NodeSender>>>`, `router_connected: AtomicBool` all **deleted** from `src/bin/sy_architect.rs`. `main()` reorder per B9: dispatcher constructed before `build_architect_ai_runtime` and `resolve_messages_db_url_from_vault`, both of which now take `Arc<RouterDispatcher>` and build their `VaultClient` from it. `state.rpc.sender_snapshot().is_connected()` replaces the deleted `AtomicBool`. Route profile (`build_architect_rpc_profile`): `system` (pre-pending: `NODE_STATUS_GET` / `CONFIG_GET` / `CONFIG_SET` / `VAULT_SECRET_CHANGED`) + `incoming` (post-pending: `user` / `chat` / `text`). No `RouteMatch::Any`.
- **Section C — Outbound admin RPC migration.** `ArchitectAdminToolContext.rpc` added; `execute_admin_action_with_context` and `fetch_inventory_status_data` now call the shared dispatcher's `send_admin_rpc`. The per-action `NodeConfig { name: format!("SY.architect.{purpose}.{}"), uuid_mode: Ephemeral }` block and the `SY.architect.status.<uuid>` block are **deleted**. All 10 outbound purposes (`tool.read`, `plan_compiler.*`, `executor.*`, `snapshot.*`, `status`, `scmd`) now flow through the canonical dispatcher.
- **Section D — System + incoming workers.** `run_architect_system_worker(state, system_rx)` and `run_architect_incoming_worker(state, incoming_rx)` implemented and spawned from `main`. `handle_architect_system_message` reads its sender from `state.rpc.sender_snapshot()`. `CONFIG_GET`, `CONFIG_SET`, and `VAULT_SECRET_CHANGED` behavior preserved; impersonation persistence via `persist_router_incoming_message` still runs from the incoming worker.
- **Section E — Origin authorization.** `protected_architect_system_action_response`, `architect_origin_authorized`, and `build_architect_forbidden_response` implemented. Inbound `NODE_STATUS_GET` / `CONFIG_GET` / `CONFIG_SET` / `VAULT_SECRET_CHANGED` from outside the allowlist (`SY.admin@hive`, `SY.config-routes@hive`, `SY.vault@hive`) receive a `*_RESPONSE` with `status: "error", error_code: "FORBIDDEN"` and never reach the handler. `NODE_STATUS_GET` is gated before `try_handle_default_node_status`. Allowlist is hardcoded per the OD-Origins-Config decision; hive-config-driven extension stays as future work.
- **Section G — SDK `VaultClient`.** G1–G6 all done (delivered with the global plan). `VaultClient` over `Arc<RouterDispatcher>`, 3 tests, all 9 Pattern 3 sites migrated. `fluxbee_sdk::resolve_resource` free function deleted from the SDK.
- **Section H1 — Admin canonical dispatcher.** Unchanged from ORPC v2 — built in `build_admin_rpc_profile()` at `sy_admin.rs:308`, constructed at `sy_admin.rs:475`.
- **Section H2 — Admin executor OpenAI lookup over canonical dispatcher.** `AdminContext.rpc: Arc<RouterDispatcher>` added; `build_admin_executor_ai_runtime` now takes `Arc<RouterDispatcher>` and constructs `VaultClient` over it. The ephemeral `NodeConfig { uuid_mode: Ephemeral }` + per-call `RouterDispatcher::connect_with_retry` block previously at `sy_admin.rs:816-842` is **deleted**.
- **Section H3 — `NodeUuidMode::Ephemeral` removed from admin executor Vault lookup.** Direct consequence of H2 closing through the canonical dispatcher.

**Optional follow-ups (out of scope for this PR — both currently green on the live path):**

- **Section H4** — `VAULT_SECRET_CHANGED` hot-refresh exercises the canonical dispatcher via `refresh_admin_executor_ai_runtime` and `handle_vault_secret_changed_architect`. Missing piece: end-to-end regression test asserting the refreshed runtime is observable from a follow-up RPC. Live path works; only test coverage is gap. **Tracked in [routerdispatcher_unification_followups.md](routerdispatcher_unification_followups.md) under P0 / H4.1–H4.4.**
- **Section H5** — Admin inbound origin-authorization audit, parity with the new architect Section E gate. Architect now rejects unauthorized callers; admin's protected actions have not been audited under the same lens. Separate scoped review. **Tracked in [routerdispatcher_unification_followups.md](routerdispatcher_unification_followups.md) under P0 / H5.1–H5.5.**

## Post-close audit (2026-06-06)

A second-pass audit was run after the close. The Section E gate as originally landed had a critical bug:

- **Bug:** `VAULT_SECRET_CHANGED` was in `protected_architect_system_action_response`. SY.vault emits the broadcast with `src_l2_name: None`, which `architect_origin_authorized` treats as missing-src → `FORBIDDEN`. Net effect: every hive-wide Vault key rotation event reaching the architect was being shorted to a forbidden response and the architect's `handle_vault_secret_changed_architect` was never running. The architect's cached `ArchitectAiRuntime` and messages-db URL would have stayed stale until the process restarted.
- **Fix:** removed `VAULT_SECRET_CHANGED` from the protected list. It is a hive-wide event, not a request/response RPC. The actual refresh path (re-resolving the secret via `VaultClient.resolve_resource`) remains end-to-end authenticated by SY.vault on the re-resolve call, so a forged event cannot leak the underlying secret — at worst it triggers a re-resolve the architect would have done eventually anyway.
- **Side note:** while reviewing the gate, `architect_origin_authorized` was rewritten from `format!("SY.admin@{hive}")`-against-each-allowed-name to `split_once('@') + matches!(node, …)` so it no longer allocates per inbound message (the gate runs once per SYSTEM_KIND message on the system worker).

## CI gate at end of PR

`scripts/router_dispatcher_guards/architect_no_ephemeral_guard.sh` exits 0 on the current tree. Combined with the 5 global guards (`no_inline_dispatcher`, `no_direct_connect`, `no_legacy_vault_helper`, `no_deprecated_attribute_on_dispatcher`, `no_shared_receiver`), every architectural invariant this doc promised is enforceable in CI today.

## Test status at close

- `cargo check --workspace --all-targets` (Rust root) — verde.
- `cargo test -p fluxbee-sdk --lib` — 147/147 verdes.
- `cargo check --workspace --all-targets` (`nodes/io` workspace) — verde.
- Go modules: `go/fluxbee-go-sdk`, `go/sy-timer`, `go/sy-wf-rules` (node tests including the new dispatcher-backed `admin`, `orchestrator`, `wf_client`), `go/sy-opa-rules` (build), `go/nodes/wf/wf-generic` (node tests) — verdes.


## Context — this is one chapter of a larger plan

After discussion, the decision was made to unify **all** router communication in fluxbee under a single dispatcher abstraction (`RouterDispatcher`, the same component currently named `RpcClient` in the SDK). The repo currently has **7 independent implementations** of the same dispatch-by-`trace_id` concept (`RouterDispatcher` in `fluxbee_sdk::rpc`, `RouterInbox` in `nodes/io/common`, two copies of `SharedRouterConnection` in `ai-generic` and `ai-frontdesk-gov`, `messageMux` in Go `sy-wf-rules`, ad-hoc `forwardOutgoing` in Go `sy-opa-rules`, plus a few simple `loop { recv }` patterns in pure responders). That divergence is recognized as legacy debt from organic growth.

The global plan canonicalizes all of them. This document is the architect-specific chapter of that plan. It exists separately because architect has architect-specific concerns (route profile, workers, auth gate, the two vault lookup sites that are architect's) that don't belong in a global SDK document.

For the SDK rename, the Tipo 1 puro migrations (`sy_identity`, `sy_vault`, `sy_storage`, `sy_policy`, `sy_config_routes`, `sy_cognition`, `sy-timer`, `wf-generic`), the Tipo 2 inline-dispatcher migrations (`io-*`, `ai-*`), the Go SDK port, and the deletion of all parallel inline dispatchers — see `routerdispatcher_unification_plan.md`.

## Implementation stance — no legacy, no deprecation period, no shims

Per `feedback_no_legacy_in_dev`: the migration is **complete** within the scope this plan defines. The following patterns are **explicitly forbidden** and CI must reject them:

- A `#[deprecated]` attribute on any SDK helper that this plan retires.
- A "legacy/deprecated until consumers migrate" period.
- A transitional flag, env var, or `#[cfg(...)]` selecting between the bespoke recv loop and `RouterDispatcher`.
- Keeping the bespoke `router_recv_loop` "for fallback" alongside the new shared `Arc<RouterDispatcher>`.
- Migrating outbound admin RPC call sites one PR at a time. All in-scope call sites land in the same PR.
- Keeping `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` available to in-scope callers "until VaultClient lands". The scope of Vault migration is decided up front (see "Vault strategy"); whatever is in scope migrates and the old surface is **deleted** for those callers in the same PR.

Rationale: ORPC v1 did a phased rollout (`OrchestratorRouterClient` introduced while the bespoke recv loop stayed) and produced a structural deadlock that needed two review passes and a full v2 rewrite to fix. We will not pay that cost twice. If a question can't be resolved up front, that scope item is removed from this PR — it does not become a shim.

## Resolution of open decisions (this round, 2026-06-01)

| Decision | Resolution | Source |
| --- | --- | --- |
| #1 — Should main chat AI stay in `SY.architect@<hive>` or move to `SY.architect.archi@<hive>`? | **Stay in `SY.architect@<hive>`.** The AI workers (main chat brain, plan compiler, designer, design auditor, real programmer, failure classifier) remain in-process as async modules. Splitting them would introduce inter-node lifecycle complexity for no operational gain. They share the canonical `Arc<RouterDispatcher>` of the architect process for any outbound RPC need. | User confirmed 2026-06-01 v3. |
| #2 — Should `SY.admin` executor stay in `SY.admin@<hive>` or move to `SY.admin.executor@<hive>`? | **Stay in `SY.admin@<hive>`.** Same reasoning as #1: the admin executor AI is an in-process module that shares the canonical `Arc<RouterDispatcher>` of the admin process. Its Vault lookup migrates to use that same shared client — no new node identity is introduced. | User confirmed 2026-06-01 v3. |
| #4 — Should `resolve_resource(&NodeSender, &mut NodeReceiver, ...)` be deleted in the same PR as `VaultClient`, or deprecated first? | **Delete in same PR for every in-scope call site. No deprecation period.** Per the global `RouterDispatcher` unification plan, "in-scope" is **all 7 Pattern 3 sites** (architect ×2, admin ×1, cognition, io-slack, ai-generic, ai-frontdesk-gov). The architect-specific PR handles only the 3 architect+admin sites; the other 4 are part of the global revamp doc. | User confirmed 2026-06-01 v3. |
| Vault scope | **Wide (7 sites total).** Closed by the global revamp decision — `RouterDispatcher` is universal, so `VaultClient` over `RouterDispatcher` is universal. The architect-specific PR delivers the SDK `VaultClient` + migrates the 3 architect+admin sites. The remaining 4 sites migrate in the global revamp's per-node migration PRs and the old `resolve_resource` free function is deleted from the SDK when the last site is migrated. **No `#[deprecated]` markers in between.** | User confirmed 2026-06-01 v3. |
| Stable AI worker node split (was open as "lifecycle owner") | **Cancelled.** Since #1 and #2 are both "stay in-process", there are no new stable AI worker nodes to define a lifecycle for. The entire "Plan B" disappears. The architect AI workers remain async tasks inside the architect process. | Implicit consequence of #1 + #2. |

**Still open (minor, can be decided during implementation):**

- `architect_allowed_origins` config-driven or hardcoded. Default: hardcoded for the initial migration, hive-config extension as a follow-up if needed.

## Summary

The original architect RPC plan correctly identified a real problem: `SY.architect`
creates router-visible helper clients such as:

```text
SY.architect.status.<uuid>@<hive>
SY.architect.tool.read.<uuid>@<hive>
SY.architect.plan_compiler.help_lookup.<uuid>@<hive>
SY.architect.executor.validate.<uuid>@<hive>
SY.architect.snapshot.nodes.<uuid>@<hive>
```

Those names are not stable runtime identities. They are transport clients created
per operation. They should be removed.

A previous draft of this plan went further and proposed splitting the architect AI workers (`archi`, `plan_compiler`, `designer`, `design_auditor`, `real_programmer`) into stable `SY.architect.<role>@<hive>` nodes. The v3 resolution (2026-06-01) cancels that direction. The AI workers stay in-process inside `SY.architect@<hive>` (and the admin executor stays in-process inside `SY.admin@<hive>`), all sharing the canonical `Arc<RouterDispatcher>` of their parent process. The "stable nodes" framing was solving a problem we don't have at the current fleet scale; the consistency win is achieved by the unified `RouterDispatcher` abstraction, not by splitting processes.

New strategy:

1. `SY.architect@<hive>` gets one canonical `RouterDispatcher` for all ordinary admin, status, snapshot, config, and incoming-router work.
2. Per-call random transport identities are deleted.
3. Architect AI workers stay in-process and share the canonical `Arc<RouterDispatcher>`.
4. Vault lookup is treated as a shared SDK debt that affects both `SY.architect` and `SY.admin`. The issue is not that a node reads Vault. The issue is that the current SDK Vault helper owns a separate `NodeReceiver`, which conflicts with canonical router-dispatch ownership. The fix is `VaultClient` over `Arc<RouterDispatcher>`.
5. `SY.admin` already has canonical router transport for admin/system traffic; its executor AI's ephemeral Vault connection is migrated to `VaultClient` over the shared `Arc<RouterDispatcher>`. The executor stays in-process.

## Core distinction

### Bad: per-call transport clients

These should disappear:

```text
SY.architect.status.<random>@<hive>
SY.architect.tool.read.<random>@<hive>
SY.architect.plan_compiler.help_lookup.<random>@<hive>
SY.architect.plan_compiler.live_query.<random>@<hive>
SY.architect.plan_compiler.pre_validate.<random>@<hive>
SY.architect.executor.validate.<random>@<hive>
SY.architect.executor.run.<random>@<hive>
SY.architect.snapshot.<random>@<hive>
```

They are not nodes. They are temporary clients used to call `SY.admin`. They are what this PR deletes.

### Good: in-process async modules sharing the canonical `Arc<RouterDispatcher>`

The AI workers inside `SY.architect` (main chat brain, plan compiler, designer, design auditor, real programmer, failure classifier) and the AI executor inside `SY.admin` stay as in-process async tasks. They do **not** become separate `SY.architect.<role>@<hive>` or `SY.admin.executor@<hive>` nodes. They access the router through the same `Arc<RouterDispatcher>` owned by their parent process — no separate identity, no separate connection, no separate inventory entry.

This was discussed and confirmed in the v3 resolution pass. The reasoning is consistency with the global `RouterDispatcher` unification model: every process owns exactly one canonical connection to the router; AI logic inside that process is async tasks sharing the same dispatcher.

## Scope

### In scope

- Replace `SY.architect` per-call admin/status/snapshot transport clients with a single canonical `Arc<RouterDispatcher>` owned by `SY.architect@<hive>`.
- Move the canonical router receive ownership from the bespoke `router_recv_loop` to `RouterDispatcher`.
- Keep non-RPC incoming router traffic working through explicit `RouterDispatcher` command channels.
- Add origin authorization for inbound architect system actions.
- Redefine inventory guardrails so they forbid random per-call clients.
- Add a parallel `SY.admin` cleanup section for the remaining executor AI Vault lookup debt.
- Introduce SDK Vault access over canonical `RouterDispatcher` (`VaultClient`). This work is shared with the global unification doc, which migrates the other 4 Pattern 3 sites.

### Not in scope

- Replacing the socket router transport.
- Changing the `Message` / `Routing` / `Meta` wire format.
- Removing the HTTP API/UI from `SY.architect`.
- Splitting architect AI workers or admin executor AI into separate nodes (the v3 resolution closed this — they stay in-process).
- Migrating the other Tipo 1 and Tipo 2 nodes to `RouterDispatcher` (that is the global unification doc).

## Current code reality

### `SY.architect`

Current router patterns:

1. Canonical but manual router connection:
   - `main` creates `NodeConfig { name: "SY.architect", uuid_mode: Persistent }`.
   - `router_connect_loop` stores a `NodeSender` in `state.router_sender`.
   - `router_recv_loop` owns the receiver and calls
     `handle_architect_system_message`.

2. Per-operation outbound admin clients:
   - `execute_admin_action_with_context` creates
     `SY.architect.{purpose}.<uuid>` with `NodeUuidMode::Ephemeral`.
   - `fetch_inventory_status_data` creates `SY.architect.status.<uuid>`.

3. Vault lookups:
   - OpenAI lookup opens a separate ephemeral connection as `SY.architect`.
   - messages-db Postgres lookup opens a separate ephemeral connection as
     `SY.architect`.
   - Both call `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)`.

4. AI workers are in-process:
   - main chat model;
   - plan compiler;
   - designer;
   - design auditor;
   - real programmer;
   - residual failure classifier.

### `SY.admin`

Current router state after ORPC work:

- `SY.admin` already uses a canonical persistent `Arc<RouterDispatcher>` for router traffic (this is the same component the SDK still names `RpcClient` until the global rename lands).
- It has an `OperationalRouteProfile` with command channels for status, system commands, and internal admin commands.
- It no longer needs a bespoke router receive loop for admin/system traffic.

Remaining admin debt:

- Admin executor AI is in-process (and stays in-process — v3 decision).
- Admin executor OpenAI lookup still creates an ephemeral `NodeConfig` with `name: "SY.admin"` and `NodeUuidMode::Ephemeral`.
- It still calls `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)`.
- Therefore the admin RPC refactor did not close Vault receiver ownership.

This matters because architect should not repeat the same partial migration.

## Target architecture

### `SY.architect@<hive>` canonical process

`SY.architect@<hive>` is the primary (and only) `SY.architect.*` process. It owns the HTTP UI/API, session state, orchestration of architect workflows, and all architect AI work as in-process async tasks (v3 resolution: no subnode splits).

It owns one canonical router dispatcher:

```rust
struct ArchitectState {
    rpc: Arc<RouterDispatcher>,
    ...
}
```

The dispatcher is constructed before `ArchitectState` (see "Startup ordering" above), so the field is plain `Arc<RouterDispatcher>` — no `OnceLock`, no `Mutex<Option<...>>`.

The canonical dispatcher uses:

```rust
NodeConfig {
    name: "SY.architect".to_string(),
    uuid_mode: NodeUuidMode::Persistent,
    ...
}
```

Note on naming: this doc uses `RouterDispatcher` for the canonical type. The SDK code currently exposes it under the legacy name `RpcClient`; the global unification PR (`routerdispatcher_unification_plan.md`) executes the rename. Whichever PR lands first introduces the new name; this PR uses whichever name is current when it lands. There is **no deprecation period** — the rename is a single mechanical change at SDK level.

### Startup ordering — the dispatcher must exist before Vault is resolved

The current architect main function in [src/bin/sy_architect.rs:5905-6019](src/bin/sy_architect.rs#L5905) resolves Vault secrets **before** the router connection is established:

1. [line 5914](src/bin/sy_architect.rs#L5914) — `build_architect_ai_runtime` resolves the OpenAI key from Vault.
2. [line 5954](src/bin/sy_architect.rs#L5954) — `resolve_messages_db_url_from_vault` resolves the messages-db Postgres URL.
3. [line 6017](src/bin/sy_architect.rs#L6017) — `router_connect_loop` is spawned (the persistent router connection).

This works today because each Vault call opens its own ephemeral `connect()`+`resolve_resource(&sender, &mut receiver, ...)` block — Vault doesn't need the architect's persistent connection to be up yet. Once the architect's Vault path goes through `VaultClient` over `Arc<RouterDispatcher>`, this ordering inverts: the dispatcher must exist **before** any Vault resolve call.

Reorder required in this PR:

1. Load `NodeConfig`, hive ID, self-ILK (no router yet).
2. **Construct `Arc<RouterDispatcher>`** via `RouterDispatcher::connect_with_retry`. Wait for the first connected state. (The architect process can tolerate a few hundred ms of startup delay here — it already does for the messages-db Postgres connect at line 5966.)
3. Construct `VaultClient::new(rpc.clone(), hive_id, caller)`.
4. Call `build_architect_ai_runtime` and `resolve_messages_db_url_from_vault` using the `VaultClient`. The functions change signature: instead of taking `&config_dir`/`&state_dir`/`&socket_dir` and opening their own connection, they take `&VaultClient` (or `&Arc<RouterDispatcher>` if they need both Vault and other system calls).
5. Build `ArchitectState` with the dispatcher and the resolved secrets.
6. Spawn workers as before (status refresh, HTTP server, etc.).

Step 2 means the dispatcher is the architect's first long-lived stateful resource. If Vault is not up yet when the architect boots (legitimate possibility during fleet cold-start), the `VaultClient` calls block/retry the same way the current ephemeral `connect()`+`resolve_resource` block does today — but through the canonical retry path, not bespoke logic per Vault call.

No `OnceLock<Arc<RouterDispatcher>>` shim is needed: the dispatcher is constructed before `ArchitectState`, so the field can be `rpc: Arc<RouterDispatcher>` directly. The earlier `OnceLock` option above is removed from the target design.

### Architect AI workers stay in-process

Architect's AI workers (main chat brain, plan compiler, designer, design auditor, real programmer, failure classifier) stay as async tasks inside the architect process. None of them becomes a separate `SY.architect.<role>@<hive>` node.

Reasoning (closed in v3 resolution):

- The complexity of inter-node lifecycle (start/stop/restart/health/config of separate nodes) does not buy operational value at the current fleet scale.
- The unification goal — one canonical `Arc<RouterDispatcher>` per process — is preserved trivially: the AI workers share the architect's dispatcher.
- Vault access for each AI worker goes through that same shared `Arc<RouterDispatcher>` via `VaultClient`.
- If at some future point one of these workers grows to need a separate identity (its own ILK, its own restart cadence, its own resource constraints), it becomes a `SY.architect.<role>@<hive>` node then — but as a separate decision, not as part of this work.

### `SY.admin` executor AI stays in-process

Same decision as architect's AI workers (closed in v3 resolution): the admin executor remains an in-process async module inside `SY.admin`. It uses the canonical `Arc<RouterDispatcher>` of the admin process for any outbound RPC (admin actions, vault) via `VaultClient`. The ephemeral `connect()` block at [sy_admin.rs:820](src/bin/sy_admin.rs#L820) is **deleted** in this PR and replaced with a `VaultClient` call through the shared dispatcher.

No `SY.admin.executor@<hive>` node is introduced.

## Architect route profile

Build a dedicated architect profile:

Command channels:

- `system`
- `incoming`

Pre-pending rules:

```text
OneOf {
  msg_type: SYSTEM_KIND,
  msgs: [
    NODE_STATUS_GET,
    CONFIG_GET,
    CONFIG_SET,
    VAULT_SECRET_CHANGED
  ]
} -> Command("system")
```

Post-pending rules:

```text
AnyMsgOfType("user") -> Command("incoming")
AnyMsgOfType("chat") -> Command("incoming")
AnyMsgOfType("text") -> Command("incoming")
```

Rules:

- Do not use `RouteMatch::Any`.
- Do not route every unknown message into `incoming`.
- Unknown messages should increment `rpc_route_unmatched_total` and log at
  debug.
- If a new incoming `msg_type` becomes part of architect behavior, add it
  explicitly.

Why:

- `system` commands must win against trace-id collisions with pending RPCs.
- ordinary inbound user/chat/text traffic should still be persisted for
  impersonation sessions.
- late admin/system responses from architect's own outbound RPC calls should be
  handled by the SDK pending/stale/response-only machinery, not by a bespoke
  receiver loop.

## Architect workers

Delete:

- `router_connect_loop`
- `router_recv_loop`
- `router_sender`
- `router_connected`

Replace with:

- `run_architect_system_worker(state, client, rx)`
- `run_architect_incoming_worker(state, rx)`

System worker:

- runs origin authorization first;
- handles `NODE_STATUS_GET`;
- handles `CONFIG_GET`;
- handles `CONFIG_SET`;
- handles `VAULT_SECRET_CHANGED`.

Incoming worker:

- logs metadata;
- preserves current impersonation behavior;
- persists messages that carry a session id through
  `persist_router_incoming_message`.

Connection state:

- Read from `state.rpc.sender_snapshot().is_connected()`.
- If a caller needs to wait, use `wait_connected().await`.
- Do not keep a separate `AtomicBool`.

Impersonation send path:

- Replace `state.router_sender.lock().await.clone()` with
  `state.rpc.sender_snapshot()`.
- Sending ordinary `Message` frames does not require a separate client.

## Architect outbound admin RPC cleanup

All admin calls that are currently routed through
`execute_admin_action_with_context` should use the shared client:

```rust
context.rpc.send_admin_rpc(AdminCommandRequest { ... }).await
```

Migrate:

- `fluxbee_system_get` / `tool.read`
- `get_or_refresh_admin_actions` / `plan_compiler.cache_refresh`
- `get_admin_action_help` / `plan_compiler.help_lookup`
- `PlanCompilerLiveQueryTool` / `plan_compiler.live_query`
- `validate_plan_with_admin` / `plan_compiler.pre_validate`
- `execute_executor_plan_with_context` / `executor.validate`
- `execute_executor_plan_with_context` / `executor.run`
- pipeline snapshot collection / `snapshot.*`
- status refresh / `fetch_inventory_status_data`
- SCMD execution / `scmd`

Important:

- These call paths do not need their own node identity.
- They do not need Vault.
- They are just architect-originated RPC calls to `SY.admin`.
- Their source identity should be the canonical `SY.architect@<hive>`.

## Vault strategy

### What stays the same

Vault semantics, wire format, authorization model, and stored secrets do **not** change. Reading a secret from Vault is the same operation it was before: same `MSG_VAULT_GET` shape over `SYSTEM_KIND`, same `meta.src_ilk` authorization gate at `SY.vault`, same response payload, same hot-refresh broadcast `VAULT_SECRET_CHANGED`. No other node has to be aware of this work.

This is not a Vault redesign. It is a **router-side** cleanup of how `SY.architect` and `SY.admin` obtain the `NodeSender`/`NodeReceiver` pair used to send the Vault RPC.

### What changes inside the SDK

The current SDK helper:

```rust
fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, caller, hive_id, resource, tenant_id, timeout)
```

takes an exclusive `&mut NodeReceiver`. That is incompatible with canonical router-dispatch ownership: a process whose receiver is owned by `RouterDispatcher` cannot also hand `&mut NodeReceiver` to a Vault helper. So today the affected nodes open a **second router connection** just to call Vault — that second connection is what shows up as Pattern 3 anti-pattern in ORPC Block F.

The fix is `VaultClient`, which wraps `Arc<RouterDispatcher>` and dispatches Vault calls through `send_with_matcher` (using the canonical receiver inside `RouterDispatcher`):

```rust
pub struct VaultClient {
    rpc: Arc<RouterDispatcher>,
    hive_id: String,
    caller: VaultCallerOwned,
}

impl VaultClient {
    pub fn new(rpc: Arc<RouterDispatcher>, hive_id: impl Into<String>, caller: VaultCallerOwned) -> Self;

    pub async fn resolve_resource(
        &self,
        resource: ResourceType,
        tenant_id: &str,
        timeout: Duration,
    ) -> Result<Option<Value>, VaultError>;

    pub async fn get(...);
    pub async fn list(...);
}
```

**Implementation invariants:**

- Build the Vault `Message` manually so `meta.src_ilk` and source/audit context are preserved (the same fields `resolve_resource` carries today).
- Use `RouterDispatcher::send_with_matcher` with an explicit `PendingMatcher` for the `MSG_VAULT_GET_RESPONSE` / `MSG_VAULT_LIST_RESPONSE` shapes.
- Do **not** create a `NodeReceiver`. Do **not** call `connect()` internally.
- Do **not** use the generic `send_system_rpc` unless that API is first extended to carry caller identity fields — and even then, `VaultClient` exists for the convenience of typed Vault calls.

### Scope decision — CLOSED (wide, 2026-06-01 v3)

The Vault migration scope is **wide**: all 7 Pattern 3 sites migrate to `VaultClient`. The free function `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` is **deleted** from the SDK once the last site migrates. **No `#[deprecated]` markers.**

Sites that migrate (per-PR split shown for traceability — the SDK deletion happens in the PR that migrates the last site):

In **this** architect-specific PR (3 sites):

- `src/bin/sy_architect.rs:6174` — OpenAI lookup.
- `src/bin/sy_architect.rs:6249` — messages-db Postgres lookup.
- `src/bin/sy_admin.rs:820` — admin executor OpenAI lookup.

In the **global RouterDispatcher unification** PR (4 sites — see `routerdispatcher_unification_plan.md`):

- `src/bin/sy_cognition.rs:1594`
- `nodes/io/io-slack/src/main.rs:739`
- `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1682`
- `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:1708`

Reasoning (v3 resolution): the global unification PR canonicalizes the router transport for those 4 nodes anyway (`io-slack`, `ai-generic`, `ai-frontdesk-gov` lose their `SharedRouterConnection` / `RouterInbox`; `sy_cognition` gets its dispatcher). Once each of those nodes owns an `Arc<RouterDispatcher>`, the Vault migration is mechanical. Doing it as part of the global revamp avoids the dual-Vault-API state that Option A in the old draft would have created.

### Migration mechanics

For each in-scope call site:

1. The owning node already (or now) owns an `Arc<RouterDispatcher>` for canonical router work.
2. The Vault call site replaces `connect() + resolve_resource(&sender, &mut receiver, ...)` with `VaultClient::new(rpc.clone(), hive_id, caller).resolve_resource(...)`.
3. The ephemeral `NodeConfig { uuid_mode: Ephemeral }` block at that site is **deleted**, not refactored into a helper, not behind a flag.
4. `VAULT_SECRET_CHANGED` hot-refresh paths continue to work — they trigger another call to `VaultClient::resolve_resource`, which goes over the same canonical dispatcher.

## Origin authorization

Architect currently accepts protected system messages without checking the
caller. That must be fixed during the router receiver migration.

Protected architect actions:

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

Allowlist:

- `SY.admin@<hive>`
- `SY.config-routes@<hive>`
- `SY.vault@<hive>`
- explicit configured origins if added to hive config later

Rules:

- No broad prefix inference unless deliberately documented and tested.
- `NODE_STATUS_GET` must also pass through the gate before
  `try_handle_default_node_status` sends a response.
- Unauthorized messages emit `FORBIDDEN` using the matching response name and
  do not reach the handler.

Admin should also be audited for equivalent system-origin authorization. The
admin executor Vault cleanup is separate from that audit, but the same threat
shape applies to inbound system commands.

## Task list

### A. Inventory and guardrails v2

- [ ] ARCH-RPC-V2-A1. Update guardrails to forbid any `SY.architect.*` identity other than the canonical `SY.architect@<hive>`. Since AI workers stay in-process, no `SY.architect.<role>@<hive>` names are legitimate.
- [ ] ARCH-RPC-V2-A2. Add CI guard that fails on:
  - `format!("SY.architect.{purpose}.{}", Uuid::new_v4().simple())`
  - `SY.architect.status.{}` client names
  - any `NodeConfig { name: "SY.architect.<anything>", ... }` literal
  - `NodeUuidMode::Ephemeral` in `src/bin/sy_architect.rs` for router/admin/Vault work
  - `router_connect_loop`
  - `router_recv_loop`
  - `router_sender`
  - `router_connected`
- [ ] ARCH-RPC-V2-A3. Do not ban all `Uuid::new_v4().simple()` in `sy_architect.rs`; some uses are legitimate app IDs such as attachments, bundles, staging dirs, and snapshots.

### B. `SY.architect` canonical RPC client

All of Section B lands in **one PR**. The bespoke `router_*` artifacts are deleted alongside the introduction of the shared `Arc<RouterDispatcher>`. No PR ships with both the new dispatcher AND the bespoke recv loop coexisting.

- [ ] ARCH-RPC-V2-B1. Store `Arc<RouterDispatcher>` in `ArchitectState`.
- [ ] ARCH-RPC-V2-B2. Build `build_architect_rpc_profile()` with `system` and `incoming` command channels. No `RouteMatch::Any`. Explicit msg_type rules per "Architect route profile" section.
- [ ] ARCH-RPC-V2-B3. Connect `SY.architect@<hive>` with `NodeUuidMode::Persistent` through `RouterDispatcher::connect_with_retry`.
- [ ] ARCH-RPC-V2-B4. **Delete** `router_connect_loop` from `sy_architect.rs` in this PR.
- [ ] ARCH-RPC-V2-B5. **Delete** `router_recv_loop` from `sy_architect.rs` in this PR.
- [ ] ARCH-RPC-V2-B6. **Delete** `router_sender` field from `ArchitectState` in this PR.
- [ ] ARCH-RPC-V2-B7. **Delete** `router_connected` field from `ArchitectState` in this PR. All callers switch to `state.rpc.sender_snapshot().is_connected()` (AF-P2a) in the same PR.
- [ ] ARCH-RPC-V2-B8. Update impersonation dispatch to send through `state.rpc.sender_snapshot()`. The old `state.router_sender.lock().await.clone()` path is **deleted**, not kept as a fallback.
- [ ] ARCH-RPC-V2-B9. **Reorder `main`** per "Startup ordering" above. `Arc<RouterDispatcher>` is constructed after `NodeConfig`/hive/self-ILK are loaded and **before** `build_architect_ai_runtime` (today line 5914) and `resolve_messages_db_url_from_vault` (today line 5954). `build_architect_ai_runtime` and `resolve_messages_db_url_from_vault` take `&VaultClient` instead of opening their own ephemeral `connect()` blocks. `ArchitectState.rpc` is constructed from the already-existing dispatcher — no `OnceLock`, no `Mutex<Option<...>>`.

### C. `SY.architect` outbound admin RPC migration

All 10 outbound categories (the 8 listed below plus `scmd` and any equivalent) migrate to the shared `Arc<RouterDispatcher>` in the **same PR** as Section B. No category is deferred. No transitional helper wraps `RouterDispatcher::connect_with_retry` per call as a "let's clean it up later" intermediate.

- [ ] ARCH-RPC-V2-C1. Add `rpc: Arc<RouterDispatcher>` to `ArchitectAdminToolContext`. Every construction site passes the shared dispatcher.
- [ ] ARCH-RPC-V2-C2. Refactor `execute_admin_action_with_context` to call `context.rpc.send_admin_rpc(...)`. The function no longer takes `socket_dir` / `state_dir` / `config_dir` — those parameters are removed in this PR.
- [ ] ARCH-RPC-V2-C3. **Delete** the per-action `NodeConfig { name: format!("SY.architect.{purpose}.{}", ...), uuid_mode: Ephemeral }` block in `execute_admin_action_with_context`. Not refactored into a helper, not behind a flag, gone.
- [ ] ARCH-RPC-V2-C4. Refactor `fetch_inventory_status_data` to reuse the shared client. **Delete** the `SY.architect.status.<uuid>` `NodeConfig` block.
- [ ] ARCH-RPC-V2-C5. All 10 purpose categories (`tool.read`, `plan_compiler.cache_refresh`, `plan_compiler.help_lookup`, `plan_compiler.live_query`, `plan_compiler.pre_validate`, `executor.validate`, `executor.run`, `snapshot.*`, `status`, `scmd`) ship migrated in this PR. Verify with a final grep that no `SY.architect.<purpose>.<uuid>` pattern remains in source.

### D. `SY.architect` system and incoming workers

- [ ] ARCH-RPC-V2-D1. Implement `run_architect_system_worker`.
- [ ] ARCH-RPC-V2-D2. Implement `run_architect_incoming_worker`.
- [ ] ARCH-RPC-V2-D3. Preserve `CONFIG_GET`, `CONFIG_SET`, and
  `VAULT_SECRET_CHANGED` behavior.
- [ ] ARCH-RPC-V2-D4. Preserve impersonation incoming persistence through
  `persist_router_incoming_message`.
- [ ] ARCH-RPC-V2-D5. Add route profile tests with `RpcTestHarness`:
  - system commands route to `system`;
  - user/chat/text route to `incoming`;
  - admin responses with pending waiters complete RPCs;
  - stale or response-only admin responses do not hit `incoming`.

### E. `SY.architect` origin authorization

- [ ] ARCH-RPC-V2-E1. Implement
  `protected_architect_system_action_response`.
- [ ] ARCH-RPC-V2-E2. Implement explicit origin allowlist.
- [ ] ARCH-RPC-V2-E3. Gate `NODE_STATUS_GET`, `CONFIG_GET`, `CONFIG_SET`, and
  `VAULT_SECRET_CHANGED` before dispatch.
- [ ] ARCH-RPC-V2-E4. Unauthorized requests emit `FORBIDDEN` with the matching
  response `msg`.
- [ ] ARCH-RPC-V2-E5. Add regression tests for each protected action with
  `src_l2_name = None`.

### F. (removed) Stable architect AI nodes

This section is gone. The decision closed in v3 is: AI workers stay in-process inside the architect (and admin) processes, sharing the canonical `Arc<RouterDispatcher>`. See "Architect AI workers stay in-process" above and the global unification doc for the dispatcher model.

There is no `SY.architect.<role>@<hive>` split planned or authorized.

### G. SDK Vault client

- [ ] VAULT-RPC-G1. Add `VaultClient` in `crates/fluxbee_sdk/src/vault.rs` over `Arc<RouterDispatcher>`.
- [ ] VAULT-RPC-G2. Preserve caller identity fields needed by Vault: `meta.src_ilk` and source/audit context. Vault auth at `SY.vault` must not see any behavior change.
- [ ] VAULT-RPC-G3. Implement Vault calls via `send_with_matcher` with an explicit `PendingMatcher` for the `MSG_VAULT_*_RESPONSE` shapes — not direct receiver reads, not `connect()` internally.
- [ ] VAULT-RPC-G4. Add tests proving concurrent Vault calls multiplex by `trace_id`.
- [ ] VAULT-RPC-G5. Add tests proving unrelated system messages do not satisfy Vault waiters.
- [ ] VAULT-RPC-G6. Migrate every in-scope caller (see Vault scope decision in "Open decisions") to `VaultClient`. **Delete** the ephemeral `connect()` block at each in-scope site. **No `#[deprecated]` markers** — the old free function is either deleted in the same PR (Option B) or kept fully-public for out-of-scope callers (Option A). No middle state.

### H. `SY.admin` parallel cleanup

Decision #2 is closed (v3): **admin executor stays in-process** inside `SY.admin@<hive>`. No `SY.admin.executor@<hive>` node is introduced.

- [ ] ADMIN-RPC-H1. Keep the existing canonical `SY.admin@<hive>` `RouterDispatcher` profile for admin/system traffic (unchanged from ORPC Block D).
- [ ] ADMIN-RPC-H2. Migrate admin executor OpenAI lookup from ephemeral `connect()` + `resolve_resource` to `VaultClient` over the canonical `Arc<RouterDispatcher>`. **Delete** the ephemeral block at [sy_admin.rs:820](src/bin/sy_admin.rs#L820), do not refactor it into a helper.
- [ ] ADMIN-RPC-H3. **Delete** `NodeUuidMode::Ephemeral` from admin executor Vault lookup.
- [ ] ADMIN-RPC-H4. Verify `VAULT_SECRET_CHANGED` still hot-refreshes `executor_runtime` after migration.
- [ ] ADMIN-RPC-H5. Audit admin inbound system commands for origin authorization parity with the architect Section E gate.

## Acceptance criteria

**Architect transport:**

- No `SY.architect.<purpose>.<random_uuid>` transport clients are created.
- `execute_admin_action_with_context` uses the shared architect `RouterDispatcher`.
- `fetch_inventory_status_data` uses the shared architect `RouterDispatcher`.
- `router_connect_loop`, `router_recv_loop`, `router_sender`, `router_connected` are **deleted** (not commented out, not behind a flag).
- Impersonation send works through `state.rpc.sender_snapshot()`.
- Incoming impersonation messages are persisted by session id via the `incoming` worker.

**Vault:**

- For the 3 architect/admin in-scope call sites, Vault lookups go through `VaultClient` over the canonical `Arc<RouterDispatcher>`. No site in scope re-introduces `connect() + resolve_resource(&sender, &mut receiver, ...)`.
- Vault calls preserve `meta.src_ilk`. `SY.vault` observes no behavioral change.
- Vault calls multiplex through `RouterDispatcher` by `trace_id`.
- The legacy free function `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` is **deleted** from the SDK as part of the global unification PR (see `routerdispatcher_unification_plan.md`). If this architect PR lands first, the function stays public until the global PR removes it; if the global PR lands first, this PR uses `VaultClient` directly. **No `#[deprecated]` attribute is added in either case.**

**Authorization:**

- Protected architect system actions (`NODE_STATUS_GET`, `CONFIG_GET`, `CONFIG_SET`, `VAULT_SECRET_CHANGED`) reject unauthorized callers with `FORBIDDEN` via the matching `*_RESPONSE` name.
- `NODE_STATUS_GET` is gated before `try_handle_default_node_status` runs.
- Pin-table regression test enforces the 4-action table (mirrors orchestrator AF-P1).

**No-legacy invariants:**

- No `#[deprecated]` attribute is introduced anywhere in this PR.
- No `#[cfg(feature = "...")]` selects between bespoke and `RouterDispatcher` transport.
- No env var or runtime flag toggles between old and new ephemeral patterns.
- No "kept temporarily" / "until callers migrate" / "for backward compat" comments are added.

**CI guard:**

- `scripts/architect_no_ephemeral_guard.sh` returns 0 hits for the patterns in ARCH-RPC-V2-A2.

## Open decisions

Decisions #1, #2, #4 from the original draft are **closed** (see "Resolution of open decisions" above). The v3 resolution also closed OD-Vault-Scope (wide) and OD-Lifecycle-Owner (n/a — no splits). Remaining:

### OD-Origins-Config — `architect_allowed_origins` config-driven or hardcoded?

Minor. The initial allowlist (`SY.admin`, `SY.config-routes`, `SY.vault`) can be hardcoded in `protected_architect_system_action_response` callers. Adding hive-config-driven extension can wait. Default: hardcoded for the initial migration.

## Implementation order

This PR is the architect+admin chapter of the global `RouterDispatcher` unification. The order inside this chapter:

1. Architect canonical `RouterDispatcher` + workers + auth (Sections B, D, E).
2. Architect outbound admin migration (Section C).
3. `VaultClient` in SDK over `Arc<RouterDispatcher>` (Section G).
4. Vault scope migration for the 3 in-scope architect/admin sites (the remaining 4 Pattern 3 sites — cognition, io-slack, ai-generic, ai-frontdesk-gov — are migrated by the global plan; see `routerdispatcher_unification_plan.md`).
5. Admin executor Vault lookup migration (Section H1–H5).

CI gate at end of this PR: every grep guard from "Inventory and guardrails v2" returns zero hits inside `sy_architect.rs` and `sy_admin.rs`. No `#[deprecated]` markers introduced. No bespoke `router_recv_loop` artifact remains in `sy_architect.rs`. No ephemeral `connect()` block remains in either binary.

The global unification doc is responsible for the rest of the fleet (other Tipo 1 / Tipo 2 nodes, the Go SDK, the remaining Vault sites). The two PRs share the same SDK abstraction (`RouterDispatcher` + `VaultClient`) — whichever lands first introduces it; the second one uses it.
