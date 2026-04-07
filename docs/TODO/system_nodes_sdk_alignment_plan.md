# Legacy System Nodes SDK Alignment Plan

**Status:** planning / pre-implementation  
**Date:** 2026-04-07  
**Scope:** `SY.config.routes`, `SY.opa.rules`, and the minimal Go SDK needed to make `SY.opa.rules` a first-class node  
**Reference nodes for alignment:** `SY.admin`, `SY.orchestrator`, `SY.identity`, `SY.cognition`, AI nodes using current SDK helpers

**Intent:**
- Bring the legacy system nodes to the same operational level as the current Fluxbee runtime contract.
- Align message envelopes, config control-plane behavior, request/reply semantics, and node lifecycle expectations.
- Do this as a dedicated infrastructure effort, not as a side effect of `SY.policy`.

---

## 1) Why this exists

`SY.config.routes` and `SY.opa.rules` are not at the same maturity level as the current system nodes, but they do not need the same treatment.

This creates three problems:

- They do not expose the same message and control-plane conventions as the newer nodes.
- `SY.admin` and `SY.orchestrator` have to compensate for legacy behavior instead of relying on a stable node contract.
- Any policy or observability work done on top of them becomes fragile because the base message/config model is inconsistent.

The key difference is:

- `SY.config.routes` is a Rust node that should be brought up to the current Rust SDK/runtime standard.
- `SY.opa.rules` is effectively carrying its own mini SDK today; the cleaner path is to turn that into a real `fluxbee-go-sdk` and then migrate `SY.opa.rules` onto it.

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

---

## 4) Work split

This should be treated as an infrastructure modernization stream with two tracks.

### Track A — `SY.config.routes`

Bring the existing Rust node up to current SDK and config conventions.

### Track B — `fluxbee-go-sdk` + `SY.opa.rules`

Build the minimum Go SDK needed to expose the same external runtime contract as the Rust SDK, then migrate `SY.opa.rules` to use it.

For now, the target is:

- no ad hoc compatibility layer embedded only in `SY.opa.rules`
- no forced Rust rewrite
- a real SDK path for Go-based nodes, with `SY.opa.rules` as first consumer

---

## 5) Detailed backlog

### SYS-ALIGN-S0 — Freeze the alignment goal

- [ ] SYS-ALIGN-S0-T1. Confirm that this effort is independent from `SY.policy`.
- [ ] SYS-ALIGN-S0-T2. Confirm that no partial "policy-only" metadata retrofits should be done first.
- [ ] SYS-ALIGN-S0-T3. Define the canonical reference contract for a modern Fluxbee system node:
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
- [ ] SYS-ALIGN-S1-T2. Produce a full protocol inventory for `SY.opa.rules`:
  - inbound types `command` / `query` / `system`
  - outbound types `command_response` / `query_response` / `system`
  - HELLO/ANNOUNCE handshake behavior
  - OPA-specific actions and responses
- [ ] SYS-ALIGN-S1-T3. Compare `SY.config.routes` against the current Rust SDK node contract.
- [ ] SYS-ALIGN-S1-T3b. Compare `SY.opa.rules` against the current Rust SDK node contract and identify what must be replicated in Go.
- [ ] SYS-ALIGN-S1-T4. Mark which mismatches are:
  - compatibility-critical
  - admin/orchestrator ergonomics
  - observability/policy-relevant
  - cleanup-only

### SYS-ALIGN-S2 — Normalize message envelopes

- [ ] SYS-ALIGN-S2-T1. Define the canonical message categories these nodes should expose.
- [ ] SYS-ALIGN-S2-T2. Standardize reply payload conventions:
  - `status`
  - `error_code`
  - `error_detail`
  - version/config fields where applicable
- [ ] SYS-ALIGN-S2-T3. Standardize metadata fields that must always be carried:
  - `msg_type`
  - `msg`
  - `action`
  - `target`
  - `trace_id`
- [ ] SYS-ALIGN-S2-T4. Remove older ad hoc differences such as mixed `error` vs `error_code` shapes.
- [ ] SYS-ALIGN-S2-T5. Freeze which parts of the Rust message model must be reproduced by `fluxbee-go-sdk`.
- [ ] SYS-ALIGN-S2-T6. Decide whether `SY.opa.rules` keeps custom `command/query` message kinds as first-class Fluxbee categories or is adapted to the same shared runtime model used by Rust nodes.

### SYS-ALIGN-S3 — Normalize config/control-plane behavior

- [ ] SYS-ALIGN-S3-T1. Decide what "live config" means for `SY.config.routes`.
- [ ] SYS-ALIGN-S3-T2. Decide whether `SY.config.routes` should expose canonical `CONFIG_GET` / `CONFIG_SET` through the SDK control-plane helpers.
- [ ] SYS-ALIGN-S3-T3. Decide how its existing domain actions relate to node config:
  - route/vpn mutations as domain admin actions
  - full effective config reads/writes as node control-plane config
- [ ] SYS-ALIGN-S3-T4. Decide what "live config" means for `SY.opa.rules`:
  - staged policy
  - current applied policy
  - router SHM state
  - backup policy
- [ ] SYS-ALIGN-S3-T5. Define whether OPA compile/apply/rollback remain domain actions or become partially expressible through a config contract.
- [ ] SYS-ALIGN-S3-T6. Ensure admin documentation clearly distinguishes:
  - stored config
  - live control-plane config
  - domain actions with side effects

### SYS-ALIGN-S4 — Admin/orchestrator contract cleanup

- [ ] SYS-ALIGN-S4-T1. Make `SY.admin` the single canonical origin of operator/system commands for these nodes.
- [ ] SYS-ALIGN-S4-T2. Ensure both nodes expose explicit, documented action catalogs that `SY.admin` can rely on.
- [ ] SYS-ALIGN-S4-T3. Ensure `SY.orchestrator` only depends on stable service lifecycle contracts, not node-specific reply quirks.
- [ ] SYS-ALIGN-S4-T4. Review whether `sy-opa-rules` and `sy-config-routes` should advertise richer capability/contracts during startup.

### SYS-ALIGN-S5 — SDK and implementation work

#### `SY.config.routes`

- [ ] SYS-ALIGN-S5-T1. Upgrade to current SDK conventions used by modern nodes.
- [ ] SYS-ALIGN-S5-T2. Replace manual config control-plane handling where shared SDK helpers already exist.
- [ ] SYS-ALIGN-S5-T3. Normalize admin replies and system replies to the current common shape.
- [ ] SYS-ALIGN-S5-T4. Add tests for request parsing and response envelope consistency.

#### `fluxbee-go-sdk`

- [ ] SYS-ALIGN-S5-T5. Define the minimum v1 scope of `fluxbee-go-sdk`.
- [ ] SYS-ALIGN-S5-T6. Implement shared Go types equivalent to the common node envelope:
  - `Message`
  - `Meta`
  - `Routing`
  - destination helpers
- [ ] SYS-ALIGN-S5-T7. Implement router framing and connection lifecycle helpers:
  - connect
  - HELLO / ANNOUNCE
  - sender / receiver
  - reconnect behavior
- [ ] SYS-ALIGN-S5-T8. Implement standard reply helpers in Go:
  - request/reply
  - error replies
  - trace propagation
- [ ] SYS-ALIGN-S5-T9. Decide whether node config control-plane helpers belong in v1 of the Go SDK or in a later phase.
- [ ] SYS-ALIGN-S5-T10. Add protocol-level tests or fixtures proving parity with the chosen Rust-side contract.

#### `SY.opa.rules`

- [ ] SYS-ALIGN-S5-T11. Migrate `SY.opa.rules` transport/envelope code to `fluxbee-go-sdk`.
- [ ] SYS-ALIGN-S5-T12. Replace its local `Message` / `Meta` / `Routing` structs with SDK equivalents.
- [ ] SYS-ALIGN-S5-T13. Replace its embedded router client lifecycle with SDK connection helpers.
- [ ] SYS-ALIGN-S5-T14. Preserve OPA SHM, compile/apply/rollback, and router reload semantics during the migration.
- [ ] SYS-ALIGN-S5-T15. Re-test admin and orchestrator interoperability after the SDK migration.

### SYS-ALIGN-S6 — Documentation and operator model

- [ ] SYS-ALIGN-S6-T1. Document the canonical action surface for `SY.config.routes`.
- [ ] SYS-ALIGN-S6-T2. Document the canonical action surface for `SY.opa.rules`.
- [ ] SYS-ALIGN-S6-T2b. Document `fluxbee-go-sdk` as the canonical Go path for Fluxbee-compatible nodes.
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

---

## 7) Recommended order

Recommended implementation order:

1. `SYS-ALIGN-S1` protocol audit
2. `SYS-ALIGN-S2` envelope normalization decisions
3. `SYS-ALIGN-S3` config/control-plane decisions
4. `SYS-ALIGN-S5` implementation
5. `SYS-ALIGN-S6` documentation updates

**Pragmatic recommendation:**
- Do `SY.config.routes` first because it is already a Rust node and the gap is mostly contract/SDK drift.
- For `SY.opa.rules`, do not build a one-off compatibility layer inside the node.
- Start by freezing the minimum `fluxbee-go-sdk` contract, then migrate `SY.opa.rules` onto that SDK.
