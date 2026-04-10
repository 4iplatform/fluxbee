# System Node Action Catalogs

**Status:** draft / canonical for current alignment scope  
**Date:** 2026-04-08  
**Scope:** `SY.config.routes`, `SY.opa.rules`  
**Purpose:** provide the explicit node action catalogs that `SY.admin` can rely on as the operator-facing contract for these aligned legacy nodes.

---

## 1) Contract intent

For both nodes in this document:

- `SY.admin` is the canonical operator entrypoint.
- The node-local surface described here is the contract that `SY.admin` targets.
- Direct node invocation may still exist for compatibility or debugging, but it is not the primary operator contract.
- Rich capability/contract discovery is **not** advertised in node startup `HELLO` / `ANNOUNCE` in v1.0; use `SY.admin` catalogs and `CONFIG_GET` instead.

This document does **not** redefine business semantics.
It freezes the action/message surface that is expected to remain stable for the current alignment work.

---

## 2) `SY.config.routes`

Current implementation reference:
- [sy_config_routes.rs](/Users/cagostino/Documents/GitHub/fluxbee/src/bin/sy_config_routes.rs)

### 2.1 Node role

`SY.config.routes` owns:
- effective static route configuration
- effective VPN assignment configuration
- config propagation from `CONFIG_CHANGED`
- live node config exposure through `CONFIG_GET` / `CONFIG_SET`

### 2.2 Canonical operator entrypoint

Operator/system commands for this node must be issued through `SY.admin`.

`SY.admin` actions that target this node:
- `list_routes`
- `add_route`
- `delete_route`
- `list_vpns`
- `add_vpn`
- `delete_vpn`
- `node_control_config_get`
- `node_control_config_set`

### 2.3 Domain admin actions

These are the legacy-but-supported domain actions that remain first-class through `SY.admin`.

#### Read/query actions

- `list_routes`
  - purpose: return the current route rule list for one hive
  - transport category: `admin`
  - expected reply shape:
    - `status`
    - `action`
    - `payload.routes`
    - `payload.config_version`

- `list_vpns`
  - purpose: return the current VPN pattern list for one hive
  - transport category: `admin`
  - expected reply shape:
    - `status`
    - `action`
    - `payload.vpns`
    - `payload.config_version`

#### Mutation actions

- `add_route`
  - purpose: append one route rule
  - transport category: `admin`
  - request body: one `RouteConfig`
  - expected reply shape:
    - `status`
    - `action`
    - `payload.route`
    - `payload.config_version`

- `delete_route`
  - purpose: remove one route rule by `prefix`
  - transport category: `admin`
  - request body:
    - `prefix`
  - expected reply shape:
    - `status`
    - `action`
    - `payload.config_version`

- `add_vpn`
  - purpose: append one VPN rule
  - transport category: `admin`
  - request body: one `VpnConfig`
  - expected reply shape:
    - `status`
    - `action`
    - `payload.vpn`
    - `payload.config_version`

- `delete_vpn`
  - purpose: remove one VPN rule by `pattern`
  - transport category: `admin`
  - request body:
    - `pattern`
  - expected reply shape:
    - `status`
    - `action`
    - `payload.config_version`

#### Canonical admin error fields

Admin replies use the modern envelope:
- `status = error`
- `action`
- `payload = null`
- `error_code`
- `error_detail`

Known error codes:
- `DUPLICATE_PREFIX`
- `DUPLICATE_PATTERN`
- `NOT_FOUND`
- `UNKNOWN_ACTION`

### 2.4 Live control-plane actions

These are the canonical live control-plane actions for the node itself.

- `CONFIG_GET`
  - purpose: return the node-owned live config contract and effective config snapshot
  - transport category: `system`
  - canonical reply: `CONFIG_RESPONSE`

- `CONFIG_SET`
  - purpose: replace selected sections of the effective routes/vpns config
  - transport category: `system`
  - supported apply mode:
    - `replace`
  - canonical reply: `CONFIG_RESPONSE`

- `NODE_STATUS_GET`
  - purpose: return default health state
  - transport category: `system`
  - canonical reply: `NODE_STATUS_GET_RESPONSE`

### 2.5 Config/control-plane interpretation

For `SY.config.routes`, the intended split is:

- domain actions:
  - `list_routes`
  - `add_route`
  - `delete_route`
  - `list_vpns`
  - `add_vpn`
  - `delete_vpn`

- node control-plane:
  - `CONFIG_GET`
  - `CONFIG_SET`
  - `NODE_STATUS_GET`

Interpretation rule:
- use domain actions when the caller wants explicit route/VPN business mutations
- use `CONFIG_GET` / `CONFIG_SET` when the caller wants the node-owned live config contract

---

## 3) `SY.opa.rules`

Current implementation reference:
- [main.go](/Users/cagostino/Documents/GitHub/fluxbee/sy-opa-rules/main.go)
- [fluxbee-go-sdk](/Users/cagostino/Documents/GitHub/fluxbee/fluxbee-go-sdk)

### 3.1 Node role

`SY.opa.rules` owns:
- staged/current/backup OPA policy material
- compile/apply/rollback flows
- policy text/status queries
- OPA reload broadcasts after apply/rollback paths that require it
- live node config exposure through `CONFIG_GET` / `CONFIG_SET`

### 3.2 Canonical operator entrypoint

Operator/system commands for this node must be issued through `SY.admin`.

`SY.admin` actions that target this node:
- `opa_get_policy`
- `opa_get_status`
- `opa_check`
- `opa_compile`
- `opa_apply`
- `opa_rollback`
- `opa_compile_apply`
- `node_control_config_get`
- `node_control_config_set`

### 3.3 OPA domain actions preserved in v1.0

The node keeps its current OPA-facing action surface in v1.0.

#### Query actions

- `get_policy`
  - purpose: read current policy text/material
  - transport category: `query`
  - canonical reply: `query_response`

- `get_status`
  - purpose: read OPA runtime/state summary
  - transport category: `query`
  - canonical reply: `query_response`

#### Command actions

- `compile_policy`
  - purpose: validate/compile staged OPA material
  - transport category: `command`
  - canonical reply: `command_response`

- `apply_policy`
  - purpose: apply a compiled policy version
  - transport category: `command`
  - canonical reply:
    - `command_response`
    - and/or `CONFIG_RESPONSE` where the current OPA/config flow already uses it

- `rollback_policy`
  - purpose: rollback to a previous policy version
  - transport category: `command`
  - canonical reply:
    - `command_response`
    - and/or `CONFIG_RESPONSE` where the current OPA/config flow already uses it

#### System-side broadcast/event

- `OPA_RELOAD`
  - purpose: notify the runtime/router side after apply/rollback paths that require reload
  - transport category: `system`

### 3.4 Live control-plane actions

- `CONFIG_GET`
  - purpose: expose the node-owned live config/control snapshot
  - transport category: `system`
  - canonical reply: `CONFIG_RESPONSE`

- `CONFIG_SET`
  - purpose: apply the new Rust-aligned control-plane subset without breaking the OPA domain surface
  - transport category: `system`
  - canonical reply: `CONFIG_RESPONSE`

- `NODE_STATUS_GET`
  - purpose: expose default health reply where applicable
  - transport category: `system`
  - canonical reply: `NODE_STATUS_GET_RESPONSE`

### 3.5 Config/control-plane interpretation

For `SY.opa.rules`, the intended split is:

- domain actions preserved in v1.0:
  - `get_policy`
  - `get_status`
  - `compile_policy`
  - `apply_policy`
  - `rollback_policy`

- node control-plane:
  - `CONFIG_GET`
  - `CONFIG_SET`
  - `NODE_STATUS_GET`

Interpretation rule:
- keep using the OPA-specific action surface for policy business operations in v1.0
- use `CONFIG_GET` / `CONFIG_SET` only for the new SDK-aligned node control-plane subset
- deeper consolidation of OPA-specific actions is deferred to v2.0

---

## 4) What `SY.admin` may assume

For both nodes, `SY.admin` may rely on:

- canonical operator entrypoint ownership
- stable action names listed in this document
- stable message categories for those actions
- canonical modern admin envelope on Rust-side admin replies
- canonical `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE` semantics for node control-plane

`SY.admin` should **not** assume:
- node-specific lifecycle behavior beyond service availability and documented replies
- that domain actions and control-plane actions are interchangeable
- that OPA-specific domain actions have already been collapsed into the generic node config model

---

## 5) Out of scope here

- redesigning route/VPN domain semantics
- redesigning OPA business semantics
- removing the OPA-specific `query` / `command` categories in v1.0
- defining the full policy/event architecture
