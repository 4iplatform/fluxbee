# SY.orchestrator - Estado actual vs spec (v1.16+)

Checklist operativo para cerrar SY.orchestrator segun:
- `docs/onworking/CHANGELOG-v1.15-to-v1.16.md`
- `docs/07-operaciones.md`
- `docs/02-protocolo.md` (seccion 7.8)

## Cerrado en esta iteracion
- [x] Bootstrap local motherbee completo (router + SY.* + conexion de `SY.orchestrator`).
- [x] Bootstrap de SY nodes corregido: no espera `SY.storage` en SHM del router (no hace HELLO al router).
- [x] Timeout de bootstrap de SY nodes ahora reporta el detalle de nodos faltantes.
- [x] Watchdog de servicios criticos + shutdown ordenado.
- [x] `add_hive` robusto (sin pasos criticos ignorados) con clasificacion de errores SSH inicial.
- [x] `add_hive` fail-fast cuando `wan.authorized_hives` bloquea el `hive_id` (`WAN_NOT_AUTHORIZED`), evitando timeout opaco de WAN.
- [x] `remove_hive` con cleanup remoto: `disable/stop/kill/reset-failed` de servicios worker antes de borrar metadata local.
- [x] `run_node` funcional (local/remoto) con:
  - [x] validacion de `runtime/version` contra `runtime-manifest.json`.
  - [x] verificacion de presencia del runtime en target.
  - [x] sync remoto por `rsync` bajo demanda antes de ejecutar `start.sh`.
- [x] `kill_node`, `run_router`, `kill_router` funcionales (local/remoto).
- [x] Soporte de mensajes `system`:
  - [x] `RUNTIME_UPDATE`
  - [x] `SPAWN_NODE`
  - [x] `KILL_NODE`
- [x] Respuesta formal a mensajes `SPAWN_NODE`/`KILL_NODE` (`*_RESPONSE`).
- [x] Persistencia de runtime manifest local:
  - [x] `/var/lib/fluxbee/orchestrator/runtime-manifest.json`
  - [x] espejo en `/var/lib/fluxbee/runtimes/manifest.json`
- [x] Verificacion periodica de runtimes (cada 5 min) con deteccion de drift por hash remoto y sync a workers.
- [x] Creacion de directorios base de runtimes/orchestrator durante bootstrap local.

## Pendiente de cierre fino
- [x] Validacion explicita de readiness de NATS en bootstrap (ademas de socket+SHM y SY nodes).
- [ ] Validacion explicita de readiness profunda de `sy-storage` (hoy valida servicio activo; falta validar DB conectada al inicio).
- [ ] Tests de integracion de `RUNTIME_UPDATE` + `SPAWN_NODE` remoto con worker real.
- [ ] Homogeneizar documentacion vieja de bootstrap (`root` vs `administrator`, ejemplos legacy y paths `/json-router`).

## Notas de compatibilidad
- API admin actual (`run_node`, `kill_node`, `run_router`, `kill_router`, `add_hive`) se mantiene compatible.
- Cambios compilados contra `sy_orchestrator` y `sy_admin`.

## Nota operativa (logs)
- Para evitar ruido de runs viejos en `journalctl`, filtrar por ventana temporal:
  - `journalctl -u sy-orchestrator --since "YYYY-MM-DD HH:MM:SS" --no-pager`
