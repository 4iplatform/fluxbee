# SY.admin Internal Command Gateway Contract (draft)

Status: draft
Date: 2026-03-14
Scope: comando interno por socket/WAN hacia `SY.admin`, manteniendo a `SY.admin` como único gateway de control
Relacionados: `docs/02-protocolo.md`, `docs/07-operaciones.md`, `docs/onworking/orchestrator_frictions.md`

---

## 1) Decisión de diseño (hard rule)

`SY.admin` sigue siendo el responsable central de ejecutar comandos de control, tanto para:

1. Entrada externa: HTTP.
2. Entrada interna: mensaje socket/WAN.

Regla: los productores internos **no** invocan `SY.orchestrator` directamente como camino productivo.  
`SY.orchestrator` permanece como backend executor para operaciones del hive.

---

## 2) Problema que resuelve

Hoy el plano interno ya existe para `SYSTEM_*` y acciones de nodo, pero el path canónico de producto está orientado a HTTP en `SY.admin`.

Necesitamos habilitar invocación "desde adentro" sin perder:

- centralización de validaciones/normalización,
- control de autorización por origen/acción,
- envelope de respuesta consistente,
- auditoría unificada.

---

## 3) Modelo canónico

### 3.1 Flujo externo (sin cambios)

Client externo -> HTTP `SY.admin` -> dispatch interno -> `SY.orchestrator@<hive>` (u otros SY) -> response.

### 3.2 Flujo interno (nuevo)

Nodo interno -> `ADMIN_COMMAND` a `SY.admin@motherbee` -> mismo dispatch interno que HTTP -> executor -> `ADMIN_COMMAND_RESPONSE`.

### 3.3 Invariante

Para un mismo `action` y payload equivalente, el resultado funcional debe ser el mismo por HTTP o por socket interno (salvo metadatos de transporte).

---

## 4) Contrato de mensaje interno (v1)

### 4.1 Request

- `routing.dst`: `Unicast("SY.admin@motherbee")`
- `meta.msg_type`: `"admin"`
- `meta.msg`: `"ADMIN_COMMAND"`
- `meta.target`: `"SY.admin@motherbee"`
- `payload`:
  - `action` (string, requerido): acción canónica admin (`run_node`, `kill_node`, `update`, etc.)
  - `hive_id` (string, opcional): hive target cuando aplica
  - `params` (object, opcional): parámetros de la acción (mismo shape canónico que HTTP interno)
  - `request_id` (string, opcional): idempotency key lado caller

Ejemplo:

```json
{
  "routing": {
    "src": "WF.orchestrator.agent@motherbee",
    "dst": "SY.admin@motherbee",
    "ttl": 16,
    "trace_id": "5a7f2f4f-bf34-4b15-bde6-7f7fe7345f3d"
  },
  "meta": {
    "msg_type": "admin",
    "msg": "ADMIN_COMMAND",
    "target": "SY.admin@motherbee"
  },
  "payload": {
    "action": "run_node",
    "hive_id": "worker-220",
    "params": {
      "node_name": "WF.batch.sync",
      "runtime": "wf.batch.sync.diag",
      "runtime_version": "current",
      "tenant_id": "tnt:..."
    },
    "request_id": "req-20260314-001"
  }
}
```

### 4.2 Response

- `routing.dst`: source original del request
- `meta.msg_type`: `"admin"`
- `meta.msg`: `"ADMIN_COMMAND_RESPONSE"`
- `payload`:
  - `status`: `ok | sync_pending | error`
  - `action`: acción ejecutada
  - `payload`: resultado de dominio
  - `error_code`: nullable
  - `error_detail`: nullable
  - `request_id`: echo opcional

Ejemplo:

```json
{
  "meta": {
    "msg_type": "admin",
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

### 4.3 Correlación

- Correlación primaria: `routing.trace_id`.
- Correlación funcional opcional: `payload.request_id`.

---

## 5) Catálogo de acciones (v1)

Acciones priorizadas para paridad con control-plane actual:

- `update`
- `sync_hint`
- `run_node`
- `kill_node`
- `get_node_status`
- `get_node_config`
- `set_node_config`
- `get_node_state`
- `get_versions`
- `list_nodes`
- `add_hive`
- `remove_hive`

Nota:
- El catálogo debe mapear al dispatch canónico ya usado por HTTP en `SY.admin`.
- No se habilitan nombres legacy (`name`, `version`, etc.) en el contrato interno.

---

## 6) Autorización y seguridad

### 6.1 Regla default

`deny-by-default` para `ADMIN_COMMAND`.

### 6.2 ACL por origen y acción

Agregar política explícita en `SY.admin`:

- allowlist de orígenes L2 permitidos (`SY.*`, `WF.*`, `AI.*`, `IO.*` según necesidad).
- matriz por acción (ejemplo: un nodo puede `get_node_status` pero no `remove_hive`).

Respuesta en rechazo:

- `status=error`
- `error_code=FORBIDDEN`
- `error_detail` con motivo mínimo auditable.

### 6.3 No bypass de gateway

En producción, el camino recomendado es:

- interno -> `SY.admin` -> executor.

El envío directo a `SY.orchestrator` queda restringido a tooling controlado/E2E.

---

## 7) Reglas de implementación en SY.admin

1. Reusar `handle_admin_command`/`handle_admin_query` para evitar forks semánticos.
2. Aplicar la misma normalización de payload que HTTP.
3. Mantener el mismo mapeo de `error_code`.
4. Registrar auditoría estructurada por request:
   - `trace_id`, `request_id`, `source_name`, `action`, `hive_id`, `status`, `latency_ms`.
5. Mantener timeouts por acción (misma política que HTTP, ajustable por env).

---

## 8) Compatibilidad y rollout

### 8.1 Compatibilidad

- HTTP no cambia.
- Nuevo canal interno se introduce como adicional.
- Sin aliases legacy en contrato interno.

### 8.2 Fases de rollout (propuesta)

- [ ] FR9-T1. Definir schema del mensaje `ADMIN_COMMAND`/`ADMIN_COMMAND_RESPONSE` en protocolo.
- [ ] FR9-T2. Implementar receiver/dispatcher de `ADMIN_COMMAND` en `SY.admin`.
- [ ] FR9-T3. Conectar dispatcher interno a handlers canónicos existentes (`handle_admin_*`).
- [ ] FR9-T4. Implementar ACL por origen+acción en `SY.admin`.
- [ ] FR9-T5. Rechazo explícito `FORBIDDEN` para origen/acción no permitidos.
- [ ] FR9-T6. Estándar de auditoría (`trace_id`, actor, acción, resultado, latencia).
- [ ] FR9-T7. SDK helper para callers internos (`admin_command_request()` con timeout y validación de respuesta).
- [ ] FR9-T8. E2E positivo: `WF.* -> SY.admin -> SY.orchestrator -> success` (`run_node/kill_node`).
- [ ] FR9-T9. E2E negativo: origen no autorizado -> `FORBIDDEN`.
- [ ] FR9-T10. E2E de paridad: misma acción por HTTP vs interno produce mismo resultado de dominio.
- [ ] FR9-T11. Documentar operación y troubleshooting en `07-operaciones.md`.
- [ ] FR9-T12. Actualizar `orchestrator_frictions.md` con estado FR-09.

---

## 9) No objetivos (v1)

- Reemplazar HTTP.
- Permitir bypass productivo a `SY.orchestrator`.
- Introducir semántica nueva de negocio en callers internos.

---

## 10) Criterio de cierre

Se considera cerrado cuando:

1. Existe canal interno estable a `SY.admin` con ACL y auditoría.
2. No hay divergencia semántica entre path HTTP y path interno para acciones equivalentes.
3. E2E de autorización y paridad en verde.
4. Docs canónicas (`02-protocolo`, `07-operaciones`, frictions) actualizadas.

