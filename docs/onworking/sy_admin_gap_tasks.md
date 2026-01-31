# SY.admin - Tareas pendientes vs spec

Lista de tareas para alinear `SY.admin` con la especificación actual.

## Endpoints y flujos faltantes
- [ ] Implementar `/islands` (POST/GET/GET {id}/DELETE) + `add_island` completo (SSH bootstrap, keygen, scp, systemd, espera WAN, repo en `/var/lib/json-router/islands`).
- [ ] Implementar `GET /island/status` (estado completo de isla).
- [ ] Implementar `GET/PUT /config/storage` con broadcast `CONFIG_CHANGED` subsystem `storage`.
- [ ] Implementar API de módulos: `/modules`, `/modules/{name}`, `/modules/{name}/{version}` (list/get/upload).
- [ ] Revisar presencia/ausencia de `SY.orchestrator` en el repo y ajustar `/nodes` y `/routers` si el binario no existe.

## Desalineaciones con la spec
- [ ] Mover `opa-version.txt` a `/var/lib/json-router/opa-version.txt` (hoy usa `/var/lib/json-router/state/opa-version.txt`).
- [ ] Alinear paths HTTP con spec (`/islands/{id}/...` en lugar de query params).
- [ ] Correlacionar request/response por `trace_id` (hoy se hace por `action`).
- [ ] OPA target: tratar `"broadcast"` como broadcast real y unicast a isla solo si `target` es un ID válido.
- [ ] `send_opa_query` debería usar unicast cuando hay `target` específico.
- [ ] Ajustar timeout de OPA (spec indica 30s en ejemplos; hoy 10s).

## Validaciones / consistencia
- [ ] Validar `island_id` y `address` en `add_island` con códigos de error de spec.
- [ ] Asegurar que `SY.admin` emite `CONFIG_CHANGED` con `version` monotónica (especialmente storage/opa).
- [ ] Confirmar que todas las respuestas HTTP tengan formato común (status + payload/errores).

## Seguimiento
- [ ] Registrar en el plan global qué endpoints van a `SY.config.routes` vs `SY.orchestrator` vs `SY.opa.rules`.
