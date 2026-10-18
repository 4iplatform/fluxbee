# SY.orchestrator internal RPC multiplexing tasks

Date: 2026-05-23 (created)
Updated: 2026-05-24 (Iter 1 Blocks A–G + H1–H3 done; H4/H5 statically validated; Iter 2 AF-P1..AF-P3 done; Iter 3 review follow-up done)
Status: Iteration 1 — Blocks A, B, C, D, E, F, G **done**; Block H: H1/H2/H3 covered by in-process `RpcTestHarness` tests; H4/H5 scripts statically verified, formal `[x]` waits on staging/CI run. Iteration 2 (audit follow-up) — AF-P1 (auth bypass fix), AF-P2a (fail-fast on reconnect), AF-P2b (observational filter), AF-P3 (doc inventory) all **done**. Iteration 3 (code-review hardening) — AF-P4 (write-side drain), AF-P5 (`wait_connected` enable), AF-P6 (Exact under observational family), AF-P7 (response-only after send) **done**; AF-P8 (tx_loop error classification hardening) **deferred** as low-priority. `cargo fmt --check`: 0 diffs across workspace.

## Goal

Move synchronous system calls back inside `SY.orchestrator` instead of exporting that problem to the rest of the hive through ephemeral relay nodes.

`SY.orchestrator` should keep a single canonical router identity, `SY.orchestrator@<hive>`, and should be able to send several internal requests concurrently by multiplexing responses by `trace_id`, in the same spirit as `SY.admin`.

Implementation stance: do not keep relay fallback paths and do not add destination-service compatibility aliases. Relay names are historical evidence only; the live contract is the canonical orchestrator identity.

## Problem statement

Today `SY.orchestrator` opens short-lived router connections for point RPCs. Those connections appear to the rest of the system as independent L2 nodes:

- `SY.orchestrator.relay.<ts>@<hive>`
- `SY.orchestrator.ilk-delete.<ts>@<hive>`
- `SY.orchestrator.vault-purge.<ts>@<hive>`

That leaks an orchestrator implementation detail into authorization rules owned by other services.

Observed effects:

- `SY.identity` contained a compatibility workaround that treated `SY.orchestrator.relay.*` as a variant of `SY.orchestrator`.
- `SY.timer` correctly rejects `TIMER_PURGE_OWNER` from `SY.orchestrator.relay.*`, because the destructive operation is reserved to the local canonical orchestrator.
- The node teardown E2E fails in the timer purge step even though the timer rule is conceptually correct.

The root problem is not `SY.timer` and not the E2E. The root problem is that `SY.orchestrator` cannot currently perform concurrent request/response work over its own router connection, so it creates extra nodes to wait for responses.

## Historical relay inventory

Known relay entry points that must not remain live in `src/bin/sy_orchestrator.rs`:

- `relay_system_action`
  - Creates `SY.orchestrator.relay.<ts>`.
  - Used by system forwarding, node status requests, and timer owner purge.
- `relay_identity_system_call_ok`
  - Creates `SY.orchestrator.relay.<ts>`.
  - Used by node ILK registration and node ILK update.
- `delete_ilk_for_teardown`
  - Creates `SY.orchestrator.ilk-delete.<ts>`.
  - Calls `SY.admin` to delete ILK state during teardown.
- `purge_vault_secrets_for_ilk`
  - Creates `SY.orchestrator.vault-purge.<ts>`.
  - Calls `SY.admin` vault operations during teardown.
- `relay_system_action_for_timer_purge`
  - Uses `relay_system_action` to call `SY.timer` with `TIMER_PURGE_OWNER`.

Reference pattern already present in `src/bin/sy_admin.rs`:

- `AdminRouterClient`
- `pending_admin`
- `sender_snapshot`
- receive loop dispatching responses to pending callers

## Implementation review 2026-05-23

The v1 implementation introduced `OrchestratorRouterClient` and migrated every call site to the canonical sender. The relay helpers are gone. However review surfaced four issues, one of them blocking:

1. **Structural deadlock (BLOCKING).** The main receive loop in `src/bin/sy_orchestrator.rs` keeps awaiting `handle_admin(...)` / `handle_system_message(...)` inline. Those handlers internally fire RPCs (`run_node_flow` -> `ensure_node_identity_registered` -> `orchestrator_identity_system_call_ok` -> `rx.recv().await`; `kill_node_flow` -> `purge_owner_timers_before_teardown` -> `orchestrator_system_action`; etc.). Because the same loop is the only consumer that calls `router_client.dispatch_pending`, the response is never delivered while the handler is awaiting it. Every `run_node`, `kill_node`, `get_node_status`, teardown, and forward path now deadlocks until the RPC timeout fires. By contrast `sy_admin` runs its `recv_loop` in a dedicated `tokio::spawn` and separates router receive ownership from operational handlers. Orchestrator must adopt the same separation. This matches the project rule `feedback_no_global_lock_across_rpc_await`.

2. **Duplication of SDK logic.** `OrchestratorRouterClient`, `OrchestratorRpcError`, `parse_orchestrator_admin_response`, `orchestrator_admin_response_payload_value`, `identity_payload_status_and_code`, `identity_payload_message`, `map_orchestrator_rpc_error_to_identity` mirror logic already present in `crates/fluxbee_sdk/src/admin.rs` and `identity.rs`. The duplication exists because the existing SDK helpers (`admin_command`, `identity_system_call_ok`, …) demand exclusive `&mut NodeReceiver`, which is incompatible with multiplexing. The right cure is to extract the multiplexing client into the SDK and delete the single-shot helpers.

3. **Meta divergence.** `send_system_rpc` sets `meta.target = Some(target)`. For `Unicast` the router already routes by `routing.dst`, so this target is redundant and creates SDK drift. Keep `meta.target = None`. Do **not** drop `meta.action_class`: for `system` messages the router can only enrich from `msg_type`, not from the system verb, so `send_system_rpc` must set `meta.action_class = classify_system_message(request_msg)`.

4. **Missing fields in result type.** `OrchestratorAdminCommandResult` only carries `status / payload / error_code / error_detail`. It drops `action / request_id / trace_id` that `AdminCommandResult` exposes. Teardown logs lose trace identity, which hurts diagnosis. The SDK type does it right.

Plus housekeeping originally listed in ORPC-12/13/14: dispatch-by-trace_id and "unrelated message not swallowed" unit coverage are now covered by Block A5/H2; runtime confirmation of teardown E2E remains Block H4.

## Decisions taken 2026-05-23

These are settled (see chat transcript on the same date):

- **Receive loop**: full parity with the safe part of `sy_admin`. The router recv loop runs in a dedicated `tokio::spawn` inside `RpcClient`; it never awaits operational handlers. Command-bearing operational handlers run in long-lived worker tasks fed by non-dropping queues.
- **Delivery semantics**: operational delivery is profile-driven. Each binary builds an `OperationalRouteProfile` in `main`, declaring named command channels (`mpsc::UnboundedSender / UnboundedReceiver`) and named observational broadcast channels (`broadcast::Sender`). Routes split into two ordered tables: `pre_pending_rules` evaluate **before** the pending matcher (for operational commands that are never RPC responses: `ADMIN_COMMAND`, `NODE_STATUS_GET`, `CONFIG_GET/SET`, `VAULT_SECRET_CHANGED`, etc.), and `post_pending_rules` evaluate **after** stale/response-only guards (for observational fan-out: `CONFIG_RESPONSE`, `query_response`, etc.). Both tables match `(meta.msg_type, meta.msg)`, first-match-wins. The SDK ships the routing engine but no predefined `sy_admin()` / `sy_orchestrator()` profiles. The pre_pending split is what prevents a legitimate operational `ADMIN_COMMAND` (or any other operational command) with a colliding `trace_id` from being misclassified by a pending matcher's `invalid_response` rule.
- **Receiver ownership**: the SDK `RpcClient` owns both `NodeSender` and `NodeReceiver`. There is no second consumer; the abstraction takes the connection whole. Consumers only get `sender_snapshot()`, **exclusive** `mpsc::UnboundedReceiver` handles for profile-declared command channels (one consumer per channel, double-take returns an explicit `RpcError::ReceiverAlreadyTaken`), and `subscribe(name)` only for profile-declared observational streams.
- **Legacy SDK helpers** (`admin_command`, `admin_command_ok`, `identity_system_call`, `identity_system_call_ok`, `set_ich_enabled`, and the underlying `wait_admin_response` / `send_action_once` / `wait_system_response` / `response_action_for` / `payload_status_and_code` / `payload_message`): deleted. All call sites migrate to `RpcClient`. No compat shims, per `feedback_no_legacy_in_dev`.
- **Audit scope**: orchestrator + SDK + full-repo audit. The grep sweep covers every `src/bin/*.rs`, every node under `nodes/**`, and the Go sources for the same structural pattern (handler awaiting an RPC inline while owning the recv loop, or creating ephemeral router nodes to dodge that). Each hit is reported with file:line and classified bug / dead code / OK-by-design; only orchestrator/sy_admin/SDK get fixed in this PR.

## Target design v2

Replace `OrchestratorRouterClient` with `fluxbee_sdk::rpc::RpcClient`.

`RpcClient` responsibilities:

- Owns `NodeSender` + `NodeReceiver`. Constructor `connect_with_retry(NodeConfig, Duration, OperationalRouteProfile) -> Arc<Self>` spawns the recv loop internally.
- Owns an `OperationalRouteProfile`:
  - `command_channels: Vec<&'static str>` declare named single-consumer `mpsc` channels;
  - `broadcast_channels: Vec<&'static str>` declare named observational `broadcast` streams;
  - `pre_pending_rules: Vec<(RouteMatch, RouteTarget)>` evaluate before the pending matcher (for operational commands that are never RPC responses);
  - `post_pending_rules: Vec<(RouteMatch, RouteTarget)>` evaluate after stale/response-only guards (for observational fan-out and broad operational catch-alls).
- `RouteMatch` supports `Exact { msg_type, msg }`, `OneOf { msg_type, msgs }`, `AnyMsgOfType(msg_type)`, and `Any`.
- `RouteTarget` supports `Command(name)`, `Broadcast(name)`, and `Drop { reason }`.
- `OperationalRouteProfile::builder().build()` validates duplicate channel names, the same name appearing as both command and broadcast, rule targets that reference missing channels, empty channel names, and broad rules (`Any`, broad `AnyMsgOfType`) that make later rules in the **same table** unreachable. Pre/post are validated independently.
- Pending table keyed by `trace_id`. Each entry carries a **declarative `PendingMatcher`** supplied by the caller, reusing `RouteMatch`:
  - `success: Vec<RouteMatch>` — matches that complete the waiter with the response payload;
  - `terminal_error: Vec<RouteMatch>` — matches that complete the waiter with a transport-level error (`Unreachable` / `TtlExceeded` parsed from payload);
  - `invalid_response: Vec<RouteMatch>` — matches that complete the waiter with `InvalidResponse` (malformed correlated response — only the caller could have collided this `trace_id`).
  Anything not matching any of the three vectors is treated as unrelated operational traffic and falls through to stale/response-only/post_pending guards while the waiter stays pending. The dispatcher uses `trace_id` only as an index.
- Response-only registry stores **only `Exact`/`OneOf`** success `RouteMatch` shapes from each `send_with_matcher` call, unless that exact shape is declared observational in `post_pending_rules` toward a `Broadcast` target. `send_admin_rpc` registers `Exact(ADMIN_KIND, MSG_ADMIN_COMMAND_RESPONSE)`. `send_system_rpc` registers `Exact(SYSTEM_KIND, response_msg)` except for binaries like `sy_admin` that explicitly broadcast that exact shape (`CONFIG_RESPONSE`). The registry is process-local and monotonic for the life of the client; it prevents broad operational catch-alls such as `Any` or `AnyMsgOfType(SYSTEM_KIND)` from receiving late or orphaned **exact-shaped** RPC responses after the stale trace TTL expires. `Any` and `AnyMsgOfType` success matchers are deliberately NEVER registered: `Any` would turn the registry into a global drop rule, and a family-wide entry permanently poisons any node whose OWN inbound commands live in that family (io.cloud's `any_msg_type(user)` frontdesk matcher vs its `user`-family edge requests — the 2026-08-26 prod outage; io.api and `sy_admin::send_admin_request` carried the identical latent bomb). Late family-wide replies are covered by the trace-keyed pending map and the recent-stale table (30s TTL) plus each node's own inbound gates.
- Recent-stale table keyed by `trace_id`, bounded by size and TTL. When a waiter completes, times out, or is drained, store its matcher briefly. A late correlated response with no active waiter is logged/metriced as stale and discarded; it must not be routed into an operational command worker.
- Dispatch on every incoming message, in this order:
  1. **pre_pending rules**: evaluate `pre_pending_rules` first-match-wins. If matched, route to the named command channel / broadcast / drop and return. This is what prevents an operational `ADMIN_COMMAND` (or `NODE_STATUS_GET`, `CONFIG_*`, etc.) with a colliding `trace_id` from being misclassified as a response.
  2. **pending matcher**: if an active waiter exists for `trace_id`, classify against its `PendingMatcher`:
     - any `RouteMatch` in `success` hits: deliver as `Ok(message)` and remove the waiter;
     - any `RouteMatch` in `terminal_error` hits: parse transport-error payload and deliver as `Err(Unreachable | TtlExceeded)`; remove the waiter;
     - any `RouteMatch` in `invalid_response` hits: deliver as `Err(InvalidResponse)`; remove the waiter;
     - none of the three vectors hits: treat as unrelated operational traffic with colliding `trace_id`, **keep the waiter pending**, fall through to the remaining guards.
  3. **stale**: if `trace_id` is in the recent-stale table and the message matches the stored matcher as success/terminal/invalid: increment `rpc_stale_response_total`, log once per trace, and discard.
  4. **response-only registry**: if the message matches a registered non-`Any` success `RouteMatch` that was not explicitly declared as a post-pending observational shape: increment `rpc_unknown_response_total`, debug log, and drop.
  5. **post_pending rules**: evaluate `post_pending_rules` first-match-wins. Route accordingly.
  6. **unmatched**: increment `rpc_route_unmatched_total`, debug log, and drop.
- Profile-declared command channels are single-consumer. The receiver is moved out via `take_command_receiver(name) -> Result<RpcCommandReceiver, RpcError>`; double-take returns `RpcError::ReceiverAlreadyTaken`. `RpcCommandReceiver` wraps `mpsc::UnboundedReceiver<Message>` so depth gauges can be decremented on `recv()`.
- Profile-declared observational streams use `subscribe(name) -> Result<broadcast::Receiver<Message>, RpcError>`. `broadcast` is allowed only for streams the binary intentionally declares as observational and lag/drop-tolerant.
- Command channel enqueue from the router recv loop is non-awaiting. A depth gauge per command channel is exposed in metrics; depth crossing a soft threshold (initial 1000) emits a single WARN log per crossing. These command channels never use `broadcast` — control commands cannot be silently dropped because a worker lagged.
- `sender_snapshot() -> NodeSender` for outgoing messages outside the RPC path.
- `send_with_matcher(outgoing, matcher, labels, timeout) -> Result<Message, RpcError>`: low-level primitive. Generates a `trace_id` (or reuses one already in `outgoing.routing.trace_id` if set), registers the pending entry with the supplied `PendingMatcher`, copies the matcher's non-`Any` and non-observational `success` `RouteMatch`es into the response-only registry (so even after stale TTL an orphaned shape is dropped, not routed), sends the message, awaits the response. `labels` carries `request` and `response` strings used in `RpcError::Timeout` for diagnostics.
- `send_system_rpc(target, request_msg, response_msg, payload, timeout)`: thin wrapper over `send_with_matcher`. Outgoing meta: `meta.target = None`, `meta.action_class = classify_system_message(request_msg)`. Matcher: `success = [Exact{SYSTEM_KIND, response_msg}]`, `terminal_error = [Exact{SYSTEM_KIND, MSG_UNREACHABLE}, Exact{SYSTEM_KIND, MSG_TTL_EXCEEDED}]`, `invalid_response = [AnyMsgOfType(SYSTEM_KIND)]`.
- `send_admin_rpc(AdminCommandRequest) -> AdminCommandResult`: thin wrapper over `send_with_matcher`. Outgoing meta as today's SDK `admin_command`. Matcher: `success = [Exact{ADMIN_KIND, MSG_ADMIN_COMMAND_RESPONSE}]`, `terminal_error = [Exact{SYSTEM_KIND, MSG_UNREACHABLE}, Exact{SYSTEM_KIND, MSG_TTL_EXCEEDED}]`, `invalid_response = [AnyMsgOfType(ADMIN_KIND)]`. Note `SYSTEM_KIND` is **not** in `invalid_response` — unrelated `SYSTEM_KIND` traffic with a colliding trace stays operational.
- `drain_pending_waiters()` invoked on reconnection, surfacing `Disconnected` to in-flight RPCs.
- Test transport surface: `RpcClient::from_test_channels(sender, receiver, profile) -> Arc<Self>` plus a `RpcTestHarness` for constructing in-process `NodeSender / NodeReceiver` fixtures. This API is available to downstream crate tests via a `test-utils` feature (or equivalent public harness module), not hidden behind plain `#[cfg(test)]` inside `fluxbee_sdk`. **The only injectable path used by tests; no trait abstraction is added.**

`SY.orchestrator` becomes a consumer of `RpcClient`:

- `main` builds this `OperationalRouteProfile`, then `Arc<RpcClient>`, and stores the client in `OrchestratorState`:
  - command channels: `admin`, `system`;
  - pre_pending rules (operational commands inbound that must never be misclassified by a colliding pending matcher):
    - `Exact { msg_type: ADMIN_KIND, msg: MSG_ADMIN_COMMAND } -> Command("admin")`;
  - post_pending rules (broad operational catch-alls, safe because stale + response-only filter responses first):
    - `AnyMsgOfType(ADMIN_KIND) -> Command("admin")`;
    - `AnyMsgOfType(SYSTEM_KIND) -> Command("system")`.
- The post_pending rules are intentionally broad because response-only shapes are filtered earlier. A late `ADMIN_COMMAND_RESPONSE`, `TIMER_RESPONSE`, or other response registered by `send_*_rpc` does not reach these broad workers.
- Instead of one receive loop, orchestrator starts long-lived category workers from `take_command_receiver("admin")?` and `take_command_receiver("system")?`. A worker calls `handle_admin(...)` / `handle_system_message(...)` directly and serializes that category by default. Bounded per-message concurrency is allowed only when a handler explicitly proves that lifecycle ordering is irrelevant.
- The recv loop inside `RpcClient` keeps running independently, so any RPC fired from inside a handler completes naturally. Handler serialization is not a workaround; it preserves operational ordering while fixing the receive-loop ownership bug.
- All `forward_system_action_to_hive`, `orchestrator_system_action`, `orchestrator_identity_system_call_ok`, `orchestrator_admin_command`, `orchestrator_system_action_for_timer_purge` become thin delegations to `RpcClient`.

`SY.admin` migrates from `AdminRouterClient` (private) to `Arc<RpcClient>` (shared SDK). The private struct and its private delivery plumbing are deleted.

`SY.admin` builds this `OperationalRouteProfile` in `main`:

- command channels:
  - `status_get` — `SYSTEM_KIND / NODE_STATUS_GET`, handled by a dedicated worker that calls `try_handle_default_node_status(&sender, &msg).await`;
  - `system_command` — `SYSTEM_KIND / {CONFIG_GET, CONFIG_SET, MSG_VAULT_SECRET_CHANGED}`;
  - `internal_admin` — `ADMIN_KIND / MSG_ADMIN_COMMAND`.
- broadcast channels:
  - `config_response` — `SYSTEM_KIND / CONFIG_RESPONSE`;
  - `query` — any `query_response`.
- pre_pending rules (these are operational commands, never RPC responses; must run before pending matchers):
  - `Exact { msg_type: SYSTEM_KIND, msg: MSG_NODE_STATUS_GET } -> Command("status_get")`;
  - `OneOf { msg_type: SYSTEM_KIND, msgs: &[MSG_CONFIG_GET, MSG_CONFIG_SET, MSG_VAULT_SECRET_CHANGED] } -> Command("system_command")`;
  - `Exact { msg_type: ADMIN_KIND, msg: MSG_ADMIN_COMMAND } -> Command("internal_admin")`.
- post_pending rules (observational fan-out, OK to run after stale/response-only filtering):
  - `Exact { msg_type: SYSTEM_KIND, msg: MSG_CONFIG_RESPONSE } -> Broadcast("config_response")`;
  - `AnyMsgOfType("query_response") -> Broadcast("query")`.

`SY.admin` does not use generic `admin` or `system` command buckets. Its operational categories are derived from `(msg_type, msg)`, matching the current `AdminRouterClient::dispatch` behavior.

### Design clarifications 2026-05-23

- **Pending matcher.** Each pending entry stores a declarative `PendingMatcher` set by the caller. Both `PendingMatcher` and `OperationalRouteProfile` rules reuse the same `RouteMatch` vocabulary (`Exact { msg_type, msg }`, `OneOf { msg_type, msgs }`, `AnyMsgOfType(msg_type)`, `Any`). The matcher has three `Vec<RouteMatch>` fields: `success`, `terminal_error`, `invalid_response`. The dispatcher uses `trace_id` only as an index; the matcher decides completion: any `success` match -> success; any `terminal_error` match -> transport-error; any `invalid_response` match -> `InvalidResponse`; none of the three -> unrelated operational traffic, **leave the waiter pending** and fall through to stale/response-only/post_pending guards. This prevents an admin RPC (`invalid_response = [AnyMsgOfType(ADMIN_KIND)]`) from treating every `SYSTEM_KIND` colliding message as malformed just because admin transport errors are encoded as exact `(SYSTEM_KIND, MSG_UNREACHABLE/TTL_EXCEEDED)` matches.
- **Stale and response-only responses.** When a waiter is removed by success, timeout, or drain, `RpcClient` keeps a short-lived stale trace entry with the same matcher. A late response for that trace increments `rpc_stale_response_total`, logs once per trace, and is discarded. Separately, every `send_with_matcher` registers its non-`Any` success response shape in the response-only registry unless the profile explicitly declares that shape as observational in `post_pending_rules`. That distinction prevents `sy_admin` from black-holing legitimate `CONFIG_RESPONSE` / `query_response` fan-out while still preventing broad catch-all workers from receiving orphaned RPC responses. Responses must not fall through to `admin`, `system`, or any profile-declared command worker as if they were new commands.
- **Operational route profile.** Settled: `RpcClient` does **not** expose hard-coded `subscribe_admin / subscribe_system / subscribe_system_command / subscribe_internal_admin` or fixed command buckets. Each binary declares its own `OperationalRouteProfile` with named command channels, named observational broadcast channels, and ordered rules over `(msg_type, msg)`. `system_command` and `internal_admin` are not protocol `msg_type` values; they are profile-derived operational channels.
- **Broadcast for operational commands.** Settled: command targets use exclusive `RpcCommandReceiver` values taken once via `take_command_receiver(name)?`. The router recv loop enqueues with non-awaiting `send()` and exposes per-channel depth in metrics; depth crossing 1000 emits a single WARN log per crossing. Backpressure is not modeled because the producer is the router. `broadcast::Sender` is only used for profile-declared observational streams.
- **H3 test shape.** Settled: `RpcClient::from_test_channels(NodeSender, NodeReceiver, profile)` plus `RpcTestHarness`, exposed to downstream crate tests via `test-utils` (or equivalent public harness module). Production uses `Arc<RpcClient>` built with `connect_with_retry`; tests build the same `Arc<RpcClient>` with in-process channel fixtures and the same profile shape so the real recv loop / dispatcher / `PendingMatcher` path is exercised. No trait abstraction is introduced; `take_test_timer_purge_result` stays only for non-transport unit tests (payload shaping, error aggregation in `errors[]`). The H3 test reads the outgoing `Message` written by the client and asserts: `routing.src` equals the client's canonical sender uuid; `routing.src_l2_name = None` (router stamps it); `routing.dst = Destination::Unicast(timer_node)`; `meta.msg_type = SYSTEM_KIND`; `meta.msg = Some("TIMER_PURGE_OWNER")`; `meta.target = None`; `meta.action_class = classify_system_message("TIMER_PURGE_OWNER")`. While the waiter is pending, the test injects (a) `(SYSTEM_KIND, "SYSTEM_UPDATE")` with a random trace, and (b) an `(ADMIN_KIND, MSG_ADMIN_COMMAND)` with the **colliding** trace; asserts (a) reaches `take_command_receiver("system")`, (b) reaches `take_command_receiver("admin")`, and the waiter stays pending. Then injects `(SYSTEM_KIND, "TIMER_RESPONSE")` with the matching trace and a canned payload, and asserts the waiter completes with that payload.

## Task list v2

ORPC-1..ORPC-14 (the v1 list, see "Task list v1 — historical" below) are subsumed by the blocks below. The previously checked items either remain valid in their narrow scope (relay deletion, identity cleanup, timer audit) or are re-done as part of the v2 refactor (`OrchestratorRouterClient` is replaced by `RpcClient`).

### Block A — `RpcClient` in SDK (`crates/fluxbee_sdk`)

- [x] **A1.** Create `fluxbee_sdk::rpc` module. Implement `OperationalRouteProfile` (with `pre_pending_rules` + `post_pending_rules`), `OperationalRouteProfileBuilder`, `RouteMatch`, `RouteTarget`, `RpcCommandReceiver`, and profile validation (each rule table validated independently for unreachable broad rules). `RpcClient::connect_with_retry(NodeConfig, Duration, OperationalRouteProfile) -> Arc<Self>` connects and spawns the recv loop internally. Public surface: `sender_snapshot()`, `drain_pending_waiters()`, exclusive `take_command_receiver(name) -> Result<RpcCommandReceiver, RpcError>` (double-take returns `RpcError::ReceiverAlreadyTaken`), `subscribe(name) -> Result<broadcast::Receiver<Message>, RpcError>`, and `RpcClient::from_test_channels(sender, receiver, profile) -> Arc<Self>` plus `RpcTestHarness` under `test-utils` / public harness support for downstream crate tests.
- [x] **A2.** Implement `send_with_matcher(outgoing, matcher, labels, timeout) -> Result<Message, RpcError>` as the low-level primitive: generates `trace_id` if absent, copies matcher non-`Any` / non-observational `success` `RouteMatch`es into the response-only registry, registers the pending entry, sends, awaits, handles timeout/stale-snapshot. Then implement `send_system_rpc(target, request_msg, response_msg, payload, timeout) -> Result<Message, RpcError>` as a wrapper: outgoing `Meta { msg_type: SYSTEM_KIND, msg: request_msg, target: None, action: None, action_class: classify_system_message(request_msg) }`; matcher `success = [Exact{SYSTEM_KIND, response_msg}]`, `terminal_error = [Exact{SYSTEM_KIND, MSG_UNREACHABLE}, Exact{SYSTEM_KIND, MSG_TTL_EXCEEDED}]`, `invalid_response = [AnyMsgOfType(SYSTEM_KIND)]`.
- [x] **A3.** Implement `send_admin_rpc(AdminCommandRequest) -> Result<AdminCommandResult, RpcError>` as a wrapper over `send_with_matcher`. Outgoing meta as today's SDK `admin_command`. Matcher: `success = [Exact{ADMIN_KIND, MSG_ADMIN_COMMAND_RESPONSE}]`, `terminal_error = [Exact{SYSTEM_KIND, MSG_UNREACHABLE}, Exact{SYSTEM_KIND, MSG_TTL_EXCEEDED}]`, `invalid_response = [AnyMsgOfType(ADMIN_KIND)]`. `SYSTEM_KIND` is **not** in `invalid_response`; unrelated `SYSTEM_KIND` traffic with a colliding trace stays operational. Reuse `AdminCommandRequest` and `AdminCommandResult` (moved from `admin.rs` into `rpc.rs`); reuse `parse_admin_response` / `admin_response_payload_value` (moved).
- [x] **A4.** Unified `RpcError` enum: `Node(NodeError)`, `Unreachable { reason, original_dst }`, `TtlExceeded { original_dst, last_hop }`, `Timeout { trace_id, target, request_msg, response_msg, timeout }`, `InvalidRequest(String)`, `InvalidResponse(String)`, `ResponseChannelClosed { trace_id }`, `Disconnected`, `InvalidRouteProfile(String)`, `UnknownRouteChannel { name }`, `ReceiverAlreadyTaken { category }`, `Rejected { action, error_code, message }` (subsumes today's `IdentityError::SystemRejected` and `AdminCommandError::Rejected`).
- [x] **A5.** Unit tests (in `crates/fluxbee_sdk/src/rpc.rs#tests`), all built on top of `RpcClient::from_test_channels(sender, receiver, profile)`:
  - route profile builder rejects duplicate channel names, unknown rule targets, command/broadcast name collisions, empty names, and broad unreachable rules (independently for pre_pending and post_pending);
  - pre_pending rules win against a colliding pending matcher (e.g. inbound `(ADMIN_KIND, MSG_ADMIN_COMMAND)` with a colliding trace routes to the `internal_admin` worker, the admin RPC waiter stays pending);
  - operational dispatch uses ordered first-match-wins rules over `(msg_type, msg)`, including `OneOf` and `Exact` before broad `AnyMsgOfType`;
  - profile-declared command receivers and broadcast subscribers are addressed by name; unknown names return explicit `RpcError`;
  - dispatch by `trace_id` with multiple concurrent RPCs;
  - unknown-`trace_id` message flows to the operational `mpsc` receiver and is not swallowed by waiters;
  - colliding `trace_id` with `meta.msg_type` not matched by `invalid_response` keeps the waiter pending and the message flows to the operational channel;
  - colliding `trace_id` where the matcher's `invalid_response` does match completes the waiter with `InvalidResponse`;
  - admin waiter receives a `SYSTEM_KIND` transport error (`MSG_UNREACHABLE`) on its `trace_id` and is completed with `Unreachable` (admin `success` is `ADMIN_KIND`, `invalid_response` is `AnyMsgOfType(ADMIN_KIND)`, and transport errors are exact `SYSTEM_KIND` matches);
  - admin waiter receives a non-terminal `SYSTEM_KIND` message with a colliding `trace_id`; assert it routes operationally and does not complete the admin waiter;
  - late `TIMER_RESPONSE` / `ADMIN_COMMAND_RESPONSE` after waiter timeout is counted as stale and discarded, not delivered to an operational receiver;
  - registered response-only shape after stale TTL is counted as unknown response and dropped before broad profile rules;
  - timeout cleans the waiter (no leak) and a subsequent RPC works;
  - `drain_pending_waiters` (reconnection) surfaces `Disconnected` to in-flight RPCs;
  - `Unreachable` / `TtlExceeded` mapped correctly from payload;
  - admin RPC response parses `status / action / request_id / trace_id`;
  - command-channel depth gauge increments per enqueue and the WARN-on-crossing fires **once per crossing** of the 1000 threshold (not on every message above it);
  - `take_command_receiver(name)` returns `RpcError::ReceiverAlreadyTaken` on double-take.

### Block B — Delete SDK legacy helpers

- [x] **B1.** Remove from `crates/fluxbee_sdk/src/admin.rs`: `admin_command`, `admin_command_ok`, `wait_admin_response`, `default_timeout`. Move `parse_admin_response`, `admin_response_payload_value`, `extract_error_message` to `rpc.rs`.
- [x] **B2.** Remove from `crates/fluxbee_sdk/src/identity.rs`: `identity_system_call`, `identity_system_call_ok`, `send_action_once`, `wait_system_response`, `response_action_for`, `payload_status_and_code`, `payload_message`, `set_ich_enabled` and every wrapper built on top of `identity_system_call_ok`. Keep only pure types (HiveFile, SHM helpers, errors not subsumed). **DONE** + migrated `nodes/test/io-test` and `src/bin/io_test_diag.rs` which used `provision_ilk`.
- [~] **B3.** Decide `IdentityError`: subsumed `SystemRejected` semantics in `RpcError::Rejected`. **Partial**: `IdentityError` is no longer constructed by the SDK, but the enum variants stay because `sy_orchestrator` (`map_rpc_error_to_identity`) and `nodes/io/*` (`io_common::identity::IdentityError`) still type errors with them. Shrinking the enum further would require migrating those call sites — deferred as follow-up. The spirit ("SDK doesn't build them") is satisfied.
- [x] **B4.** Update `crates/fluxbee_sdk/src/lib.rs` `pub use`: expose `rpc::{RpcClient, RpcError, AdminCommandRequest, AdminCommandResult, SystemRpcRequest, ...}`. Also updated `prelude.rs`. `pub mod admin` removed entirely.

### Block C — `sy_orchestrator` refactor

- [x] **C1.** Replace `OrchestratorRouterClient` with `Arc<RpcClient>` from the SDK. Delete:
  - `struct OrchestratorRouterClient`
  - `enum OrchestratorRpcError`
  - `struct OrchestratorAdminCommandRequest` / `struct OrchestratorAdminCommandResult`
  - `fn default_rpc_timeout`
  - `fn map_orchestrator_rpc_error_to_identity`
  - `fn identity_payload_status_and_code` / `fn identity_payload_message`
  - `fn parse_orchestrator_admin_response` / `fn orchestrator_admin_response_payload_value`
  - `OrchestratorState::router_client: OnceLock<...>` and `fn router_client(&self)`.
- [x] **C2.** In `main`, build the orchestrator `OperationalRouteProfile` (`admin` and `system` command channels; pre_pending rule `Exact{ADMIN_KIND, MSG_ADMIN_COMMAND} -> Command("admin")`; post_pending broad catch-alls `AnyMsgOfType(ADMIN_KIND) -> Command("admin")` and `AnyMsgOfType(SYSTEM_KIND) -> Command("system")`), then build `Arc<RpcClient>` via `RpcClient::connect_with_retry`. Store as `OrchestratorState::rpc: OnceLock<Arc<RpcClient>>`. Helper `build_orchestrator_rpc_profile()` lives in `sy_orchestrator.rs`.
- [x] **C3.** Inline `select! { msg = receiver.recv() }` arm removed. Replaced with `run_admin_worker` + `run_system_worker` spawned tasks fed by `take_command_receiver("admin"|"system")`. Serialized per category. Main loop only handles watchdog + SIGTERM/SIGINT now.
- [x] **C4.** `handle_admin` and `handle_system_message` signatures kept as `&NodeSender + &OrchestratorState` since workers clone the `Arc` once and call by ref per message; equivalent to the doc's intent without unnecessary churn.
- [x] **C5.** Operational ordering audit: serialized category handling is sufficient (workers process one message at a time). `OrchestratorState` mutables verified (`storage_path`, `blob_sync_last_desired`, etc., all `Mutex`-guarded). No global mutable static touched by handlers without locking was found. Block H1 now exercises the critical no-deadlock shape with real `run_admin_worker` / `run_system_worker` tasks and the production `RpcClient` dispatcher path.
- [x] **C6.** `orchestrator_system_action`, `orchestrator_identity_system_call_ok`, `orchestrator_admin_command`, `orchestrator_system_action_for_timer_purge`, `forward_system_action_to_hive(_with_timeout)` rewritten as thin delegations to `state.rpc()?.send_system_rpc(SystemRpcRequest{...})` / `.send_admin_rpc(AdminCommandRequest{...})`. Function names kept.
- [x] **C7.** `#[cfg(test)]` shortcuts kept for unit tests (payload shaping). They do not count toward Block H3 acceptance.
- [x] **C8.** `trace_id` propagated to `delete_ilk_for_teardown` response payload (all three branches: ok / not_found / error). `purge_vault_secrets_for_ilk` per-key error records keep the existing shape; bulk `trace_id` propagation to its `errors[]` array deferred (each per-key call has its own `trace_id` available from `AdminCommandResult` if needed — current call sites don't surface it).

### Block D — `sy_admin` migration

- [x] **D1.** Replace `AdminRouterClient` (private to `src/bin/sy_admin.rs`) with `Arc<RpcClient>` from the SDK. Delete the struct and its private dispatch/delivery internals.
- [x] **D2.** Migrate every call site (~80 references to `&AdminRouterClient` / `Arc<AdminRouterClient>`) to `Arc<RpcClient>`. In `main`, build the `sy_admin` `OperationalRouteProfile` with `status_get`, `system_command`, `internal_admin`, `config_response`, and `query` channels. pre_pending rules for `NODE_STATUS_GET`, `CONFIG_GET / CONFIG_SET / VAULT_SECRET_CHANGED`, and `ADMIN_COMMAND`; post_pending rules for `CONFIG_RESPONSE` and `query_response`. Operational workers get exclusive receivers via `take_command_receiver("status_get" | "system_command" | "internal_admin")?`; observational consumers use `subscribe("config_response")?` and `subscribe("query")?`. The three internal helpers `send_l2_action_request`, `send_admin_request`, and `send_system_request_with_meta` are rewritten over `RpcClient::send_with_matcher` with their own `PendingMatcher` shapes (`AnyMsgOfType("command")/AnyMsgOfType("command_response")`, `AnyMsgOfType(ADMIN_KIND)`, `Exact{SYSTEM_KIND, expected_response}` respectively). Any current `sy_admin` broadcast subscriber that is *not* observational (i.e., it actually consumes commands) is refactored to a single owner during this block — duplication-for-convenience is removed, not preserved.
- [x] **D3.** Move `try_handle_default_node_status` and other special-cases out of `dispatch` (where they were inlined in the private client) into explicit workers/subscribers:
  - `status_get` worker calls `try_handle_default_node_status(&sender, &msg).await`;
  - `system_command` worker calls `handle_system_command`;
  - `internal_admin` worker calls `handle_internal_admin_command`;
  - `config_response` subscriber feeds the current OPA config response collection path;
  - `query` subscriber feeds the current OPA query response collection path.
- [x] **D4.** Re-run existing `sy_admin` tests; they are the strongest battery and act as the validation gate for `RpcClient` before orchestrator changes land.

### Block E — Other SDK legacy call sites

- [x] **E1.** Inventory done. 7 real callers of deleted helpers found and migrated: `sy_architect` (3 sites), `admin_internal_command_diag`, `identity_negative_diag`, `identity_merge_diag`, `identity_replica_sync_diag`, `identity_provision_complete_diag`. The 0 remaining call sites in `sy_cognition`, `sy_storage`, `sy_policy`, `sy_vault`, `sy_config_routes` confirmed by grep (those nodes never used the deleted helpers). `sy_identity` mentions of `set_ich_enabled` are an internal `IdentityStore` method, not the SDK helper. Also migrated `nodes/test/io-test` and `src/bin/io_test_diag.rs` during Block B because they used `provision_ilk`.
- [~] **E2.** Of the 6 diags that build ephemeral `connect()`, 1 was migrated (`io_test_diag` during B2). The remaining 5 (`blob_sync_diag`, `inventory_hold_diag`, `jetstream_envelope_diag`, `orch_system_diag`, `wf_nats_diag`) **don't fit the "1 RPC ephemeral" criterion** (BlobToolkit workflow, consumer loops, NATS, mini-protocol). Explicit decision (2026-05-23): leave them as-is. None blocks Block B. **Tracked under "What did NOT migrate" below.**

### Block F — Full-repo structural audit

- [x] **F1.** Grepped `src/bin/*.rs` and `nodes/**/*.rs`. **0 instances** of the deadlock pattern outside `sy_orchestrator` (already fixed by Block C). **0 instances** of `SY.*.relay.*` / `*.helper.*` / `*.ephemeral.*` node names. 5 instances of the related `connect()+vault-lookup+drop` anti-pattern documented separately (see Patrón 3 below).
- [x] **F2.** Go audit: `sy-timer` request/respond simple; `sy-wf-rules` has its own per-trace_id mux (equivalent Go to `RpcClient`); `sy-opa-rules` has separate `forwardOutgoing` goroutine; `wf-generic` simple dispatcher. **No bugs found in Go.**
- [x] **F3.** Findings table published below ("Block F findings table"). Classification: bug fixed / OK-by-design / anti-pattern follow-up.
- [x] **F4.** Decision: do **not** open a `go/fluxbee-go-sdk/rpc.go` task. The 3 long-lived Go nodes (`sy-wf-rules`, `sy-opa-rules`, `sy-timer`) already have safe dispatchers per their own design; no critical mass justifies replicating `RpcClient` in Go right now.

#### Block F findings table

**Pattern 1 — handler `.await` an RPC inside the recv loop:**

| File | Classification |
| --- | --- |
| `src/bin/sy_orchestrator.rs` (ex-1082) | bug HISTORIC — fixed by Block C |
| `src/bin/sy_admin.rs` | OK by-design — migrated to `RpcClient` in Block D |
| `src/bin/sy_cognition.rs:583` | OK by-design — handler only emits `sender.send(response)` |
| `src/bin/sy_config_routes.rs:151` | OK by-design — handler does not fire RPCs |
| `src/bin/sy_storage.rs:414` | OK by-design — only emits responses |
| `src/bin/sy_identity.rs:2826` | OK by-design — only emits responses |
| `src/bin/sy_policy.rs:125` | OK by-design — no internal RPCs |
| `src/bin/sy_vault.rs:244` | OK by-design — simple request/respond |
| `src/bin/sy_architect.rs:6645` | OK by-design — `router_recv_loop` handler only emits responses; ephemeral admin RPCs (Block E) use separate `RpcClient` connections |
| `nodes/test/{ai,io}-test-*/src/main.rs` | OK by-design — test binaries, request/respond |
| `go/sy-timer/main.go:202` | OK by-design — handler only emits responses |
| `go/sy-wf-rules/node/mux.go` | OK by-design — per-trace_id dispatcher in own goroutine (Go equivalent of `RpcClient`) |
| `go/sy-opa-rules/main.go:1268` | OK by-design — `forwardOutgoing` goroutine separate from recv loop |
| `go/nodes/wf/wf-generic/node/node.go:367` | OK by-design — simple dispatcher |

**Pattern 2 — `SY.*.relay.*` / `*.helper.*` / `*.ephemeral.*` node names:** 0 hits across the repo.

**Pattern 3 — `NodeUuidMode::Ephemeral` with canonical node name (vault lookup anti-pattern):**

| File | Use |
| --- | --- |
| `src/bin/sy_admin.rs:820` | `name: "SY.admin"` + Ephemeral + `connect()` + `resolve_resource` (Openai vault) |
| `src/bin/sy_cognition.rs:1594` | `connect()` + `resolve_resource` for cognition vault lookup |
| `nodes/io/io-slack/src/main.rs:739` | `connect()` + `resolve_resource` for io-slack vault lookup |
| `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:1682` | `connect()` + `resolve_resource` for ai-generic vault lookup |
| `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:1708` | `connect()` + `resolve_resource` for ai-frontdesk-gov vault lookup |
| `src/bin/sy_architect.rs:6174` | `name: "SY.architect"` + Ephemeral + `connect()` + `resolve_resource` (Openai vault) — **added by AF-P3 audit** |
| `src/bin/sy_architect.rs:6249` | `name: "SY.architect"` + Ephemeral + `connect()` + `resolve_resource` (Postgres `messages_db` vault) — **added by AF-P3 audit** |

Classification: **anti-pattern follow-up**, not a bug. Each lookup announces/withdraws the L2 from the router unnecessarily. No deadlock, no identity spoof. **Out of ORPC scope.** Suggested follow-up: extend SDK with `RpcClient::resolve_resource` or `VaultClient` that wraps `Arc<RpcClient>`, then migrate the 7 call sites.

**Separate category — one-shot `RpcClient` ephemeral RPCs:**

| File | Use |
| --- | --- |
| `src/bin/sy_architect.rs:12943` | `RpcClient::connect_with_retry` + `send_admin_rpc` + drop (one-off admin action) |
| `src/bin/sy_architect.rs:13047` | `RpcClient::connect_with_retry` + `send_admin_rpc` + drop (architect status fetch) |

Superseded note (2026-05-24): runtime inventory showed these clients as many visible `SY.architect.*.<uuid>` nodes. They are not orchestrator regressions and do use the SDK `RpcClient`, but they are still an operational anti-pattern for `SY.architect`. Follow-up is tracked in `docs/onworking COA/sy_architect_rpc_multiplexing_tasks.md`: architect should reuse one canonical `SY.architect@<hive>` `RpcClient` for status refresh, tool reads, plan compiler calls, executor calls, and later vault lookups.

### Block G — Relay residue cleanup (subsumes ORPC-9, 13, 14)

- [x] **G1.** Final grep done. Remaining hits classified:
  - `docs/onworking COA/orchestrator_internal_rpc_multiplexing_tasks.md` — this task file, OK.
  - `src/bin/sy_orchestrator.rs:17199` — negative regression test, OK.
  - `src/bin/sy_identity.rs:5572` — negative regression test, OK.
  - `docs/sy-timer.md:256` — false positive: markdown bold `**Solo SY.orchestrator.**` (Spanish prose), not a wildcard pattern.
  - 0 live behavior references. 0 `authorized_name_variants`. 0 `ilk-delete` / `vault-purge` in code.
- [x] **G2.** `docs/sy_identity_audit.md` already clean (no mentions of deleted helpers). `docs/onworking COA/orchestrator_frictions.md` FR-10 entry updated to mark CLOSED and reference the ORPC plan. `crates/fluxbee_sdk/README.md` does not exist (skipped). `docs/sy-timer.md` does not need changes.
- [~] **G3.** PR reference and final `✓` to be added when this branch merges to `main`. H4/H5 live E2E gates block final closure.

### Block H — Integration / E2E tests

- [x] **H1.** Orchestrator no-deadlock test: fire `run_node` from admin while a `system` message arrives in parallel; assert both complete within timeout. Implemented as `h1_run_node_remote_does_not_block_system_worker`, using real `run_admin_worker`, `run_system_worker`, `build_orchestrator_rpc_profile()`, and `RpcTestHarness`.
- [x] **H2.** Orchestrator: simulate a message with an unrelated or colliding `trace_id` arriving during a pending RPC; assert (i) the RPC keeps waiting when the message is operational/unrelated, (ii) the message is delivered to its category operational worker/channel, (iii) nothing is dropped, (iv) a malformed correlated response fails the waiter with `InvalidResponse`, and (v) a late correlated response after timeout is counted as stale and discarded rather than delivered to a worker. Implemented as `h2_orchestrator_rpc_routes_collisions_invalid_and_stale`.
- [x] **H3.** Orchestrator: `purge_owner_timers_before_teardown` exercised through `RpcClient::from_test_channels` / `RpcTestHarness` exposed by `fluxbee_sdk` under `test-utils` (no trait abstraction; the production type is the one under test), using the same orchestrator `OperationalRouteProfile` shape. The test reads the outgoing `Message` written by the client and asserts: `routing.src = sender.uuid()` (canonical sender), `routing.src_l2_name = None`, `routing.dst = Destination::Unicast(timer_node)`, `meta.msg_type = SYSTEM_KIND`, `meta.msg = Some("TIMER_PURGE_OWNER")`, `meta.target = None`, `meta.action_class = classify_system_message("TIMER_PURGE_OWNER")`. While the purge waiter is pending, the test injects (a) `(SYSTEM_KIND, "SYSTEM_UPDATE")` with random trace, and (b) an `(ADMIN_KIND, MSG_ADMIN_COMMAND)` with the **colliding** trace; asserts (a) reaches `take_command_receiver("system")`, (b) reaches `take_command_receiver("admin")`, and the waiter stays pending. Then injects `(SYSTEM_KIND, "TIMER_RESPONSE")` with the matching trace and a canned payload, and asserts the waiter completes with that payload. Implemented as `h3_timer_purge_uses_canonical_rpc_client_and_routes_collisions`.
- [~] **H4.** E2E `scripts/node_teardown_completeness_e2e.sh` — **static validation done 2026-05-24**: script has 0 mentions of `relay`, 0 timeout bumps, 0 added grants. Exercises exactly the ORPC-fixed flows (`purge_instance=true` → `kill_node_flow` → `purge_owner_timers_before_teardown` → `TIMER_PURGE_OWNER`, plus `delete_ilk_for_teardown` and `purge_vault_secrets_for_ilk` via `orchestrator_admin_command`). All three paths covered by unit tests post-refactor (H3 fakes the canonical RpcClient transport). **Live run pending**: requires a running router + hive (`BASE=http://127.0.0.1:8080` + identity service); not available in dev workstation. Mark `[x]` after first staging/CI run.
- [~] **H5.** Identity E2E — **static validation done 2026-05-24**: scripts `identity_register_strict_e2e.sh`, `identity_node_registration_e2e.sh`, `identity_provision_complete_e2e.sh`, `identity_negative_e2e.sh`, `identity_replica_sync_e2e.sh`, `identity_merge_alias_e2e.sh`, `identity_test_nodes_publish_e2e.sh` exist; none contains relay references. `ILK_REGISTER` and `ILK_UPDATE` paths from orchestrator now go through `orchestrator_identity_system_call_ok` → `state.rpc().send_system_rpc(SystemRpcRequest)` with the canonical sender uuid, so `routing.src_l2_name` will be stamped as `SY.orchestrator@<hive>` by the router. **Live run pending**: same infra requirement as H4. Mark `[x]` after first staging/CI run.

### Execution order

1. **A** (RpcClient + tests). Foundation.
2. **D** (sy_admin migrates first). `sy_admin` has the stronger test battery; it acts as the validation gate for `RpcClient` before orchestrator changes land.
3. **C** (orchestrator refactor with proven `RpcClient`).
4. **B** (delete legacy SDK helpers — only safe once C and D no longer use them).
5. **E** (other call sites — must land in the same PR, otherwise compilation breaks).
6. **F** (full-repo audit) — runs in parallel from block 2 onward, reports findings, doesn't gate.
7. **G** (final relay sweep + docs).
8. **H** (integration / E2E) — merge gate.

## Closure status 2026-05-24

### What is DONE

| Block | Status | Evidence |
| --- | --- | --- |
| **A** | ✓ DONE | `crates/fluxbee_sdk/src/rpc.rs` complete. SDK RPC tests 34/34 green. |
| **B** | ✓ DONE | `admin.rs` deleted entirely; identity helpers gone; lib.rs/prelude.rs updated. |
| **C** | ✓ DONE | `OrchestratorRouterClient` removed; workers via `take_command_receiver`; sy_orchestrator 86/86 green. |
| **D** | ✓ DONE | `AdminRouterClient` removed; `OperationalRouteProfile` with 3 command channels + 2 broadcasts; sy_admin 65/65 green. |
| **E** | ✓ DONE | 7 call sites migrated (sy_architect + 5 diags + io-test). 5 diags out of scope by criterion. |
| **F** | ✓ DONE | Full repo + Go audit; 0 deadlock bugs outside orchestrator; findings table published. |
| **G** | ✓ DONE | Relay residue grep clean; FR-10 doc closed; identity_audit clean. |
| **H** | PARTIAL | H1/H2/H3 done with `RpcTestHarness`. H4/H5 statically verified (script grep: 0 relay refs, 0 timeout bumps, exercise canonical post-refactor paths). Live run pending — requires running router + hive (`BASE=http://127.0.0.1:8080` + identity service). |

### Build + test results

- `cargo check --workspace`: passed, warnings only.
- `fluxbee-sdk` `rpc::tests`: 34/34 green.
- `sy_admin`: 65/65 green. `sy_orchestrator`: 86/86 green. `sy_architect`: 154/154 green. `sy_identity`: 27/27 green.

### What did NOT migrate (and why)

These are intentional deferrals, not omissions. Each is documented either above (Block B/E/F notes) or here.

| Item | Reason | Follow-up |
| --- | --- | --- |
| `IdentityError` enum still has `Unreachable / TtlExceeded / SystemRejected / Timeout / ActionTimeout / Node / Json` variants | `sy_orchestrator::map_rpc_error_to_identity` constructs them locally from `RpcError` for downstream typing; `io_common::identity::IdentityError` in the io nodes is a separate enum with the same name. Encogerlo exigía migrar todos esos call sites. **SDK ya no las construye**, satisface el espíritu del doc. | Split into `IdentityRpcError` (deprecate) + `IdentityShmError` once io nodes adopt `RpcError` directly. |
| `IdentitySystemRequest` / `IdentitySystemResult` structs still public in `identity.rs` | Used as return types by the custom dispatchers in `nodes/gov/ai-frontdesk-gov` and `nodes/ai/ai-generic` (`SharedRouterConnection::GovIdentityBridge`). | Migrate those 2 nodes to `Arc<RpcClient>`; then types can be removed. |
| 5 diags not migrated: `blob_sync_diag`, `inventory_hold_diag`, `jetstream_envelope_diag`, `orch_system_diag`, `wf_nats_diag` | None matches the "1 RPC ephemeral" criterion: `BlobToolkit` multi-step workflow, consumer loops, NATS, mini-protocol dispatcher. None uses deleted helpers. | Per-diag: extend `BlobToolkit::publish_blob_and_confirm` to accept `&RpcClient`; declare profile with `system_update` channel for `orch_system_diag`. Tracked separately. |
| 7 vault-lookup ephemeral connect anti-patterns: `sy_admin:820`, `sy_cognition:1594`, `io-slack:739`, `ai-generic:1682`, `ai-frontdesk-gov:1708`, `sy_architect:6174`, `sy_architect:6249` (last two added by AF-P3) | `resolve_resource()` SDK helper requires `(&NodeSender, &mut NodeReceiver)`; each caller creates an ephemeral connect+drop per vault lookup. Not a deadlock, not a spoof; just spurious router announce/withdraw. | Add `VaultClient` (or `RpcClient::resolve_resource`) to SDK, then migrate the 7 sites. |
| Go `RpcClient` equivalent | F4 decision: not needed. `sy-wf-rules` already has its own per-trace_id mux; `sy-opa-rules` has a separate forward goroutine; `sy-timer` is request/respond simple. No critical mass. | Re-evaluate only if a Go node grows the deadlock pattern. |
| H4, H5 E2E (`scripts/node_teardown_completeness_e2e.sh`, identity E2E) | Need a live hive/router. | Run when CI / staging environment is available. |

## Iteration 2 — Audit follow-up 2026-05-24

External audit surfaced four issues after Blocks A–G closed. Severity-ordered:

### AF-P1 — `sy_orchestrator` bypasses authorization on lifecycle/read actions (🔴 SECURITY)

**Bug**: The allowlist `is_allowed_system_source_name` is applied inside an `if matches!(action, ...)` block ([sy_orchestrator.rs:1532-1547](src/bin/sy_orchestrator.rs#L1532-L1547)) that lists 13 protected actions. The dispatcher `match` further down ([sy_orchestrator.rs:1722-1747](src/bin/sy_orchestrator.rs#L1722-L1747)) **also** accepts and executes `START_NODE`, `RESTART_NODE`, `GET_RUNTIMES`, `LIST_NODES`, `GET_RUNTIME` — none of which goes through the allowlist. Any node routable to the orchestrator can manipulate lifecycle of other nodes. Pre-ORPC bug uncovered by the audit.

**Fix (settled with dev)**: single source of truth.

- New helper `fn protected_system_action_response(action: &str) -> Option<&'static str>` returning the `*_RESPONSE` name for every protected action (the 13 current + 5 missing = 18 total). `INVENTORY_REQUEST` returns `INVENTORY_RESPONSE` (existing asymmetry preserved).
- `handle_system_message` replaces the current `if matches!(...) { match action { /* forbidden branches */ } }` block (≈115 lines) with:

  ```rust
  if let Some(response_name) = protected_system_action_response(action) {
      if !is_allowed_system_source_name(state, msg.routing.src_l2_name.as_deref()) {
          tracing::warn!(action, src_uuid = %msg.routing.src, "blocked");
          let payload = forbidden_system_source_payload(msg, msg.routing.src_l2_name.as_deref());
          let _ = send_system_action_response(sender, msg, response_name, payload).await;
          return Ok(());
      }
  }
  ```

- Dispatcher `match action { ... }` (lines 1676-1764) stays intact.

**Tests**: one regression test per newly-protected action (`START_NODE`, `RESTART_NODE`, `GET_RUNTIMES`, `LIST_NODES`, `GET_RUNTIME`) asserting that `src_l2_name=None` returns the `FORBIDDEN` payload via the correct `*_RESPONSE` name.

- [x] **AF-P1.** Implemented. New helper `protected_system_action_response` covers 18 actions (the 13 pre-existing + 5 newly-gated: `START_NODE`, `RESTART_NODE`, `GET_RUNTIMES`, `LIST_NODES`, `GET_RUNTIME`). Tests added: `protected_system_action_response_covers_all_18_protected_actions`, `protected_system_action_response_returns_none_for_unknown_action`, `protected_system_action_response_gates_lifecycle_actions_added_by_af_p1`, `protected_actions_emit_forbidden_response_when_origin_is_unauthorized` (6 actions × FORBIDDEN payload + response_name assertions via `RpcTestHarness`). `sy_orchestrator` test count 83 → 90.

### AF-P2a — `RpcClient::send_with_matcher` should fail-fast during reconnect (🟡 UX/timeout friction)

**Bug**: `send_with_matcher` registers the pending waiter and then calls `NodeSender::send` ([rpc.rs:1043](crates/fluxbee_sdk/src/rpc.rs#L1043)). `NodeSender::send` only enqueues into the mpsc backing the connection manager ([split.rs:121](crates/fluxbee_sdk/src/split.rs#L121)) — it does **not** validate connection state. During a reconnect, the manager drains that queue ([node_client.rs:157](crates/fluxbee_sdk/src/node_client.rs#L157)) without sending. The RPC then times out (default 5s, often 30s for lifecycle) instead of failing with `Disconnected`.

**Fix (settled with dev)**: strict fail-fast.

- Expose `NodeSender::is_connected() -> bool` and `NodeSender::wait_connected()` as `pub` methods delegating to `ConnectionState` (already exists, just not exposed).
- In `RpcClient::send_with_matcher`, before registering the pending entry:

  ```rust
  if !self.sender.is_connected() {
      return Err(RpcError::Disconnected);
  }
  ```

- After `self.sender.send(outgoing).await`, normalize `NodeError::Disconnected -> RpcError::Disconnected` (today it surfaces as `RpcError::Node(NodeError::Disconnected)` — change to a single error variant the caller can `match` cleanly).
- Race window between check and send is acceptable (post-send catches it too).

**Tests**: `send_with_matcher` against a harness whose sender is disconnected → `Err(RpcError::Disconnected)` immediately, pending map stays empty.

- [x] **AF-P2a.** Implemented. `NodeSender::is_connected()` and `NodeSender::wait_connected()` exposed as `pub` in `split.rs`. `send_with_matcher` pre-checks `is_connected()` before registering the waiter (fails fast with `Disconnected`) and maps `NodeError::Disconnected` from the post-send path to `RpcError::Disconnected` so callers see a single variant. Test added: `send_with_matcher_fails_fast_when_sender_is_disconnected`.

### AF-P2b — Response-only registry observational exemption must check `RouteTarget` (🟡 Latent semantic bug)

**Bug**: `register_response_only` skips registering a success shape if it appears in `post_pending_rules` ([rpc.rs:973](crates/fluxbee_sdk/src/rpc.rs#L973)). But `post_pending_declares_exact` / `post_pending_declares_family` look only at the `RouteMatch`, not at the `RouteTarget`. With the orchestrator profile that has `AnyMsgOfType(SYSTEM_KIND) -> Command("system")`, **any** `(SYSTEM_KIND, *)` success shape gets quietly skipped from the registry — so a late correlated response after the stale TTL falls through to the `system` worker as if it were a new command.

Today no profile actually triggers the failure (sy_admin's CONFIG_RESPONSE is Broadcast; orchestrator's broad rule is Command but no current `send_*_rpc` has its success shape match the `AnyMsgOfType(SYSTEM_KIND)` family AND survive past stale TTL). But the SDK is more permissive than the doc contract.

**Fix (settled with dev)**:

- Rename `post_pending_declares_exact` → `post_pending_declares_observational_exact` (idem `_family`).
- Both methods must require `RouteTarget::Broadcast(_)` on the matched rule:

  ```rust
  fn post_pending_declares_observational_family(&self, msg_type: &str) -> bool {
      self.profile.post_pending_rules.iter().any(|(rule, target)| {
          matches!(rule, RouteMatch::AnyMsgOfType(t) if t == msg_type)
              && matches!(target, RouteTarget::Broadcast(_))
      })
  }
  ```

**Tests**: (a) profile with `AnyMsgOfType(SYSTEM_KIND) -> Command("worker")` + `send_system_rpc`; inject a late correlated response after timeout/stale → count as `metric_unknown_responses`, **NOT** delivered to the worker. (b) profile with `Exact(SYSTEM_KIND, "CONFIG_RESPONSE") -> Broadcast("config")` + `send_system_rpc` whose success is `CONFIG_RESPONSE` → response is **broadcast** (observational exemption applies).

- [x] **AF-P2b.** Implemented. `post_pending_declares_exact` / `post_pending_declares_family` renamed to `post_pending_declares_observational_exact` / `_observational_family`. Both now require `RouteTarget::Broadcast(_)` to grant the response-only registry exemption — `Command` targets no longer slip responses past the registry. Tests added: `post_pending_command_catch_all_does_not_exempt_response_only_registry` (broad Command rule → late response counted as `unknown_responses`, not delivered to worker), `post_pending_broadcast_rule_does_exempt_response_only_registry` (CONFIG_RESPONSE Broadcast rule → response fans out, no unknown count). SDK test count 137 → 140.

### AF-P3 — Ephemeral connect inventory undercount in doc (🟢 Documentation)

**Gap**: Block F table "Pattern 3" lists 5 vault-lookup ephemeral sites. Audit found 2 more in `sy_architect`:

- [sy_architect.rs:6174](src/bin/sy_architect.rs#L6174) — vault Openai lookup, `name: "SY.architect"` + Ephemeral + `connect()` + `resolve_resource`.
- [sy_architect.rs:6249](src/bin/sy_architect.rs#L6249) — vault Postgres messages_db lookup, same pattern.

Total real: **7 sites**, not 5. Also: the 2 `RpcClient::connect_with_retry` ephemeral admin/status RPCs in `sy_architect` (lines 12943 and 13047, migrated in Block E) are a **different** category — they use the canonical abstraction one-shot per operation, which is acceptable for a node that only needs occasional outbound RPCs. They are not anti-patterns; they should be listed separately in the audit doc as "one-shot RpcClient ephemeral RPC (acceptable)".

**Fix**: doc-only. Update the Pattern 3 table in Block F + the "What did NOT migrate" row.

- [x] **AF-P3.** Updated Block F Pattern 3 table from 5 → 7 sites (added `sy_architect:6174` and `sy_architect:6249`), added separate "one-shot `RpcClient` ephemeral RPCs (acceptable)" subsection for `sy_architect:12943` and `sy_architect:13047`. Updated the matching row in "What did NOT migrate".

### AF-P4 — Write-side disconnect must wake pending RPC waiters (🟡 Hidden timeout bug)

**Bug**: AF-P2a fixed the pre-send disconnected path and receive-side disconnect drain, but a write-side socket failure in `tx_loop` only flipped `ConnectionState` to disconnected. If the reader task was aborted by the connection manager before it emitted a receiver error, `RpcClient::recv_loop` had no signal to call `drain_pending_waiters`; in-flight RPCs could still wait for timeout.

**Fix**: `tx_loop` now receives the app-facing receiver channel and sends `Err(NodeError::Io(_))` when `write_frame` fails. `RpcClient::recv_loop` already treats `Io(_)` as connection loss, so all pending waiters complete with `RpcError::Disconnected`.

**Tests**: `tx_loop_write_error_notifies_receiver`.

- [x] **AF-P4.** Implemented.

### AF-P5 — `ConnectionState::wait_connected` must not lose reconnect notifications (🟡 Async race)

**Bug**: `wait_connected()` checked the atomic state and then awaited `Notify::notified()`. A reconnect between those two operations could be missed because `notify_waiters()` does not store a permit for future waiters.

**Fix**: create and enable the `Notified` future before checking `is_connected()`, then await only if the state is still disconnected.

**Tests**: `wait_connected_returns_after_reconnect_signal`.

- [x] **AF-P5.** Implemented.

### AF-P6 — Observational family rules must exempt exact response shapes (🟢 SDK semantics)

**Bug**: AF-P2b required `RouteTarget::Broadcast(_)`, but exact success shapes only checked exact/one-of post-pending rules. A profile with `AnyMsgOfType("query_response") -> Broadcast("query")` plus an RPC success `Exact("query_response", "QUERY_DONE")` still registered that exact shape in the response-only registry and could drop valid observational events.

**Fix**: `post_pending_declares_observational_exact` now treats a broadcast `AnyMsgOfType` rule for the same `msg_type` as an observational exemption for exact success shapes.

**Tests**: `response_only_skips_exact_success_declared_by_observational_family`.

- [x] **AF-P6.** Implemented.

### AF-P7 — Failed sends must not pollute the response-only registry (🟢 Cleanup)

**Bug**: `send_with_matcher` registered success shapes in `response_only` before `NodeSender::send`. If the send failed, the pending waiter was removed but the response-only shape stayed behind.

**Fix**: register response-only shapes only after `sender.send(outgoing).await` succeeds. The active pending waiter is already installed before send, so correlated immediate responses are still handled by the pending table.

**Tests**: `send_failure_does_not_register_response_only_shape`.

- [x] **AF-P7.** Implemented.

### Iteration 3 verification

- `cargo test -p fluxbee-sdk -- --nocapture`: 144/144 passed.
- `cargo test --bin sy_orchestrator -- --nocapture`: 90/90 passed.
- `cargo test --bin sy_admin`: 65/65 passed.
- `cargo check --workspace`: clean.
- `git diff --check`: passed.
- `rustfmt --check --edition 2021 crates/fluxbee_sdk/src/node_client.rs crates/fluxbee_sdk/src/split.rs crates/fluxbee_sdk/src/rpc.rs src/bin/sy_orchestrator.rs`: passed.
- **`cargo fmt --all` executed 2026-05-24**: pre-existing fmt debt in `nodes/ai/ai-generic/src/bin/ai_node_runner.rs` cleaned in the same pass; `cargo fmt --check` now returns **0 diffs across the workspace**.

### AF-P8 — `tx_loop` error classification cleanup (🟢 OPTIONAL hardening — deferred)

**Optional**: AF-P4 currently forwards `disconnect_tx.send(Err(NodeError::Io(err)))` and the recv-loop classifier accepts both `Io(_)` and `Disconnected`. If a future change tightens that classifier to only `Disconnected`, the write-side path silently regresses. Hardening would convert the `tx_loop` error to `NodeError::Disconnected` before forwarding (keeping the original `Io` in `tracing::warn!`).

**Status**: not blocking. The classifier covers both cases today. Deferred as low-priority follow-up.

- [ ] **AF-P8.** Deferred. Implement only if the classifier contract changes.

### Iteration 2 execution order

1. AF-P1 (security, immediate).
2. AF-P2a (UX, short fix).
3. AF-P2b (latent bug, preventive).
4. AF-P3 (doc, trivial).

### Iteration 3 execution order

1. AF-P4 (write-side disconnect waiter drain).
2. AF-P5 (`wait_connected` notification race).
3. AF-P6 (exact success under observational family).
4. AF-P7 (failed-send response-only cleanup).

## Task list v1 — historical

Kept for traceability. Items marked [x] are real progress but live inside `OrchestratorRouterClient` (the v1 abstraction), which Block C deletes. The work is not lost — it informed the v2 design, but it does not satisfy v2 acceptance criteria by itself.

- [x] ORPC-1. Design `OrchestratorRouterClient`. *Subsumed by Block A1–A4.*
- [x] ORPC-2. Refactor orchestrator receive ownership. *Closed by Block C3–C5; recv loop now lives inside `RpcClient`.*
- [x] ORPC-3. Canonical system RPC helper. *Re-implemented in Block A2 + C6.*
- [x] ORPC-4. Canonical identity RPC helper. *Re-implemented in Block A2 + C6.*
- [x] ORPC-5. Migrate system action call sites. *Re-validated in Block C6.*
- [x] ORPC-6. Migrate identity call sites. *Re-validated in Block C6.*
- [x] ORPC-7. Migrate admin call sites used by teardown. *Re-validated in Block C6.*
- [x] ORPC-8. Remove relay helpers. *Already done in v1; verified in Block G1.*
- [x] ORPC-9. Clean `SY.identity` relay residue. *Verified in Block G1; negative tests stay.*
- [~] ORPC-10. Audit `SY.timer`. *Static code path verified clean; live re-run gated on Block H4.*
- [ ] ORPC-11. Review node teardown E2E. *Block H4.*
- [x] ORPC-12. Focused coverage. *SDK side done (Block A5); orchestrator-side H1–H3 done in Block H.*
- [x] ORPC-13. Final cleanup sweep. *Done in Block G1.*
- [x] ORPC-14. Update docs. *Done in Block G2.*

## Open review notes (post 2026-05-23 review)

- Dispatch by `trace_id` alone is not strong enough for v2. The pending table uses `trace_id` only as the lookup key; completion is decided by the declarative `PendingMatcher`. Exact terminal transport errors are separate from invalid-response families, so an admin RPC can accept `(SYSTEM_KIND, MSG_UNREACHABLE)` without treating every `SYSTEM_KIND` collision as malformed.
- Responses after waiter removal are not operational commands. The recent-stale trace table catches late correlated responses after success, timeout, or drain and drops them with metrics/logging. The response-only registry catches registered non-`Any` / non-observational RPC response shapes even after stale TTL expiry and drops/logs them before profile routing.
- `OrchestratorRouterClient::sender_snapshot` returned a cloned `NodeSender` and worked in v1 because the SDK keeps the clone valid across transparent reconnections. `RpcClient` keeps the same contract (`NodeSender: Clone` is assumed stable; `RwLock<NodeSender>` is only needed if the SDK ever exposes a sender-replace API, which it does not today).
- Operational ordering: handlers run serialized by category in v2. This matches lifecycle expectations and avoids introducing new ordering bugs while fixing the recv-loop deadlock. Block C5/H1 record the ordering proof for the current implementation; parallelism can be added later only per handler and bounded.
- Command-channel queueing is profile-declared `mpsc` wrapped by `RpcCommandReceiver` with a depth gauge and WARN-on-soft-threshold (1000). Bounded `try_send` with fatal-on-overflow is deferred unless an audit shows the router itself can outrun the consumer. Until then, unbounded + depth metric is the contract.
- Test transport: `RpcClient::from_test_channels` / `RpcTestHarness` is the single injectable path and must be available to downstream crate tests via `test-utils` or an equivalent public harness module, not plain `#[cfg(test)]` inside `fluxbee_sdk`. No `RpcPort` trait is added — the production type is what tests exercise, so the dispatcher, matcher, stale-response handling, depth metric, and drain logic are all covered by the same code path that runs in production.

## Fresh review notes 2026-05-24

Review scope: code read-through of the implemented ORPC refactor after Blocks A-G. Initial review was doc-only; the follow-up fixes below were applied on 2026-05-24. H1/H2/H3 were then implemented as in-process orchestrator tests; H4/H5 remain live E2E gates.

### Findings to discuss

- [x] **FRR-1 / P1: `RpcClient` does not drain waiters on the real reconnect signal.** `RpcClient::recv_loop` drained pending waiters only on `NodeError::Disconnected` (`crates/fluxbee_sdk/src/rpc.rs:786-793`). The real socket reader sends `NodeError::Io(...)` on EOF/read failure before the connection manager reconnects (`crates/fluxbee_sdk/src/node_client.rs:271-284`). **Fixed 2026-05-24**: connection-loss receiver errors now include `Disconnected` and `Io(_)`; `Json` remains a non-draining malformed-frame error. Covered by `recv_loop_io_error_drains_pending_waiters`.

- [x] **FRR-2 / P1: `send_with_matcher` can overwrite an active waiter when a caller supplies a duplicate `trace_id`.** The API explicitly reuses `outgoing.routing.trace_id` when set, but registration used plain `pending.insert(trace_id, PendingEntry { ... })` (`crates/fluxbee_sdk/src/rpc.rs:1031-1049`). **Fixed 2026-05-24**: active duplicate `trace_id` is rejected before send with `RpcError::InvalidRequest`. Covered by `send_with_matcher_rejects_duplicate_active_trace_id`.

- [x] **FRR-3 / P2: route-profile validation only checks the first broad rule.** `validate_rule_table` found the first broad rule with `position(...)` and only validated later rules against that one (`crates/fluxbee_sdk/src/rpc.rs:401-421`). **Fixed 2026-05-24**: validation now checks every broad rule against following rules. Covered by `profile_builder_rejects_later_broad_unreachable_rule`.

- [x] **FRR-4 / P2: `sy_admin` OPA observational collectors subscribe after sending the trigger.** `send_opa_action` called `broadcast_config_changed(...).await` before `client.subscribe("config_response")` (`src/bin/sy_admin.rs:11685-11698`). `send_opa_query` sent the query before `client.subscribe("query")` (`src/bin/sy_admin.rs:11737-11741`). **Fixed 2026-05-24**: both paths create the broadcast receiver before emitting the trigger.

- [x] **FRR-5 / P3: residual legacy/dead scaffolding remains after helper deletion.** `cargo check --workspace` reported unused identity imports and dead structs/functions left from the deleted single-shot helpers: `IlkProvisionResponsePayload`, `UnreachablePayload`, `TtlExceededPayload`, `parse_provision_payload`, `is_prefixed_uuid` (`crates/fluxbee_sdk/src/identity.rs:14-22`, `241-267`, `928-955`, `1621`). `sy_orchestrator` also kept an unused local `connect_with_retry` (`src/bin/sy_orchestrator.rs:2960-2973`) plus an unused `mpsc` import in normal builds. **Fixed 2026-05-24**: listed helper leftovers removed. Remaining warnings are pre-existing dead-code warnings outside this ORPC bug set.

### Verification run during fresh review

- `cargo check --workspace`: passed, warnings only.
- `cargo test -p fluxbee-sdk rpc::tests`: 34/34 passed.
- `cargo test -p json-router --bin sy_orchestrator -- --nocapture`: 86/86 passed.
- `cargo test -p json-router --bin sy_admin -- --nocapture`: 65/65 passed.

### Non-issues confirmed

- No live `AdminRouterClient` / `OrchestratorRouterClient` implementation remains; remaining mentions are historical docs/comments.
- No live `SY.orchestrator.relay.*`, `SY.orchestrator.ilk-delete.*`, or `SY.orchestrator.vault-purge.*` code path found; relay-name hits are negative tests or unrelated IO relay terminology.
- `sy_admin` and `sy_orchestrator` build profiles in their own binaries, with no SDK-predefined profile coupling.
- H1/H2/H3 now exercise the production `RpcClient` transport/dispatcher path through `RpcTestHarness`; H4/H5 still need live router/hive validation.

## Acceptance criteria

- `run_node` does not create `SY.orchestrator.relay.*` connections.
- `ILK_REGISTER` and `ILK_UPDATE` reach `SY.identity` as `SY.orchestrator@<hive>`.
- `TIMER_PURGE_OWNER` reaches `SY.timer` as `SY.orchestrator@<local-hive>`.
- `SY.timer` accepts node teardown purge without adding relay exceptions.
- `SY.identity` no longer needs the `SY.orchestrator.relay.*` compatibility alias.
- `SY.timer` has no relay-specific workaround or wildcard grant for this path.
- The node teardown completeness E2E passes without timer or identity relay workarounds.
- The E2E remains a lifecycle check and does not encode relay-specific behavior.
- Normal orchestrator event handling still works while one or more internal RPCs are pending. **In particular, `run_node` / `kill_node` / `get_node_status` complete without timing out, because the recv loop is not blocked on the handler that fired the internal RPC.** (New v2 criterion — review item 1.)
- `fluxbee_sdk::rpc::RpcClient` is the only RPC client abstraction used by `sy_orchestrator` and `sy_admin`. No private `OrchestratorRouterClient` / `AdminRouterClient` remains. (New v2 criterion — review item 2.)
- Operational routing is profile-driven and matches `(msg_type, msg)` where needed. `sy_admin` preserves `status_get`, `system_command`, `internal_admin`, `config_response`, and `query` delivery semantics without hard-coded SDK profiles.
- The single-shot SDK helpers (`admin_command`, `identity_system_call_ok`, etc.) are deleted. Every former caller uses `RpcClient`. (New v2 criterion — decision 3.)
- Repo audit (Block F) is published in this file; every reported hit is classified as bug / dead code / OK-by-design.

## Out of scope for this task

- Public exposure and bind-address hardening for `SY.identity` sync API.
- Cryptographic proof of node identity.
- Router-side reservation of the `SY.*` namespace.
- Changing timer authorization semantics.
- Broad grants such as `SY.orchestrator.*`.
- Replicating `RpcClient` in the Go SDK (decided after Block F2 reports findings).

Those are valid follow-up security tasks, but they should not be mixed with fixing orchestrator's internal request/response transport.
