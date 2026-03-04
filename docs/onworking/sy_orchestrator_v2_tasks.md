# SY.orchestrator v2 - Plan de Implementacion (Piloto, spec-first)

Fecha: 2026-03-02
Estado: backlog de ejecucion
Fuente de verdad: `docs/onworking/SY.orchestrator — Spec de Cambios v2.md`

## 1. Decisiones de base

- Este proyecto esta en piloto: no se prioriza backward compatibility.
- La spec v2 manda sobre el comportamiento actual.
- Se aceptan cambios breaking en protocolo interno, API y flujo operativo.
- SSH queda solo para provisioning (`add_hive`/`remove_hive`), no para operacion diaria.

## 2. Objetivo tecnico

Cerrar el cambio de arquitectura a:

- orchestrator local en cada hive,
- control-plane por socket L2 (`SY.admin` -> `SY.orchestrator@hive`),
- update unificado por `SYSTEM_UPDATE`,
- `SPAWN_NODE`/`KILL_NODE` genericos para cualquier tipo de nodo.

## 3. Gaps actuales contra v2 (confirmados)

- `sy_orchestrator` sigue ejecutando remoto por SSH en `execute_on_hive` para targets no locales.
- `add_hive` no deja orchestrator worker operativo como agente de control-plane.
- El contrato activo de update es `RUNTIME_UPDATE` (falta `SYSTEM_UPDATE`).
- `sy_admin` no tiene `POST /hives/{id}/update`.
- `SPAWN_NODE`/`KILL_NODE` actuales no siguen aun el contrato v2 centrado en `node_name`.

## 4. Backlog por fases (sin modo legacy)

### Fase 0 - Freeze de direccion v2

- [ ] V0.1 Marcar en docs de trabajo que el modelo remoto-SSH queda deprecado para operacion diaria.
- [ ] V0.2 Alinear `sy_orchestrator_tasks.md` y `sy_admin_tasks.md` con referencia a este plan v2.
- [ ] V0.3 Congelar nuevos cambios fuera de spec v2 para evitar desvio.
- [ ] V0.4 Declarar explicitamente jerarquia documental: `SY.orchestrator — Spec de Cambios v2.md` > `docs/02-protocolo.md` > `docs/07-operaciones.md` mientras dure esta migracion.

Salida:

- equipo ejecutando una sola hoja de ruta.

### Fase 1 - Contrato de protocolo y mensajes

- [ ] V1.1 Formalizar `SYSTEM_UPDATE`/`SYSTEM_UPDATE_RESPONSE` en `docs/02-protocolo.md`.
- [ ] V1.2 Retirar `RUNTIME_UPDATE` del flujo canónico (dejarlo explicitamente obsoleto en docs).
- [ ] V1.3 Formalizar contrato v2 de `SPAWN_NODE`/`KILL_NODE` (campos, errores, semantica `force`).
- [ ] V1.4 Definir codigos de error canonicos para update (`ok/sync_pending/partial/error/rollback`).
- [ ] V1.5 Actualizar `docs/07-operaciones.md` para eliminar flujo remoto por SSH en operacion diaria y reflejar modelo local-only en workers.
- [ ] V1.6 Revisar y corregir tablas de API/ownership en `docs/07-operaciones.md` segun endpoints y mensajes v2.

Salida:

- contratos cerrados para implementar codigo sin ambiguedad.

### Fase 2 - Tareas de SY.admin (obligatorias)

- [ ] V2.1 Implementar endpoint `POST /hives/{id}/update`.
- [ ] V2.2 Validar payload de update (`category`, `manifest_version`, `manifest_hash`).
- [ ] V2.3 Enviar mensaje `SYSTEM_UPDATE` a `SY.orchestrator@{hive}` por canal `system`.
- [ ] V2.4 Ajustar timeouts HTTP/admin para updates remotos (caso `sync_pending`).
- [ ] V2.5 Actualizar handlers de `/hives/{id}/nodes` para contrato v2 (`SPAWN_NODE`/`KILL_NODE`).
- [ ] V2.6 Ajustar serializacion de respuestas de admin al nuevo contrato de system responses.

Salida:

- admin expone todo el control-plane requerido por v2.

### Fase 3 - Orchestrator por rol (motherbee vs worker)

- [x] V3.1 Gatear `add_hive`/`remove_hive` a `role=motherbee`.
- [x] V3.2 Hacer que worker rechace provisioning actions con error explicito.
- [x] V3.3 En `add_hive`, instalar `sy-orchestrator` + unit systemd en worker y habilitar arranque.
- [x] V3.4 Validar readiness del worker por presencia de `SY.orchestrator@worker-*` en L2.
- [x] V3.5 Ajustar bootstrap para que `rt-gateway` sea dependencia de systemd del orchestrator (segun spec v2).

Nota (2026-03-02): `add_hive` ahora espera WAN + presencia de `SY.orchestrator@<worker>` en LSA y devuelve `WORKER_ORCHESTRATOR_TIMEOUT` si no converge. El unit remoto de `sy-orchestrator` se genera con dependencia explícita de `rt-gateway` (`After/Wants/Requires`).

Salida:

- cada worker queda con orchestrator local funcional.

### Fase 4 - Eliminar ejecucion remota SSH en operacion

- [x] V4.1 Reescribir `execute_on_hive` para ejecucion exclusivamente local.
- [x] V4.2 Mover ejecucion remota a unicast de mensajes hacia orchestrator destino.
- [x] V4.3 Actualizar `run_node`/`kill_node`/`run_router`/`kill_router` para modelo local-only en destino.
- [x] V4.4 Quitar rutas de codigo de SSH operativo en flujos de run/kill/update.

Nota (2026-03-02): V4.2/V4.3 quedaron implementadas con forward `system` request/response y deshabilitacion explicita de SSH en `execute_on_hive`. V4.4 desactiva ademas la propagacion SSH de `RUNTIME_UPDATE` y del watchdog de sync remoto; la actualizacion remota pasa por `SYSTEM_UPDATE` (`POST /hives/{id}/update` en `SY.admin`).

Salida:

- operaciones de runtime/router/nodos sin SSH remoto.

### Fase 5 - SYSTEM_UPDATE engine local

- [x] V5.1 Implementar handler `SYSTEM_UPDATE` en orchestrator.
- [x] V5.2 Soportar categorias `runtime`, `core`, `vendor`.
- [x] V5.3 Verificar manifest local (version/hash) y responder `sync_pending` si no converge.
- [x] V5.4 Instalar localmente desde `dist/` con hash-gate previo.
- [x] V5.5 Health gate + rollback local por categoria.
- [x] V5.6 Emitir `SYSTEM_UPDATE_RESPONSE` con detalle de `updated/unchanged/restarted/errors`.

Nota (2026-03-02): `SYSTEM_UPDATE` ahora responde con `hive`, `updated`, `unchanged`, `restarted`, `errors` y soporta `status=rollback` para `category=core` cuando falla el health gate y se revierte instalación local. Queda pendiente cerrar V5.4 de forma explícita sobre layout final `dist/` (Fase 7).

Salida:

- update determinista por hive, disparado por admin, aplicado localmente.

### Fase 6 - SPAWN/KILL generico de nodos

- [x] V6.1 Migrar `SPAWN_NODE` a contrato v2 basado en `node_name`.
- [x] V6.2 Resolver runtime/version segun regla TYPE.campo1 definida por spec.
- [x] V6.3 Ejecutar nodo como unit transient systemd (estrategia unica definida).
- [x] V6.4 Migrar `KILL_NODE` con `force=false/true` (SIGTERM/SIGKILL).
- [x] V6.5 Ajustar listados y estado para incluir nodos AI/IO/WF/SY/RT bajo mismo contrato.

Nota (2026-03-03): `SPAWN_NODE` acepta `node_name` (y mantiene compatibilidad con `name`), deriva `runtime` desde `node_name` cuando no se envía explícito y usa `runtime_version` (alias `version`). `KILL_NODE` acepta `force` y lo mapea a `SIGKILL` (o `SIGTERM` por defecto).
Nota (2026-03-03): `list_nodes` agrega campos uniformes `node_name`, `hive` y `kind` (AI/IO/WF/SY/RT/UNKNOWN) tanto para snapshot local como para nodos remotos por LSA.

Salida:

- spawn/kill uniforme para cualquier runtime de negocio o sistema.

### Fase 7 - Dist por Syncthing + provisioning final

- [x] V7.1 Formalizar layout `/var/lib/fluxbee/dist` en motherbee y worker.
- [x] V7.2 Configurar folder `fluxbee-dist` separado de `fluxbee-blob`.
- [x] V7.3 Integrar `dist` en `hive.yaml` generado por add_hive.
- [x] V7.4 Verificar que add_hive deja worker con dist sincronizando antes de primer update.
- [x] V7.5 Cerrar SSH post-bootstrap segun politica final del equipo (cerrado o restringido).
- [x] V7.6 Cambiar `remove_hive` a estrategia socket-first: pedir cleanup al `SY.orchestrator@worker` por mensaje system y usar SSH solo como fallback tecnico.
- [x] V7.7 Endurecer modo `harden_ssh=true` con verificacion estricta post-bootstrap (password login efectivamente bloqueado y key operativa para canal de mantenimiento).
- [x] V7.8 Normalizar contrato de `remove_hive` para distinguir claramente `remote_cleanup=socket_ok/socket_timeout/ssh_fallback_ok/ssh_fallback_failed/local_only`.

Nota (2026-03-04): `sy_orchestrator` y `install.sh` ya priorizan layout `dist/` para runtime/core/vendor con fallback legacy (`/var/lib/fluxbee/{runtimes,core,vendor}`), `add_hive` genera bloque `dist` en `hive.yaml` worker, la reconciliación de `config.xml` Syncthing asegura folders separados `fluxbee-blob` + `fluxbee-dist` (local/worker, con restart condicional solo si cambia config), `add_hive` ejecuta probe explícito de sincronización dist: por defecto es no estricto (si no converge, continúa con `dist_sync_ready=false`), y en modo estricto (`require_dist_sync=true`) devuelve `DIST_SYNC_TIMEOUT`. Además aplica hardening SSH post-bootstrap en modo restringido por defecto (gate + authorized_keys con `from=` y comando forzado; opt-out explícito con `restrict_ssh=false` o env `FLUXBEE_ADD_HIVE_RESTRICT_SSH=0`).
Nota (2026-03-04): `remove_hive` ahora ejecuta cleanup remoto por socket (`REMOVE_HIVE_CLEANUP`) con timeout corto y fallback SSH best-effort; la respuesta incluye `remote_cleanup_via` para diagnóstico (`socket`/`ssh_fallback`/`local_only`). `harden_ssh=true` incluye verificación estricta post-bootstrap: la key debe seguir operativa con `sudo -n` y el login por password debe quedar rechazado.
Nota (2026-03-04): en `add_hive`, `restrict_ssh` se reporta como estado aplicado. Si el gate restringido no verifica (p.ej. `empty SSH_ORIGINAL_COMMAND`), el flujo cae automáticamente a key no restringida, mantiene `harden_ssh` y expone `restrict_ssh_requested=true` + `restrict_ssh=false`.
Nota (2026-03-04): `remove_hive` normaliza `payload.remote_cleanup` con semántica canónica: `socket_ok`, `socket_timeout`, `ssh_fallback_ok`, `ssh_fallback_failed`, `local_only`. Se mantiene `remote_cleanup_via` como metadato auxiliar para observabilidad.

Salida:

- canal de distribucion de software consistente con spec v2.

## 5. E2E imprescindibles (gates de avance)

### Gate G1 (fin Fase 3)

- [ ] E2E-1 add_hive -> worker con `sy-orchestrator` activo y visible por L2.

### Gate G2 (fin Fases 4-5)

- [ ] E2E-2 `POST /hives/{id}/update` categoria `runtime` -> `ok`.
- [ ] E2E-3 `POST /hives/{id}/update` con manifest no convergido -> `sync_pending`.
- [ ] E2E-4 update `core` fallido -> rollback verificable.

Referencia: `scripts/orchestrator_system_update_api_e2e.sh` valida `sync_pending` + `ok` (y acepta `rollback` en `category=core`).

### Gate G3 (fin Fase 6)

- [ ] E2E-5 spawn/kill de nodo `IO.*` remoto desde admin motherbee sin SSH operativo.
- [ ] E2E-6 spawn/kill de nodo `AI.*` remoto con `force` en kill.

Referencia: `scripts/orchestrator_spawn_kill_v2_e2e.sh` (usa `node_name`, `runtime_version` y valida `SIGTERM`/`SIGKILL`).

### Gate G4 (fin Fase 7)

- [ ] E2E-7 update `vendor` completo via `SYSTEM_UPDATE`.
- [ ] E2E-8 validacion de que no quedan caminos SSH en run/kill/update.
- [ ] E2E-9 `remove_hive` con worker online usa cleanup por socket (sin depender de SSH), y con worker offline cae a fallback controlado.
- [ ] E2E-10 `add_hive` con `harden_ssh=true` valida bloqueo de password SSH y continuidad operativa por orchestrator/socket.

## 6. Definicion de Done v2

- [ ] `SYSTEM_UPDATE` reemplaza operativamente a `RUNTIME_UPDATE`.
- [ ] `SY.admin` maneja update/spawn/kill remoto por API y socket L2.
- [ ] `SY.orchestrator` de cada worker ejecuta local; motherbee no hace SSH operativo.
- [ ] Dist de software por Syncthing funcional para runtime/core/vendor.
- [ ] E2E G1..G4 en verde.

## 7. Orden recomendado de ejecucion

1. Fase 0
2. Fase 1
3. Fase 2
4. Fase 3
5. Fase 4
6. Fase 5
7. Fase 6
8. Fase 7
