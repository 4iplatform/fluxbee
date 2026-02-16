# SY.orchestrator - Estado actual vs spec (v1.16+)

Checklist operativo para cerrar SY.orchestrator según:
- `docs/onworking/CHANGELOG-v1.15-to-v1.16.md`
- `docs/07-operaciones.md`
- `docs/02-protocolo.md` (sección 7.8)

## Cerrado en esta iteración
- [x] Bootstrap local motherbee completo (router + SY.* + conexión de `SY.orchestrator`).
- [x] Watchdog de servicios críticos + shutdown ordenado.
- [x] `add_hive` robusto (sin pasos críticos ignorados) con clasificación de errores SSH inicial.
- [x] `run_node` funcional (local/remoto) con:
  - [x] validación de `runtime/version` contra `runtime-manifest.json`.
  - [x] verificación de presencia del runtime en target.
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
- [x] Verificación periódica de runtimes (cada 5 min) con detección de drift por hash remoto y sync a workers.
- [x] Creación de directorios base de runtimes/orchestrator durante bootstrap local.

## Pendiente de cierre fino (no bloqueante)
- [ ] Validación explícita de readiness de NATS en bootstrap (además de socket+SHM y SY nodes).
- [ ] Tests de integración de `RUNTIME_UPDATE` + `SPAWN_NODE` remoto con worker real.
- [ ] Homogeneizar documentación vieja de bootstrap (`root` vs `administrator`, ejemplos legacy).

## Notas de compatibilidad
- API admin actual (`run_node`, `kill_node`, `run_router`, `kill_router`, `add_hive`) se mantiene compatible.
- Cambios compilados contra `sy_orchestrator` y `sy_admin`.
