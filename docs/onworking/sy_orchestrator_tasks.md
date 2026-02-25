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
- [x] Hardening de origen para mensajes `system` sensibles:
  - [x] Validación de origen permitido (`routing.src` UUID -> nombre L2 via SHM del router) para `RUNTIME_UPDATE`/`SPAWN_NODE`/`KILL_NODE`.
  - [x] Allowlist configurable por `ORCH_SYSTEM_ALLOWED_ORIGINS` (default: `SY.admin,WF.orch.diag`, expandido a `@<hive>`).
  - [x] Recomendación para producción: `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin` (reservar `WF.orch.diag` para E2E).
  - [x] Respuesta explícita `FORBIDDEN` para `SPAWN_NODE_RESPONSE`/`KILL_NODE_RESPONSE` cuando origen no autorizado.
- [x] Respuesta formal a mensajes `SPAWN_NODE`/`KILL_NODE` (`*_RESPONSE`).
- [x] Persistencia de runtime manifest local:
  - [x] `/var/lib/fluxbee/orchestrator/runtime-manifest.json`
  - [x] espejo en `/var/lib/fluxbee/runtimes/manifest.json`
- [x] Verificacion periodica de runtimes (cada 5 min) con deteccion de drift por hash remoto y sync a workers.
- [x] Creacion de directorios base de runtimes/orchestrator durante bootstrap local.

## Pendiente de cierre fino
- [x] Validacion explicita de readiness de NATS en bootstrap (ademas de socket+SHM y SY nodes).
- [x] Validacion explicita de readiness profunda de `sy-storage` (bootstrap ahora exige respuesta `status=ok` de `storage.metrics.get` via NATS, validando camino `SY.orchestrator -> NATS -> SY.storage -> DB`).
- [x] Tests de integracion de `RUNTIME_UPDATE` + `SPAWN_NODE` remoto con worker real (script: `scripts/orchestrator_runtime_update_spawn_e2e.sh`, helper: `orch_system_diag`; envio ajustado a `dst` por nombre L2 y manejo explicito de `UNREACHABLE/TTL_EXCEEDED`).
- [x] Cierre operativo del E2E `RUNTIME_UPDATE + SPAWN_NODE + KILL_NODE` en worker real:
  - [x] Contrato payload alineado en `orch_system_diag` (`target`/`unit` en lugar de `hive`/`name`).
  - [x] Sync de runtimes endurecido en `sy_orchestrator` (staging remoto en `/tmp` + promocion con `sudo`), evitando fallas de permisos en `/var/lib/fluxbee/runtimes`.
  - [x] Ejecucion validada end-to-end (2026-02-21) con respuesta `status=ok` en `SPAWN_NODE_RESPONSE` y `KILL_NODE_RESPONSE`.
- [x] Homogeneizar documentacion vieja de bootstrap (`root` vs `administrator`, ejemplos legacy y paths `/json-router`).
- [x] Resolver inconsistencia OPA/router que bloqueaba E2E (`Destination::Resolve` + contrato `dst` + parseo OPA):
  - `dst` por nombre L2 documentado y soportado en router (FIB directo).
  - resolver OPA ajustado a `opa_json_dump` prioritario (fallback `opa_value_dump`).
  - hardening de origen en `sy_orchestrator` implementado y documentado en `docs/onworking/sy_router_tasks.md`.
- [x] Cierre completo de LSA router/WAN (estado, seguridad, secuencia y UUID remoto):
  - Verificado y cerrado en `docs/onworking/sy_router_tasks.md` (P0..P3 completos).

## Notas de compatibilidad
- API admin actual (`run_node`, `kill_node`, `run_router`, `kill_router`, `add_hive`) se mantiene compatible.
- Cambios compilados contra `sy_orchestrator` y `sy_admin`.

## Nota operativa (logs)
- Para evitar ruido de runs viejos en `journalctl`, filtrar por ventana temporal:
  - `journalctl -u sy-orchestrator --since "YYYY-MM-DD HH:MM:SS" --no-pager`

## TODO - Versionado y propagacion de software (infra)

Objetivo:
- compatibilizar e implementar el modelo de `docs/onworking/software-distribution-spec.md` para tres categorias:
  - **runtimes** de nodos (base ya implementada en 7.8),
  - **core** de plataforma (`rt-gateway`, `sy-*`),
  - **vendor** de terceros vendorizados (actualmente Syncthing).

Referencia de diseno:
- `docs/onworking/software-distribution-spec.md`

Modo de trabajo acordado:
- `doc-first`: no tocar código de distribución hasta cerrar decisiones de colisiones en documentación.

### 0) Colisiones abiertas spec vs implementacion actual (resolver primero)
- [x] C1 (decisión): Vendor sin internet; fuente única en repo local (`/var/lib/fluxbee/vendor/*`).
- [x] C2 (decisión): Contrato canónico `hive.yaml` para blob sync = `blob.sync.enabled/tool/api_port/data_dir`.
- [x] C3 (decisión): Origen de distribución core = `/var/lib/fluxbee/core/bin/*` (no `/usr/bin/*`).
- [x] C4 (decisión): Flags vendor quedan hardcodeadas en orchestrator por ahora.

Implementación pendiente de decisiones C1/C3:
- [x] Remover fallback de instalación por package manager en orchestrator (local/remoto) para vendor.
- [x] Migrar `add_hive`/bootstrap de copia core desde `/usr/bin/*` a `/var/lib/fluxbee/core/bin/*`.
- [x] Completar validación por manifest core durante bootstrap/add_hive.

### 1) Contrato de versionado de runtimes (hardening)
- [ ] Definir `schema_version` del `runtime-manifest.json` y politica de compatibilidad.
- [ ] Exigir monotonicidad de `payload.version` en `RUNTIME_UPDATE` (rechazo explicito de updates stale).
- [ ] Formalizar `error_code` de versionado (`VERSION_MISMATCH` / `MANIFEST_INVALID`) para respuestas deterministas.
- [ ] Documentar politica de rollback de runtime (`current` anterior) y criterio de activacion.

### 2) Rollout de runtimes por worker (robustez operativa)
- [ ] Registrar resultado por worker en cada sync (`ok/error`, motivo, duracion, hash final).
- [ ] Agregar modo canary (subset de workers) antes de rollout global.
- [ ] Definir y aplicar politica de retencion de versiones en `/var/lib/fluxbee/runtimes` (cleanup seguro).
- [ ] Agregar verificacion post-sync obligatoria por worker (hash remoto == hash local) con retry acotado.

### 3) Versionado de binarios core
- [x] Definir manifest de componentes core (servicio, version, hash, build_id).
- [x] Diseñar flujo de promocion motherbee -> workers para binarios core (staging + verificacion + switch atomico).
- [ ] Definir orden de restart por dependencia (`rt-gateway`/`sy-*`) con health-gate entre pasos.
- [ ] Implementar rollback de core por componente ante falla de health-check.

### 4) Versionado de vendor (Syncthing y futuros)
- [ ] Definir/validar `vendor-manifest.json` (version monotona, hash, size, upstream_version).
- [ ] Implementar propagacion vendor desde repo master (`/var/lib/fluxbee/vendor`) a workers (sin package manager remoto).
- [ ] Implementar rollback vendor por componente y verificacion de drift por hash en worker.
- [ ] Alinear unit/service de vendor para usar ruta instalada por orchestrator (sin depender de `/usr/bin` del host).

### 5) API/observabilidad de versiones
- [ ] Exponer endpoint admin para version efectiva por hive (runtimes + core).
- [ ] Persistir historial de despliegues (deployment_id, actor, target_hives, resultado).
- [ ] Agregar alertas de drift versionado (manifest o binarios core) entre motherbee y workers.

### 6) Validacion E2E de versionado/distribucion
- [ ] Script E2E: `RUNTIME_UPDATE` canary -> global -> verificacion -> rollback.
- [ ] Caso negativo E2E: update stale rechazado con `error_code` explicito (sin timeout opaco).
- [ ] Caso E2E de drift remoto: deteccion + auto-resync + evidencia en API/logs.
- [ ] Script E2E de vendor: drift de binario + reconciliacion + health check Syncthing.

### Criterio de salida de este TODO
- [ ] Se puede desplegar version nueva de runtime, core y vendor en worker real con:
  - rollout controlado (canary/global),
  - trazabilidad completa por hive,
  - rollback verificable en caso de falla.
