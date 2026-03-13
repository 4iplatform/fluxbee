# Node Spawn Configuration — Technical Note

**Status:** v1.1
**Date:** 2026-03-13
**Audience:** Developers implementing orchestrator, admin API, and node runtimes (AI, WF, IO)
**Parent spec:** `SY.orchestrator — Spec de Cambios v2`, `07-operaciones.md`

---

## 1. Problem

When orchestrator spawns a business node (AI, WF, IO), the node needs configuration data to operate: API keys, prompts, model settings, channel config, workflow definitions, etc. Without this data the node cannot function.

---

## 2. Design Decisions

1. **Config arrives via admin API.** The architect sends `POST /hives/{hive}/nodes` with runtime config payload.
2. **Two files per node instance, one writer per file:**
   - `config.json` (writer: orchestrator)
   - `state.json` (writer: node)
3. **Fixed canonical base path:** `/var/lib/fluxbee/nodes`.
4. **Spawn is fail-closed on existing config:** if `config.json` already exists, spawn returns `NODE_ALREADY_EXISTS`.
5. **Post-spawn config updates are orchestrator-driven:** `PUT .../config` updates `config.json` atomically, then orchestrator sends `CONFIG_CHANGED`.
6. **Node state is read-only for control plane:** `GET .../state` reads `state.json`; if missing, returns `payload.state = null`.
7. **System nodes (`SY.*`, `RT.*`) are out of scope.** This model applies to business nodes (`AI`, `WF`, `IO`).

---

## 3. Filesystem Layout

```
/var/lib/fluxbee/nodes/
├── AI/
│   └── AI.frontdesk@motherbee/
│       ├── config.json   # written by orchestrator
│       └── state.json    # written by node
├── WF/
│   └── WF.onboarding@worker-220/
│       ├── config.json
│       └── state.json
└── IO/
    └── IO.whatsapp@worker-220/
        ├── config.json
        └── state.json
```

**Rules:**
- First level: node kind (`AI`, `WF`, `IO`).
- Second level: full L2 name including `@hive`.
- Files: `config.json` (orchestrator-owned), `state.json` (node-owned).
- Directory and file creation is deterministic and fixed under `/var/lib/fluxbee/nodes`.

Path resolution:

```rust
fn node_dir(node_name: &str) -> PathBuf {
    let node_kind = node_name.split('.').next().unwrap();
    PathBuf::from("/var/lib/fluxbee/nodes")
        .join(node_kind)
        .join(node_name)
}

fn config_path(node_name: &str) -> PathBuf {
    node_dir(node_name).join("config.json")
}

fn state_path(node_name: &str) -> PathBuf {
    node_dir(node_name).join("state.json")
}
```

---

## 4. Ownership Contract

| File | Writer | Reader | Created when | Purpose |
|------|--------|--------|-------------|---------|
| `config.json` | SY.orchestrator | Node | Spawn | Runtime config + `_system` metadata |
| `state.json` | Node | Orchestrator (diagnostic read-only) | First node write | Runtime internal state |

**Invariant:** orchestrator never writes `state.json`; node never writes `config.json`.

---

## 5. Admin API Contract

### 5.1 Spawn with Config

`POST /hives/{hive}/nodes`

```json
{
  "node_name": "AI.frontdesk.gov",
  "runtime": "ai.frontdesk.gov",
  "runtime_version": "current",
  "config": {
    "tenant_id": "tnt:...",
    "model": "gpt-4",
    "system_prompt": "..."
  }
}
```

Behavior:
- orchestrator resolves node L2,
- resolves identity ILK,
- writes `/var/lib/fluxbee/nodes/<KIND>/<node@hive>/config.json`,
- fails if `config.json` already exists.

### 5.2 Read Config

`GET /hives/{hive}/nodes/{node_name}/config`

- Returns current `config.json`.

### 5.3 Update Config

`PUT /hives/{hive}/nodes/{node_name}/config`

- orchestrator reads existing `config.json`, merges patch, writes atomically, increments config version metadata, and sends `CONFIG_CHANGED` to node.

### 5.4 Read State (diagnostic)

`GET /hives/{hive}/nodes/{node_name}/state`

- If `state.json` exists: return parsed JSON in `payload.state`.
- If not exists: return `payload.state = null`.

---

## 6. Spawn Flow

1. Admin request arrives with runtime + optional config.
2. Orchestrator validates runtime/version and target hive.
3. Orchestrator resolves/creates ILK in identity (primary route).
4. Orchestrator builds `config.json` with user config + `_system` block.
5. Orchestrator writes `config.json` atomically under `/var/lib/fluxbee/nodes/...`.
6. Orchestrator starts runtime (`start.sh`).
7. Node reads `config.json` and starts operation; node creates/updates `state.json` as needed.

---

## 7. File Permissions

- Directories (`/var/lib/fluxbee/nodes`, kind dirs, node dirs): `0700`.
- Files (`config.json`, `state.json` when created by node): `0600`.

---

## 8. Error Codes

| Code | Condition |
|------|-----------|
| `NODE_ALREADY_EXISTS` | `config.json` already exists for node |
| `CONFIG_WRITE_FAILED` | orchestrator could not persist `config.json` |
| `NODE_CONFIG_NOT_FOUND` | GET/PUT config on node without `config.json` |
| `NODE_STATE_READ_FAILED` | invalid/unreadable `state.json` |
| `IDENTITY_REGISTER_FAILED` | orchestrator could not complete identity register/update |

---

## 9. Security Notes (pilot)

- Secrets in `config.json` are plaintext for now.
- Restrict access by filesystem permissions and host hardening.
- Future work: secret-manager references and encrypted-at-rest handling.

---

## 10. Current Implementation Status

Implemented in core:
- `SY.orchestrator` two-file layout + atomic writes + strict spawn behavior.
- `SY.admin` endpoints:
  - `GET /hives/{hive}/nodes/{node}/config`
  - `PUT /hives/{hive}/nodes/{node}/config`
  - `GET /hives/{hive}/nodes/{node}/state`
- Relay behavior supported for remote hive target.

Pending:
- dedicated E2E coverage for ownership edge-cases (`config exists` respawn fail, state endpoint local/remoto with explicit assertions).
