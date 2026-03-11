# SY.identity v2 - Backlog de implementación (spec-first, sin legacy)

Fecha base: 2026-03-10  
Spec fuente: `docs/10-identity-v2.md`

## 0) Estado actual (arranque de implementación)

- [x] Binario `sy_identity` creado en `src/bin/sy_identity.rs` y compilando.
- [x] Loop de conexión al router + reconexión implementado.
- [x] Acciones system v2 cableadas con respuestas:
  - `ILK_PROVISION(_RESPONSE)`
  - `ILK_REGISTER(_RESPONSE)`
  - `ILK_ADD_CHANNEL(_RESPONSE)`
  - `ILK_UPDATE(_RESPONSE)`
  - `TNT_CREATE(_RESPONSE)`
  - `TNT_APPROVE(_RESPONSE)`
- [x] Validación estricta de payload con `serde(deny_unknown_fields)` por acción.
- [x] Validación de IDs prefijados (`ilk:`, `ich:`, `tnt:`) en frontera protocolo.
- [x] Allowlist autoritativo por acción (resolviendo `routing.src` UUID -> node name vía SHM/LSA).
- [x] Bootstrap de schema PostgreSQL en `SY.identity` primary (motherbee), con tablas `identity_*`.
- [ ] Persistencia real en PostgreSQL (fase B pendiente).
- [ ] SHM `jsr-identity-*` propia (fase C pendiente).
- [ ] Sync primary/replica por socket identity (fase D pendiente).

## 1) Reglas de trabajo

- Sin backward compatibility.
- Sin payloads legacy ni aliases de campos.
- SHM fijo + seqlock (mismo patrón que router/config/lsa/opa).
- Sync identity solo por socket SY.identity<->SY.identity (no CONFIG_CHANGED, no broadcast).
- ILK temporal siempre real (`ilk:<uuid-v4>`), nunca pseudo-id.
- Herramientas para nodos externos (IO/AI/WF fuera del core) viven en `fluxbee_sdk`.
  - `json-router` implementa runtime/plataforma (writer SHM, servicios SY/RT).
  - `fluxbee_sdk` expone helpers de consumo (lookup/provision/contratos de integración).

## 2) Decisiones cerradas (tomadas en spec)

- `ILK_PROVISION` (IO) crea ILK temporal real.
- `ILK_REGISTER` (frontdesk/orchestrator) crea o completa ILK.
- `ILK_ADD_CHANNEL` (frontdesk) asocia canal y puede mergear ILK temporal.
- `identity_ilks.tenant_id` no nulo: provisión entra en `default_tenant`; reasignación solo en `temporary -> complete`.
- Alias de merge (`old_ilk -> canonical_ilk`) con TTL (`merge_alias_ttl_secs`) para mensajes en vuelo.
- DB con columnas denormalizadas e índices únicos parciales (`email`, `node_name`).
- `identity_ichs` con `tenant_id` directo + `UNIQUE(channel_type, address, tenant_id)`.

## 3) Fase A - Contrato de protocolo y autorización

- [x] A1. Implementar mensajes de sistema en SY.identity:
  - `ILK_PROVISION`, `ILK_PROVISION_RESPONSE`
  - `ILK_REGISTER`, `ILK_REGISTER_RESPONSE`
  - `ILK_ADD_CHANNEL`, `ILK_ADD_CHANNEL_RESPONSE`
  - `ILK_UPDATE`, `ILK_UPDATE_RESPONSE`
  - `TNT_CREATE`, `TNT_CREATE_RESPONSE`
  - `TNT_APPROVE`, `TNT_APPROVE_RESPONSE`
- [x] A2. Implementar validación estricta de payload por acción (campos requeridos/tipos).
- [x] A3. Implementar allowlist por acción (enforcement autoritativo):
  - `ILK_PROVISION`: `IO.*@*`
  - `ILK_REGISTER`: `AI.frontdesk@*`, `SY.orchestrator@*`
  - `ILK_ADD_CHANNEL`: `AI.frontdesk@*`
  - `ILK_UPDATE` nodo/sistema: `SY.orchestrator@*`
- [x] A4. Normalizar/validar IDs prefijados (`ilk:`, `ich:`, `tnt:`) en frontera protocolo.

Salida:
- API de mensajes identity v2 cerrada y sin ambigüedad de registradores.

## 4) Fase B - Modelo DB y migraciones

- [x] B1. Crear tablas:
  - `identity_tenants`
  - `identity_ilks`
  - `identity_ichs`
  - `identity_vocabulary`
  - `identity_ilk_aliases`
- [x] B2. Implementar índices/constraints:
  - unique parcial `identity_ilks(email, tenant_id)` (no deleted)
  - unique parcial `identity_ilks(node_name)` (no deleted)
  - unique `identity_ichs(channel_type, address, tenant_id)`
  - índices de alias por `canonical_ilk_id` y `expires_at`
- [x] B3. Implementar reglas de transición:
  - provisión en `default_tenant`
  - reasignación de tenant solo en `registration_status=temporary`
- [x] B4. Implementar GC de alias expirados + soft-delete de ILK temporal mergeado.

Salida:
- persistencia consistente con unicidad y política de merge.

## 5) Fase C - SHM identity v2 (fijo + seqlock)

- [x] C1. Agregar región `jsr-identity-<hive>` en `src/shm/mod.rs`:
  - `IDENTITY_MAGIC`, `IDENTITY_VERSION`
  - `IdentityHeader`, `TenantEntry`, `IlkEntry`, `IchEntry`, `IchMappingEntry`, `IlkAliasEntry`
  - `IdentityRegionWriter`, `IdentityRegionReader`
- [x] C2. Implementar `layout_identity()` con `MAX_*` configurables.
- [x] C3. Implementar lectura consistente con seqlock (doble lectura de `seq`).
- [x] C4. Implementar escritura de mapping hash con linear probing + tombstones.
- [x] C5. Alinear tamaños SHM con DB para evitar truncamiento:
  - `channel_type` 32
  - `address` 256

Salida:
- región identity operativa y escalable para lookup O(1) promedio.

## 6) Fase D - Sync primary/replica por socket

- [x] D1. Implementar full sync chunked (`IDENTITY_FULL_SYNC`) en cold start.
- [x] D2. Implementar delta sync (`IDENTITY_DELTA`) para create/update/delete.
- [x] D3. Implementar versionado/orden de deltas y retry/ack por worker.
- [ ] D4. Implementar métricas de convergencia (lag, deltas pendientes, tiempo full sync).
  - tracking detallado en `docs/onworking/diagnostics_tasks.md` sección `6.6 Identity sync (SY.identity)`.

Salida:
- replicación identity estable entre motherbee (PRIMARY) y workers (REPLICA).

## 7) Fase E - Integración con IO/frontdesk/orchestrator

- [ ] E1. IO:
  - [x] helper lookup ICH->ILK en SHM (SDK `crates/fluxbee_sdk/src/identity.rs`):
    - `resolve_ilk_from_shm_name`
    - `resolve_ilk_from_hive_id`
    - `resolve_ilk_from_hive_config`
  - [x] helper `ILK_PROVISION` reusable en SDK (`crates/fluxbee_sdk/src/identity.rs`):
    - `provision_ilk`
    - `IlkProvisionRequest` / `IlkProvisionResult`
  - [x] nodo/runtime de referencia para validar infraestructura sin tocar IO productivo:
    - binario `src/bin/io_test_diag.rs` (usa helpers SDK de lookup/provision)
    - script `scripts/io_test_node_e2e.sh` (runtime fixture + sync-hint + update + run/kill)
  - [ ] integrar en runtimes IO reales (fuera de este repo) para:
    - lookup en SHM
    - miss -> `ILK_PROVISION`
    - usar `ilk_id` en `meta.src_ilk`
- [ ] E2. Frontdesk:
  - completar registro vía `ILK_REGISTER` sobre ILK temporal
  - canal extra por `ILK_ADD_CHANNEL`
- [ ] E3. Orchestrator:
  - registro de nodos por `ILK_REGISTER`
  - updates de metadata por `ILK_UPDATE`
- [ ] E4. Merge temporal:
  - crear alias `old->canonical`
  - mantener alias por TTL
  - cleanup al vencer

Salida:
- flujo operativo end-to-end de alta humana y alta de nodos.

## 8) Fase F - OPA y canonicalización de ILK

- [ ] F1. Exponer `data.identity` y `data.identity_aliases` desde reader SHM.
- [ ] F2. Canonicalizar `src_ilk` en reglas antes de derivar tenant/capabilities.
- [ ] F3. Regla de ruteo a frontdesk para `registration_status=temporary`.
- [ ] F4. Validar que mensajes con ILK temporal mergeado sigan resolviendo por alias durante TTL.

Salida:
- decisiones L3 consistentes sin perder mensajes en ventana de merge.

## 9) Fase G - E2E y criterios de aceptación

- [ ] G1. E2E provisión:
  - ICH desconocido -> `ILK_PROVISION` -> ILK temporal real -> ruta a frontdesk.
- [ ] G2. E2E complete:
  - `ILK_REGISTER` completa ILK temporal y reasigna tenant.
- [ ] G3. E2E add_channel + merge:
  - nuevo ICH -> temporal -> `ILK_ADD_CHANNEL` -> alias activo -> convergencia a canonical.
- [ ] G4. E2E node registration:
  - spawn de `AI.*`/`WF.*` con ILK persistente por `node_name`.
- [ ] G5. E2E replica:
  - full sync en worker nuevo + deltas runtime.
- [ ] G6. Pruebas negativas:
  - actor no autorizado (`UNAUTHORIZED_REGISTRAR`)
  - IDs mal formados
  - duplicados (`DUPLICATE_EMAIL`, `DUPLICATE_NODE_NAME`, ICH unique)

Salida:
- gate de aceptación identity v2 completo.

## 10) Alineación documental al cerrar implementación

- [ ] DOC1. `docs/02-protocolo.md` (mensajes identity v2).
- [ ] DOC2. `docs/03-shm.md` (región identity v2 real).
- [ ] DOC3. `docs/SY_nodes_spec.md` (identity fuera de CONFIG_CHANGED).
- [ ] DOC4. `docs/07-operaciones.md` (excepción DB ownership + operación identity).
- [ ] DOC5. `README.md` (estado de implementación de identity).
