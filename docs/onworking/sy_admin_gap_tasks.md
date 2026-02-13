# SY.admin - Tareas pendientes vs spec

Lista de tareas para alinear `SY.admin` con la especificación actual.

## Endpoints y flujos faltantes
- [ ] Implementar `/hives` (POST/GET/GET {id}/DELETE) + `add_hive` completo (SSH bootstrap, keygen, scp, systemd, espera WAN, repo en `/var/lib/fluxbee/hives`).
- [x] Implementar `GET /hive/status` (estado completo de isla).
- [x] Implementar `GET/PUT /config/storage` con broadcast `CONFIG_CHANGED` subsystem `storage`.
- [x] Implementar API de módulos: `/modules`, `/modules/{name}`, `/modules/{name}/{version}` (list/get/upload).
- [ ] Revisar presencia/ausencia de `SY.orchestrator` en el repo y ajustar `/nodes` y `/routers` si el binario no existe.

## Desalineaciones con la spec
- [x] Mover `opa-version.txt` a `/var/lib/fluxbee/opa-version.txt` (hoy usa `/var/lib/fluxbee/state/opa-version.txt`).
- [x] Alinear paths HTTP con spec (`/hives/{id}/...` en lugar de query params).
- [x] Correlacionar request/response por `trace_id` (hoy se hace por `action`).
- [x] OPA target: tratar `"broadcast"` como broadcast real y unicast a isla solo si `target` es un ID válido.
- [x] `send_opa_query` debería usar unicast cuando hay `target` específico.
- [x] Ajustar timeout de OPA (spec indica 30s en ejemplos; hoy 10s).

## Validaciones / consistencia
- [ ] Validar `hive_id` y `address` en `add_hive` con códigos de error de spec.
- [ ] Asegurar que `SY.admin` emite `CONFIG_CHANGED` con `version` monotónica (especialmente storage/opa).
- [ ] Confirmar que todas las respuestas HTTP tengan formato común (status + payload/errores).

## Seguimiento
- [ ] Registrar en el plan global qué endpoints van a `SY.config.routes` vs `SY.orchestrator` vs `SY.opa.rules`.
