# SY.admin - Tareas pendientes vs spec (estado real)

Lista de tareas para alinear `SY.admin` con la especificacion actual y con el modelo motherbee/worker.

## Cerrado
- [x] Endpoints REST de hives implementados:
  - [x] `POST /hives`
  - [x] `GET /hives`
  - [x] `GET /hives/{id}`
  - [x] `DELETE /hives/{id}`
- [x] `GET /hive/status`.
- [x] `GET/PUT /config/storage` con `CONFIG_CHANGED` (`subsystem=storage`).
- [x] API de modulos: `/modules`, `/modules/{name}`, `/modules/{name}/{version}`.
- [x] Correlacion request/response por `trace_id` para admin y OPA.
- [x] OPA target broadcast/unicast alineado y timeout de OPA en 30s.

## Pendiente critico (impacta pruebas)
- [x] Corregir routing multi-hive de acciones de nodos/routers:
  - `/hives/{hive}/nodes|routers` ahora enruta a orchestrator local (`SY.orchestrator@motherbee`).
  - Se propaga `target` en payload para que orchestrator ejecute sobre hive remota.
- [x] Corregir listado multi-hive de nodos/routers:
  - `GET /hives/{hive}/nodes` y `GET /hives/{hive}/routers` ahora devuelven vista del hive target (no snapshot local de motherbee).
- [x] Corregir contrato de payload para `kill_node`:
  - HTTP mantiene `{"name": ...}` por compatibilidad.
  - `SY.admin` normaliza a `node_name` antes de enviar a orchestrator.
- [x] Corregir contrato de payload para `kill_router`:
  - HTTP mantiene `{"name": ...}` por compatibilidad.
  - `SY.admin` normaliza a `service` antes de enviar a orchestrator.

## Pendiente alto (consistencia API)
- [x] Definir y aplicar version monotona para `CONFIG_CHANGED` en routes/vpns/storage.
  - `routes` y `vpns` comparten stream monotono (`routes-vpns`) para evitar conflictos en `SY.config.routes`.
  - `storage` usa stream monotono separado.
  - Persistencia local en `state/config_versions/*.txt` (sobre `json_router::paths::state_dir()`).
  - Si se envia `version` manual <= actual, responde `409 VERSION_MISMATCH`.
- [x] Unificar formato de respuesta HTTP y codigos:
  - `SY.admin` ahora mapea `error_code -> HTTP status` (400/404/409/422/501/502/503/504).
  - respuestas de admin/OPA incluyen envelope consistente con `status`, `action`, `payload`, `error_code`, `error_detail`.
- [x] Eliminar coexistencia de rutas legacy para nodos/routers y dejar estrategia canónica:
  - removidos handlers legacy `/nodes` y `/routers` en `SY.admin`.
  - canónico único: `/hives/{id}/nodes` y `/hives/{id}/routers`.

## Pendiente medio
- [x] Revalidar contrato `add_hive` desde API con matriz de errores esperados de spec.
  - mapeo HTTP en `SY.admin` ajustado para `SSH_*`, `INVALID_HIVE_ID`, `MISSING_WAN_LISTEN`, `COPY_FAILED`, `CONFIG_FAILED`.
  - smoke E2E agregado: `scripts/admin_add_hive_matrix.sh`.
  - checklist actualizado con cobertura manual de `WAN_TIMEOUT`.
- [x] Agregar pruebas de integracion end-to-end para:
  - [x] `/hives/{id}/nodes` (run/kill)
  - [x] `/hives/{id}/routers` (run/kill)
  - [x] `/config/storage` (broadcast + confirmacion)
  - Script E2E agregado: `scripts/admin_nodes_routers_storage_e2e.sh`.

## Seguimiento
- [ ] Registrar mapeo final de endpoints por ownership:
  - `SY.config.routes`
  - `SY.orchestrator`
  - `SY.opa.rules`

## Artefactos de validacion
- [x] Checklist manual E2E con `curl` para API admin/orchestrator: `docs/onworking/sy_admin_e2e_curl_checklist.md`.
