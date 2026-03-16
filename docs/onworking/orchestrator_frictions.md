# Orchestrator Frictions (working draft)

Status: working
Date: 2026-03-14
Scope: SY.admin, SY.orchestrator, router, SDK protocol, identity integration
Reference input: `fluxbee-core-change-request.md` (draft v1), `docs/onworking/system-inventory-spec.md` (v1.0)

---

## 1) Objetivo de este documento

Centralizar fricciones reales (confirmadas en código) para discutir diseño y ejecución.
Es temporal y orientado a decisión.

---

## 2) Estado real vs fricciones

### FR-01 — Identity primary resolution en workers

Estado: CLOSED (implementado y validado E2E)

Qué pasa hoy:
- El primary de identity write está fijado por convención dura en `motherbee`.
- `sy_orchestrator` valida coherencia `role/hive_id` y enruta writes a `SY.identity@motherbee`.
- No hay fallback local para writes identity en workers.

Evidencia:
- `src/bin/sy_orchestrator.rs:37`
- `src/bin/sy_orchestrator.rs:5540`
- `src/bin/sy_orchestrator.rs:5892`
- `scripts/inventory_identity_primary_routing_e2e.sh` (D4/D5)

Resultado:
- Se cierra fricción de resolución de primary.
- Sin riesgo residual abierto en el eje primary/registro (FR-01 y FR-02 cerrados).

---

### FR-02 — Spawn puede continuar aunque falle identity register

Estado: CLOSED (core + E2E + docs)

Qué pasa hoy:
- `run_node` exige registro identity exitoso para continuar.
- Fallos de identity register ya no tienen camino soft-fail.
- FR-02 queda cerrado: spawn sin registro identity exitoso está prohibido.

Evidencia:
- `src/bin/sy_orchestrator.rs:5843`
- `src/bin/sy_orchestrator.rs:5851`
- `src/bin/sy_orchestrator.rs:5894`
- `src/bin/sy_orchestrator.rs:6187`

Nota de spec:
- `docs/10-identity-v2.md:302` indica que el orchestrator no debería spawnear sin confirmación síncrona de ILK en DB.

Lista de tareas FR-02 (ejecución):
- [x] FR2-T1. Cambiar default de `identity_register_required()` a `true` en core.
- [x] FR2-T2. Eliminar camino soft-fail en `ensure_node_identity_registered` (si status != ok -> error duro siempre).
- [x] FR2-T3. Tratar `missing_tenant_id` e `identity_unavailable` como error explícito de spawn (no `skipped`).
- [x] FR2-T4. Mantener `ORCH_IDENTITY_REGISTER_REQUIRED` solo como override temporal de test, o removerla completamente (decisión hard no-legacy sugerida: remover).
- [x] FR2-T5. Actualizar mensajes de error/contrato HTTP para que `run_node` devuelva `IDENTITY_REGISTER_FAILED` consistente en todos los fallos de registro.
- [x] FR2-T6. Agregar E2E negativo dedicado FR-02: spawn sin `tenant_id` debe fallar siempre. (`scripts/identity_register_strict_e2e.sh`)
- [x] FR2-T7. Agregar E2E negativo dedicado FR-02: con identity no disponible, spawn debe fallar siempre. (`scripts/identity_register_strict_e2e.sh`)
- [x] FR2-T8. Actualizar docs (`10-identity-v2.md` y este doc) declarando que spawn sin identity register exitoso está prohibido.

Criterio de cierre FR-02:
- `run_node` no puede devolver `status=ok` si `payload.identity.register.status != ok`.
- No existen respuestas `register.status=skipped` en flujos de spawn productivos.
- E2E FR-02 positivos/negativos en verde.

---

### FR-03 — Normalización de `node_name@hive` vs `target_hive`

Estado: CLOSED

Qué pasa hoy:
- Existe normalización única.
- Si `node_name` trae `@hive`, ese hive manda.
- En spawn remoto se propaga `identity_primary_hive_id` al worker.
- En kill remoto aplica la misma precedencia: `node_name@hive` manda sobre el hive del endpoint.
- Validado en modo estricto de inventario (`REQUIRE_INVENTORY_PRESENT=1`) con runtime long-lived `wf.inventory.hold.diag` y versión fixture dedicada por test.

Evidencia:
- `src/bin/sy_orchestrator.rs:5037`
- `src/bin/sy_orchestrator.rs:5798`
- `src/bin/sy_orchestrator.rs:5803`
- `src/bin/sy_orchestrator.rs:5879`
- `src/bin/sy_orchestrator.rs:5627`
- `scripts/inventory_node_name_cross_hive_e2e.sh` (spawn+inventory+kill cruzado)

---

### FR-04 — Campos L3 tipados (`src_ilk/dst_ilk/ich/ctx/ctx_seq/ctx_window`)

Estado: ON HOLD (dependencia externa de spec cognitive)

Regla de trabajo actual:
- Este frente no se toca por ahora.
- No implementar cambios de `ctx` / `ctx_window` hasta cerrar la spec cognitive onworking.

Qué pasa hoy:
- El protocolo/documentación define L3 tipado.
- `Meta` en SDK no expone esos campos (solo `context` libre).
- Router hoy canonicaliza `src_ilk` leyendo `meta.context["src_ilk"]`.
- No está materializada la inyección real de `ctx_window` en router.

Evidencia:
- `docs/02-protocolo.md:94`
- `docs/02-protocolo.md:99`
- `crates/fluxbee_sdk/src/protocol.rs:43`
- `crates/fluxbee_sdk/src/protocol.rs:57`
- `src/router/mod.rs:3781`
- `src/router/mod.rs:3785`
- `docs/04-routing.md:652`

Impacto:
- Desalineación contrato vs runtime real.
- Dependencia implícita en carriers legacy (`meta.context`).

Lista de tareas FR-04 (ON HOLD, solo preparación):
- [ ] FR4-T1. Congelar alcance y contrato final de L3 en spec cognitive (fuente única de verdad para `ctx/ctx_window`).
- [ ] FR4-T2. Definir matriz de compatibilidad backward (`meta.context` legacy vs campos tipados en `Meta`).
- [ ] FR4-T3. Diseñar migración por fases (SDK -> router -> storage) con feature flags y criterio de rollback.
- [ ] FR4-T4. Definir límites y semántica operativa de `ctx_window` (tamaño, truncado, casos de omisión).
- [ ] FR4-T5. Definir plan de pruebas (unit + integración + E2E) para evitar regresiones de tracing/conversación.

Criterio de salida de ON HOLD FR-04:
- Spec cognitive cerrada y aprobada.
- Plan de migración firmado (fases, compat, métricas de adopción).
- Recién ahí se habilita implementación en core/SDK/router.

---

### FR-05 — Config per-node unicast (API de control) + lectura de estado

Estado: CLOSED

Qué pasa hoy:
- Existe endpoint admin canónico per-node config:
  - `GET /hives/{hive}/nodes/{node}/config`
  - `PUT /hives/{hive}/nodes/{node}/config`
- El update per-node usa unicast a L2 del nodo con `CONFIG_CHANGED` (`subsystem=node_config`) como señal de hot-reload.
- El flujo soporta hive remoto vía relay motherbee -> `SY.orchestrator@target`.
- Ya existe endpoint canónico de diagnóstico runtime por nodo:
  - `GET /hives/{hive}/nodes/{node}/state` (read-only; `payload.state = null` si no hay archivo).

Evidencia:
- `src/bin/sy_admin.rs`
- `src/bin/sy_orchestrator.rs`
- `scripts/node_config_per_node_e2e.sh`

Lista de tareas FR-05:
- [x] FR5-T1. Endpoint admin per-node config (`GET/PUT /hives/{hive}/nodes/{node}/config`).
- [x] FR5-T2. Acción canónica en orchestrator (`get_node_config` / `set_node_config`).
- [x] FR5-T3. Soporte remoto por relay (`NODE_CONFIG_GET/SET` via system messages).
- [x] FR5-T4. Señal unicast de hot-reload (`CONFIG_CHANGED subsystem=node_config`) al nodo target.
- [x] FR5-T5. E2E dedicado config (`scripts/node_config_per_node_e2e.sh`).
- [x] FR5-T6. Endpoint `GET /hives/{hive}/nodes/{node}/state` (read-only, `payload: null` si no existe).
- [x] FR5-T7. E2E de state endpoint (existente/no existente + hive remoto). (`scripts/node_config_per_node_e2e.sh`)

---

### FR-06 — Ownership de archivos de nodo (revisión v1.1: two-file model)

Estado: CLOSED

Qué pasa hoy:
- Implementación migrada a two-file ownership con writer único por archivo.
- Path base canónico fijo: `/var/lib/fluxbee/nodes`.
- Spawn fail-closed si existe `config.json` del nodo (`NODE_ALREADY_EXISTS`).

Objetivo v1.1 propuesto:
- Dos archivos por instancia, un único writer por archivo:
  - `config.json` (writer: orchestrator)
  - `state.json` (writer: nodo)
- Invariante: orchestrator nunca escribe `state.json`; nodo nunca escribe `config.json`.
- Spawn fail-closed si `config.json` ya existe (`NODE_ALREADY_EXISTS`), para evitar overwrite accidental.

Evidencia de contexto:
- `docs/onworking/node-spawn-config-spec.md` (v1.1 draft)
- `src/bin/sy_orchestrator.rs` (layout two-file + permisos + fail-closed)
- `src/bin/sy_admin.rs` (`GET /hives/{hive}/nodes/{node}/state`)

Lista de tareas FR-06 (migración v1.1):
- [x] FR6R-T1. Cambiar layout on-disk a carpeta por nodo (`.../<TYPE>/<node@hive>/config.json` + `state.json`).
- [x] FR6R-T2. Definir base path final canónica fija en core: `/var/lib/fluxbee/nodes`.
- [x] FR6R-T3. Spawn: crear `config.json` atómico con bloque `_system`; fallar si ya existe.
- [x] FR6R-T4. `PUT .../config`: merge/write atómico de `config.json` + señal `CONFIG_CHANGED`.
- [x] FR6R-T5. `GET .../config`: leer `config.json` exclusivamente.
- [x] FR6R-T6. `GET .../state`: leer `state.json` read-only, devolver `payload=null` si no existe.
- [x] FR6R-T7. Permisos de archivo/directorio (`0700` dirs, `0600` files) en create/update.
- [x] FR6R-T8. E2E de migración/ownership (writer único por archivo + respawn fail when config exists). (`scripts/node_config_per_node_e2e.sh`)

Criterio de cierre FR-06:
- No existe ningún camino donde orchestrator escriba `state.json`.
- No existe ningún camino donde nodo necesite escribir `config.json`.
- Respawn con `config.json` preexistente falla de forma explícita y estable.
- E2E FR6R en verde.

---

### FR-07 — Schema canónico de status/health para nodos

Estado: CLOSED (core + E2E + docs)

Qué pasa hoy:
- Existe contrato canónico de status con implementación en orchestrator/admin y cobertura E2E completa.
- Specs canónicas actualizadas (`02-protocolo.md`, `07-operaciones.md`) + README operativo para devs.

Lista de tareas FR-07:
- [x] FR7-T1. Definir contrato canónico de `STATUS_RESPONSE` en protocolo (campos mínimos obligatorios + extensiones).
- [x] FR7-T2. Definir enum `lifecycle_state`: `STARTING|RUNNING|STOPPING|STOPPED|FAILED|UNKNOWN`.
- [x] FR7-T3. Definir enum `health_state`: `HEALTHY|DEGRADED|ERROR|UNKNOWN`.
- [x] FR7-T4. Definir enum `health_source`: `NODE_REPORTED|ORCHESTRATOR_INFERRED|UNKNOWN`.
- [x] FR7-T5. Definir contrato de timestamps/versión (`observed_at`, `config_version`, `status_version`) para diagnósticos consistentes.
- [x] FR7-T6. Implementar acción canónica en orchestrator para status por nodo (`get_node_status`) con soporte local y remoto.
- [x] FR7-T7. Exponer endpoint admin estable para status por nodo/hive (sin romper endpoints existentes).
- [x] FR7-T8. Incluir estado de config/state ownership (presencia de `config.json` / `state.json`) en status estandarizado.
- [x] FR7-T9. Definir precedencia de health en la respuesta final (`NODE_REPORTED` primero, fallback inferido en timeout).
- [x] FR7-T10. E2E de status (node reportado, timeout con inferencia, nodo detenido + caso config inválida en local). (`scripts/node_status_fr7_e2e.sh`)
- [x] FR7-T11. Actualizar docs (`02-protocolo.md`, `07-operaciones.md`) y runbooks de troubleshooting.

Borrador inicial para discusión:
- `docs/onworking/node-status-contract-draft.md`

Implementado en esta etapa (2026-03-14):
- `GET /hives/{hive}/nodes/{node_name}/status` en `SY.admin`.
- `get_node_status` en `SY.orchestrator` con relay cross-hive (`NODE_STATUS_GET` / `NODE_STATUS_GET_RESPONSE`).
- Payload base con `lifecycle_state`, `health_state`, `health_source`, `status_version` y bloques `config/state/process/runtime/identity`.
- Precedencia de health implementada: `NODE_REPORTED` (si responde en <=2s) y fallback inferido en timeout/unreachable.
- SDK helper default: `fluxbee_sdk::try_handle_default_node_status` (opt-out con `NODE_STATUS_DEFAULT_HANDLER_ENABLED=0`).
- Alineación de shape con spec: `observed_at` ISO-8601 UTC, `process.{pid,exit_code,restart_count,started_at}` y `config.updated_at` ISO-8601.
- E2E FR7 T7/T8/T9/T10 en verde; validación T10 full confirmada en `motherbee` (`t10_mode=full`).
- Script dedicado T11/T12 ejecutado en verde: `scripts/node_status_fr7_t11_t12_e2e.sh` (`FAILED/UNKNOWN` validado y monotonicidad post-restart `status_version_before=2` -> `status_version_after=3`).
- Documentación canónica alineada:
  - `docs/02-protocolo.md` incorpora `NODE_STATUS_GET/NODE_STATUS_GET_RESPONSE`.
  - `docs/07-operaciones.md` incorpora endpoints/runbook de status por nodo.
  - `README.md` agrega quick checks de status/config/state.

Criterio de cierre FR-07:
- Existe un payload canónico de status consumible por UI/operación sin parsing ad-hoc por runtime.
- El mismo contrato aplica en local/remote (relay) con compat backward razonable.
- E2E FR-07 en verde para los estados principales.

---

### FR-08 — Consistencia `versions` vs artefacto runtime real (`start.sh`)

Estado: CLOSED (core + E2E + docs)

Qué pasa hoy:
- `GET /hives/{hive}/versions` incluye readiness por runtime/version (`runtime_present`, `start_sh_executable`).
- `POST /hives/{hive}/update` valida artefactos de runtimes `current` y responde `sync_pending` si no están listos.
- `run_node` preflight valida `start.sh` (existencia + executable) y falla explícitamente con `RUNTIME_NOT_PRESENT` sin auto-update.
- Semántica de versión en update endurecida: `local > requested` => `VERSION_MISMATCH`.

Evidencia:
- `src/bin/sy_orchestrator.rs` (readiness + update hardening + spawn preflight)
- `scripts/runtime_lifecycle_fr8_e2e.sh` (T11/T12)
- checks manuales T13/T14 (spawn fail explícito + update `sync_pending` con artefacto faltante)
- `docs/onworking/runtime-lifecycle-spec.md` (FR8-T1..T16 en `[x]`)

Criterio de cierre FR-08:
- Cumplido (2026-03-13). Ver checklist consolidado en `docs/onworking/runtime-lifecycle-spec.md`.

---

### FR-09 — SY.admin internal gateway (HTTP + socket unificado)

Estado: CLOSED (core + E2E + docs)

Qué pasa hoy:
- `SY.admin` ya acepta `ADMIN_COMMAND` / `ADMIN_COMMAND_RESPONSE` por socket.
- Existe adapter interno a handlers actuales con lock monocomando global compartido HTTP+socket.
- Selector de destino endurecido: `payload.target` canónico, `node_name@hive` con precedencia y rechazo de `hive_id` legacy fuera de acciones de dominio hive.
- Registry único versionado para acciones internas (`INTERNAL_ACTION_REGISTRY`, `INTERNAL_ACTION_REGISTRY_VERSION`).
- Exposición de capacidades por introspección (`GET /admin/actions` + `list_admin_actions` por socket).
- Guard de CI activo contra drift catálogo HTTP/socket.
- E2E matrix all-actions en verde (incluye `inventory`).
- SDK ya expone helper canónico `admin_command()` para callers internos.

Evidencia:
- `src/bin/sy_admin.rs` (handler interno + dispatch + selector + registry + `list_admin_actions`)
- `crates/fluxbee_sdk/src/admin.rs` (helper SDK)
- `scripts/admin_internal_socket_actions_e2e.sh` (FR9-T9)
- `scripts/admin_http_socket_parity_e2e.sh` (FR9-T10)
- `scripts/admin_internal_target_required_e2e.sh` (FR9-T13/T17)
- `scripts/admin_internal_hive_id_selector_e2e.sh` (FR9-T14/T15)
- `scripts/admin_internal_node_name_precedence_e2e.sh` (FR9-T18)
- `scripts/admin_action_catalog_parity_check.sh` + `.github/workflows/admin-catalog-guard.yml` (FR9-T21/T22)
- `scripts/admin_list_actions_e2e.sh` (FR9-T23)
- `scripts/admin_all_actions_matrix_e2e.sh` (FR9-T24)
- `docs/onworking/admin-internal-gateway-spec.md` (checklist FR9)

Estado de tareas FR-09:
- Cerradas: FR9-T1..T24

Criterio de cierre FR-09:
- Cumplido (2026-03-15). Ver checklist consolidado en `docs/onworking/admin-internal-gateway-spec.md`.

---

### FR-10 — Unificar llamadas de identity en orchestrator con helpers del SDK (opción 1)

Estado: OPEN (backlog)

Qué pasa hoy:
- `SY.orchestrator` usa implementación propia de transporte para `ILK_REGISTER/ILK_UPDATE` (`relay_system_action` + parse/manual errors).
- El SDK ya expone helpers canónicos de identity (`identity_system_call`, `identity_system_call_ok`), pero orchestrator no los usa.

Decisión acordada (alcance mínimo):
- Adoptar opción 1: migrar solo el flujo identity de `run_node` a helpers SDK, sin rediseñar loop principal ni refactor global de mensajería.
- Mantener compatibilidad del contrato externo actual (`IDENTITY_REGISTER_FAILED`, payloads de `run_node`, timeouts observables).

Límite explícito SDK vs negocio:
- SDK (`fluxbee_sdk::identity`) solo centraliza transporte y manejo genérico de llamadas identity (`trace_id`, timeout, unreachable/ttl, status/error_code).
- `SY.orchestrator` mantiene reglas de negocio: cuándo llamar, target fijo `SY.identity@motherbee`, armado de payload (`tenant_id`, `ilk_type`, `identification`), persistencia local `node->ilk` y contrato externo de admin/E2E.
- No mover al SDK decisiones de flujo de spawn ni validaciones de negocio específicas de orchestrator.

No objetivo en esta etapa:
- No reemplazar `forward_system_action_to_hive` ni `relay_system_action` para el resto de acciones.
- No cambiar el modelo de concurrencia del receiver principal de orchestrator.

Lista de tareas FR-10 (backlog):
- [x] FR10-T1. Extraer un wrapper local en orchestrator para crear `NodeSender/NodeReceiver` de relay y ejecutar una llamada identity SDK dentro de ese contexto.
- [x] FR10-T2. Migrar `ensure_node_identity_registered` para usar `fluxbee_sdk::identity::identity_system_call_ok` con `MSG_ILK_REGISTER`.
- [x] FR10-T3. Migrar `apply_node_identity_update` para usar `fluxbee_sdk::identity::identity_system_call_ok` con `MSG_ILK_UPDATE`.
- [x] FR10-T4. Mapear `IdentityError -> OrchestratorError` preservando códigos/mensajes ya consumidos por admin/E2E.
- [x] FR10-T5. Mantener `identity_target` explícito (`SY.identity@motherbee`) en payload de respuesta para no romper diagnósticos actuales.
- [ ] FR10-T6. Si aparece lógica reusable, subir al SDK solo helpers agnósticos de transporte/protocolo identity (no reglas de negocio de orchestrator).
- [ ] FR10-T7. Unit tests en orchestrator para mapeo de error y paths `ok/error` de registro/actualización identity.
- [ ] FR10-T8. Re-ejecutar regresión E2E mínima: `identity_register_strict_e2e.sh` + `inventory_identity_primary_routing_e2e.sh`.
- [ ] FR10-T9. Actualizar docs de implementación (`10-identity-v2.md` + este doc) indicando que orchestrator usa helper SDK para identity calls.

Criterio de cierre FR-10:
- No hay envío manual de `ILK_REGISTER/ILK_UPDATE` en orchestrator fuera del wrapper de transición.
- E2E FR-02 + D4/D5 siguen en verde sin cambios de contrato externo.
- Lógica reusable de llamada identity queda centralizada en SDK.

---

## 3) Decisiones pendientes (para discutir)

### D-01 (identity primary)

Estado: CERRADA
- Regla activa: primary de writes identity fijo en `SY.identity@motherbee`.
- `role=motherbee` exige `hive_id=motherbee`; sin nombres alternativos/legacy.
- Validado por E2E D4/D5.

### D-02 (strict register)

Estado: CERRADA (core + E2E + docs)
- `run_node` ahora exige register identity exitoso.
- Se removió dependencia de flag `ORCH_IDENTITY_REGISTER_REQUIRED` en el flow de spawn.
- Validado por E2E negativos (`identity_register_strict_e2e.sh`) y alineado en `10-identity-v2.md`.

### D-03 (L3 migration)

Definir plan en dos fases:
1) agregar campos tipados en SDK/protocolo sin romper,
2) router/storage migran a tipado,
3) deprecación gradual de carrier en `meta.context`.

### D-04 (config per-node)

Estado: CERRADA
- Se adopta modo dual explícito en operación:
  - config global/subsystem: `CONFIG_CHANGED` broadcast (actual).
  - config per-node: API per-node + unicast `CONFIG_CHANGED` (`subsystem=node_config`) al nodo target.
- No se agrega `CONFIG_SET/GET` nuevo en protocolo por ahora; se documenta equivalente explícito.

### D-05 (effective config file ownership)

Estado: CERRADA
- La decisión de single-file queda reemplazada por revisión v1.1 de two-file ownership.
- Se mantiene el principio de writer único, ahora con separación explícita config/state.

### D-06 (modelo de archivos por nodo)

Estado: CERRADA
- Se adopta dirección técnica: `config.json` (orchestrator) + `state.json` (node).
- Path canónico fijo en core: `/var/lib/fluxbee/nodes`.
- Validado E2E: ownership + respawn fail-closed (`NODE_ALREADY_EXISTS`).

---

## 4) Orden sugerido de ejecución

1. FR-07 (status schema común).
2. FR-04 (ON HOLD hasta cerrar spec cognitive de L3/CTX; avanzar solo tareas preparatorias de contrato).
3. FR-10 (migración acotada de identity calls a SDK, opción 1).
4. INV-D3 (stale inventory) cuando exista trigger canónico desde API admin.

---

## 5) Log de decisiones

| Fecha | Tema | Decisión | Owner | Estado |
|------|------|----------|-------|--------|
| 2026-03-12 | Documento inicial | Creado draft de fricciones | core | abierto |
| 2026-03-12 | Inventario simplificado | Se adopta dirección de `system-inventory-spec.md` con primary fijo en `SY.identity@motherbee` y sin campo nuevo en SHM identity | core | cerrado |
| 2026-03-12 | Regla dura control-plane | `role=motherbee` exige `hive_id=motherbee`; sin nombres alternativos/legacy para primary L2 | core | cerrado |
| 2026-03-13 | FR-01 validación | D4/D5 E2E pasan; enrutamiento identity write a `SY.identity@motherbee` sin fallback local | core | cerrado |
| 2026-03-13 | FR-02 core strict | `run_node` falla siempre ante register identity no exitoso; removido soft-fail/flag estricto en spawn | core | parcial |
| 2026-03-13 | FR-02 E2E negativos | `identity_register_strict_e2e.sh` valida `missing_tenant_id` e `identity_unavailable` con fallo explícito `IDENTITY_REGISTER_FAILED` | core | parcial |
| 2026-03-13 | FR-02 cierre documental | `10-identity-v2.md` declara gate obligatorio fail-closed para spawn sin registro identity exitoso | core | cerrado |
| 2026-03-13 | FR-05 avance | API per-node config implementada (`GET/PUT /hives/{hive}/nodes/{node}/config`) + relay remoto | core | cerrado |
| 2026-03-13 | FR-06 cierre (v1.0) | Config efectiva single-file bajo `${STATE_DIR}/node-configs` con creación en spawn y update atómico | core | superseded |
| 2026-03-13 | FR-05/06 validación E2E v1.0 | `scripts/node_config_per_node_e2e.sh` pasó en `worker-220` (spawn+get+put+get, `config_version` 1→2) | core | cerrado |
| 2026-03-13 | FR-06 reabierta (v1.1) | Se acuerda migrar a two-file ownership (`config.json`/`state.json`) para eliminar write-races y separar responsabilidades | core | abierto |
| 2026-03-13 | FR-06 implementación v1.1 | Core migra a `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/{config.json,state.json}` con permisos estrictos y spawn fail-closed | core | cerrado |
| 2026-03-13 | FR-05 state endpoint | `GET /hives/{hive}/nodes/{node}/state` implementado en `SY.admin`/`SY.orchestrator` con relay remoto | core | cerrado |
| 2026-03-13 | FR-05/06 E2E v1.1 | `node_config_per_node_e2e.sh` extendido con checks de `state` (existing/missing) y respawn fail-closed `NODE_ALREADY_EXISTS` | core | cerrado |
| 2026-03-13 | FR-03 cierre E2E | `inventory_node_name_cross_hive_e2e.sh` valida precedencia de `node_name@hive` en spawn+kill contra endpoint cruzado | core | cerrado |
| 2026-03-13 | FR-03 validación estricta | FR-03 pasa en modo estricto (`REQUIRE_INVENTORY_PRESENT=1`) con fixture `prepare-only` y `runtime_version` dedicada por caso | core | cerrado |
| 2026-03-13 | FR-08 cierre integral | Readiness en `versions`, hardening de `update/spawn`, E2E T11..T14 y docs operativas alineadas | core | cerrado |
| 2026-03-14 | FR-10 decisión técnica | Se adopta migración opción 1: orchestrator pasa llamadas identity (`ILK_REGISTER/ILK_UPDATE`) a helper SDK, sin refactor global de mensajería | core | abierto |

---

## 6) Alineación con `system-inventory-spec.md` (propuesta simplificada)

### 6.1 Lo que calza con el código actual

- No crear región SHM nueva: viable. El layout LSA actual alcanza para representar inventario completo.
- Self-entry en LSA (`HIVE_FLAG_SELF`): viable sin cambiar structs `RemoteHiveEntry`/`RemoteNodeEntry`.
- SY.orchestrator como reader SHM: viable y consistente con el rol de control plane.
- SY.admin delegando a orchestrator (`INVENTORY_REQUEST/RESPONSE`): viable con el patrón actual de relay.

### 6.2 Decisión adoptada para primary

Se adopta el enfoque simple (y duro):
- no agregar `primary_hive_id` al `IdentityHeader`;
- primary identity write target fijo: `SY.identity@motherbee`;
- no permitir fallback local para writes identity en worker;
- `role=motherbee` requiere `hive_id=motherbee` en instalación/bootstrap.

Razón:
- evita migración de layout/version de identity SHM;
- mantiene el inventario enfocado en topología (no en estado interno de sync identity).

### 6.3 Efecto sobre FR-01

FR-01 queda cerrado en implementación actual:
- worker no adivina primary por heurística local/WAN para writes de identity;
- si `SY.identity@motherbee` no es enrutable/alcanzable, falla en forma explícita (sin fallback local), validado por D5.

---

## 7) Lista de tareas actualizada (alineada a `system-inventory-spec.md`)

### Fase A — LSA self-entry (sin región nueva)

- [x] INV-A1. Definir `HIVE_FLAG_SELF` en flags de LSA.
- [x] INV-A2. Hacer que gateway escriba entrada local "self" en `jsr-lsa-<hive>` (nodos locales agregados de routers locales).
- [x] INV-A3. Actualizar self-entry con los mismos triggers de LSA (connect/disconnect, refresh, stale).
- [x] INV-A4. Ajustar readers para distinguir local vs remoto usando `HIVE_FLAG_SELF`.

Criterio de aceptación A:
- `jsr-lsa-<hive>` contiene local + remotos con conteos coherentes en un snapshot único.

### Fase B — Orchestrator inventory reader + mensajes

- [x] INV-B1. Implementar `read_inventory()` leyendo `jsr-lsa-<hive>`.
- [x] INV-B2. Implementar `INVENTORY_REQUEST` / `INVENTORY_RESPONSE`.
- [x] INV-B3. Soportar scope `global|hive|summary`.
- [x] INV-B4. Soportar filtro por tipo (`AI|WF|IO|SY|RT`).

Criterio de aceptación B:
- orchestrator devuelve inventario consolidado sin round-trip remoto ni consulta al router por mensaje.

### Fase C — Admin API inventory

- [x] INV-C1. `GET /inventory` (global).
- [x] INV-C2. `GET /inventory/{hive_id}` (por hive).
- [x] INV-C3. `GET /inventory?type=<KIND>` (filtro tipo).
- [x] INV-C4. `GET /inventory/summary` (resumen liviano).
- [x] INV-C5. Endpoints admin delegan en orchestrator (no SHM directo en admin).

Criterio de aceptación C:
- API de inventario consistente y desacoplada del data plane.

### Fase D — E2E y regresión FR-01/FR-03

- [x] INV-D1. E2E: spawn/kill se refleja en inventario. (`scripts/inventory_spawn_kill_e2e.sh`)
- [x] INV-D2. E2E: add_hive/remove_hive se refleja en inventario. (`scripts/inventory_add_remove_hive_e2e.sh`)
  - Incluye visibilidad por API de `DELETE /hives/{id}` (removida/vista removida) y `POST /hives` (presente).
- [ ] INV-D3. E2E: hive stale aparece como stale. (`scripts/inventory_stale_hive_e2e.sh`)
  - Estado: bloqueado por contrato operativo actual. Sin endpoint `/hives/{id}/routers*` en `SY.admin`, falta trigger canónico para inducir `stale` sin workaround manual.
  - No duplica validación de delete/add (cubierta en INV-D2).
- [x] INV-D4. E2E: worker enruta writes a `SY.identity@motherbee` sin fallback local. (`scripts/inventory_identity_primary_routing_e2e.sh`)
- [x] INV-D5. E2E negativo: worker falla registro identity cuando `SY.identity@motherbee` es inalcanzable. (`scripts/inventory_identity_primary_routing_e2e.sh`)
- [x] INV-D6. E2E regresión: `node_name@hive` cruzado vs endpoint hive mantiene identidad/routing correctos. (`scripts/inventory_node_name_cross_hive_e2e.sh`)
  - El criterio duro valida routing en spawn/kill por `node_name@hive`; la observación en inventory puede ser efímera con runtimes de vida corta.
  - Modo estricto (`REQUIRE_INVENTORY_PRESENT=1`) requiere runtime long-lived (ej.: `wf.inventory.hold.diag`).
  - Si `spawn` responde `RUNTIME_NOT_PRESENT`, el E2E intenta auto-remediar con `sync-hint + update(runtime)` y reintenta; para `wf.inventory.hold.diag` agrega seed automático de runtime source si sigue faltando.
  - El seed automático usa el mismo `NODE_NAME` del test FR-03 para evitar fixture bakeada con un nombre distinto y falsos negativos de presencia en inventory.
  - En modo estricto, el E2E prepara previamente un runtime fixture long-lived para el mismo `NODE_NAME` del caso, evitando arrastre de versiones `current` bakeadas con otro nombre.
  - En modo estricto, el `spawn` usa explícitamente `runtime_version=$STRICT_RUNTIME_VERSION` (no `current`) para evitar carreras de manifest durante el mismo test.
  - El seed/prepare de runtime se ejecuta en modo `PREPARE_ONLY=1` (sin spawn/kill), para no dejar `config.json` residual que provoque `NODE_ALREADY_EXISTS` en el spawn real del caso.

Criterio de aceptación D:
- FR-01 y FR-03 cerrados con evidencia automatizada.

---

## 8) Preguntas de diseño abiertas

- [ ] ¿`INVENTORY_RESPONSE` vive en `02-protocolo.md` como mensaje canónico de control plane?
- [ ] ¿Habrá helper de inventario en SDK o queda en core/admin en primera etapa?
- [ ] ¿Se agrega persistencia histórica fuera de SHM (futuro), o se mantiene snapshot vivo solamente?
