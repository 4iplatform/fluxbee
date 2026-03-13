# System Inventory — Technical Note

**Status:** v1.0
**Date:** 2026-03-12
**Audience:** Developers implementing router, orchestrator, admin API
**Parent specs:** `03-shm.md`, `05-conectividad.md`, `07-operaciones.md`, `10-identity-v2.md`

---

## 1. Problem

The system needs a global inventory: what hives exist, what nodes are running, their status, their type. Today the information exists but is scattered across multiple SHM regions and not consolidated for consumption by orchestrator or admin API.

Currently:
- `jsr-<router-uuid>`: nodes connected to each local router.
- `jsr-lsa-<hive>`: nodes and hives visible via WAN (remote only).
- SY.orchestrator: knows hives from add_hive state, but not a unified live view.
- SY.admin: serves endpoints like `/hives/{id}/nodes` but requires round-trips to orchestrator.

The goal is to make the full system inventory available locally in SHM on every hive, readable by any process without round-trips.

---

## 2. Design Decisions

1. **No new SHM region.** The inventory is built from data already present in existing regions. No seventh region.

2. **Router is the natural inventory keeper.** The router already reads all local router SHM regions (via IRP peer discovery) and all remote topology (via LSA). It has the complete picture.

3. **Router consolidates local + remote into LSA.** Today LSA only contains remote hives/nodes. We extend it so the gateway also writes a snapshot of the local hive's nodes into `jsr-lsa-<hive>`. This makes LSA the single source for the full system inventory.

4. **Orchestrator reads SHM, not the router.** Orchestrator reads `jsr-lsa-<hive>` directly from SHM (already consolidated local+remote by the gateway self-entry). No message passing, no round-trip to the router process.

5. **Admin queries orchestrator, not the router.** Admin sends inventory requests to SY.orchestrator, which reads SHM and returns the consolidated view. The router is never queried directly — it's a data plane component, not a control plane responder.

6. **Primary identity target is fixed and canonical.** The control-plane hive id is mandatory and fixed to `motherbee`. Therefore all identity writes target `SY.identity@motherbee` (not configurable, no SHM field). Workers must route writes to `@motherbee` and fail explicitly if unreachable.

---

## 3. How the Inventory is Built

### 3.1 Local Nodes

Every router writes its connected nodes to `jsr-<router-uuid>`. On a hive with multiple routers, each router has its own SHM region. The gateway router (the one with `is_gateway=1`) already scans all peer router regions to build LSA for remote hives.

**Extension:** The gateway also writes a "self" entry in `jsr-lsa-<hive>` representing the local hive. This entry contains all locally connected nodes (aggregated from all local router SHM regions), plus local routes and VPNs.

### 3.2 Remote Nodes

Already in `jsr-lsa-<hive>` via the existing LSA exchange between gateways. No change needed.

### 3.3 Consolidated View

After the extension, `jsr-lsa-<hive>` contains:
- One entry for the local hive (self) with all local nodes.
- One entry per remote hive with all remote nodes.
- Status flags per hive (alive, stale, deleted) and per node (active, stale, deleted).

This is the complete system inventory, readable by any local process via SHM.

---

## 4. LSA Extension

### 4.1 Self-Entry in LSA

The gateway writes a `RemoteHiveEntry` for its own hive with a special flag:

```rust
pub const HIVE_FLAG_SELF: u16 = 0x0010;  // This entry represents the local hive
```

The self-entry is updated on the same triggers as outbound LSA: node connect/disconnect, config change, periodic refresh.

### 4.2 Updated LSA Content

After extension, `jsr-lsa-<hive>` on any hive contains:

```
┌────────────────────────────────────────────────────────────────┐
│ LsaHeader                                                       │
├────────────────────────────────────────────────────────────────┤
│ RemoteHiveEntry[0]: local hive (HIVE_FLAG_SELF)                 │
│   - hive_id: "produccion"                                       │
│   - status: alive                                               │
│   - router_id / router_name: local gateway identity             │
│   - nodes[]: all nodes connected to any local router            │
│   - routes[]: local static routes                               │
│   - vpns[]: local VPN table                                     │
├────────────────────────────────────────────────────────────────┤
│ RemoteHiveEntry[1]: "worker-220" (remote, from LSA exchange)    │
│   - hive_id: "worker-220"                                       │
│   - status: alive                                               │
│   - nodes[]: nodes reported by worker-220 gateway               │
│   - routes[]: ...                                               │
│   - vpns[]: ...                                                 │
├────────────────────────────────────────────────────────────────┤
│ RemoteHiveEntry[2]: "worker-330" (remote)                       │
│   - ...                                                         │
└────────────────────────────────────────────────────────────────┘
```

### 4.3 No Structural Changes to RemoteHiveEntry

The existing `RemoteHiveEntry` and `RemoteNodeEntry` structures are sufficient. The only addition is the `HIVE_FLAG_SELF` flag to distinguish the local entry. No layout change, no version bump needed for LSA.

---

## 5. Orchestrator as Inventory Consumer

SY.orchestrator reads the inventory from SHM directly. It does NOT query the router via messages.

### 5.1 Reading the Inventory

```rust
impl Orchestrator {
    fn read_inventory(&self) -> Inventory {
        let mut inventory = Inventory::new();

        // Read LSA region (contains self + all remote hives)
        if let Some(lsa) = self.map_lsa_region() {
            for hive_entry in lsa.hives.iter() {
                if hive_entry.flags & FLAG_ACTIVE == 0 { continue; }

                let is_local = hive_entry.flags & HIVE_FLAG_SELF != 0;
                let hive = HiveInfo {
                    hive_id: hive_entry.hive_id.to_string(),
                    is_local,
                    status: flags_to_status(hive_entry.flags),
                    router_id: hive_entry.router_uuid,
                    router_name: hive_entry.router_name.to_string(),
                    last_seen: hive_entry.last_updated,
                    node_count: hive_entry.node_count,
                };
                inventory.hives.push(hive);

                for node_entry in hive_entry.nodes.iter() {
                    if node_entry.flags & FLAG_ACTIVE == 0 { continue; }
                    let node = NodeInfo {
                        node_name: node_entry.name.to_string(),
                        hive_id: hive_entry.hive_id.to_string(),
                        uuid: node_entry.uuid,
                        status: flags_to_status(node_entry.flags),
                        vpn_id: node_entry.vpn_id,
                        connected_at: node_entry.connected_at,
                        node_type: extract_type(&node_entry.name),
                    };
                    inventory.nodes.push(node);
                }
            }
        }

        inventory
    }
}
```

### 5.2 Inventory Data Structure

```rust
pub struct Inventory {
    pub hives: Vec<HiveInfo>,
    pub nodes: Vec<NodeInfo>,
    pub total_hive_count: u32,
    pub total_node_count: u32,
    pub updated_at: u64,
}

pub struct HiveInfo {
    pub hive_id: String,
    pub is_local: bool,
    pub status: String,           // "alive", "stale", "deleted"
    pub router_id: [u8; 16],
    pub router_name: String,
    pub last_seen: u64,
    pub node_count: u32,
}

pub struct NodeInfo {
    pub node_name: String,        // Full L2 name with @hive
    pub hive_id: String,
    pub uuid: [u8; 16],
    pub status: String,           // "active", "stale", "deleted"
    pub vpn_id: u32,
    pub connected_at: u64,
    pub node_type: String,        // "AI", "WF", "IO", "SY", "RT"
}
```

---

## 6. Admin API — Inventory Endpoints

SY.admin delegates all inventory queries to SY.orchestrator. Admin does NOT read SHM directly.

### 6.1 Global Inventory

`GET /inventory`

SY.admin sends request to SY.orchestrator, which reads SHM and returns:

```json
{
  "status": "ok",
  "payload": {
    "total_hives": 3,
    "total_nodes": 47,
    "updated_at": "2026-03-12T10:00:00Z",
    "hives": [
      {
        "hive_id": "motherbee",
        "is_local": true,
        "status": "alive",
        "router_name": "RT.gateway@motherbee",
        "last_seen": "2026-03-12T10:00:00Z",
        "node_count": 12
      },
      {
        "hive_id": "worker-220",
        "is_local": false,
        "status": "alive",
        "router_name": "RT.gateway@worker-220",
        "last_seen": "2026-03-12T09:59:55Z",
        "node_count": 20
      },
      {
        "hive_id": "worker-330",
        "is_local": false,
        "status": "stale",
        "router_name": "RT.gateway@worker-330",
        "last_seen": "2026-03-12T09:58:00Z",
        "node_count": 15
      }
    ]
  }
}
```

### 6.2 Inventory by Hive

`GET /inventory/{hive_id}`

```json
{
  "status": "ok",
  "payload": {
    "hive_id": "worker-220",
    "status": "alive",
    "router_name": "RT.gateway@worker-220",
    "last_seen": "2026-03-12T09:59:55Z",
    "nodes": [
      {
        "node_name": "AI.soporte.l1@worker-220",
        "node_type": "AI",
        "status": "active",
        "vpn_id": 10,
        "connected_at": "2026-03-12T08:00:00Z"
      },
      {
        "node_name": "IO.whatsapp@worker-220",
        "node_type": "IO",
        "status": "active",
        "vpn_id": 0,
        "connected_at": "2026-03-12T08:00:05Z"
      },
      {
        "node_name": "SY.config.routes@worker-220",
        "node_type": "SY",
        "status": "active",
        "vpn_id": 0,
        "connected_at": "2026-03-12T08:00:01Z"
      }
    ]
  }
}
```

### 6.3 Inventory by Node Type

`GET /inventory?type=AI`

Filters the global inventory by node type prefix. Same response format as global but filtered.

### 6.4 Inventory Summary

`GET /inventory/summary`

Lightweight version for dashboards and monitoring:

```json
{
  "status": "ok",
  "payload": {
    "total_hives": 3,
    "hives_alive": 2,
    "hives_stale": 1,
    "total_nodes": 47,
    "nodes_by_type": {
      "AI": 8,
      "WF": 4,
      "IO": 6,
      "SY": 18,
      "RT": 3
    },
    "nodes_by_status": {
      "active": 45,
      "stale": 2,
      "deleted": 0
    }
  }
}
```

---

## 7. Message Flow (Admin → Orchestrator)

Admin does not read SHM. It sends a system message to orchestrator:

```json
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "SY.orchestrator@<hive>",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "INVENTORY_REQUEST"
  },
  "payload": {
    "scope": "global",
    "filter_hive": null,
    "filter_type": null
  }
}
```

Response from orchestrator:

```json
{
  "meta": {
    "type": "system",
    "msg": "INVENTORY_RESPONSE"
  },
  "payload": {
    "status": "ok",
    "total_hives": 3,
    "total_nodes": 47,
    "hives": [ ... ],
    "nodes": [ ... ]
  }
}
```

Scope options: `global`, `hive`, `summary`.

---

## 8. Implementation Tasks

Implementation status (2026-03-12):
- Fase A: completada en código.
- Fase B: completada en código.
- Fase C: completada en código.
- Fase D: pendiente de validación E2E en entorno integrado.

### Phase A — LSA Self-Entry

- [x] A1. Add `HIVE_FLAG_SELF` constant to LSA flags.
- [x] A2. Gateway writes self-entry in `jsr-lsa-<hive>` with all locally connected nodes (aggregated from all local router SHM regions).
- [x] A3. Self-entry updates on same triggers as outbound LSA (node connect/disconnect, periodic refresh).
- [x] A4. Readers of `jsr-lsa` can distinguish self from remote via `HIVE_FLAG_SELF`.

**Exit criteria:** `jsr-lsa-<hive>` on any hive contains the complete system topology (local + remote).

### Phase B — Orchestrator Inventory Reader

- [x] B1. Implement `read_inventory()` in orchestrator reading `jsr-lsa-<hive>`.
- [x] B2. Implement `INVENTORY_REQUEST` / `INVENTORY_RESPONSE` message handler.
- [x] B3. Support scope: global, hive, summary.
- [x] B4. Support filter by node type.

**Exit criteria:** Orchestrator can return full system inventory from SHM without any remote call.

### Phase C — Admin API Endpoints

- [x] C1. Implement `GET /inventory` (global).
- [x] C2. Implement `GET /inventory/{hive_id}` (by hive).
- [x] C3. Implement `GET /inventory?type=AI` (by type filter).
- [x] C4. Implement `GET /inventory/summary` (lightweight).
- [x] C5. All endpoints delegate to orchestrator via INVENTORY_REQUEST.

**Exit criteria:** Admin API exposes full system inventory. AI architect can query it.

### Phase D — Validation

- [x] D1. E2E: inventory reflects node spawn/kill in real time. (`scripts/inventory_spawn_kill_e2e.sh`)
- [x] D2. E2E: inventory reflects add_hive/remove_hive via API (`/inventory`). (`scripts/inventory_add_remove_hive_e2e.sh`)
  - Cubre explícitamente `DELETE /hives/{id}` -> hive removida (o vista removida) en inventario, y `POST /hives` -> hive presente.
- [ ] D3. E2E: stale hive appears as stale in inventory. (`scripts/inventory_stale_hive_e2e.sh`)
  - Estado: bloqueado por contrato operativo actual. `SY.admin` no expone `/hives/{id}/routers*` y aún falta trigger canónico no-legacy para forzar transición `alive -> stale` en pruebas.
  - Nota: `D3` no valida delete/add; eso ya queda cubierto por `D2`.
- [x] D4. E2E: worker identity writes are routed to `SY.identity@motherbee` (no fallback to local replica). (`scripts/inventory_identity_primary_routing_e2e.sh`)
- [ ] D5. E2E negative: if `SY.identity@motherbee` is unreachable, worker fails identity write routing explicitly (no local fallback). (`scripts/inventory_identity_primary_routing_e2e.sh`)

---

## 9. What This Does NOT Cover

- Node config content is not part of the inventory (see `node-spawn-config-spec.md`).
- Identity metadata (ILK details, capabilities) is not in the inventory — that's in `jsr-identity-<hive>`.
- Runtime/version information per node is not in LSA today. Could be added later if needed.
- Historical inventory (who was running when) is not persisted. This is a live snapshot only.
- Primary hive identification is fixed by rule: control-plane hive id MUST be `motherbee`; identity writes target `SY.identity@motherbee`. Not stored in inventory SHM.

---

## 9.1 Breaking Rule (No Legacy)

- `role: motherbee` implies `hive_id: motherbee` (required).
- Install/bootstrap must create the control-plane hive as `motherbee`.
- Configurations using control-plane `hive_id` values other than `motherbee` are invalid and must be migrated.

---

## 10. Relationship to Existing Specs

| Spec | Change needed |
|------|---------------|
| `03-shm.md` | Document HIVE_FLAG_SELF in LSA flags |
| `05-conectividad.md` | Document self-entry in LSA |
| `07-operaciones.md` | Add inventory endpoints to admin API reference |
| `SY_nodes_spec.md` | Add INVENTORY_REQUEST/RESPONSE to orchestrator message catalog |
