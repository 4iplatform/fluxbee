# RouterDispatcher unification — global revamp

Date: 2026-06-01
Status: Draft
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
| Rename `RpcClient` → `RouterDispatcher`? | **Yes.** Mechanical rename in the SDK. No alias kept. No `pub use RpcClient = RouterDispatcher`. |
| Expose `connect(&NodeConfig) -> (NodeSender, NodeReceiver)` to nodes? | **No.** That function becomes `pub(crate)`. Nodes get a connection by calling `RouterDispatcher::connect_with_retry`. The two-tuple form is an internal detail. |
| AI workers split into separate nodes? | **No.** Closed in the architect doc. AI workers are async tasks sharing the parent process's `Arc<RouterDispatcher>`. |
| Deprecation period for the old surfaces (`RouterInbox`, `SharedRouterConnection`, `messageMux`, `forwardOutgoing`, free `resolve_resource`)? | **No.** Per `feedback_no_legacy_in_dev`. Each is deleted in the same PR that migrates its last caller. |
| Vault scope? | **Wide (all 7 Pattern 3 sites).** 3 sites in the architect doc, 4 sites here. |

## 4. Target SDK shape (Rust)

### 4.1 The type

```rust
// crates/fluxbee_sdk/src/rpc.rs (rename module to dispatcher.rs in a follow-up commit)
pub struct RouterDispatcher { /* fields unchanged from current RpcClient */ }

impl RouterDispatcher {
    pub async fn connect_with_retry(
        config: NodeConfig,
        profile: OperationalRouteProfile,
        retry: ConnectRetry,
    ) -> Result<Arc<Self>, RpcError>;

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

### 4.2 What becomes `pub(crate)`

```rust
// crates/fluxbee_sdk/src/node_client.rs
pub(crate) async fn connect(config: &NodeConfig) -> Result<(NodeSender, NodeReceiver), NodeError>;
```

Reason: the only legitimate caller of the raw `connect(&NodeConfig) -> (NodeSender, NodeReceiver)` function is the dispatcher itself. Every node-level use of this function in the repo today is a symptom of the inline-dispatcher problem this work fixes. After the migration, the function exists only as an implementation detail of `RouterDispatcher::connect_with_retry`. **CI guard:** the function must not appear in any `nodes/**`, `src/bin/**`, or `crates/**` outside `crates/fluxbee_sdk/src/`.

### 4.3 Test harness

`RpcTestHarness` stays as-is and is renamed `RouterDispatcherTestHarness`. Internal field types are renamed mechanically.

## 5. Target SDK shape (Go)

The Go SDK currently has `rpc.go` (modeled after the Rust `RpcClient`, but with less coverage — Go nodes use it for `sy-timer` and `wf-generic` and not much else). Two Go binaries (`sy-wf-rules`, `sy-opa-rules`) wrote their own bespoke versions on top of the lower-level `client.go` connection because the Go `rpc.go` did not cover their needs at the time.

We bring the Go SDK to parity:

```go
// go/fluxbee-go-sdk/dispatcher.go  (rename rpc.go)
type RouterDispatcher struct { /* renamed from RpcClient if it exists, else introduced */ }

func ConnectWithRetry(cfg NodeConfig, profile OperationalRouteProfile, retry ConnectRetry) (*RouterDispatcher, error)

func (d *RouterDispatcher) SenderSnapshot() NodeSender
func (d *RouterDispatcher) TakeCommandReceiver(name string) (RpcCommandReceiver, error)
func (d *RouterDispatcher) Subscribe(name string) (<-chan Message, error)
func (d *RouterDispatcher) SendWithMatcher(msg Message, matcher PendingMatcher, timeout time.Duration) (Message, error)
func (d *RouterDispatcher) SendSystemRPC(...) (Message, error)
func (d *RouterDispatcher) SendAdminRPC(...) (Message, error)
func (d *RouterDispatcher) Close() error
```

`OperationalRouteProfile`, `PendingMatcher`, `RouteMatch`, `RouteTarget` are mirrored from the Rust SDK with identical semantics. **Const-asserts (compile-time) on the Go side that the protocol byte sizes match `feedback_rust_repr_c_to_go_sizes`** continue to apply — we do **not** change the wire layout in this work.

## 6. Per-node migration plan

The migration table below covers every fluxbee process. Each row is a self-contained PR (small, mechanical), except where noted as bundled.

### 6.1 Rust nodes — pure responders (Tipo 1)

These currently have a naive `loop { receiver.recv() }` and have never multiplexed. They get an `Arc<RouterDispatcher>` instead. Behavior is unchanged from the outside.

| Node | Current pattern | PR scope |
| --- | --- | --- |
| `sy_identity` (`src/bin/sy_identity.rs`) | bespoke `loop { recv }` | introduce `Arc<RouterDispatcher>`, single command channel; CI guard added |
| `sy_vault` (`src/bin/sy_vault.rs`) | bespoke `loop { recv }` | same |
| `sy_storage` (`src/bin/sy_storage.rs`) | bespoke `loop { recv }` | same |
| `sy_policy` (`src/bin/sy_policy.rs`) | bespoke `loop { recv }` | same |
| `sy_config_routes` (`src/bin/sy_config_routes.rs`) | bespoke `loop { recv }` | same |
| `sy_cognition` (`src/bin/sy_cognition.rs`) | bespoke `loop { recv }` + Pattern 3 Vault site at line 1594 | same + migrate Vault site to `VaultClient` (4-of-7) |

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
| `io-slack` (`nodes/io/io-slack/src/main.rs`) | `RouterInbox` consumer + Pattern 3 Vault site at line 739 | same + migrate Vault site (5-of-7) |
| `io-sim` (`nodes/io/io-sim/src/main.rs`) | `RouterInbox` consumer | same |
| `io-linkedhelper` (`nodes/io/io-linkedhelper/src/main.rs`) | `RouterInbox` consumer | same |
| `ai-generic` (`nodes/ai/ai-generic/src/bin/ai_node_runner.rs`) | `SharedRouterConnection` (lines 442–479) + Pattern 3 Vault site at line 1682 | adopt `Arc<RouterDispatcher>`, **delete** `SharedRouterConnection`, **delete** `wait_for_shared_reconnect` (replaced by dispatcher reconnect signal); migrate Vault site (6-of-7) |
| `ai-frontdesk-gov` (`nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs`) | `SharedRouterConnection` (lines 481–520, near-copy of ai-generic) + Pattern 3 Vault site at line 1708 | same + migrate Vault site (7-of-7) |

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

| Node | Current pattern | PR scope |
| --- | --- | --- |
| `sy-timer` (`go/sy-timer/`) | already uses `fluxbee-go-sdk/rpc.go` | rename usage to `RouterDispatcher` mechanically |
| `wf-generic` (Go runtime) | uses `fluxbee-go-sdk/rpc.go` | rename usage mechanically |
| `sy-wf-rules` (`go/sy-wf-rules/`) | inline `messageMux` (`node/mux.go`) | adopt `*RouterDispatcher`, **delete** `messageMux` |
| `sy-opa-rules` (`go/sy-opa-rules/`) | inline `RouterClient` + `forwardOutgoing` (`main.go:1264`, `1307`) | adopt `*RouterDispatcher`, **delete** `RouterClient` + `forwardOutgoing` |

The Go SDK bring-up (Section 5) is a precondition for `sy-wf-rules` and `sy-opa-rules` migrations. The Go SDK PR lands first; the two Go-node PRs follow.

### 6.6 Diagnostic binaries

The repo has 9 `src/bin/*_diag.rs` binaries that talk to the router with `RpcClient::connect_with_retry` and `NodeUuidMode::Persistent`. They are **not** anti-pattern — they already use the canonical dispatcher. They are included in scope of this work for a single reason: when the SDK rename PR (Section 9 step 1) renames `RpcClient` → `RouterDispatcher`, these binaries must compile against the new name in the same PR. Leaving them out would mean the rename PR ships with broken `cargo build --bins`, which contradicts the no-legacy stance.

In-scope diagnostic binaries (mechanical rename only):

- `src/bin/identity_merge_diag.rs`
- `src/bin/identity_negative_diag.rs`
- `src/bin/identity_replica_sync_diag.rs`
- `src/bin/identity_provision_complete_diag.rs`
- `src/bin/admin_internal_command_diag.rs`
- `src/bin/io_test_diag.rs`
- `src/bin/inventory_hold_diag.rs`
- `src/bin/blob_sync_diag.rs`
- `src/bin/orch_system_diag.rs`

Per-binary scope: replace `RpcClient` → `RouterDispatcher` everywhere it appears, including `Arc<RpcClient>` field types, `RpcClient::connect_with_retry` call sites, and `use fluxbee_sdk::RpcClient` imports. No behavioral change. No `connect_with_retry` signature changes. This is mechanical and lands as part of step 1 of Section 9 (the SDK rename PR), not as a separate PR.

If a future audit shows a `_diag` binary using the inline patterns this plan eliminates (it does not today), that one becomes its own line item.

### 6.7 Test nodes — explicitly out of scope

`nodes/test/ai-test-cognition`, `nodes/test/ai-test-gov`, `nodes/test/io-test-cognition`, and `nodes/test/io-test` use the `connect()` + `loop { recv() }` pattern and run only on-demand against ephemeral environments. They are deliberately **out of scope** for this work:

- They are test harnesses, not production nodes — no inventory presence, no fleet operations care about them.
- Their multiplexing behavior does not matter: ephemeral runs do not stress concurrent in-flight RPCs.
- Migrating them would pull `RouterDispatcher` plumbing into test code that does not benefit from it.

When the SDK rename PR lands, these binaries either compile because they use the SDK's lower-level `connect()`/`NodeConfig` API (which is renamed, not removed) or they are updated mechanically as a trivial follow-up. They do **not** gate any production migration in this plan, and a `_test_*` binary surfacing a new inline dispatcher is treated as a test-only concern.

## 7. `VaultClient` and the 4 remaining Pattern 3 sites

`VaultClient` is introduced by whichever PR lands first — the architect-specific PR or any of the 6.x PRs. After it exists, the global plan migrates these 4 sites:

| Site | File | Migrated in |
| --- | --- | --- |
| `sy_cognition` | `src/bin/sy_cognition.rs:1594` | 6.1 sy_cognition PR |
| `io-slack` | `nodes/io/io-slack/src/main.rs:739` | 6.2 io-slack PR |
| `ai-generic` | `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1682` | 6.2 ai-generic PR |
| `ai-frontdesk-gov` | `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:1708` | 6.2 ai-frontdesk-gov PR |

The PR that migrates the **last** site (whichever it is in scheduling order) also **deletes** `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` from the SDK. No `#[deprecated]` attribute is added at any point.

## 8. CI guards (added incrementally as each migration lands)

Place under `scripts/router_dispatcher_guards/`:

1. `no_inline_dispatcher.sh` — fails on `struct .*RouterInbox\b`, `struct SharedRouterConnection\b`, `struct RouterClient\b` (in `crates/fluxbee_ai_sdk/`), `type messageMux\b`, `func .*forwardOutgoing\b` outside their original definition site (which will be the SDK, not bespoke).
2. `no_direct_connect.sh` — fails on `fluxbee_sdk::connect(` or unqualified `connect(&NodeConfig` in `nodes/**` and `src/bin/**` Rust code.
3. `no_legacy_vault_helper.sh` — fails on `resolve_resource(&NodeSender` or `resolve_resource(&sender, &mut receiver` anywhere in the repo after the final site migrates.
4. `no_deprecated_attribute_on_dispatcher.sh` — fails if `#[deprecated]` ever appears on `RouterDispatcher`, `RpcClient`, `connect_with_retry`, `VaultClient`, or `resolve_resource`. The forbidden state is "marked deprecated but kept" — we never deprecate, we delete.

Each guard's first turn-on is in the PR that finishes its scope. Earlier PRs may have failing-locally guards until that final PR — that is fine, the guards only run in CI once they are committed.

## 9. Implementation order

The order respects three constraints: (a) the SDK abstraction must exist before consumers migrate, (b) we never run with two abstractions coexisting longer than one PR boundary, and (c) we don't ship a PR that touches more than one "type of node" at a time (so review remains tractable).

1. **SDK Rust rename PR** — `RpcClient` → `RouterDispatcher`, `connect` → `pub(crate)`, `RouterDispatcherTestHarness`. No node-level changes in this PR. All existing call sites in `src/bin/sy_orchestrator.rs`, `src/bin/sy_admin.rs`, and the 9 diagnostic binaries listed in Section 6.6 are updated mechanically (they already use the canonical type).
2. **SDK Rust `VaultClient` PR** — new `crates/fluxbee_sdk/src/vault.rs`. No call site migrations yet. The architect-specific PR may instead land first and introduce `VaultClient`; whichever PR is first does this, the other one uses what already exists.
3. **`sy_identity`, `sy_vault`, `sy_storage`, `sy_policy`, `sy_config_routes` migration PRs** — one per node, mechanical. Order is free.
4. **`sy_cognition` migration PR** — same shape, plus Vault site migration.
5. **io-* bundle PR** — `io-api`, `io-slack`, `io-sim`, `io-linkedhelper` migrate together; `RouterInbox` deleted from `nodes/io/common`; io-slack Vault site migrated.
6. **ai-generic + ai-frontdesk-gov + fluxbee_ai_sdk bundle PR** — both `SharedRouterConnection` definitions deleted; `fluxbee_ai_sdk::RouterClient` / `RouterReader` / `RouterWriter` / `AiNodeConfig` deleted; `NodeRuntime` rewritten to take `Arc<RouterDispatcher>`; both Vault sites migrated. If this is the PR that closes the last Vault site, it also **deletes** the free `resolve_resource` function.
7. **Go SDK bring-up PR** — `RouterDispatcher` in `go/fluxbee-go-sdk/`, `sy-timer` and `wf-generic` rename usages.
8. **`sy-wf-rules` migration PR** — delete `messageMux`.
9. **`sy-opa-rules` migration PR** — delete `RouterClient` + `forwardOutgoing`.

After step 9: no inline dispatcher remains anywhere, no second router connection is opened by any node for any reason, and `RouterDispatcher` is the single router-side abstraction in the codebase.

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
