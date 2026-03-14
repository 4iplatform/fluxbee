# Node Status Contract

**Status:** v1.0
**Date:** 2026-03-13
**Audience:** Developers implementing orchestrator, admin API, and node runtimes (AI/WF/IO/SY)
**Parent specs:** `node-spawn-config-spec.md`, `system-inventory-spec.md`
**Resolves:** FR-07 (status/health)

---

## 1. Purpose

Define a canonical status contract for every node in the system. This eliminates ad-hoc status responses and enables consistent monitoring, diagnostics, and automated operation across local and remote hives.

---

## 2. Surfaces

The same `node_status` payload is used in both surfaces — no dual schema.

| Surface | Mechanism | Responder |
|---------|-----------|-----------|
| Admin API (external) | `GET /hives/{hive_id}/nodes/{node_name}/status` | SY.admin → SY.orchestrator |
| Control plane (internal) | `meta.type="admin"`, `meta.action="get_node_status"` | SY.orchestrator (consolidates) |

---

## 3. Reporting Model

SY.orchestrator is always the responder. It consolidates status from two sources:

### 3.1 Orchestrator-Owned Data (Always Available)

Orchestrator knows this even if the node is dead:

- `lifecycle_state` — derived from systemd unit status and SHM/inventory.
- `config` block — derived from config.json on disk.
- `process` block — derived from systemd unit metadata.
- `runtime` block — derived from spawn request and manifest.
- `identity` block — derived from ILK registration.

### 3.2 Node-Reported Data (Available Only When Node is Alive)

If the node is alive and responsive, orchestrator forwards the status request to it. The node responds with:

- `health_state` — only the node knows if it's healthy, degraded, or in error.
- `extensions` — runtime-specific metrics and state.
- `last_error` — last error the node encountered.

Orchestrator uses node-reported health as primary input when available.

### 3.3 Mixed Response Flow

```
Admin API request
  → SY.admin forwards to SY.orchestrator
  → Orchestrator builds base status (lifecycle, config, process, runtime, identity)
  → If lifecycle_state is RUNNING:
      → Orchestrator forwards get_node_status to the node (timeout: 2s)
          ├── Node responds → merge health_state + extensions + last_error (health_source=NODE_REPORTED)
          └── Node timeout  → Orchestrator infers health (see 3.4, health_source=ORCHESTRATOR_INFERRED)
  → If lifecycle_state is STOPPED, FAILED, STARTING:
      → health_state = UNKNOWN, health_source=UNKNOWN (no forward, node cannot respond)
  → Orchestrator increments status_version if state changed, returns consolidated response
```

### 3.4 Health Inference (When Node Does Not Respond)

Health inference is fallback-only.

Precedence:
1. If node responds in time with valid `health_state`, orchestrator uses it (`health_source=NODE_REPORTED`).
2. If node does not respond and `lifecycle_state=RUNNING`, orchestrator infers (`health_source=ORCHESTRATOR_INFERRED`).
3. If there is not enough signal, orchestrator returns `health_state=UNKNOWN`, `health_source=UNKNOWN`.

When fallback inference applies, orchestrator uses:

| Condition | Inferred `health_state` |
|-----------|------------------------|
| `lifecycle_state=RUNNING` + `config.exists=true` + `config.valid=true` + no persisted `last_error` | `HEALTHY` |
| `lifecycle_state=RUNNING` + `config.exists=true` + `config.valid=false` | `ERROR` |
| `lifecycle_state=RUNNING` + `config.exists=false` | `ERROR` |
| `lifecycle_state=RUNNING` + persisted `last_error` exists | `DEGRADED` |
| Cannot determine | `UNKNOWN` |

This keeps status useful during migration when nodes do not implement the handler yet, without overriding node-reported health once available.

---

## 4. Canonical States

### 4.1 `lifecycle_state` (Process State)

Owned by orchestrator. Derived from systemd unit status and inventory.

| Value | Meaning |
|-------|---------|
| `STARTING` | Unit is activating, node not yet connected to router |
| `RUNNING` | Unit active, node visible in SHM/inventory |
| `STOPPING` | Shutdown signal sent, node still in process list |
| `STOPPED` | Clean shutdown (exit code 0) or never started |
| `FAILED` | Abnormal termination (exit code != 0, crash, OOM, signal) |
| `UNKNOWN` | Cannot determine (unit missing, SHM inconsistent) |

### 4.2 `health_state` (Operational Health)

Primary signal from the node; effective value is published by orchestrator (node-reported when available, inferred as fallback). Only meaningful when `lifecycle_state = RUNNING`.

| Value | Meaning |
|-------|---------|
| `HEALTHY` | Operating normally, processing messages |
| `DEGRADED` | Operating with partial functionality (node reports reason) |
| `ERROR` | Node detected a problem that prevents normal operation |
| `UNKNOWN` | Node did not respond to status request, or not running |

`health_state` and `lifecycle_state` are independent. Valid combinations:

| lifecycle | health | Interpretation |
|-----------|--------|----------------|
| `RUNNING` | `HEALTHY` | Normal operation |
| `RUNNING` | `DEGRADED` | Needs attention but functional |
| `RUNNING` | `ERROR` | Node is up but cannot process correctly |
| `RUNNING` | `UNKNOWN` | Node alive but did not respond to status query |
| `STOPPED` | `UNKNOWN` | Not running, health not applicable |
| `FAILED` | `UNKNOWN` | Crashed, health not applicable |
| `STARTING` | `UNKNOWN` | Not ready yet |

---

## 5. Payload (`node_status`)

### 5.1 Required Fields

```json
{
  "schema_version": "1",
  "node_name": "AI.soporte.l1@worker-220",
  "hive_id": "worker-220",
  "observed_at": "2026-03-13T18:00:00Z",
  "lifecycle_state": "RUNNING",
  "health_state": "HEALTHY",
  "health_source": "NODE_REPORTED",
  "status_version": 7
}
```

| Field | Type | Owner | Description |
|-------|------|-------|-------------|
| `schema_version` | string | spec | Contract version, for forward compatibility |
| `node_name` | string | orchestrator | Full L2 name with @hive |
| `hive_id` | string | orchestrator | Hive where node runs |
| `observed_at` | string | orchestrator | ISO 8601 timestamp of this status snapshot |
| `lifecycle_state` | string | orchestrator | Process state (see 4.1) |
| `health_state` | string | orchestrator | Effective operational health (node-reported when available, inferred otherwise) |
| `health_source` | string | orchestrator | `NODE_REPORTED` \| `ORCHESTRATOR_INFERRED` \| `UNKNOWN` |
| `status_version` | u64 | orchestrator | Monotonic counter, increments on any state change |

### 5.2 Optional Blocks

```json
{
  "runtime": {
    "name": "ai.soporte",
    "requested_version": "current",
    "resolved_version": "1.2.0"
  },
  "process": {
    "unit": "fluxbee-node-AI.soporte.l1-worker-220",
    "pid": 12345,
    "exit_code": null,
    "restart_count": 1,
    "started_at": "2026-03-13T08:00:00Z"
  },
  "config": {
    "path": "/var/lib/fluxbee/nodes/AI/AI.soporte.l1@worker-220/config.json",
    "exists": true,
    "valid": true,
    "config_version": 2,
    "updated_at": "2026-03-13T14:00:00Z"
  },
  "state": {
    "path": "/var/lib/fluxbee/nodes/AI/AI.soporte.l1@worker-220/state.json",
    "exists": true
  },
  "identity": {
    "ilk_id": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "tenant_id": "tnt:7c9e6679-7425-40de-944b-e07fc1f90ae7"
  },
  "last_error": {
    "code": "API_KEY_INVALID",
    "message": "OpenAI returned 401 Unauthorized",
    "at": "2026-03-13T17:59:00Z"
  },
  "extensions": {
    "messages_processed": 1247,
    "avg_response_ms": 1200,
    "model": "gpt-4",
    "escalation_count": 43
  }
}
```

**Block ownership:**

| Block | Owner | Source |
|-------|-------|--------|
| `runtime` | orchestrator | Spawn request + manifest |
| `process` | orchestrator | Systemd unit metadata |
| `config` | orchestrator | config.json on disk |
| `state` | orchestrator | state.json existence check |
| `identity` | orchestrator | ILK registration data |
| `last_error` | node | Node-reported, merged on response |
| `extensions` | node | Runtime-specific, merged on response |

**Rules:**

- `extensions` allows runtime-specific data without breaking compatibility.
- Unknown fields must be ignored by consumers (forward-compatible).
- Absence of an optional block does not invalidate the contract.
- `config.exists=false` indicates the node has not been properly spawned (no config.json written by orchestrator).
- `config.valid=false` with `last_error` indicates config exists but could not be applied by the node.

---

## 6. Config State (Separate from Health)

Config state is reported in the `config` block.
`health_state` is ideally node-reported. Orchestrator maps config/error signals to health only in fallback mode (node timeout/no handler), per section 3.4.

| Scenario | `config.exists` | `config.valid` | `lifecycle_state` | `health_state` ownership |
|----------|-----------------|----------------|-------------------|--------------------------|
| Node responds | any | any | any | Node-reported (`health_source=NODE_REPORTED`) |
| Node timeout + RUNNING | true/false | true/false | RUNNING | Orchestrator-inferred (`health_source=ORCHESTRATOR_INFERRED`) |
| Node not runnable (STOPPED/FAILED/STARTING) | any | any | STOPPED/FAILED/STARTING | Orchestrator sets `UNKNOWN` (`health_source=UNKNOWN`) |

**Key principle:** config and health are independent observations.
- Orchestrator observes config on disk and reports `config.exists`/`config.valid`.
- Node reports health when reachable.
- Orchestrator infers health only as fallback.

---

## 7. Status Version

`status_version` is a monotonic counter owned by orchestrator, scoped per node. It increments when any of these change:

- `lifecycle_state`
- `health_state`
- `config.config_version`

Consumers can poll and detect changes by comparing `status_version` without parsing the full payload.

Orchestrator persists `status_version` per node on disk at `/var/lib/fluxbee/nodes/{TYPE}/{node_name}/status_version`. On each increment, orchestrator reads the current value, increments, and writes back. This survives orchestrator restarts and maintains true monotonicity for consumers doing diff/alerting.

---

## 8. Admin API

### 8.1 Single Node Status

`GET /hives/{hive_id}/nodes/{node_name}/status`

```json
{
  "action": "node_status",
  "status": "ok",
  "payload": {
    "node_status": {
      "schema_version": "1",
      "node_name": "AI.soporte.l1@worker-220",
      "hive_id": "worker-220",
      "observed_at": "2026-03-13T18:00:00Z",
      "lifecycle_state": "RUNNING",
      "health_state": "HEALTHY",
      "health_source": "NODE_REPORTED",
      "status_version": 7,
      "runtime": { ... },
      "process": { ... },
      "config": { ... },
      "identity": { ... },
      "extensions": { ... }
    }
  }
}
```

### 8.2 Error Responses

| Code | Condition |
|------|-----------|
| `NODE_NOT_FOUND` | Node name not in inventory |
| `HIVE_NOT_FOUND` | Hive not in topology |
| `STATUS_UNAVAILABLE` | Orchestrator cannot determine status (SHM not readable, etc.) |
| `TIMEOUT` | Node did not respond to health query within timeout |

```json
{
  "action": "node_status",
  "status": "error",
  "error_code": "NODE_NOT_FOUND",
  "error_detail": "node 'WF.demo@worker-220' not found in inventory"
}
```

---

## 9. Internal Messages

### 9.1 Status Request (Orchestrator → Node)

```json
{
  "routing": {
    "src": "SY.orchestrator@worker-220",
    "dst": "AI.soporte.l1@worker-220",
    "ttl": 2,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "admin",
    "action": "get_node_status"
  },
  "payload": {}
}
```

Note: `dst` uses the canonical L2 node name. The router resolves to UUID internally.

### 9.2 Status Response (Node → Orchestrator)

The node responds with its self-reported health and extensions:

```json
{
  "routing": {
    "src": "AI.soporte.l1@worker-220",
    "dst": "SY.orchestrator@worker-220",
    "ttl": 2,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "admin",
    "action": "get_node_status_response"
  },
  "payload": {
    "health_state": "HEALTHY",
    "last_error": null,
    "extensions": {
      "messages_processed": 1247,
      "avg_response_ms": 1200
    }
  }
}
```

The node only reports what it owns: `health_state`, `last_error`, `extensions`. Orchestrator merges this with its own data to build the complete `node_status`.

### 9.3 Timeout Handling

If the node does not respond within 2 seconds, orchestrator uses fallback health inference (section 3.4) and sets `health_source=ORCHESTRATOR_INFERRED`. The consolidated response still includes all orchestrator-owned data (lifecycle, config, process, identity). `extensions` is set to `null` since only the node can provide runtime-specific data.

---

## 10. Node Implementation Guide

### 10.1 Minimum Viable Status (All Nodes)

Every node runtime should handle `get_node_status` and respond with at minimum:

```json
{
  "health_state": "HEALTHY"
}
```

This is the bare minimum. A node without handler will timeout and orchestrator will use fallback inference (section 3.4); `UNKNOWN` is used only when signals are insufficient.

### 10.2 Recommended Status (AI/IO/WF Nodes)

```json
{
  "health_state": "HEALTHY",
  "last_error": null,
  "extensions": {
    "messages_processed": 1247,
    "uptime_secs": 36000
  }
}
```

### 10.3 Degraded Reporting

When a node detects partial functionality (e.g., one of several backends is down, rate limited, using fallback config):

```json
{
  "health_state": "DEGRADED",
  "last_error": {
    "code": "BACKEND_RATE_LIMITED",
    "message": "OpenAI API rate limited, using cached responses",
    "at": "2026-03-13T17:59:00Z"
  },
  "extensions": {
    "fallback_active": true,
    "cached_responses_served": 23
  }
}
```

### 10.4 Error Reporting

When a node cannot operate correctly:

```json
{
  "health_state": "ERROR",
  "last_error": {
    "code": "API_KEY_INVALID",
    "message": "OpenAI returned 401 Unauthorized",
    "at": "2026-03-13T17:59:00Z"
  }
}
```

---

## 11. Rollout

### Phase 1 — Non-breaking

- Add `GET /hives/{hive}/nodes/{node_name}/status` endpoint.
- Add `get_node_status` / `get_node_status_response` message handler in orchestrator.
- Implement health inference for non-responsive nodes.
- Orchestrator responds with consolidated data even if node doesn't implement the handler.
- Existing endpoints unchanged.
- Summary endpoint (`/nodes/status`) deferred to inventory spec extension.

### Phase 2 — Node Adoption

- Runtime SDK (`fluxbee_sdk`) adds `get_node_status` handler with default `HEALTHY` response.
- Node runtimes opt in to richer reporting (extensions, degraded detection).
- Consumers migrate to canonical status endpoint.

### Phase 3 — Consolidation

- Mark ad-hoc status responses as deprecated.
- Update `02-protocolo.md` with `get_node_status` message definition.
- Update `07-operaciones.md` with status endpoints.

---

## 12. Implementation Tasks (FR-07)

- [x] FR7-T1. Implement `node_status` schema in orchestrator (this document).
- [x] FR7-T2. Implement `GET /hives/{hive}/nodes/{node_name}/status` in SY.admin.
- [x] FR7-T3. Implement `get_node_status` forward from orchestrator to node with 2s timeout.
- [x] FR7-T4. Implement health inference (section 3.4) for non-responsive or handler-less nodes.
- [x] FR7-T5. Implement `status_version` persistence in `/var/lib/fluxbee/nodes/{TYPE}/{node_name}/status_version`.
- [x] FR7-T6. Add `get_node_status` default handler to `fluxbee_sdk`.
- [x] FR7-T7. E2E: node RUNNING + HEALTHY → full status response with node-reported health. (`scripts/node_status_fr7_e2e.sh`)
- [x] FR7-T8. E2E: node STOPPED → lifecycle=STOPPED, health=UNKNOWN. (`scripts/node_status_fr7_e2e.sh`)
- [x] FR7-T9. E2E: node RUNNING but unresponsive → lifecycle=RUNNING, health inferred (HEALTHY if config valid, no errors). (`scripts/node_status_fr7_e2e.sh`)
- [x] FR7-T10. E2E: node with invalid config → config.valid=false, node reports health=ERROR. (`scripts/node_status_fr7_e2e.sh`, validated with `t10_mode=full` on `motherbee`)
- [ ] FR7-T11. E2E: FAILED node (crash) → lifecycle=FAILED, health=UNKNOWN. (`scripts/node_status_fr7_t11_t12_e2e.sh`)
- [ ] FR7-T12. E2E: status_version survives orchestrator restart (monotonic across restarts). (`scripts/node_status_fr7_t11_t12_e2e.sh`)

---

## 13. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `node-spawn-config-spec.md` | Config block in status reads from config.json/state.json defined there |
| `system-inventory-spec.md` | Inventory provides node list; status provides per-node health detail |
| `runtime-lifecycle-spec.md` | Runtime readiness is a prerequisite; status reports post-spawn state |
| `10-identity-v2.md` | Identity block in status comes from ILK registration |
| `02-protocolo.md` | Needs update with get_node_status message definition |
| `07-operaciones.md` | Needs update with status API endpoints |
