# Legacy System Nodes SDK Alignment Plan

**Status:** planning / pre-implementation  
**Date:** 2026-04-07  
**Scope:** `SY.config.routes`, `SY.opa.rules`, and the minimal Go SDK needed to make `SY.opa.rules` a first-class node  
**Reference nodes for alignment:** `SY.admin`, `SY.orchestrator`, `SY.identity`, `SY.cognition`, AI nodes using current SDK helpers

**Intent:**
- Bring the legacy system nodes to the same operational level as the current Fluxbee runtime contract.
- Align message envelopes, config control-plane behavior, request/reply semantics, and node lifecycle expectations.
- Do this as a dedicated infrastructure effort, not as a side effect of `SY.policy`.
- For `SY.opa.rules`, take a compatibility-first v1.0 path:
  - keep the current OPA-facing behavior working
  - preserve current OPA-related actions and mechanisms unless a change is required by the new config/control-plane support
  - extract reusable runtime/protocol pieces into `fluxbee-go-sdk`
- Treat `fluxbee-go-sdk` as a real reusable SDK for third-party or future internal Go nodes, but allow its first cut to be shaped by `SY.opa.rules` compatibility needs.

---

## 1) Why this exists

`SY.config.routes` and `SY.opa.rules` are not at the same maturity level as the current system nodes, but they do not need the same treatment.

This creates three problems:

- They do not expose the same message and control-plane conventions as the newer nodes.
- `SY.admin` and `SY.orchestrator` have to compensate for legacy behavior instead of relying on a stable node contract.
- Any policy or observability work done on top of them becomes fragile because the base message/config model is inconsistent.

The key difference is:

- `SY.config.routes` is a Rust node that should be brought up to the current Rust SDK/runtime standard.
- `SY.opa.rules` is effectively carrying its own mini SDK today; the cleaner path is to extract the runtime/protocol pieces it already uses into a real `fluxbee-go-sdk`, add the newer config/control-plane support there, and leave deeper OPA-specific cleanup for a later version.

**Important decision:**
- Do **not** partially retrofit these nodes only for policy metadata.
- First bring them up to the current system contract.
- Only then decide what policy, audit, or runtime governance data should be emitted.

---

## 2) Current diagnosis

### 2.1 `SY.config.routes`

Current state in [`src/bin/sy_config_routes.rs`](/Users/cagostino/Documents/GitHub/fluxbee/src/bin/sy_config_routes.rs):

- Uses the Rust runtime and shared `Message` / `Meta`, but still handles admin and config flows in a legacy/manual style.
- Implements its own admin action surface:
  - `list_routes`
  - `list_vpns`
  - `add_route`
  - `delete_route`
  - `add_vpn`
  - `delete_vpn`
- Uses `MSG_CONFIG_CHANGED` + `CONFIG_RESPONSE` for config propagation, but not the same node config control-plane contract used by newer nodes.
- Builds replies manually instead of using the richer control-plane helpers already present in the SDK.
- Admin error payloads still follow older conventions such as `error` instead of the more standardized `error_code` shape used elsewhere.

### 2.2 `SY.opa.rules`

Current state in [`sy-opa-rules/main.go`](/Users/cagostino/Documents/GitHub/fluxbee/sy-opa-rules/main.go):

- It is a Go standalone service and does **not** use the Rust SDK at all.
- It defines its own local `Message`, `Meta`, and `Routing` structs.
- It speaks a custom router contract:
  - inbound types: `system`, `command`, `query`
  - outbound types: `command_response`, `query_response`, `system`
- It uses OPA-specific actions and replies such as:
  - `compile_policy`
  - `apply_policy`
  - `rollback_policy`
  - `get_policy`
  - `get_status`
  - `CONFIG_RESPONSE`
  - `OPA_RELOAD`
- It performs its own HELLO/ANNOUNCE handshake and router client behavior outside the Rust SDK lifecycle.

### 2.3 What this means

Today these two nodes are "special cases" in different ways:

- `SY.config.routes` is a Rust node on old message/config conventions.
- `SY.opa.rules` is a non-SDK node with its own protocol model.

That makes them hard to reason about as first-class Fluxbee nodes, but the remediation path is different for each one.

---

## 3) Target state

The target is **not** "make them identical internally".  
The target is:

- both nodes behave like first-class Fluxbee system nodes from the outside
- `SY.admin` can invoke them through stable, explicit contracts
- `SY.orchestrator` can manage them like normal critical services
- config/control-plane behavior is predictable and documented
- message and reply semantics are auditable and shared with the rest of the stack

### 3.1 Minimum external contract both nodes should satisfy

- Stable runtime name and node identity behavior
- Standard request/reply envelopes on the router
- Standard error shape
- Standard trace propagation
- Clear separation between:
  - admin-origin actions
  - system/control-plane actions
  - query/read actions
- Explicit config contract:
  - what is live node config
  - what is persisted config
  - what is compile/apply state

### 3.2 Alignment reference

For v1 alignment, these nodes should behave closer to:

- `SY.admin`
- `SY.orchestrator`
- `SY.identity`
- `SY.cognition`
- AI/IO nodes that already implement `CONFIG_GET` / `CONFIG_SET` through the current SDK

### 3.3 Canonical modern node contract reference

For this initiative, the baseline contract to mirror is the one exposed today by the newer Rust nodes and the shared Rust SDK.

This is the reference shape to compare against and reproduce.

#### Envelope baseline

- Common envelope type: `Message`
- Common metadata block: `Meta`
- Common routing block: `Routing`
- Shared router-facing concepts:
  - source identity
  - destination
  - TTL
  - `trace_id`

#### Runtime lifecycle baseline

- Node connects through the router socket
- Node performs `HELLO`
- Node receives `ANNOUNCE`
- Node keeps a reconnect-capable sender/receiver lifecycle
- Node identity/UUID is stable and persisted according to node runtime rules

#### Reply model baseline

Replies should be standardized around:

- `status`
- `error_code`
- `error_detail`
- stable action echo where appropriate
- trace continuity from request to reply

Older shapes such as ad hoc `error` fields are considered legacy.

#### Config/control-plane baseline

Modern nodes distinguish between:

- live node control-plane config
- persisted effective config
- domain actions with side effects

Where applicable, newer nodes already expose canonical `CONFIG_GET` / `CONFIG_SET` and `CONFIG_RESPONSE` semantics through shared helpers.

#### Important boundary

This baseline does **not** force every node to have identical internal implementation.

It does require that, from the outside:

- `SY.admin` sees a stable request/reply contract
- `SY.orchestrator` sees stable lifecycle behavior
- operators see a coherent distinction between query, control-plane config, and domain actions

### 3.4 Rust SDK to Go SDK migration matrix

To build `fluxbee-go-sdk` correctly, the immediate task is **not** to port the whole Rust SDK.

The task is to identify which Rust SDK modules define the current external contract and classify them as:

- `Go v1.0`: required for the compatibility-first `SY.opa.rules` cut
- `Go v1.1+`: valuable next step after the first migration
- `Not in Go SDK scope now`: internal or unrelated to the first consumer

#### Go v1.0 — must be reproduced first

- [`protocol.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/protocol.rs)
  - reason: canonical `Message`, `Meta`, `Routing`, `Destination`, shared protocol constants, HELLO/ANNOUNCE builders
  - Go requirement: reproduce the **current Rust wire format**, including the newer metadata fields
- [`socket/connection.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/socket/connection.rs)
  - reason: canonical router framing
  - Go requirement: same frame size limits and read/write semantics
- [`node_client.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/node_client.rs)
  - reason: canonical node connection lifecycle
  - Go requirement: connect, HELLO, ANNOUNCE, reconnect, UUID persistence, router socket discovery
- [`split.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/split.rs)
  - reason: sender/receiver abstraction and disconnect behavior
  - Go requirement: equivalent node sender/receiver model with trace-safe send path
- [`status.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/status.rs)
  - reason: default node status helper is part of the modern operational baseline
  - Go requirement: at least the same default `NODE_STATUS_GET` handling path or a clearly equivalent helper

#### Go v1.0 — selectively required by `SY.opa.rules`

- [`admin.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/admin.rs)
  - reason: `SY.opa.rules` is an admin-invoked system node and should follow the same admin request/reply contract
  - Go requirement: not necessarily a full admin client in v1.0, but the **envelope and response semantics** must match the Rust side where admin interoperability matters
- [`node_config.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/node_config.rs)
  - reason: this is the main newer Rust-SDK capability we likely want `SY.opa.rules` to start supporting now
  - Go requirement: bring in the subset needed for canonical `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE` behavior without breaking the current OPA action surface
- [`send_normalization.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/send_normalization.rs)
  - reason: `NodeSender::send()` currently applies outbound normalization in Rust
  - Go requirement: defer unless a first Go node actually needs it; do not force it into the initial compatibility cut without a concrete need

#### Go v1.1+ — likely next after first migration

- [`client_config.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/client_config.rs)
  - reason: convenience wrapper around node config + local NATS resolution
  - Go requirement: useful later, but not essential for first router SDK cut
- [`policy.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/policy.rs)
  - reason: shared policy classification vocabulary
  - Go requirement: only after/if Go nodes need to emit policy metadata under the same contract
- [`identity.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/identity.rs)
  - reason: possible future parity point if Go nodes need identity/ILK SHM helpers
  - Go requirement: optional future addition; visible as useful, but not a blocker for v1.0

#### Not in Go SDK scope now

- [`managed_node.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/managed_node.rs)
  - reason: orchestrator/managed-node operations, not generic node SDK baseline
- [`node_secret.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/node_secret.rs)
  - reason: secret persistence helper, orthogonal to first Go node contract
- [`payload.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/payload.rs)
  - reason: useful only if text payload normalization is brought into Go v1
- [`blob/`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/blob/mod.rs)
  - reason: same as payload/blob offload; defer unless required
- [`cognition.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/cognition.rs)
  - reason: domain-specific storage/cognition schema, not SDK transport core
- [`nats.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/nats.rs)
  - reason: not needed for first `SY.opa.rules` router contract work
- [`node_client.rs`]-adjacent higher-level convenience modules such as:
  - [`prelude.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/prelude.rs)
  - [`comm/`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/comm/mod.rs)
  - [`thread.rs`](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/thread.rs)
  - [`split.rs`] helpers beyond the sender/receiver baseline
  - reason: not required to make `SY.opa.rules` a first-class node on the current wire/runtime contract

#### Important implementation note

For `fluxbee-go-sdk`, parity means:

- same wire format
- same message field names
- same canonical constants
- same reply/error semantics

It does **not** mean:

- every Rust convenience helper must exist on day one
- every Rust domain module must be reimplemented in Go immediately
- `SY.opa.rules` must lose its current working OPA-specific surface in v1.0

### 3.5 Versioned migration approach for `SY.opa.rules`

#### v1.0 — compatibility-first

Goals:

- keep the current OPA-related behavior working
- preserve current OPA action names, message patterns, and operational semantics unless change is required by the new config/control-plane support
- extract reusable runtime/protocol pieces into `fluxbee-go-sdk`
- add canonical Rust-SDK-style config/control-plane support where needed now

This means v1.0 should:

- keep current OPA command/query behavior working
- keep current SHM and OPA reload behavior working
- add support for newer config/control-plane integration, especially `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE` if adopted
- avoid broad rewrites of the OPA domain logic

#### v2.0 — deeper OPA migration

Goals:

- migrate remaining OPA-specific logic that still lives as legacy/custom code
- reduce node-specific protocol handling further
- move more of the node behavior onto stable SDK helpers where that produces real simplification

The full cleanup of `SY.opa.rules` is intentionally deferred until after the compatibility-first v1.0 cut.

### 3.6 Exact `node_config` subset for `SY.opa.rules` v1.0

Based on the current Rust-side admin/control-plane flow, the exact contract `SY.opa.rules` needs now is:

- `SY.admin` sends:
  - `msg_type = system`
  - `msg = CONFIG_GET` or `CONFIG_SET`
  - `target = node_config_control`
  - request/reply correlation through the same `trace_id`
- the node replies with:
  - `msg_type = system`
  - `msg = CONFIG_RESPONSE`
  - `target = node_config_control`
  - same `trace_id`

For `fluxbee-go-sdk` v1.0, the recommended approach is to include the **full current Rust `node_config.rs` helper surface**, because it is already small, canonical, and self-contained.

That means including:

- constants:
  - `MSG_CONFIG_GET`
  - `MSG_CONFIG_SET`
  - `MSG_CONFIG_RESPONSE`
  - `NODE_CONFIG_CONTROL_TARGET`
  - `NODE_CONFIG_APPLY_MODE_REPLACE`
- payload structs:
  - `NodeConfigGetPayload`
  - `NodeConfigSetPayload`
  - `NodeConfigControlResponse`
- request/response helpers:
  - `is_node_config_get_message`
  - `is_node_config_set_message`
  - `is_node_config_response_message`
  - `parse_node_config_request`
  - `parse_node_config_response`
  - `build_node_config_get_message`
  - `build_node_config_set_message`
  - `build_node_config_response_message`
  - `build_node_config_response_message_runtime_src`
- support types:
  - `NodeConfigEnvelopeOptions`
  - `NodeConfigControlRequest`
  - `NodeConfigControlError`

Why include the whole helper set instead of only a server-side subset:

- it is already the canonical Rust control-plane contract
- it is small enough that partial porting buys little and creates future drift risk
- it is useful for third-party Go nodes, not only for `SY.opa.rules`
- it allows `SY.opa.rules` to add `CONFIG_GET` / `CONFIG_SET` cleanly without touching its OPA-specific actions

What v1.0 should **not** do:

- replace existing OPA command/query/config actions with node-config control-plane actions
- force `CONFIG_GET` / `CONFIG_SET` to become the primary way to compile/apply/rollback OPA policy

The intended v1.0 split is:

- existing OPA actions keep working as they do today
- canonical node config control-plane support is added alongside them
- deeper unification is a later decision

---

## 4) Work split

This should be treated as an infrastructure modernization stream with two tracks.

### Track A — `SY.config.routes`

Bring the existing Rust node up to current SDK and config conventions.

### Track B — `fluxbee-go-sdk` + `SY.opa.rules`

Build the minimum Go SDK needed to support `SY.opa.rules` without breaking its current working OPA behavior, then use that SDK to add newer Rust-SDK-aligned control-plane pieces.

For now, the target is:

- no ad hoc compatibility layer embedded only in `SY.opa.rules`
- no forced Rust rewrite
- preserve the current OPA-facing actions and mechanisms in v1.0 unless change is required by the config/control-plane work
- a real SDK path for Go-based nodes, with `SY.opa.rules` as first consumer
- a public-facing SDK shape that can be used by third parties building Fluxbee-compatible Go nodes

---

## 5) Detailed backlog

### SYS-ALIGN-S0 — Freeze the alignment goal

- [x] SYS-ALIGN-S0-T1. Confirm that this effort is independent from `SY.policy`.
- [x] SYS-ALIGN-S0-T2. Confirm that no partial "policy-only" metadata retrofits should be done first.
- [x] SYS-ALIGN-S0-T3. Define the canonical reference contract for a modern Fluxbee system node:
  - envelope
  - request/reply types
  - error model
  - trace behavior
  - config control-plane expectations

### SYS-ALIGN-S1 — Audit the current gap

- [ ] SYS-ALIGN-S1-T1. Produce a full protocol inventory for `SY.config.routes`:
  - inbound msg types
  - admin actions
  - system messages
  - reply message shapes
  - error payload shapes
- [x] SYS-ALIGN-S1-T2. Produce a full protocol inventory for `SY.opa.rules`:
  - inbound types `command` / `query` / `system`
  - outbound types `command_response` / `query_response` / `system`
  - HELLO/ANNOUNCE handshake behavior
  - OPA-specific actions and responses
- [ ] SYS-ALIGN-S1-T3. Compare `SY.config.routes` against the current Rust SDK node contract.
- [x] SYS-ALIGN-S1-T3b. Compare `SY.opa.rules` against the current Rust SDK node contract and identify what must be replicated in Go.
- [ ] SYS-ALIGN-S1-T4. Mark which mismatches are:
  - compatibility-critical
  - admin/orchestrator ergonomics
  - observability/policy-relevant
  - cleanup-only

### SYS-ALIGN-S2 — Normalize message envelopes

- [x] SYS-ALIGN-S2-T1. Define the canonical message categories these nodes should expose.
- [ ] SYS-ALIGN-S2-T2. Standardize reply payload conventions:
  - `status`
  - `error_code`
  - `error_detail`
  - version/config fields where applicable
- [x] SYS-ALIGN-S2-T3. Standardize metadata fields that must always be carried:
  - `msg_type`
  - `msg`
  - `action`
  - `target`
  - `trace_id`
- [ ] SYS-ALIGN-S2-T4. Remove older ad hoc differences such as mixed `error` vs `error_code` shapes.
- [x] SYS-ALIGN-S2-T5. Freeze which parts of the Rust message model must be reproduced by `fluxbee-go-sdk`.
- [x] SYS-ALIGN-S2-T6. Decide whether `SY.opa.rules` keeps custom `command/query` message kinds as first-class Fluxbee categories or is adapted to the same shared runtime model used by Rust nodes.

### SYS-ALIGN-S2A — Freeze `fluxbee-go-sdk` v1 minimum scope

- [x] SYS-ALIGN-S2A-T1. Freeze `fluxbee-go-sdk` v1.0 as a **transport/runtime SDK** with a public reusable surface, not yet full parity with every Rust helper.
- [x] SYS-ALIGN-S2A-T2. Include in v1 the minimum common envelope types:
  - `Message`
  - `Meta`
  - `Routing`
  - destination helpers
- [x] SYS-ALIGN-S2A-T3. Include in v1 the minimum router transport behavior:
  - unix socket connect
  - HELLO
  - ANNOUNCE handling
  - frame read/write
  - reconnect loop
- [x] SYS-ALIGN-S2A-T4. Include in v1 the minimum node lifecycle helpers:
  - node identity / UUID loading
  - sender / receiver abstraction
  - request reply helper
  - trace propagation helper
- [x] SYS-ALIGN-S2A-T5. Include in v1 standardized reply helpers for:
  - success replies
  - error replies
  - system message replies
  - query / command reply patterns if those categories remain in scope
- [x] SYS-ALIGN-S2A-T6. Explicitly defer from v1 unless needed by the first migration:
  - higher-level admin client helpers
  - policy helpers
  - manifest helpers
  - storage/query convenience clients
- [x] SYS-ALIGN-S2A-T7. Freeze the first consumer contract:
  - `SY.opa.rules` must be migratable to `fluxbee-go-sdk` without changing OPA SHM semantics
  - the SDK should be reusable by future internal or third-party Go nodes, but v1 is allowed to be minimal
- [x] SYS-ALIGN-S2A-T8. Keep package boundaries, naming, and exported API stable enough that `fluxbee-go-sdk` can be documented and published as the Go counterpart of the Rust SDK.
- [x] SYS-ALIGN-S2A-T9. Freeze the exact node-config subset that enters Go v1.0 for `SY.opa.rules`:
  - `CONFIG_GET`
  - `CONFIG_SET`
  - `CONFIG_RESPONSE`
  - helper builders/parsers needed for those flows

### SYS-ALIGN-S3 — Normalize config/control-plane behavior

- [x] SYS-ALIGN-S3-T1. Decide what "live config" means for `SY.config.routes`.
- [x] SYS-ALIGN-S3-T2. Decide whether `SY.config.routes` should expose canonical `CONFIG_GET` / `CONFIG_SET` through the SDK control-plane helpers.
- [x] SYS-ALIGN-S3-T3. Decide how its existing domain actions relate to node config:
  - route/vpn mutations as domain admin actions
  - full effective config reads/writes as node control-plane config
- [x] SYS-ALIGN-S3-T4. Decide what "live config" means for `SY.opa.rules`:
  - staged policy
  - current applied policy
  - router SHM state
  - backup policy
- [x] SYS-ALIGN-S3-T5. Define whether OPA compile/apply/rollback remain domain actions or become partially expressible through a config contract.
- [x] SYS-ALIGN-S3-T6. Ensure admin documentation clearly distinguishes:
  - stored config
  - live control-plane config
  - domain actions with side effects

### SYS-ALIGN-S4 — Admin/orchestrator contract cleanup

- [x] SYS-ALIGN-S4-T1. Make `SY.admin` the single canonical origin of operator/system commands for these nodes.
  - Operator/system commands for `SY.config.routes` and `SY.opa.rules` are canonically issued through `SY.admin`.
  - Any direct node-local admin/command/query surface that remains is treated as implementation compatibility, not as the primary operator contract.
- [x] SYS-ALIGN-S4-T2. Ensure both nodes expose explicit, documented action catalogs that `SY.admin` can rely on.
  - Canonical action catalogs are documented in [system_node_action_catalogs.md](/Users/cagostino/Documents/GitHub/fluxbee/docs/TODO/system_node_action_catalogs.md).
- [x] SYS-ALIGN-S4-T3. Ensure `SY.orchestrator` only depends on stable service lifecycle contracts, not node-specific reply quirks.
  - Audit result: `SY.orchestrator` interacts with `SY.config.routes` and `SY.opa.rules` through systemd/unit management, router SHM visibility, remote file deployment, and generic `CONFIG_CHANGED` dispatch.
  - It does **not** parse node-specific `CONFIG_RESPONSE` payloads from those nodes to manage their lifecycle.
  - Local cleanup: shared service/unit constants now centralize the legacy-aligned nodes so the orchestrator contract stays service-oriented rather than reply-shape-oriented.
- [x] SYS-ALIGN-S4-T4. Review whether `sy-opa-rules` and `sy-config-routes` should advertise richer capability/contracts during startup.
  - Review result: **do not** extend node startup handshake in v1.0.
  - Current node `HELLO` / `ANNOUNCE` payloads do not carry a first-class node capability/contract field, and changing that would widen the wire contract beyond this alignment stream.
  - Canonical rich contract exposure for these nodes remains:
    - `SY.admin` action catalogs
    - `CONFIG_GET` / `CONFIG_RESPONSE` contract payloads
  - Startup remains intentionally minimal: identity, presence, router attachment, and service lifecycle only.

### SYS-ALIGN-S5 — SDK and implementation work

#### `SY.config.routes`

- [x] SYS-ALIGN-S5-T1. Upgrade to current SDK conventions used by modern nodes.
- [x] SYS-ALIGN-S5-T2. Replace manual config control-plane handling where shared SDK helpers already exist.
- [x] SYS-ALIGN-S5-T3. Normalize admin replies and system replies to the current common shape.
- [x] SYS-ALIGN-S5-T4. Add tests for request parsing and response envelope consistency.

#### `fluxbee-go-sdk`

- [x] SYS-ALIGN-S5-T5. Define the minimum v1.0 scope of `fluxbee-go-sdk`.
- [x] SYS-ALIGN-S5-T6. Implement shared Go types equivalent to the common node envelope:
  - `Message`
  - `Meta`
  - `Routing`
  - destination helpers
- [x] SYS-ALIGN-S5-T7. Implement router framing and connection lifecycle helpers:
  - connect
  - HELLO / ANNOUNCE
  - sender / receiver
  - reconnect behavior
- [x] SYS-ALIGN-S5-T8. Implement standard reply helpers in Go:
  - request/reply
  - error replies
  - trace propagation
- [x] SYS-ALIGN-S5-T9. Implement the node config control-plane subset needed by `SY.opa.rules` v1.0.
- [x] SYS-ALIGN-S5-T10. Add protocol-level tests or fixtures proving parity with the chosen Rust-side contract.

#### `SY.opa.rules`

- [x] SYS-ALIGN-S5-T11. Migrate `SY.opa.rules` transport/envelope code to `fluxbee-go-sdk`.
- [x] SYS-ALIGN-S5-T12. Replace its local `Message` / `Meta` / `Routing` structs with SDK equivalents.
- [x] SYS-ALIGN-S5-T13. Replace its embedded router client lifecycle with SDK connection helpers.
- [x] SYS-ALIGN-S5-T14. Add Rust-SDK-aligned config/control-plane support without breaking the current OPA action surface.
- [x] SYS-ALIGN-S5-T15. Preserve OPA SHM, compile/apply/rollback, and router reload semantics during the migration.
- [x] SYS-ALIGN-S5-T16. Re-test admin and orchestrator interoperability after the SDK migration.
- [ ] SYS-ALIGN-S5-T17. Leave deeper OPA-specific cleanup for v2.0.

### SYS-ALIGN-S6 — Documentation and operator model

- [ ] SYS-ALIGN-S6-T1. Document the canonical action surface for `SY.config.routes`.
- [ ] SYS-ALIGN-S6-T2. Document the canonical action surface for `SY.opa.rules`.
- [x] SYS-ALIGN-S6-T2b. Document `fluxbee-go-sdk` as the canonical Go path for Fluxbee-compatible nodes.
- [ ] SYS-ALIGN-S6-T3. Document what operators should treat as:
  - read/query
  - config control-plane
  - domain action
  - lifecycle/admin action
- [ ] SYS-ALIGN-S6-T4. Update admin help/docs once the contracts are frozen.

---

## 6) Non-goals for this document

- This is **not** yet the `SY.policy` integration plan.
- This does **not** decide the final online policy architecture.
- This does **not** require rewriting `SY.opa.rules` in Rust.
- This does **not** require redesigning route/VPN semantics or OPA semantics themselves.

The goal is narrower:
- make these legacy nodes coherent with the current Fluxbee runtime contract first

For `fluxbee-go-sdk`, that still means a real external SDK:
- the first consumer is `SY.opa.rules`
- but the design target is a third-party-usable Go SDK, analogous in intent to the Rust SDK

---

## 7) Recommended order

Recommended implementation order:

1. `SYS-ALIGN-S1` protocol audit
2. `SYS-ALIGN-S2` envelope normalization decisions
3. `SYS-ALIGN-S2A` freeze `fluxbee-go-sdk` v1 minimum scope
4. `SYS-ALIGN-S3` config/control-plane decisions
5. `SYS-ALIGN-S5` implementation
6. `SYS-ALIGN-S6` documentation updates

**Pragmatic recommendation:**
- Do not change working OPA behavior in `SY.opa.rules` unless it is part of the new config/control-plane support being added now.
- For `SY.opa.rules`, do not build a one-off compatibility layer inside the node.
- Start by freezing the minimum `fluxbee-go-sdk` contract plus the node-config subset it needs, then migrate `SY.opa.rules` onto that SDK.

---

## 8) Execution order

To avoid losing the sequence, the execution order for this stream is:

1. Freeze the canonical modern node contract reference.
2. Audit `SY.opa.rules` against that contract.
3. Freeze `fluxbee-go-sdk` v1.0 minimum scope.
4. Freeze the `node_config` subset that `SY.opa.rules` needs now.
5. Implement `fluxbee-go-sdk` core transport/runtime pieces.
6. Implement the config/control-plane subset needed now.
7. Migrate `SY.opa.rules` to `fluxbee-go-sdk` without changing the current OPA-facing behavior.
8. Re-test `SY.admin` and `SY.orchestrator` interoperability with `SY.opa.rules`.
9. Audit `SY.config.routes` against the modern Rust node contract.
10. Bring `SY.config.routes` up to current Rust SDK/config conventions.
11. Update docs and operator-facing action/config references.

**Immediate next step:**
- Start with steps 1 to 4, because if the Go SDK scope and the config subset are not frozen first, `SY.opa.rules` will just grow another embedded compatibility layer.

## 9) Linux validation checklist for `SY.opa.rules`

When this migration slice is ready for runtime validation on Linux, verify:

1. Service boot and router attach
   - start `sy-opa-rules`
   - confirm HELLO/ANNOUNCE completes
   - confirm node appears as `SY.opa.rules@<hive>`

2. Query interoperability from `SY.admin`
   - run `opa_get_status`
   - run `opa_get_policy`
   - confirm replies arrive as `query_response`
   - confirm `meta.action` remains `get_status` / `get_policy`

3. Command interoperability from `SY.admin`
   - run `opa_check`
   - run `opa_compile`
   - run `opa_apply`
   - run `opa_rollback`
   - confirm replies preserve `command_response` / `CONFIG_RESPONSE` semantics expected by admin

4. Config control-plane interoperability
   - send `CONFIG_GET`
   - send `CONFIG_SET` with a valid OPA control payload
   - send `CONFIG_SET` with an invalid payload
   - confirm `CONFIG_RESPONSE` shape stays stable

5. Router / SHM semantics
   - confirm current/staged/backup policy files still behave the same
   - confirm router SHM status is still visible in `get_status` / `CONFIG_GET`
   - confirm `OPA_RELOAD` still broadcasts after apply/rollback paths that should trigger it

6. Failure behavior
   - send malformed `compile_policy`
   - send malformed `CONFIG_SET`
   - confirm error replies still carry the expected `error_code` / `error_detail`
