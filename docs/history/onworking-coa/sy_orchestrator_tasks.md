# SY.orchestrator - Estado actual vs spec (v1.16+)

> Nota de direccion (2026-03-02): para la nueva arquitectura piloto, el backlog activo paso a `docs/onworking/sy_orchestrator_v2_tasks.md` y toma como fuente de verdad `docs/onworking/SY.orchestrator — Spec de Cambios v2.md`. Este archivo queda como historico de cierre v1.16.
> Nota de alcance (2026-03-08): cualquier referencia a `RUNTIME_UPDATE`, paths fuera de `dist/` o flows legacy en este documento debe interpretarse como histórico, no como contrato operativo vigente.

Checklist operativo para cerrar SY.orchestrator segun:
- `docs/onworking/CHANGELOG-v1.15-to-v1.16.md`
- `docs/07-operaciones.md`
- `docs/02-protocolo.md` (seccion 7.8)

## Cerrado en esta iteracion
- [x] Bootstrap local motherbee completo (router + SY.* + conexion de `SY.orchestrator`).
- [x] Bootstrap de SY nodes corregido para no depender de supuestos viejos sobre `SY.storage`; el servicio hoy hace `HELLO` al router y además mantiene su camino NATS para metrics/DB.
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
- [x] Dist sync authority corregida:
  - `fluxbee-dist` deja de reconciliarse como bidireccional genérico
  - motherbee usa `sendonly`
  - workers usan `receiveonly`
  - esto alinea Syncthing con el axioma operativo: motherbee publica software y los workers solo reciben
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
- [x] Tests de integracion de `RUNTIME_UPDATE` + `SPAWN_NODE` remoto con worker real (script: `scripts/orchestrator_system_update_spawn_e2e.sh`, helper: `orch_system_diag`; envio ajustado a `dst` por nombre L2 y manejo explicito de `UNREACHABLE/TTL_EXCEEDED`).
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

## TODO - Runtime materialization + `sync_pending` global (core)

Contexto del problema:
- `publish` local de un runtime nuevo materializa carpeta y `bin/start.sh`.
- `SYSTEM_UPDATE category=runtime` puede devolver `sync_pending` por faltantes de runtimes no relacionados.
- `spawn` posterior termina en `RUNTIME_NOT_PRESENT`.
- En algunos casos se observa desaparición/poda del runtime recién publicado durante la ventana de no convergencia.

Lectura técnica original del problema:
- `apply_system_update_local(..., request category=runtime)` mezclaba:
  - retención de artifacts,
  - validación de presencia,
  - y decisión de convergencia.
- `verify_runtime_current_artifacts(...)` validaba el manifest runtime global, no solo el runtime/version objetivo.
- `runtime_verify_and_retain` podía operar con manifest cacheado en memoria.
- Esto deja acoplados tres problemas:
  - readiness global,
  - retención destructiva,
  - y deploy puntual de runtime.

Objetivo:
- permitir deploy y validación confiable de un runtime puntual sin bloquearlo por faltantes ajenos,
- evitar poda/desmaterialización prematura,
- y hacer observable el estado real de materialización por `runtime/version`.

### Fase R1 - Triage reproducible y evidencia
- [ ] R1-T1. Capturar correlación temporal completa para un caso reproducible:
  - fin de `publish`,
  - primer `SYSTEM_UPDATE sync_pending`,
  - primer `missing directory`,
  - `spawn_failed` con `RUNTIME_NOT_PRESENT`.
- [ ] R1-T2. Guardar payloads completos de `SYSTEM_UPDATE` en varios retries, incluyendo `payload.errors[]`.
- [ ] R1-T3. Guardar snapshot de `/var/lib/fluxbee/dist/runtimes/manifest.json`:
  - antes de `publish`,
  - después de `publish`,
  - después del primer `sync_pending`.
- [ ] R1-T4. Verificar en paralelo existencia de:
  - `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/bin/start.sh`
  - durante los retries de `SYSTEM_UPDATE`.
- [ ] R1-T5. Confirmar si el manifest en memoria de `SY.orchestrator` queda stale respecto del manifest en disco tras `publish`.

### Fase R2 - Corrección de fuente de verdad del manifest
- [ ] R2-T1. Revisar todos los caminos que leen `state.runtime_manifest` y clasificar cuáles pueden usar cache y cuáles deben recargar desde disco.
- [x] R2-T2. Cambiar `runtime_verify_and_retain` para no usar manifest stale como fuente preferida.
- [ ] R2-T3. Definir estrategia explícita de invalidación/refresh del cache runtime manifest cuando `publish` o `SYSTEM_UPDATE` cambian el estado local.
- [x] R2-T4. Agregar logs de diagnóstico cuando se detecte divergencia entre manifest en memoria y manifest en disco.

### Fase R3 - Separar readiness puntual vs consistencia global
- [x] R3-T1. Introducir validación por alcance para `SYSTEM_UPDATE category=runtime`:
  - `runtime`
  - `runtime_version`
  - opcionalmente `target_hives`
- [x] R3-T2. Hacer que el update puntual no falle ni quede en `sync_pending` por faltantes de `AI.*` / `IO.*` / `WF.*` ajenos al runtime solicitado.
- [x] R3-T3. Mantener una validación global aparte para salud general del sistema, sin usarla como bloqueo duro del deploy puntual.
- [x] R3-T4. Documentar la semántica nueva:
  - `targeted readiness`
  - vs `global runtime health`

### Fase R4 - Retención segura y no destructiva
- [x] R4-T1. Evitar `apply_runtime_retention` destructivo antes de validar convergencia del runtime objetivo.
- [x] R4-T2. Si el sistema está en `sync_pending`, usar modo de retención no destructivo o diferir la poda.
- [x] R4-T3. Garantizar que un runtime recién publicado y materializado no pueda ser podado durante la misma ventana en la que todavía se está verificando.
- [x] R4-T4. Agregar evidencia explícita en logs cuando un runtime/version quede retenido por protección de convergencia.

### Fase R5 - Materialization state explícito
- [x] R5-T1. Exponer estado de materialización local por `runtime/version`:
  - directorio presente,
  - `start.sh` presente,
  - hash/manifest esperado,
  - timestamp de última verificación.
- [x] R5-T2. Hacer que `spawn` pueda consultar ese estado previo y reportar mejor causa raíz cuando el runtime no está materializado.
- [x] R5-T3. Evaluar endpoint/admin payload para consultar materialización sin depender de inferencia indirecta desde `SYSTEM_UPDATE`.

### Fase R6 - E2E y criterio de cierre
- [x] R6-T1. Caso E2E positivo:
  - `publish` runtime nuevo,
  - `SYSTEM_UPDATE` puntual,
  - `spawn`,
  - ejecución correcta.
  - Script agregado: `scripts/orchestrator_runtime_targeted_e2e.sh`
  - Alcance actual: validación en hive local (`motherbee` / hive con `SY.orchestrator` presente).
  - Validado en `motherbee` el `2026-03-30` con resultado final `status=ok`.
- [ ] R6-T2. Caso E2E negativo:
  - faltantes globales en otros runtimes,
  - deploy puntual igual debe converger si el artifact objetivo está materializado.
- [ ] R6-T3. Caso E2E de no-regresión:
  - watchdog/retry/retention no deben borrar el runtime recién publicado durante la ventana de verificación.
- [ ] R6-T4. Cerrar workaround operativo actual (`reusar 0.1.0 y pisar binario`) una vez que el flujo puntual quede estable.

Nota operativa:
- En esta iteración se cubre solo `R6-T1`.
- `R6-T2` a `R6-T4` quedan explícitamente para validación posterior de dev en worker real.
- El harness `scripts/orchestrator_runtime_targeted_e2e.sh` está orientado a hive local; no intenta cubrir worker hives sin `SY.orchestrator` remoto.

### Criterio de salida de este TODO
- [ ] Un runtime nuevo puede publicarse, verificarse y spawnearse sin quedar bloqueado por faltantes globales no relacionados.
- [ ] `SY.orchestrator` no poda artifacts de runtime durante estados no convergentes.
- [ ] Existe una fuente de verdad coherente para manifest runtime entre disco y memoria.
- [ ] El estado de materialización por `runtime/version` es visible y utilizable operativamente.

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

Implementación ejecutada para decisiones C1/C3:
- [x] Remover fallback de instalación por package manager en orchestrator (local/remoto) para vendor.
- [x] Migrar `add_hive`/bootstrap de copia core desde `/usr/bin/*` a `/var/lib/fluxbee/core/bin/*`.
- [x] Completar validación por manifest core durante bootstrap/add_hive.

### 1) Contrato de versionado de runtimes (hardening)
- [x] Definir `schema_version` del `runtime-manifest.json` y politica de compatibilidad.
- [x] Exigir monotonicidad de `payload.version` en `RUNTIME_UPDATE` (rechazo explicito de updates stale).
- [x] Formalizar `error_code` de versionado (`VERSION_MISMATCH` / `MANIFEST_INVALID`) para respuestas deterministas.
- [x] Documentar politica de rollback de runtime (`current` anterior) y criterio de activacion.

### 2) Rollout de runtimes por worker (robustez operativa)
- [x] Registrar resultado por worker en cada sync (`ok/error`, motivo, duracion, hash final).
- [x] Agregar modo canary (subset de workers) antes de rollout global (`RUNTIME_UPDATE.payload.target_hives`).
- [x] Definir y aplicar politica de retencion de versiones en `/var/lib/fluxbee/runtimes` (cleanup seguro).
- [x] Agregar retry acotado para verificacion post-sync por worker (hash remoto == hash local con reintentos acotados).

### 3) Versionado de binarios core
- [x] Definir manifest de componentes core (servicio, version, hash, build_id).
- [x] Diseñar flujo de promocion motherbee -> workers para binarios core (staging + verificacion + switch atomico).
- [x] Definir orden de restart por dependencia (`rt-gateway`/`sy-*`) con health-gate entre pasos.
- [x] Implementar rollback de core por componente ante falla de health-check.

### 4) Versionado de vendor (Syncthing y futuros)
- [x] Definir/validar `vendor-manifest.json` (version monotona, hash, size, upstream_version).
- [x] Implementar propagacion vendor desde repo master (`/var/lib/fluxbee/vendor`) a workers (sin package manager remoto).
- [x] Implementar rollback vendor por componente (la verificacion de drift/hash ya esta activa en worker).
- [x] Alinear unit/service de vendor para usar ruta instalada por orchestrator (sin depender de `/usr/bin` del host).

### 5) API/observabilidad de versiones
- [x] Exponer endpoint admin para version efectiva por hive (runtimes + core + vendor).
- [x] Persistir historial de despliegues (deployment_id, actor, target_hives, resultado).
- [x] Agregar alertas de drift versionado (runtime/core/vendor) entre motherbee y workers.

### 6) Validacion E2E de versionado/distribucion
- [x] Script E2E: `RUNTIME_UPDATE` canary -> global -> verificacion -> rollback. (`scripts/orchestrator_runtime_rollout_e2e.sh`)
- [x] Caso negativo E2E: update stale rechazado con `error_code` explicito (sin timeout opaco). (`scripts/orchestrator_system_update_stale_e2e.sh`)
- [x] Caso E2E de drift remoto: deteccion + auto-resync + evidencia en API/logs. (`scripts/orchestrator_drift_runtime_e2e.sh`)
- [x] Script E2E de vendor: drift de binario + reconciliacion + health check Syncthing. (`scripts/orchestrator_drift_vendor_e2e.sh`)
- [x] Script E2E de vendor rollback: falla inducida + rollback aplicado + evidencia en `/deployments`. (`scripts/orchestrator_vendor_rollback_e2e.sh`)

### Criterio de salida de este TODO
- [x] Se puede desplegar version nueva de runtime, core y vendor en worker real con:
  - rollout controlado (canary/global),
  - trazabilidad completa por hive,
  - rollback verificable en caso de falla.

Nota de cierre:
- [x] E2E `scripts/orchestrator_vendor_rollback_e2e.sh` estabilizado para escenario degradado (runtime_update-only + timeouts extendidos), con evidencia de `rollback applied` y convergencia final.

## TODO - Endurecimiento SSH remoto (sin password en operación)

Objetivo:
- Eliminar dependencia de password remoto en `SY.orchestrator` sin perder capacidad de recuperación operativa.
- Migrar a key central de motherbee + `sudo -n` + controles de comando en worker.

Principio operativo acordado:
- **No deshabilitar password al inicio.**
- La desactivación de password queda para la última fase, cuando todo el flujo remoto esté validado en E2E.

### Fase S1 - Preparación sin riesgo
- [x] Definir key única de motherbee en `/var/lib/fluxbee/ssh/motherbee.key` (generada una vez por instalación).
- [x] Mantener compatibilidad transitoria con esquema legacy por-hive (`/var/lib/fluxbee/hives/<id>/ssh.key`) durante migración.
- [x] Documentar permisos mínimos de archivos/paths de key en motherbee.

Permisos mínimos acordados (S1):
- Directorio de key global: `/var/lib/fluxbee/ssh` con `0700`.
- Privada motherbee: `/var/lib/fluxbee/ssh/motherbee.key` con `0600`.
- Pública motherbee: `/var/lib/fluxbee/ssh/motherbee.key.pub` con `0644`.

### Fase S2 - Privilegios remotos sin password (manteniendo password habilitado)
- [x] Reemplazar en orchestrator el uso de `sudo -S` por `sudo -n` para comandos remotos.
- [x] Definir/instalar `sudoers` mínimo en worker (NOPASSWD, allowlist de comandos Fluxbee necesarios).
- [x] Validar por E2E que `add_hive`, sync runtime/core/vendor, restart, rollback y health-check operen solo con `sudo -n`.

### Fase S3 - Key central + authorized_keys restringido
- [x] Cambiar `add_hive` para usar la key de motherbee (sin generar key nueva por worker).
- [x] En worker, registrar pública con restricciones:
  - `from=<ip/cidr motherbee>`
  - `command=/usr/local/bin/fluxbee-ssh-gate.sh`
  - `no-port-forwarding,no-X11-forwarding,no-agent-forwarding,no-pty`
- [x] Instalar `fluxbee-ssh-gate.sh` en worker con allowlist de comandos reales usados por orchestrator (ssh/scp/rsync/systemctl/hash/stat/install/etc.).
- [x] Agregar logging de denegaciones en gate script para auditoría.

Nota operativa S3:
- Política fija en código (sin toggles por entorno):
  - **Modo transicional operativo (default)**: `add_hive` vuelve a ejecutar provisión remota completa para mantener operación.
  - Se omite la restricción `authorized_keys` (`from+command`) y no se fuerza `ssh-gate` en el alta (modo inseguro temporal).
  - La seguridad completa de acceso remoto queda diferida al rediseño con agente por worker.
- Break-glass solo por consola/out-of-band (ver `docs/07-operaciones.md` sección 2.7).

### Fase S4 - Validación integral previa al corte
- [ ] E2E completo en worker real:
  - add/remove hive,
  - runtime rollout canary/global/rollback,
  - core drift + rollback,
  - vendor drift + rollback,
  - reconciliación watchdog.
- [ ] Ejecutar runner unificado S4: `scripts/orchestrator_ssh_hardening_s4_e2e.sh`.
- [ ] Simular fallo parcial y verificar recuperación sin intervención manual.  
  Script: `scripts/orchestrator_partial_failure_recovery_e2e.sh` (incluido en runner S4).
- [x] Confirmar acceso de emergencia documentado (break-glass) antes de corte de password.  
  Documento: `docs/07-operaciones.md` (sección 2.7).

### Fase S5 - Corte final (password off)
- [ ] Deshabilitar `PasswordAuthentication` en workers administrados (solo al cerrar S1..S4).
- [ ] Quitar secretos de password remoto del código/config (`BOOTSTRAP_SSH_PASS` y equivalentes).
- [ ] Eliminar código legacy de bootstrap por password una vez confirmada estabilidad.

### Criterio de salida de este TODO
- [ ] Operación remota de orchestrator 100% por key + `sudo -n`, sin password en runtime.
- [ ] E2E verde en entorno real post-corte.
- [ ] Auditoría básica: comandos remotos permitidos explícitamente y denegaciones logueadas.
