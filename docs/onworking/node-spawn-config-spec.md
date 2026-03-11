# Node Spawn Configuration — Technical Note

**Status:** v1.0
**Date:** 2026-03-10
**Audience:** Developers implementing orchestrator, admin API, and node runtimes (AI, WF, IO)
**Parent spec:** `SY.orchestrator — Spec de Cambios v2`, `07-operaciones.md`

---

## 1. Problem

When orchestrator spawns a business node (AI, WF, IO), the node needs configuration data to operate: API keys, prompts, model settings, channel config, workflow definitions, etc. Without this data the node cannot function. The current spawn mechanism (`start.sh`) does not provide a standard way to deliver this configuration.

---

## 2. Design Decisions

1. **Config arrives via admin API.** The architect (human or AI) sends a POST to SY.admin with the full node configuration. This is the single entry point.

2. **Orchestrator creates the config file.** Before executing `start.sh`, orchestrator writes a JSON config file to a standard path. The node reads it at startup.

3. **Single file per node instance.** One file serves as both initial config (written by orchestrator) and retention (updated by the node). Orchestrator creates it once; after that, the node owns it.

4. **If the file already exists, spawn fails.** Orchestrator returns error to admin. This prevents accidental overwrites of a running node's config.

5. **Post-spawn updates via CONFIG_CHANGED.** Changes to node config (new prompt, new model, etc.) are delivered as CONFIG_CHANGED messages. The node receives them, updates its own file.

6. **Secrets in plaintext for now.** API keys are stored in the JSON config file. This is a known security trade-off for the pilot phase. Migration to a secure secret store is deferred.

7. **Config is opaque to orchestrator.** Orchestrator does not validate the content of the config payload — it persists it as-is. Each runtime type defines its own config schema.

8. **System nodes (SY.*, RT.*) are excluded.** They have their own config mechanisms (hive.yaml, dedicated YAML files). This spec applies only to business nodes: AI, WF, IO.

---

## 3. Filesystem Layout

```
/var/lib/fluxbee/nodes/
├── AI/
│   ├── AI.soporte.l1@produccion.json
│   ├── AI.frontdesk@produccion.json
│   └── AI.ventas.closer@produccion.json
├── WF/
│   ├── WF.onboarding@produccion.json
│   └── WF.billing@produccion.json
└── IO/
    ├── IO.whatsapp@produccion.json
    └── IO.slack@produccion.json
```

**Rules:**

- First level: node type prefix (`AI`, `WF`, `IO`).
- File name: full L2 node name including `@hive` suffix, with `.json` extension.
- L2 name is guaranteed unique by orchestrator — no file collision possible.
- Orchestrator creates the type directory if it doesn't exist.
- Base path: `/var/lib/fluxbee/nodes/` (not configurable, consistent with other fluxbee state paths).

**Path resolution by the node:**

```rust
fn config_path(node_name: &str) -> PathBuf {
    let node_type = node_name.split('.').next().unwrap(); // "AI", "WF", "IO"
    PathBuf::from("/var/lib/fluxbee/nodes")
        .join(node_type)
        .join(format!("{}.json", node_name))
}
```

---

## 4. Admin API Contract

### 4.1 Spawn with Config

Extends `POST /hives/{hive}/nodes`:

```json
{
  "node_name": "AI.soporte.l1",
  "runtime": "ai.soporte",
  "runtime_version": "current",
  "config": {
    "api_provider": "openai",
    "api_key": "sk-...",
    "model": "gpt-4",
    "temperature": 0.7,
    "system_prompt": "You are a support agent specialized in billing...",
    "max_tokens": 4096,
    "tenant_id": "tnt:uuid-of-acme"
  }
}
```

**Fields:**

| Field | Required | Description |
|-------|----------|-------------|
| `node_name` | Yes | L2 name without `@hive` (hive suffix added by orchestrator) |
| `runtime` | Yes | Runtime identifier from manifest |
| `runtime_version` | Yes | Version or `"current"` |
| `config` | No | Opaque JSON object, persisted as node config file. If omitted, orchestrator creates a minimal config with only system-generated fields |

### 4.2 Update Config Post-Spawn

`PUT /hives/{hive}/nodes/{node_name}/config`:

```json
{
  "system_prompt": "Updated prompt with new billing procedures...",
  "temperature": 0.5
}
```

SY.admin sends CONFIG_CHANGED to the target node. The node receives it, merges updates into its config file, and applies changes.

---

## 5. Orchestrator Spawn Flow

```
1. SY.admin sends SPAWN_NODE to SY.orchestrator@<hive>
   with payload including `config` object.

2. SY.orchestrator validates:
   a. Runtime exists in manifest.
   b. Runtime version available locally (or sync_pending).
   c. No existing config file at target path (node not already spawned).

3. SY.orchestrator requests ILK from SY.identity:
   a. Check if ILK exists for node_name → reuse.
   b. If not → ILK_REGISTER → get new ILK.

4. SY.orchestrator builds initial config file:
   a. Start with the `config` object from the request.
   b. Inject system-generated fields:
      - `_system.ilk_id`: the ILK assigned by identity.
      - `_system.node_name`: full L2 name with @hive.
      - `_system.hive_id`: hive where the node runs.
      - `_system.runtime`: runtime name.
      - `_system.runtime_version`: resolved version.
      - `_system.created_at`: timestamp.
      - `_system.created_by`: ILK of the requesting actor.

5. SY.orchestrator writes config file:
   - Creates type directory if missing.
   - Writes to `/var/lib/fluxbee/nodes/{TYPE}/{node_name}@{hive}.json`.
   - Atomic write (tmp + rename).

6. SY.orchestrator executes `start.sh` for the runtime.

7. Node starts, reads its config file, begins operation.
```

---

## 6. Config File Structure

The config file has two zones: user-provided config (opaque, runtime-specific) and system-injected metadata (standard, always present).

### 6.1 Example: AI Node

```json
{
  "_system": {
    "ilk_id": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "node_name": "AI.soporte.l1@produccion",
    "hive_id": "produccion",
    "runtime": "ai.soporte",
    "runtime_version": "1.2.0",
    "created_at": "2026-03-10T10:00:00Z",
    "created_by": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7"
  },
  "api_provider": "openai",
  "api_key": "sk-...",
  "model": "gpt-4",
  "temperature": 0.7,
  "system_prompt": "You are a support agent specialized in billing...",
  "max_tokens": 4096
}
```

### 6.2 Example: IO Node

```json
{
  "_system": {
    "ilk_id": "ilk:a1b2c3d4-5678-90ab-cdef-1234567890ab",
    "node_name": "IO.whatsapp@produccion",
    "hive_id": "produccion",
    "runtime": "io.whatsapp",
    "runtime_version": "1.0.0",
    "created_at": "2026-03-10T10:00:00Z",
    "created_by": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7"
  },
  "webhook_url": "https://api.whatsapp.com/...",
  "webhook_secret": "whsec_...",
  "phone_number_id": "123456789",
  "access_token": "EAAx..."
}
```

### 6.3 Example: WF Node

```json
{
  "_system": {
    "ilk_id": "ilk:b2c3d4e5-6789-0abc-def1-234567890abc",
    "node_name": "WF.onboarding@produccion",
    "hive_id": "produccion",
    "runtime": "wf.onboarding",
    "runtime_version": "1.0.0",
    "created_at": "2026-03-10T10:00:00Z",
    "created_by": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7"
  },
  "flow_definition": {
    "steps": ["greeting", "collect_data", "verify", "confirm"],
    "timeout_secs": 300
  },
  "triggers": ["new_customer", "channel_switch"]
}
```

### 6.4 System Fields Contract

The `_system` block is always present and written by orchestrator. Nodes should treat it as read-only.

| Field | Type | Description |
|-------|------|-------------|
| `_system.ilk_id` | string | ILK assigned to this node instance |
| `_system.node_name` | string | Full L2 name with @hive |
| `_system.hive_id` | string | Hive where node runs |
| `_system.runtime` | string | Runtime identifier |
| `_system.runtime_version` | string | Resolved version |
| `_system.created_at` | string | ISO 8601 timestamp of spawn |
| `_system.created_by` | string | ILK of the actor who requested the spawn |

---

## 7. Node Lifecycle with Config

### 7.1 First Start

```
Node starts
  → reads /var/lib/fluxbee/nodes/{TYPE}/{node_name}.json
  → parses _system block (gets ILK, hive, runtime info)
  → parses runtime-specific config (api_key, prompt, etc.)
  → connects to router with node_name from _system
  → begins operation
```

### 7.2 Restart (Same Config)

```
Node restarts (crash, systemd restart, manual)
  → reads same config file (still there, not recreated)
  → resumes operation with existing config
  → ILK is same (persisted in identity, reused by orchestrator)
```

### 7.3 Config Update

```
Architect sends PUT /hives/{hive}/nodes/{node_name}/config
  → SY.admin sends CONFIG_CHANGED (subsystem: node_config, target: node L2 name)
  → Node receives CONFIG_CHANGED
  → Node merges new fields into its config
  → Node writes updated config file (atomic: tmp + rename)
  → Node applies changes (e.g., switch model, update prompt)
```

### 7.4 Node Kill

```
SY.admin sends DELETE /hives/{hive}/nodes/{node_name}
  → Orchestrator sends SIGTERM (or SIGKILL if force)
  → Config file is NOT deleted (retained for potential re-spawn or audit)
  → To fully remove: admin can send a separate cleanup action (future)
```

---

## 8. Error Codes

| Code | Condition |
|------|-----------|
| `NODE_ALREADY_EXISTS` | Config file already exists at target path |
| `RUNTIME_NOT_AVAILABLE` | Runtime not in manifest |
| `RUNTIME_NOT_PRESENT` | Runtime in manifest but `start.sh` missing locally |
| `CONFIG_WRITE_FAILED` | Could not write config file (permissions, disk) |
| `IDENTITY_FAILED` | Could not create or retrieve ILK from SY.identity |

---

## 9. Security Notes (Pilot Phase)

- API keys and secrets are stored in plaintext JSON on disk.
- File permissions should be restricted: `0600`, owned by the fluxbee service user.
- Directory permissions: `0700` for type directories.
- Future iterations should introduce encrypted storage or integration with a secret manager (HashiCorp Vault, systemd credentials, etc.).
- The `config` payload in the admin API travels over the internal network (SY.admin → orchestrator). If the admin API is exposed externally, it must be behind TLS + authentication.

---

## 10. Relationship to Other Specs

| Spec | Relationship |
|------|-------------|
| `SY.orchestrator v2` | Orchestrator implements the spawn flow with config file creation |
| `10-identity-v2.md` | ILK registration happens before config file creation |
| `07-operaciones.md` | Paths and layout align with `/var/lib/fluxbee/` convention |
| `14-runtime-rollout-motherbee.md` | Runtime availability is a prerequisite for spawn |
| `02-protocolo.md` | CONFIG_CHANGED used for post-spawn updates |
