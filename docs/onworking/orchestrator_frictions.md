# Orchestrator Frictions (working draft)

Status: working
Date: 2026-03-13
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

Estado: OPEN (prioridad media-baja)

Qué pasa hoy:
- No existe contrato de mensaje canónico de status de nodo en protocolo core para estados como `UNCONFIGURED`, `FAILED_CONFIG`, `DEGRADED`.
- Hay respuestas ad-hoc por acción/servicio.

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

Estado: REABIERTA
- La decisión de single-file queda reemplazada por revisión v1.1 de two-file ownership.
- Se mantiene el principio de writer único, ahora con separación explícita config/state.

### D-06 (modelo de archivos por nodo)

Estado: PARCIAL
- Se adopta dirección técnica: `config.json` (orchestrator) + `state.json` (node).
- Path canónico fijo en core: `/var/lib/fluxbee/nodes`.
- Pendiente de cierre: validación E2E final de ownership/respawn fail-closed.

---

## 4) Orden sugerido de ejecución

1. FR-07 (status schema común).
2. FR-04 (ON HOLD hasta cerrar spec cognitive de L3/CTX).

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

Criterio de aceptación D:
- FR-01 y FR-03 cerrados con evidencia automatizada.

---

## 8) Preguntas de diseño abiertas

- [ ] ¿`INVENTORY_RESPONSE` vive en `02-protocolo.md` como mensaje canónico de control plane?
- [ ] ¿Habrá helper de inventario en SDK o queda en core/admin en primera etapa?
- [ ] ¿Se agrega persistencia histórica fuera de SHM (futuro), o se mantiene snapshot vivo solamente?
