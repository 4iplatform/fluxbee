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
- [ ] Corregir routing multi-hive de acciones de nodos/routers:
  - Hoy `/hives/{hive}/nodes|routers` envia destino `SY.orchestrator@{hive}`.
  - En el modelo actual solo existe orchestrator en motherbee.
  - Debe enviar a orchestrator local y pasar `target` en payload.
- [ ] Corregir contrato de payload para `kill_node`:
  - Hoy HTTP envia `{"name": ...}`.
  - Orchestrator espera `node_name` o `unit`.
- [ ] Corregir contrato de payload para `kill_router`:
  - Hoy HTTP envia `{"name": ...}`.
  - Orchestrator espera `service` (default `rt-gateway`).

## Pendiente alto (consistencia API)
- [ ] Definir y aplicar version monotona para `CONFIG_CHANGED` en routes/vpns/storage.
- [ ] Unificar formato de respuesta HTTP y codigos (hoy muchos errores salen como 500 generico).
- [ ] Revisar coexistencia de rutas legacy (`/nodes?hive=...`) vs rutas nuevas (`/hives/{id}/nodes`) y dejar una estrategia canonica.

## Pendiente medio
- [ ] Revalidar `add_hive` desde API con matriz de errores esperados de spec (`HIVE_EXISTS`, `INVALID_ADDRESS`, `SSH_*`, `WAN_TIMEOUT`).
- [ ] Agregar pruebas de integracion end-to-end para:
  - [ ] `/hives/{id}/nodes` (run/kill)
  - [ ] `/hives/{id}/routers` (run/kill)
  - [ ] `/config/storage` (broadcast + confirmacion)

## Seguimiento
- [ ] Registrar mapeo final de endpoints por ownership:
  - `SY.config.routes`
  - `SY.orchestrator`
  - `SY.opa.rules`
