# SY.admin Internal Command Gateway (Unified Spec)

**Status:** v1.2-draft  
**Date:** 2026-03-14  
**Audience:** Developers implementing `SY.admin`, internal callers (`AI/WF/IO`), SDK  
**Parent specs:** `docs/02-protocolo.md`, `docs/07-operaciones.md`

---

## 1. Purpose

Definir un único modelo para que `SY.admin` reciba comandos por:

1. HTTP (externo)
2. socket/WAN (`ADMIN_COMMAND`, interno)

con ejecución equivalente y transparente: mismo resultado funcional, diferente envelope de transporte.

---

## 2. Scope and ownership

### 2.1 In scope

- Gateway de comandos de control en `SY.admin`
- Paridad HTTP/socket
- Routing por `target` + precedencia `node_name@hive`
- Inventory por canal interno
- Cobertura de comandos actuales y futuros con registry + CI guard

### 2.2 Out of scope

- `identity` en `SY.admin`

Ownership de identity:
- `SY.orchestrator`: ILK de nodos (spawn)
- `AI.frontdesk`: ILK de humanos

`SY.admin` no define endpoints/acciones identity en este diseño.

---

## 3. Hard design rules

1. `SY.admin` es el único gateway de control (HTTP + socket).
2. No bypass productivo a `SY.orchestrator`.
3. Canónico de destino interno: `payload.target`.
4. `hive_id` no se usa como selector de destino en comandos internos nuevos.
5. Excepción de `hive_id`: solo acciones de dominio de hive (`add_hive/get_hive/remove_hive`).
6. Precedencia dura: `node_name@hive` manda sobre `target`.
7. Monocommand bloqueante global (HTTP + socket comparten lock).

---

## 4. Message contract

### 4.1 Request (`ADMIN_COMMAND`)

```json
{
  "routing": {
    "src": "WF.architect@motherbee",
    "dst": "SY.admin@motherbee",
    "ttl": 16,
    "trace_id": "5a7f2f4f-bf34-4b15-bde6-7f7fe7345f3d"
  },
  "meta": {
    "type": "admin",
    "msg": "ADMIN_COMMAND",
    "target": "SY.admin@motherbee"
  },
  "payload": {
    "action": "run_node",
    "target": "worker-220",
    "params": {
      "node_name": "WF.batch.sync",
      "runtime": "wf.batch.sync.diag",
      "runtime_version": "current"
    },
    "request_id": "req-20260314-001"
  }
}
```

Campos:
- `action` (required)
- `target` (optional, requerido para acciones hive-scoped salvo excepciones de dominio)
- `params` (optional object)
- `request_id` (optional)

### 4.2 Response (`ADMIN_COMMAND_RESPONSE`)

```json
{
  "meta": {
    "type": "admin",
    "msg": "ADMIN_COMMAND_RESPONSE"
  },
  "payload": {
    "status": "ok",
    "action": "run_node",
    "payload": {
      "node_name": "WF.batch.sync@worker-220",
      "runtime": "wf.batch.sync.diag",
      "version": "0.0.1"
    },
    "error_code": null,
    "error_detail": null,
    "request_id": "req-20260314-001"
  }
}
```

Correlación:
- primaria: `routing.trace_id`
- opcional: `payload.request_id`

---

## 5. Unified execution model

### 5.1 Internal canonical command

```rust
struct AdminCommand {
    action: String,
    target: Option<String>,
    params: serde_json::Value,
    trace_id: String,
    request_id: Option<String>,
    origin: CommandOrigin, // Http | Socket
}

struct AdminCommandResult {
    status: String, // ok | sync_pending | error
    action: String,
    payload: serde_json::Value,
    error_code: Option<String>,
    error_detail: Option<serde_json::Value>,
}
```

### 5.2 Single execution entrypoint

Ambas entradas deben converger en:

`execute_admin_command(ctx, client, cmd) -> AdminCommandResult`

Pipeline:
1. validar action en registry
2. normalizar payload (`target`, no-legacy)
3. aplicar precedencia `node_name@hive > target`
4. adquirir lock monocomando
5. ejecutar handler existente
6. convertir a `AdminCommandResult`

### 5.3 Adapters

- HTTP adapter: `method/path/query/body` -> `AdminCommand` -> `execute_admin_command` -> HTTP response
- Socket adapter: `ADMIN_COMMAND` -> `AdminCommand` -> `execute_admin_command` -> `ADMIN_COMMAND_RESPONSE`

Regla: adapters sin lógica de negocio.

---

## 6. Action registry (current and future)

Se define `action_registry` como fuente única de verdad:

```rust
struct ActionSpec {
    requires_target: bool,
    timeout_kind: TimeoutKind,
    handler_kind: HandlerKind,
}
```

`handler_kind` referencia wrappers a código existente:
- `handle_admin_query`
- `handle_admin_command`
- `handle_hive_update_command`
- `handle_hive_sync_hint_command`
- `handle_inventory_http`
- `handle_opa_http`
- `handle_opa_query`
- modules handler (si se expone por socket)

---

## 7. Action mapping (baseline v1)

### 7.1 Core/hive/node actions

| Endpoint | Action | Base handler |
|---|---|---|
| `GET /hive/status` | `hive_status` | `handle_admin_query` |
| `GET /hives` | `list_hives` | `handle_admin_query` |
| `POST /hives` | `add_hive` | `handle_admin_command` |
| `GET /hives/{id}` | `get_hive` | `handle_admin_command` |
| `DELETE /hives/{id}` | `remove_hive` | `handle_admin_command` |
| `POST /hives/{id}/update` | `update` | `handle_hive_update_command` |
| `POST /hives/{id}/sync-hint` | `sync_hint` | `handle_hive_sync_hint_command` |
| `GET /hives/{id}/versions` | `get_versions` | `handle_admin_query` |
| `GET /hives/{id}/nodes` | `list_nodes` | `handle_admin_query` |
| `POST /hives/{id}/nodes` | `run_node` | `handle_admin_command` |
| `DELETE /hives/{id}/nodes/{name}` | `kill_node` | `handle_admin_command` |
| `GET /hives/{id}/nodes/{name}/status` | `get_node_status` | `handle_admin_command` |
| `GET /hives/{id}/nodes/{name}/config` | `get_node_config` | `handle_admin_command` |
| `PUT /hives/{id}/nodes/{name}/config` | `set_node_config` | `handle_admin_command` |
| `GET /hives/{id}/nodes/{name}/state` | `get_node_state` | `handle_admin_command` |

### 7.2 Inventory

Acción interna: `inventory` (mapea a `handle_inventory_http`)

Params:
- `scope`: `global|hive|summary`
- `filter_hive` (optional)
- `filter_type` (optional)

### 7.3 Routes/VPN/Storage/OPA/Modules

- Routes/VPN CRUD: acciones canónicas existentes
- `PUT /config/routes|vpns|storage`: requieren wrapper que preserve versioning+broadcast
- OPA: acciones explícitas mapeadas a handlers OPA actuales
- Modules: decisión explícita (exponer por socket o dejar solo HTTP)

---

## 8. Routing and precedence details

Para `run_node/kill_node/get_node_*`:

1. si `params.node_name` trae `@hive`, ese hive manda
2. si `target` difiere, se ignora (warning)
3. si no hay `@hive`, `target` es obligatorio
4. si falta destino, `INVALID_REQUEST`

Ejemplo:
- `target=motherbee` + `node_name="WF.x@worker-220"` -> destino real `worker-220`

---

## 9. Concurrency and timeout

- lock monocomando global compartido
- una ejecución activa a la vez
- misma política de timeout por acción que HTTP
- comportamiento bajo contención: espera (no error inmediato)

---

## 10. Implementation tasks (FR9)

- [ ] FR9-T1. Agregar handler `ADMIN_COMMAND` / `ADMIN_COMMAND_RESPONSE` en `SY.admin`.
- [ ] FR9-T2. Implementar `dispatch_internal_admin_command()` como adapter a handlers existentes.
- [ ] FR9-T3. Corregir mapping v1 según este spec (sin routers legacy).
- [ ] FR9-T4. Incluir acción `inventory` en canal interno.
- [ ] FR9-T5. Mantener lock monocomando global HTTP+socket.
- [ ] FR9-T6. Mantener validación no-legacy (`name`/`version` inválidos).
- [ ] FR9-T7. Aplicar precedencia `node_name@hive` en normalización única.
- [ ] FR9-T8. SDK helper `admin_command()` para callers internos.
- [ ] FR9-T9. E2E socket: `run_node/kill_node/get_node_status/update/sync_hint/inventory`.
- [ ] FR9-T10. E2E paridad funcional HTTP vs socket (subset crítico).
- [ ] FR9-T11. Actualizar `docs/02-protocolo.md` con `ADMIN_COMMAND`.
- [ ] FR9-T12. Actualizar `docs/07-operaciones.md` con gateway interno.
- [ ] FR9-T13. Resolver hive destino en socket solo por `payload.target` (excepto acciones de dominio hive).
- [ ] FR9-T14. Rechazar `hive_id` como selector de destino en `ADMIN_COMMAND` hive-scoped.
- [ ] FR9-T15. Mantener `hive_id` solo para `add_hive/get_hive/remove_hive`.
- [ ] FR9-T16. Endurecer nuevas rutas de control para no depender de fallback `hive_id`.
- [ ] FR9-T17. E2E negativo: `run_node` con `hive_id` sin `target` falla.
- [ ] FR9-T18. E2E precedencia: `node_name@hive` gana sobre `target`.
- [ ] FR9-T19. Definir y versionar `action_registry` único.
- [ ] FR9-T20. Mapping completo de `ADMIN_COMMAND` para todas las acciones actuales de `SY.admin`.
- [ ] FR9-T21. Test de paridad de catálogo (HTTP action <-> registry/socket).
- [ ] FR9-T22. Guard CI: endpoint/acción HTTP nueva sin mapping socket => fail.
- [ ] FR9-T23. Exponer `list_admin_actions` para introspección de capacidades.
- [ ] FR9-T24. E2E matrix all-actions (incluye inventory).

---

## 11. Not in scope (v1)

- ACL por origen/acción (defer)
- Idempotencia fuerte por `request_id` (defer)
- Modelo concurrente fino (se mantiene monocomando)
- Reemplazar HTTP

---

## 12. Acceptance criteria

Cerrado cuando:

1. existe `execute_admin_command` único;
2. HTTP y socket pasan por ese ejecutor;
3. precedencia `node_name@hive > target` centralizada;
4. inventory operativo por socket;
5. registry + CI guard + E2E matrix garantizan cobertura de comandos actuales y futuros.

---

## 13. Relationship to other specs

| Spec | Relationship |
|------|-------------|
| `docs/02-protocolo.md` | Definición de `ADMIN_COMMAND`/`ADMIN_COMMAND_RESPONSE` |
| `docs/07-operaciones.md` | Operación del gateway interno |
| `docs/onworking/orchestrator_frictions.md` | Seguimiento FR-09 |
| `docs/onworking/node-spawn-config-spec.md` | Reuso de path de ejecución para `run_node`/`set_node_config` |
| `docs/onworking/runtime-lifecycle-spec.md` | `update`/`sync_hint`/`get_versions` por gateway |
| `docs/onworking/system-inventory-spec.md` | Acción interna `inventory` |

