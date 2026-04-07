# Legacy System Nodes SDK Alignment Plan

**Status:** planning / pre-implementation  
**Date:** 2026-04-07  
**Scope:** `SY.config.routes` and `SY.opa.rules`  
**Reference nodes for alignment:** `SY.admin`, `SY.orchestrator`, `SY.identity`, `SY.cognition`, AI nodes using current SDK helpers

**Intent:**
- Bring the legacy system nodes to the same operational level as the current Fluxbee runtime contract.
- Align message envelopes, config control-plane behavior, request/reply semantics, and node lifecycle expectations.
- Do this as a dedicated infrastructure effort, not as a side effect of `SY.policy`.

---

## 1) Why this exists

`SY.config.routes` and `SY.opa.rules` are not at the same maturity level as the current system nodes.

This creates three problems:

- They do not expose the same message and control-plane conventions as the newer nodes.
- `SY.admin` and `SY.orchestrator` have to compensate for legacy behavior instead of relying on a stable node contract.
- Any policy or observability work done on top of them becomes fragile because the base message/config model is inconsistent.

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

That makes them hard to reason about as first-class Fluxbee nodes.

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

### Track B — `SY.opa.rules`

Decide whether to:

- keep it in Go but formally align its external contract to the Fluxbee runtime model
- or migrate it toward shared helpers / a Rust implementation later

For now, the minimum requirement is contract alignment, not language migration.

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
- [ ] SYS-ALIGN-S1-T3. Compare both inventories against the current SDK-driven node contract.
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
- [ ] SYS-ALIGN-S2-T5. Decide whether `SY.opa.rules` keeps custom `command/query` message kinds or is adapted to the current shared runtime model.

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

#### `SY.opa.rules`

- [ ] SYS-ALIGN-S5-T5. Decide if the Go service remains standalone long-term or is only a compatibility bridge.
- [ ] SYS-ALIGN-S5-T6. If it remains in Go, define and implement a formal compatibility layer to the Fluxbee node contract.
- [ ] SYS-ALIGN-S5-T7. If it migrates later, prepare a staged migration plan that preserves OPA SHM behavior and operational semantics.
- [ ] SYS-ALIGN-S5-T8. Add protocol-level tests or fixtures proving parity with the chosen contract.

### SYS-ALIGN-S6 — Documentation and operator model

- [ ] SYS-ALIGN-S6-T1. Document the canonical action surface for `SY.config.routes`.
- [ ] SYS-ALIGN-S6-T2. Document the canonical action surface for `SY.opa.rules`.
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
- This does **not** require rewriting `SY.opa.rules` in Rust immediately.
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
- Do `SY.opa.rules` second because it likely needs an explicit compatibility decision before implementation work.
