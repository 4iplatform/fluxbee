# RouterDispatcher unification — global revamp

Date: 2026-06-01
Status: **Closed including all follow-ups (2026-06-07)** — every objective in §1–§9 met, every P0 + P1 item from [routerdispatcher_unification_followups.md](routerdispatcher_unification_followups.md) closed. All 8 inline dispatchers deleted, all 9 Vault sites migrated to `VaultClient`, `fluxbee_sdk::resolve_resource` free function deleted, `fluxbee_sdk::connect` flipped to `pub(crate)`, Rust `TimerClient::new_with_dispatcher` shipped, architect transport refactor (B + C + D + E) landed, admin Vault Section H1/H2/H3 over canonical dispatcher, admin Section H5 origin-auth gate live, VAULT_SECRET_CHANGED hot-refresh regression tests in CI, Go SDK gained Stale/UnknownResponse classification + visible command-channel drop counters. 7 CI guards under `scripts/router_dispatcher_guards/` active in strict mode and clean.

## Implementation status (2026-06-06)

**Every step closed:**

- Step 1 — SDK rename `RpcClient` → `RouterDispatcher` + 6 Group A diag binaries + `nodes/test/io-test`.
- Step 2 — `VaultClient` over `Arc<RouterDispatcher>` + 3 tests (preserves `meta.src_ilk` / `routing.src_l2_name`, multiplexes by `trace_id`, classifies wrong-msg-same-trace as `InvalidResponse`).
- Step 3 — 6 Tipo 1 puro Rust nodes migrated (`sy_policy`, `sy_config_routes`, `sy_vault`, `sy_identity`, `sy_storage`, `sy_cognition`). Vault sites 4-of-9 / 5-of-9 / 6-of-9 closed.
- Step 4 — io-* bundle migrated (`io-api`, `io-slack`, `io-sim`, `io-linkedhelper`). `RouterInbox` and related plumbing deleted from `nodes/io/common/src/provision.rs`. Vault site 7-of-9 closed.
- Step 5 — ai-* bundle migrated. `fluxbee_ai_sdk::NodeRuntime` rewritten over `Arc<RouterDispatcher>`. `router_client.rs` (`RouterClient`, `RouterReader`, `RouterWriter`, `AiNodeConfig`) deleted. `SharedRouterConnection` deleted from `ai-generic` and `ai-frontdesk-gov`. Vault sites 8-of-9 + 9-of-9 closed.
- Step 6 — Go SDK `RouterDispatcher` built in `go/fluxbee-go-sdk/dispatcher.go` (mirror of Rust API). `TimerClient` is dispatcher-backed through `NewTimerClient(*RouterDispatcher, ...)`; no sender/receiver constructor remains public.
- Step 7 — `sy-wf-rules` migrated. `messageMux` deleted (the partial wrapper was the 8th inline dispatcher in disguise); admin/orchestrator/wf-node clients now call `dispatcher.SendSystemRPC` / `dispatcher.SendAdminRPC`.
- Step 8 — `sy-opa-rules` migrated. The local `RouterClient` wrapper and `forwardOutgoing` are deleted; the service owns the canonical `RouterDispatcher`.
- Step 9 — Architect Vault sites (×2) + admin Vault site (×1) now go through the **canonical persistent** `Arc<RouterDispatcher>` (no per-call ephemeral connect). `blob_sync_diag`, `orch_system_diag`, `inventory_hold_diag`, 4 test nodes, all 5 examples (`wf_client`, `node_test`, `timer_client`, `timer_recurring`, `timer_restart` + `examples/support/timer_example.rs`) migrated. `fluxbee_sdk::resolve_resource` (free function) + `list_then_get_first` helper + legacy `BlobToolkit::publish_blob_and_confirm` deleted. **Rust `TimerClient` gained `new_with_dispatcher`** and its sender/receiver constructor is crate-private. `fluxbee_sdk::connect` is now **`pub(crate)`**, Go raw `Connect` is package-private, and the SDK re-exports were trimmed; the `no_direct_connect.sh` CI guard is active in strict mode and clean.

## Architect transport refactor (§9 — see architect doc for detail)

The architect-specific chapter (`sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md`) was also fully implemented as part of this close-out:

- **Section B (canonical RouterDispatcher)** — `ArchitectState.rpc: Arc<RouterDispatcher>` added; `router_connect_loop`, `router_recv_loop`, `router_sender: Arc<Mutex<Option<NodeSender>>>`, `router_connected: AtomicBool` **all deleted** from `src/bin/sy_architect.rs`. Startup reordered: dispatcher constructed before Vault lookups so `build_architect_ai_runtime` + `resolve_messages_db_url_from_vault` reuse it instead of opening ephemeral connections. `state.rpc.sender_snapshot().is_connected()` replaces the deleted `AtomicBool`. The architect route profile (`build_architect_rpc_profile`) declares `system` (pre-pending: `NODE_STATUS_GET` / `CONFIG_GET` / `CONFIG_SET` / `VAULT_SECRET_CHANGED`) and `incoming` (post-pending: `user` / `chat` / `text`) command channels — no `RouteMatch::Any`.
- **Section C (outbound admin RPC)** — `ArchitectAdminToolContext.rpc` added; `execute_admin_action_with_context` and `fetch_inventory_status_data` now call `context.rpc.send_admin_rpc(...)` / `state.rpc.send_admin_rpc(...)`. The per-action `NodeConfig { name: format!("SY.architect.{purpose}.{}"), uuid_mode: Ephemeral }` block at `execute_admin_action_with_context` and the `SY.architect.status.<uuid>` block at `fetch_inventory_status_data` are **deleted**. All 10 outbound admin RPC purposes (`tool.read`, `plan_compiler.*`, `executor.*`, `snapshot.*`, `status`, `scmd`) now flow through the canonical dispatcher.
- **Section D (system + incoming workers)** — `run_architect_system_worker(state, system_rx)` and `run_architect_incoming_worker(state, incoming_rx)` spawned from `main`. `handle_architect_system_message` reads its sender from `state.rpc.sender_snapshot()` (the deleted lock-protected `Option<NodeSender>` is gone).
- **Section E (origin authorization)** — `protected_architect_system_action_response`, `architect_origin_authorized`, and `build_architect_forbidden_response` implemented. Inbound protected RPCs (`NODE_STATUS_GET` / `CONFIG_GET` / `CONFIG_SET`) from outside the allowlist (`SY.admin@hive`, `SY.config-routes@hive`, `SY.vault@hive`) receive a `*_RESPONSE` with `status: "error", error_code: "FORBIDDEN"` and never reach the handler. `NODE_STATUS_GET` is gated before `try_handle_default_node_status`. `VAULT_SECRET_CHANGED` remains an open broadcast event and is filtered by Vault interest before refresh.
- **Section H1 / H2 / H3 (admin Vault)** — `AdminContext.rpc: Arc<RouterDispatcher>` added (the canonical admin dispatcher built in `main` at `sy_admin.rs:475`); `build_admin_executor_ai_runtime` now takes the dispatcher and constructs `VaultClient` over it. The ephemeral `NodeConfig { uuid_mode: Ephemeral }` + `RouterDispatcher::connect_with_retry` block previously at `sy_admin.rs:816-842` is **deleted**. The `VAULT_SECRET_CHANGED` hot-refresh path (`refresh_admin_executor_ai_runtime`) now flows through the same canonical dispatcher.

## CI guards (§8)

`scripts/router_dispatcher_guards/` contains 7 guard scripts plus `run_all.sh`. The `.github/workflows/router-dispatcher-guards.yml` workflow runs `run_all.sh` on every push and pull request. All guards run on the current tree in strict mode and report clean:

| Guard | Status |
| --- | --- |
| `no_inline_dispatcher.sh` | OK — `RouterInbox`, `SharedRouterConnection`, `fluxbee_ai_sdk::RouterClient`, `messageMux`, `forwardOutgoing` all absent |
| `no_direct_connect.sh` | OK (strict) — no `fluxbee_sdk::connect(...)` outside SDK, no public Go `Connect`, no public Rust `NodeReceiver` / `from_test_channels`, no legacy blob publish+confirm |
| `no_legacy_vault_helper.sh` | OK — `fluxbee_sdk::resolve_resource(...)` references zero call sites (and zero imports, single-line or multi-line use-list) |
| `no_deprecated_attribute_on_dispatcher.sh` | OK — no `#[deprecated]` on `RouterDispatcher` / `RpcClient` / `connect_with_retry` / `VaultClient` / `resolve_resource` |
| `no_shared_receiver.sh` | OK — no Go function aliases a `NodeReceiver` across `Recv()` + arg-pass |
| `architect_no_ephemeral_guard.sh` | OK — no `SY.architect.<purpose>.{}` literals, no `NodeUuidMode::Ephemeral`, no deleted `router_*_loop` / `router_sender` / `router_connected` in `sy_architect.rs` / `sy_admin.rs` |
| `origin_auth_gates_present.sh` | OK — architect Section E + admin H5 gate symbols are present |

Guards use portable `grep -E` scans plus Python multi-line regex where single-line matching would miss Rust/Go use-lists or multi-line call sites.

## Test status at close

- `cargo check --workspace --all-targets` (Rust root) — verde, sólo warnings preexistentes.
- `cargo test -p fluxbee-sdk --lib` — **147/147 verdes** (incluyendo los 3 tests `VaultClient` y todos los tests `TimerClient<'a>` legacy + dispatcher backend).
- `cargo check --workspace --all-targets` (`nodes/io` workspace) — verde.
- Go modules: `go/fluxbee-go-sdk`, `go/sy-timer`, `go/sy-wf-rules` (node tests), `go/sy-opa-rules` (build), `go/nodes/wf/wf-generic` (node tests) — todos verdes.

## What remains (truly nothing in scope of this plan)

- `examples/timer_client.rs`, `examples/timer_recurring.rs`, `examples/timer_restart.rs`, `examples/support/timer_example.rs` — migrated to `TimerClient::new_with_dispatcher`.
- The 5 §8 CI guards + the architect §A2 guard + the admin/architect origin-auth presence guard exist on disk, are wired through `.github/workflows/router-dispatcher-guards.yml`, and pass.
- `fluxbee_sdk::connect` is `pub(crate)`.
- `fluxbee_sdk::resolve_resource` deleted from public surface (and from the SDK).
- All 8 inline dispatchers gone. Every fluxbee process owns exactly one canonical `Arc<RouterDispatcher>` (or `*RouterDispatcher` in Go).

Optional follow-ups that are explicitly **out of scope** of this plan but worth noting for whoever picks them up next:

- Section H4 (regression test for `VAULT_SECRET_CHANGED` hot-refresh through the new canonical dispatcher path). The path exists and is exercised by the existing handlers; what's missing is an end-to-end assertion test.
- Section H5 (admin inbound origin-authorization audit, parity with the new architect Section E gate). Architect now rejects unauthorized callers on protected system actions; admin's parity audit is a separate scoped review.

## Post-close audit (2026-06-06)

A second-pass audit was run after the close. Two real bugs were found and fixed in the same window, plus dead-code cleanup. Anything beyond that is captured in `routerdispatcher_unification_followups.md` (the next-steps tracker).

### Bugs found and fixed in this audit

1. **Origin auth gate was rejecting `VAULT_SECRET_CHANGED` broadcasts.** [src/bin/sy_architect.rs:6664-6678](src/bin/sy_architect.rs#L6664-L6678). SY.vault emits the broadcast with `src_l2_name: None` ([src/bin/sy_vault.rs:470](src/bin/sy_vault.rs#L470)), which my Section E gate treated as "missing src" → `FORBIDDEN` response. Net effect: every Vault hot-refresh on `SY.architect` was silently being shorted to a forbidden response, exactly the opposite of what the section was supposed to enable. Fix: removed `VAULT_SECRET_CHANGED` from `protected_architect_system_action_response` (it is a hive-wide event, not a protected RPC) and documented that the refresh path itself remains end-to-end auth-gated by SY.vault when it re-resolves the secret. Forging the event only triggers a re-resolve the architect would have done anyway.
2. **Dead error path in `VaultClient::send_vault_action`.** [crates/fluxbee_sdk/src/vault.rs:678-701](crates/fluxbee_sdk/src/vault.rs#L678-L701). The post-await `match response.meta.msg { MSG_UNREACHABLE | MSG_TTL_EXCEEDED => err }` was unreachable: the pending matcher classifies those as `terminal_error`, so `send_with_matcher` returns `RpcError::Unreachable`/`TtlExceeded` directly and `map_rpc_error` converts to `VaultError::Service` before this match is ever evaluated. Removed.

### Cleanups completed in this audit

- Deleted ~387 lines of `#[cfg(any())]` zombie blocks left over from the `SharedRouterConnection` migration in [nodes/ai/ai-generic/src/bin/ai_node_runner.rs](nodes/ai/ai-generic/src/bin/ai_node_runner.rs) and [nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs](nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs). These were silently kept-out-of-build but violated `feedback_no_legacy_in_dev` — they referenced the deleted `SharedRouterConnection` API and would have rotted further.
- Removed 14 unused imports/variables across 6 files (`sy_architect.rs`, `sy_vault.rs`, both `ai_node_runner.rs`, `io-test-cognition/src/main.rs`, `examples/timer_recurring.rs`, `examples/wf_client.rs`, `examples/node_test.rs`).
- `architect_origin_authorized` rewritten to `split_once('@') + matches!(...)` so it no longer allocates 3 `String`s per inbound SYSTEM message (was a hot path).

### Audit-final verification (2026-06-06 post-fix)

- `cargo check --workspace --all-targets` — verde. Only 3 unused-import warnings remain, all in `src/bin/fluxbee-publish.rs`, all pre-dating this plan.
- `cargo test -p fluxbee-sdk --lib` — 147/147.
- Go modules: all verde.
- All 7 CI guards — verde.

Scope: SDK (Rust + Go) + all fluxbee nodes + Pattern 3 Vault sites
Related:

- `docs/onworking COA/orchestrator_internal_rpc_multiplexing_tasks.md` — ORPC (orchestrator + admin already on the canonical dispatcher)
- `docs/onworking COA/sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md` — architect-specific chapter (Vault 3 of 7 sites)

## 1. Statement of the problem

The repo currently has **8 independent in-process implementations** of the same conceptual machine: "given one socket connection to the router, multiplex many in-flight RPCs by `trace_id`, route incoming non-RPC traffic into the right inbox, and surface a clean async API to the surrounding node code."

| # | Implementation | Location | Language |
| --- | --- | --- | --- |
| 1 | `RpcClient` (the canonical one) | `crates/fluxbee_sdk/src/rpc.rs` | Rust |
| 2 | `RouterInbox` + `Mutex<RouterInbox>` plumbing | `nodes/io/common/src/provision.rs` | Rust |
| 3 | `SharedRouterConnection` (ai-generic) | `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:442` | Rust |
| 4 | `SharedRouterConnection` (ai-frontdesk-gov, near-copy of #3) | `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:481` | Rust |
| 5 | `fluxbee_ai_sdk::RouterClient` + `RouterReader` + `RouterWriter` (the layer **under** #3 and #4, also driving `NodeRuntime`) | `crates/fluxbee_ai_sdk/src/router_client.rs:31` | Rust |
| 6 | `messageMux` | `go/sy-wf-rules/node/mux.go:20` | Go |
| 7 | `forwardOutgoing` + `RouterClient` | `go/sy-opa-rules/main.go:1264` | Go |
| 8 | naive `loop { recv }` patterns in pure responders | various Tipo 1 nodes | Rust + Go |

The conceptual revelation that motivates this work:

> `RpcClient` is **not** Remote Procedure Call as a new technology. It is the same router socket every node has always had, with `trace_id`-keyed multiplexing and an `OperationalRouteProfile` glued on top. It is router communication, period. The name is misleading. Every node already needs this — the ones that don't have it grew bespoke versions because no single SDK abstraction existed when they were written.

The fix is to recognize this and unify on one abstraction, renamed for clarity: **`RouterDispatcher`**. Every fluxbee process (Rust or Go) owns exactly **one** `Arc<RouterDispatcher>` (or `*RouterDispatcher` in Go), and every router interaction — sending, receiving, RPC waiting, broadcast subscription, Vault lookup — goes through it.

## 2. Non-goals

- Replacing the socket transport. The Unix-socket router connection stays.
- Changing the wire format (`Message`, `Routing`, `Meta`, `MSG_*`). Same on the wire.
- Adding RPC features that don't already exist. This is a unification, not a feature expansion.
- Renaming `OperationalRouteProfile`, `PendingMatcher`, `RouteMatch`, `RouteTarget` — those names are already correct.
- Touching `SY.architect` or `SY.admin` Vault sites — those are the architect doc's job. We migrate the other 4 here.

## 3. Decisions taken (2026-06-01 v3)

| Decision | Resolution |
| --- | --- |
| One transport abstraction or two (Tipo 1 vs Tipo 2)? | **One.** Every node uses `RouterDispatcher`. The Tipo 1/Tipo 2 distinction is operational (do you ever initiate RPCs?), not architectural. Code-wise they are the same. |
| Rename `RpcClient` → `RouterDispatcher`? | **Yes.** Mechanical rename in the SDK. No alias kept. No `pub use RpcClient = RouterDispatcher`. The rename PR does **not** change the `connect_with_retry` signature (it stays `(config: NodeConfig, delay: Duration, profile: OperationalRouteProfile)` to keep the rename mechanical). If we later want a `ConnectRetry` struct, that is a separate PR. |
| Expose `connect(&NodeConfig) -> (NodeSender, NodeReceiver)` to nodes? | **Eventually no — but not in the rename PR.** Today every Tipo 1 puro binary, the 9 `*_diag` binaries, the test nodes, and the AI SDK reach `fluxbee_sdk::connect()` directly. Until all of those callers migrate, `connect` stays `pub`. The change to `pub(crate)` happens in the **last** PR of this plan (after every consumer in the workspace has switched to `RouterDispatcher::connect_with_retry`). That PR also deletes the legacy free function from the public surface of `crates/fluxbee_sdk`. |
| AI workers split into separate nodes? | **No.** Closed in the architect doc. AI workers are async tasks sharing the parent process's `Arc<RouterDispatcher>`. |
| Deprecation period for the old surfaces (`RouterInbox`, `SharedRouterConnection`, `messageMux`, `forwardOutgoing`, free `resolve_resource`)? | **No.** Per `feedback_no_legacy_in_dev`. Each is deleted in the same PR that migrates its last caller. |
| Vault scope? | **Wide (all 9 Pattern 3 sites).** 3 sites in the architect doc (architect ×2, admin ×1), 6 sites here. Two sites — `sy_identity` and `sy_storage` — are **not** ephemeral connections (they call `resolve_resource` with the node's persistent `&NodeSender` + `&mut NodeReceiver`), but they still depend on the `&mut NodeReceiver` ownership model and therefore become incompatible when those nodes adopt `Arc<RouterDispatcher>`. They migrate as part of the same per-node PR that introduces the dispatcher. |

## 4. Target SDK shape (Rust)

### 4.1 The type

```rust
// crates/fluxbee_sdk/src/rpc.rs (rename module to dispatcher.rs in a follow-up commit)
pub struct RouterDispatcher { /* fields unchanged from current RpcClient */ }

impl RouterDispatcher {
    // Signature is preserved exactly as it exists today on RpcClient.
    // The rename PR is mechanical — no parameter shape changes here.
    pub async fn connect_with_retry(
        config: NodeConfig,
        delay: Duration,
        profile: OperationalRouteProfile,
    ) -> Result<Arc<Self>, NodeError>;

    pub fn sender_snapshot(&self) -> NodeSender;
    pub async fn take_command_receiver(&self, name: &str) -> Result<RpcCommandReceiver, RpcError>;
    pub fn subscribe(&self, name: &str) -> Result<broadcast::Receiver<Message>, RpcError>;
    pub async fn send_with_matcher(&self, msg: Message, matcher: PendingMatcher, timeout: Duration) -> Result<Message, RpcError>;
    pub async fn send_system_rpc(&self, ...) -> Result<Message, RpcError>;
    pub async fn send_admin_rpc(&self, ...) -> Result<Message, RpcError>;
    pub async fn drain_pending_waiters(&self);
    pub fn metric_stale_responses(&self) -> u64;
    pub fn metric_unknown_responses(&self) -> u64;
    pub fn metric_route_unmatched(&self) -> u64;
    // ... existing methods, just renamed type
}
```

If a future change wants to introduce a `ConnectRetry` struct (max attempts, backoff, etc.), that lands as its own PR with explicit migration of every existing call site (`sy_orchestrator`, `sy_admin`, `sy_architect`, the 9 diag binaries, the 6 per-node migration PRs). It is **not** smuggled into the rename PR.

### 4.2 What becomes `pub(crate)` — and when

```rust
// crates/fluxbee_sdk/src/node_client.rs
// FINAL state (the last PR of this plan):
pub(crate) async fn connect(config: &NodeConfig) -> Result<(NodeSender, NodeReceiver), NodeError>;
```

Reason: the only legitimate caller of the raw `connect(&NodeConfig) -> (NodeSender, NodeReceiver)` function is the dispatcher itself. Every node-level use of this function today is a symptom of the inline-dispatcher problem this work fixes.

**But the change cannot land in the rename PR.** Today these callers reach `fluxbee_sdk::connect()` directly:

- All 6 Tipo 1 puro binaries via their local `connect_with_retry` wrappers.
- The 9 `*_diag` binaries (some via `connect()`, others via `RpcClient::connect_with_retry` which itself calls `connect()` internally — the second group is fine, the first group blocks `pub(crate)`).
- The 4 test nodes (Section 6.7) which deliberately stay on the raw API.
- `crates/fluxbee_ai_sdk/src/router_client.rs::RouterClient::connect` — until 6.4 deletes it.

The flip to `pub(crate)` happens as the **final PR of the whole plan**, after every production migration above. Test nodes (Section 6.7) are exempted from the `pub(crate)` requirement by being in scope of the rename and **not** in scope of the `pub(crate)` constraint — to preserve that, the SDK keeps a `pub(crate)` plus a `#[cfg(any(test, feature = "test-harness"))] pub use` path, **or**, more cleanly, test nodes are migrated to `RouterDispatcher::connect_with_retry` in the same final PR. Pick the migration route — it's cheaper than a permanent feature shim.

**CI guard activated by that final PR:** the function must not appear in any `nodes/**`, `src/bin/**`, or `crates/**` outside `crates/fluxbee_sdk/src/`.

### 4.3 Test harness

`RpcTestHarness` stays as-is and is renamed `RouterDispatcherTestHarness`. Internal field types are renamed mechanically.

## 5. Target SDK shape (Go) — this is **construction**, not rename

The Go SDK started with no multiplexed `RpcClient`: `rpc.go` only had bare request builders/parsers and synchronous receiver-consuming helpers. The migration deletes those receiver-consuming helpers (`AwaitSystemResponse`, `RequestSystemRPC`) and makes `RouterDispatcher` the only public router transport.

- [go/sy-timer/main.go:141 + main.go:202](go/sy-timer/main.go#L141) — naive Recv loop.
- [go/nodes/wf/wf-generic/node/node.go:319 + node.go:367](go/nodes/wf/wf-generic/node/node.go#L319) — naive Recv loop, **and** at line 328 it hands the same `*NodeReceiver` to `NewSDKTimerSender(sender, receiver, ...)` so the `TimerClient` can `Recv()` on the same receiver the main loop is using. That is a latent multiplexing bug (whichever Recv wins the next message wins it) that today only doesn't bite us because the workload is light.
- [go/sy-wf-rules/node/service.go:93](go/sy-wf-rules/node/service.go#L93) — same, then drives via `messageMux`.
- [go/sy-opa-rules/main.go:1240](go/sy-opa-rules/main.go#L1240) — same, then drives via bespoke `RouterClient` + `forwardOutgoing`.

So the Go side is **green-field construction**, not a rename:

```go
// go/fluxbee-go-sdk/dispatcher.go — NEW FILE (not a rename)
type RouterDispatcher struct {
    sender   *NodeSender
    receiver *NodeReceiver
    profile  OperationalRouteProfile
    // pending: map[traceID]chan Message
    // command channels: map[string]chan Message
    // broadcast channels: map[string]chan Message
    // ...
}

func ConnectWithRetry(cfg NodeConfig, delay time.Duration, profile OperationalRouteProfile) (*RouterDispatcher, error)

func (d *RouterDispatcher) SenderSnapshot() *NodeSender
func (d *RouterDispatcher) TakeCommandReceiver(name string) (CommandReceiver, error)
func (d *RouterDispatcher) Subscribe(name string) (<-chan Message, error)
func (d *RouterDispatcher) SendWithMatcher(ctx context.Context, msg Message, matcher PendingMatcher, timeout time.Duration) (Message, error)
func (d *RouterDispatcher) SendSystemRPC(...) (Message, error)
func (d *RouterDispatcher) SendAdminRPC(...) (Message, error)
func (d *RouterDispatcher) Close() error
```

`OperationalRouteProfile`, `PendingMatcher`, `RouteMatch`, `RouteTarget` are mirrored from the Rust SDK with identical semantics. **Const-asserts (compile-time) on the Go side that the protocol byte sizes match `feedback_rust_repr_c_to_go_sizes`** continue to apply — we do **not** change the wire layout in this work.

**`TimerClient` must move onto the dispatcher in the same PR.** Today `TimerClient` calls `receiver.Recv()` directly (see [timer_client.go](go/fluxbee-go-sdk/timer_client.go) and the `NewSDKTimerSender` constructor in [actions.go:341](go/nodes/wf/wf-generic/node/actions.go#L341)). Once `RouterDispatcher` owns the receiver, `TimerClient` must consume timer responses through `dispatcher.SendWithMatcher(...)` keyed on `trace_id`, not through the shared receiver. The Go SDK bring-up PR includes:

1. Construct `RouterDispatcher` with the existing wire helpers (`BuildSystemRequest`, `ParseSystemResponse`) reused inside the new methods.
2. Rewrite `TimerClient` to take `*RouterDispatcher` instead of `*NodeSender`+`*NodeReceiver`. All `TimerClient` methods (`Schedule`, `Cancel`, `Reschedule`, etc.) call `dispatcher.SendWithMatcher` or `dispatcher.SendSystemRPC`.
3. Update `wf-generic`'s `NewSDKTimerSender` and the main Recv loop to use dispatcher command/broadcast channels. The shared-`*NodeReceiver` aliasing bug disappears as a side effect.
4. Update `sy-timer`'s `Service` to own `*RouterDispatcher` and replace its `receiver.Recv` loop with a `dispatcher.TakeCommandReceiver("system")` channel.

The bare builders/parsers in `rpc.go` (`BuildSystemRequest`, `BuildSystemResponse`, `ParseSystemResponse`) stay as building blocks. The receiver-consuming helpers (`AwaitSystemResponse`, `RequestSystemRPC`) are deleted once no consumer is left — same final-state rule as the Rust `connect()` decision in 4.2.

## 6. Per-node migration plan

The migration table below covers every fluxbee process. Each row is a self-contained PR (small, mechanical), except where noted as bundled.

### 6.1 Rust nodes — pure responders (Tipo 1)

These currently have a naive `loop { receiver.recv() }` and have never multiplexed. They get an `Arc<RouterDispatcher>` instead. Behavior is unchanged from the outside.

| Node | Current pattern | PR scope |
| --- | --- | --- |
| `sy_identity` (`src/bin/sy_identity.rs`) | bespoke `loop { recv }` + local `connect_with_retry` wrapper (line 3955) + Vault site at line 5272 using persistent `&mut NodeReceiver` | introduce `Arc<RouterDispatcher>`, delete local `connect_with_retry` wrapper, migrate Vault site to `VaultClient` (4-of-9) |
| `sy_vault` (`src/bin/sy_vault.rs`) | bespoke `loop { recv }` + local `connect_with_retry` wrapper (line 1593) | introduce `Arc<RouterDispatcher>`, delete local wrapper |
| `sy_storage` (`src/bin/sy_storage.rs`) | bespoke `loop { recv }` + local `connect_with_retry` wrapper (line 2852) + Vault site at line 2417 using persistent `&mut NodeReceiver` | introduce `Arc<RouterDispatcher>`, delete local wrapper, migrate Vault site to `VaultClient` (5-of-9) |
| `sy_policy` (`src/bin/sy_policy.rs`) | bespoke `loop { recv }` + local `connect_with_retry` wrapper (line 263) | introduce `Arc<RouterDispatcher>`, delete local wrapper |
| `sy_config_routes` (`src/bin/sy_config_routes.rs`) | bespoke `loop { recv }` + local `connect_with_retry` wrapper (line 867) | introduce `Arc<RouterDispatcher>`, delete local wrapper |
| `sy_cognition` (`src/bin/sy_cognition.rs`) | bespoke `loop { recv }` + local `connect_with_retry` wrapper (line 4261) + ephemeral Vault site at line 1610 | introduce `Arc<RouterDispatcher>`, delete local wrapper, migrate Vault site to `VaultClient` (6-of-9) |

**Note on local `connect_with_retry` wrappers.** Each of these binaries has its own local `connect_with_retry(&NodeConfig, Duration)` that wraps `fluxbee_sdk::connect()`. They were copy-pasted before the canonical `RouterDispatcher::connect_with_retry` existed. Each per-node PR **deletes** its local wrapper in the same change that adopts the canonical one. No node ships with both coexisting.

Each PR:

1. Add `Arc<RouterDispatcher>` to the node's state.
2. Replace `connect(&cfg) + loop { receiver.recv() }` with `RouterDispatcher::connect_with_retry(cfg, profile, retry)` + per-channel handlers.
3. **Delete** the bespoke recv loop in the same PR. Not behind a flag, not commented out.
4. CI guard: no `connect(&NodeConfig)` literal call remains in the node's source.

### 6.2 Rust nodes — bespoke inline dispatchers (Tipo 2)

These have their own dispatcher invented. Their per-node PR deletes the bespoke implementation in the same change.

| Node | Bespoke implementation | PR scope |
| --- | --- | --- |
| `io-api` (`nodes/io/io-api/src/main.rs`) | `RouterInbox` consumer | adopt `Arc<RouterDispatcher>`, **delete** `RouterInbox` usages in this node |
| `io-slack` (`nodes/io/io-slack/src/main.rs`) | `RouterInbox` consumer + Pattern 3 Vault site at line 751 | same + migrate Vault site (7-of-9) |
| `io-sim` (`nodes/io/io-sim/src/main.rs`) | `RouterInbox` consumer | same |
| `io-linkedhelper` (`nodes/io/io-linkedhelper/src/main.rs`) | `RouterInbox` consumer | same |
| `ai-generic` (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs`) | `SharedRouterConnection` (lines 442–479) + Pattern 3 Vault site at line 1691 | adopt `Arc<RouterDispatcher>`, **delete** `SharedRouterConnection`, **delete** `wait_for_shared_reconnect` (replaced by dispatcher reconnect signal); migrate Vault site (8-of-9) |
| `ai-frontdesk-gov` (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`) | `SharedRouterConnection` (lines 481–520, near-copy of ai-generic) + Pattern 3 Vault site at line 1720 | same + migrate Vault site (9-of-9, deletes free `resolve_resource`) |

Bundling note: `io-api`, `io-slack`, `io-sim`, `io-linkedhelper` all consume `RouterInbox` from `nodes/io/common/src/provision.rs`. The four migrations land **together** so that `RouterInbox` itself can be deleted in the same PR. Keeping `RouterInbox` "for backward compat" while migrating consumers one at a time is exactly the legacy pattern this plan forbids.

Similarly, the two `SharedRouterConnection` definitions (ai-generic and ai-frontdesk-gov) land in **one PR** that removes both inline structs simultaneously.

### 6.3 Eliminate the common-crate plumbing

| File / type | Action |
| --- | --- |
| `nodes/io/common/src/provision.rs::RouterInbox` | **Delete** (after 6.2 io-* migrations land) |
| `nodes/io/common/src/provision.rs::*Inbox*` plumbing structs that exist only to pass `Arc<Mutex<RouterInbox>>` around | **Delete** as part of the same PR |
| `nodes/ai/ai-generic/src/bin/ai_node_runner.rs::SharedRouterConnection` | **Delete** in the ai-generic migration |
| `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs::SharedRouterConnection` | **Delete** in the ai-frontdesk-gov migration |

### 6.4 `fluxbee_ai_sdk` — the dispatcher that lives **under** the ai-* nodes

`crates/fluxbee_ai_sdk` is the AI-runtime support crate consumed by `ai-generic` and `ai-frontdesk-gov`. It defines its own router-side abstraction layer that the two `SharedRouterConnection` structs sit on top of:

- `crates/fluxbee_ai_sdk/src/router_client.rs::RouterClient` — wraps `fluxbee_sdk::connect()` and exposes `read` / `write` / `read_timeout` / `split`. Internally owns a `NodeReceiver` + `NodeSender`. No `trace_id` multiplexing — every caller of `read()` competes for the next message off the receiver.
- `crates/fluxbee_ai_sdk/src/router_client.rs::RouterReader` and `::RouterWriter` — the split halves.
- `crates/fluxbee_ai_sdk/src/runtime.rs::NodeRuntime` — owns a `RouterClient` and drives the AI node's main loop.
- `crates/fluxbee_ai_sdk/src/lib.rs` re-exports `AiNodeConfig` and `RouterClient` for downstream nodes.

This is the 8th dispatcher. It is the **base layer** of both `SharedRouterConnection` implementations: deleting `SharedRouterConnection` in 6.2 without touching this would leave the AI crate driving the router through a non-multiplexed `connect()`+`recv()` wrapper, which is exactly the legacy pattern this work eliminates.

PR scope (bundled with the ai-* migrations in 6.2):

1. **Delete** `crates/fluxbee_ai_sdk/src/router_client.rs::{RouterClient, RouterReader, RouterWriter, AiNodeConfig}` in the same PR that deletes both `SharedRouterConnection` definitions.
2. **Rewrite** `crates/fluxbee_ai_sdk/src/runtime.rs::NodeRuntime` to own an `Arc<RouterDispatcher>` directly instead of a `RouterClient`. `NodeRuntime::new` takes `Arc<RouterDispatcher>` as a parameter; the caller (ai-generic, ai-frontdesk-gov) constructs the dispatcher and hands it in.
3. The migrated `ai-generic` and `ai-frontdesk-gov` binaries construct the canonical `Arc<RouterDispatcher>` themselves and pass it to `NodeRuntime`. The path `binary → SharedRouterConnection → fluxbee_ai_sdk::RouterClient → fluxbee_sdk::connect` collapses to `binary → Arc<RouterDispatcher> → fluxbee_sdk router internals`.
4. The lib.rs re-export line `pub use router_client::{AiNodeConfig, RouterClient};` is **deleted**. Any external consumer of `fluxbee_ai_sdk` that imported `RouterClient` is updated in the same PR. (Current count of such consumers in the repo: 2 — `ai-generic` and `ai-frontdesk-gov`. There are no others.)

**CI guard addition:** `no_inline_dispatcher.sh` also fails on `struct RouterClient\b` inside `crates/fluxbee_ai_sdk/`. Combined with the existing global guard (no `connect(&NodeConfig)` outside the SDK), this closes the path that created this dispatcher in the first place.

### 6.5 Go nodes

Reclassified after closer reading (see Section 5): **none** of these are mechanical renames. All four migrate from raw `Connect + Recv` patterns to `RouterDispatcher`.

| Node | Current pattern | PR scope |
| --- | --- | --- |
| `sy-timer` (`go/sy-timer/`) | raw `fluxbeesdk.Connect` + `s.receiver.Recv(ctx)` loop at [main.go:202](go/sy-timer/main.go#L202) | adopt `*RouterDispatcher`, replace Recv loop with `dispatcher.TakeCommandReceiver("system")` |
| `wf-generic` (Go runtime, `go/nodes/wf/wf-generic/`) | raw `sdk.Connect` + `receiver.Recv(ctx)` loop at [node.go:367](go/nodes/wf/wf-generic/node/node.go#L367) AND shares the same `*NodeReceiver` with `TimerClient` via `NewSDKTimerSender(sender, receiver, ...)` at [node.go:328](go/nodes/wf/wf-generic/node/node.go#L328) — latent multiplexing bug | adopt `*RouterDispatcher`, route timer responses through the dispatcher (`SendWithMatcher`), eliminate the shared-receiver aliasing |
| `sy-wf-rules` (`go/sy-wf-rules/`) | inline `messageMux` (`node/mux.go`) on top of raw `Connect` | adopt `*RouterDispatcher`, **delete** `messageMux` |
| `sy-opa-rules` (`go/sy-opa-rules/`) | inline `RouterClient` + `forwardOutgoing` (`main.go:1264`, `1307`) on top of raw `Connect` | adopt `*RouterDispatcher`, **delete** `RouterClient` + `forwardOutgoing` |

The Go SDK bring-up (Section 5) is a precondition for **all four** Go node migrations. The Go SDK PR lands first; the four Go-node PRs follow. `wf-generic` cannot land independently of the Go SDK `TimerClient` refactor, because today `TimerClient` shares the same receiver with the main loop — there is no way to migrate one without the other.

### 6.6 Diagnostic binaries

The repo has 9 `src/bin/*_diag.rs` binaries that talk to the router. They split into two groups by how they connect today:

**Group A — already use `RpcClient::connect_with_retry`** (mechanical rename in step 1):

- `src/bin/identity_merge_diag.rs`
- `src/bin/identity_negative_diag.rs`
- `src/bin/identity_replica_sync_diag.rs`
- `src/bin/admin_internal_command_diag.rs`
- `src/bin/io_test_diag.rs`
- `src/bin/identity_provision_complete_diag.rs`

These migrate in the **SDK rename PR (step 1)** because they already use the canonical type — leaving them on the old name would break `cargo build --bins`.

**Group B — call raw `fluxbee_sdk::connect()` directly** (migrate in the final PR, step 9):

- `src/bin/orch_system_diag.rs`
- `src/bin/inventory_hold_diag.rs`
- `src/bin/blob_sync_diag.rs`

These keep working unchanged through steps 1–8 because `connect()` remains `pub` throughout. In step 9 (`pub(crate)` flip), they migrate to `RouterDispatcher::connect_with_retry` in the same PR that activates the CI guard.

If a future audit shows a `_diag` binary using one of the inline dispatcher patterns this plan eliminates (today none do), that one becomes its own line item.

### 6.7 Test nodes — explicitly out of scope until step 9

`nodes/test/ai-test-cognition`, `nodes/test/ai-test-gov`, `nodes/test/io-test-cognition`, and `nodes/test/io-test` use the `connect()` + `loop { recv() }` pattern and run only on-demand against ephemeral environments. They are **out of scope for steps 1–8**:

- They are test harnesses, not production nodes — no inventory presence, no fleet operations care about them.
- Their multiplexing behavior does not matter: ephemeral runs do not stress concurrent in-flight RPCs.
- Migrating them mid-plan would pull `RouterDispatcher` plumbing into test code that does not benefit from it.

**They are migrated in step 9** alongside the Group B diag binaries, because keeping them on `pub` `connect()` while everything else is on `pub(crate)` requires a permanent feature flag or test-only `pub use` — either is a legacy shim that violates `feedback_no_legacy_in_dev`. Migrating the test nodes mechanically in step 9 is cheaper than maintaining that escape hatch. The migration is purely the type swap (`RouterDispatcher::connect_with_retry` instead of `connect()`), no behavior change.

A `_test_*` binary surfacing a *new* inline dispatcher between now and step 9 is treated as a test-only concern and does not gate the plan.

## 7. `VaultClient` and the 6 remaining Pattern 3 sites

`VaultClient` is introduced by whichever PR lands first — the architect-specific PR or any of the 6.x PRs. After it exists, the global plan migrates these 6 sites (the architect doc migrates the other 3):

| Site | File | Connection style today | Migrated in |
| --- | --- | --- | --- |
| `sy_identity` | `src/bin/sy_identity.rs:5272` | Persistent `&NodeSender` + `&mut NodeReceiver` (Option-C fix) — not ephemeral | 6.1 sy_identity PR |
| `sy_storage` | `src/bin/sy_storage.rs:2417` | Persistent `&NodeSender` + `&mut NodeReceiver` | 6.1 sy_storage PR |
| `sy_cognition` | `src/bin/sy_cognition.rs:1610` | Ephemeral `connect()` | 6.1 sy_cognition PR |
| `io-slack` | `nodes/io/io-slack/src/main.rs:751` | Ephemeral `connect()` | 6.2 io-slack PR |
| `ai-generic` | `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1691` | Ephemeral `connect()` | 6.2 ai-generic PR |
| `ai-frontdesk-gov` | `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:1720` | Ephemeral `connect()` | 6.2 ai-frontdesk-gov PR |

The two non-ephemeral sites (`sy_identity`, `sy_storage`) deserve a separate note: today they pass `&mut NodeReceiver` *through the main recv loop*, which works only because the loop is a single naive `receiver.recv()` consumer. Once those nodes adopt `Arc<RouterDispatcher>`, the receiver is owned by the dispatcher and these call sites stop compiling. They are **not** optional in the migration — they must convert to `VaultClient` in the same PR that introduces the dispatcher for that node, otherwise that PR doesn't build.

The PR that migrates the **last** site (whichever it is in scheduling order — across the architect doc and the 6 here) also **deletes** `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` from the SDK. No `#[deprecated]` attribute is added at any point.

## 8. CI guards (added incrementally as each migration lands)

These guards run substring/regex scans of source text. **Substring scans are fragile** (the previous draft proposed `connect(&NodeConfig` and `resolve_resource(&NodeSender`, which miss the actual code patterns — `connect(config)`, `connect(&node_config)`, multi-line `resolve_resource(\n  &sender,\n  ...`). Each guard below specifies what it actually has to find and how.

Place under `scripts/router_dispatcher_guards/`. Each guard uses `ast-grep` (preferred — it parses real Rust/Go syntax) with a regex fallback when ast-grep is unavailable in CI.

1. **`no_inline_dispatcher.sh`** — uses ast-grep patterns:
   - Rust: `struct $NAME { $$$ }` where `$NAME` matches `RouterInbox`, `SharedRouterConnection`, or `RouterClient` (the AI SDK one), allowed only in their original definition path (which will be the SDK or deleted entirely).
   - Go: `type $NAME struct { $$$ }` where `$NAME` matches `messageMux`, plus `func ($_ *$NAME) forwardOutgoing($_)` for the OPA case.
2. **`no_direct_connect.sh`** — call-site match, not substring:
   - Rust ast-grep pattern: `fluxbee_sdk::connect($$$)` and `connect($$$).await` inside files matched by `nodes/**`, `src/bin/**`, `crates/!(fluxbee_sdk)/**`. (The negation excludes the SDK itself.)
   - Also flag local re-implementations: any `async fn connect_with_retry($$$) { $$$ }` inside `src/bin/sy_*.rs` is forbidden after the per-node migration of that binary lands. Whitelist by path is maintained as part of the migration progress, not as a permanent escape hatch.
   - Go ast-grep pattern: `fluxbeesdk.Connect($$$)` inside `go/**` outside the SDK package.
3. **`no_legacy_vault_helper.sh`** — function-call match (handles multi-line):
   - Rust ast-grep pattern: `fluxbee_sdk::resolve_resource($$$)` anywhere in the workspace. Activates in the PR that migrates the 9th Vault site. The pattern matches regardless of whether `&sender` is on the same line as the open-paren — that was the gap in the previous draft.
   - Also flags `use fluxbee_sdk::resolve_resource` and re-exports.
4. **`no_deprecated_attribute_on_dispatcher.sh`** — ast-grep:
   - Rust pattern: `#[deprecated$$$]` immediately preceding any item named `RouterDispatcher`, `RpcClient`, `connect_with_retry`, `VaultClient`, or `resolve_resource`.
5. **`no_shared_receiver.sh`** (Go-only, added when `wf-generic` migrates) — ast-grep pattern: any function whose body both reads from `$RECEIVER.Recv($$$)` *and* passes `$RECEIVER` to another function (e.g., `NewSDKTimerSender($_, $RECEIVER, $$$)`). This is the latent bug Section 5 calls out; the guard prevents it from coming back.

Each guard's first turn-on is in the PR that finishes its scope. Until that PR, the guard exists in the repo but is `exit 0`-stubbed with a `TODO: enable after <PR-name>` comment, or is added only at activation time — either is fine, pick one and stick to it for the whole plan.

If ast-grep is not installed in CI, the guards fall back to multi-line `pcregrep -M` patterns that explicitly match across whitespace/newlines. No guard relies on a single-line regex of a function signature.

## 9. Implementation order

The order respects four constraints: (a) the SDK abstraction must exist before consumers migrate, (b) we never run with two abstractions coexisting longer than one PR boundary, (c) we don't ship a PR that touches more than one "type of node" at a time (so review remains tractable), and (d) `pub(crate)` on `connect()` is the **last** step, after every external caller is gone.

1. **SDK Rust rename PR** — `RpcClient` → `RouterDispatcher` as a type alias rename only. `connect_with_retry` keeps its current `(NodeConfig, Duration, OperationalRouteProfile)` signature. `RouterDispatcherTestHarness` renamed. `connect()` stays `pub`. Call-site updates limited to mechanical type-name swaps in:
   - `src/bin/sy_orchestrator.rs`, `src/bin/sy_admin.rs`, `src/bin/sy_architect.rs` (already use the canonical type).
   - The 6 Group A `*_diag` binaries listed in Section 6.6.
   - `nodes/test/io-test/src/main.rs`.
   The 3 Group B `*_diag` binaries (`orch_system_diag`, `inventory_hold_diag`, `blob_sync_diag`) are **not touched** in this PR — they keep working unchanged because they only use `fluxbee_sdk::connect()` (which is still `pub`).
2. **SDK Rust `VaultClient` PR** — new `crates/fluxbee_sdk/src/vault.rs` over `Arc<RouterDispatcher>`. No call site migrations yet. The architect-specific PR may instead land first and introduce `VaultClient`; whichever PR is first does this, the other one uses what already exists.
3. **Tipo 1 puro migration PRs** — one per node, no shared blast radius. Each PR adopts `Arc<RouterDispatcher>`, deletes the binary's local `connect_with_retry` wrapper, and (if applicable) migrates its Vault site. Order is free among themselves but they all land before any later step that depends on `connect()` being `pub(crate)`:
   - `sy_policy`
   - `sy_config_routes`
   - `sy_vault`
   - `sy_identity` (+ Vault site, 4-of-9)
   - `sy_storage` (+ Vault site, 5-of-9)
   - `sy_cognition` (+ Vault site, 6-of-9)
4. **io-* bundle PR** — `io-api`, `io-slack`, `io-sim`, `io-linkedhelper` migrate together; `RouterInbox` deleted from `nodes/io/common`; io-slack Vault site migrated (7-of-9).
5. **ai-* + fluxbee_ai_sdk bundle PR** — both `SharedRouterConnection` definitions deleted; `fluxbee_ai_sdk::RouterClient` / `RouterReader` / `RouterWriter` / `AiNodeConfig` deleted; `NodeRuntime` rewritten to take `Arc<RouterDispatcher>`; both Vault sites migrated (8-of-9 and 9-of-9). This PR **deletes** the free `resolve_resource` function from the SDK because it closes the last Vault site.
6. **Go SDK bring-up PR** — construct `RouterDispatcher` in `go/fluxbee-go-sdk/dispatcher.go`, refactor `TimerClient` to use it, refactor `sy-timer` and `wf-generic` to use it. This PR is bundled because `wf-generic` cannot migrate independently of the `TimerClient` refactor (see Section 5).
7. **`sy-wf-rules` migration PR** — delete `messageMux`.
8. **`sy-opa-rules` migration PR** — delete `RouterClient` + `forwardOutgoing`.
9. **Final cleanup PR** — `fluxbee_sdk::connect()` → `pub(crate)`. The 5 remaining `*_diag` binaries that still called raw `connect()` migrate to `RouterDispatcher::connect_with_retry` in this PR. The 4 test nodes in Section 6.7 migrate too (cheaper than maintaining a test-only escape hatch in the SDK). The CI guard `no_direct_connect.sh` is activated. The corresponding Go-side helpers (`AwaitSystemResponse`, `RequestSystemRPC`) are deleted.

After step 9: no inline dispatcher remains anywhere, no second router connection is opened by any node for any reason, no caller in the workspace reaches `fluxbee_sdk::connect()`, and `RouterDispatcher` is the single router-side abstraction in the codebase.

## 10. Why this is the right shape

This work is not adding a new abstraction. It is acknowledging that the abstraction we already have (`RpcClient`, in its v2 form) is the correct one and was always going to be the answer — the other 7 inline implementations are evidence of organic growth, not of independent design decisions. Each of them, looked at carefully, is a partial reimplementation of the same trace_id-multiplex + route-profile machine (or a thin wrapper that would have been a partial reimplementation as soon as it grew a second concurrent caller).

The risk of leaving them is divergence: a future feature added to the canonical `RouterDispatcher` (a metric, a behavior, a security gate) lands in 1 of 7 places. ORPC v1 already demonstrated what that costs.

The cost of doing this work is bounded: every individual PR is mechanical, the wire format does not change, no external consumer (router, vault, identity) sees any behavior difference. The hard part — the canonical dispatcher itself — is already written and proven by orchestrator and admin.

## 11. What this plan does **not** authorize

- Re-architecting `OperationalRouteProfile`. The profile DSL is fine.
- Introducing new RPC patterns. `send_with_matcher`, `send_system_rpc`, `send_admin_rpc` cover every use case in the repo.
- Splitting any node (`SY.architect.archi@<hive>`, `SY.admin.executor@<hive>`, etc.). Settled in the architect doc.
- Changing tenant/identity/ILK semantics. The dispatcher carries the same `meta` it always did.
- Adding a `#[deprecated]` attribute, a feature flag, a transitional API, or a "v2.1 → v2.2" comment anywhere in this work. Per `feedback_no_legacy_in_dev` (and per the same rule that closed ORPC v1 cleanly): the migration is complete within each PR or it doesn't ship.
