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
- [x] Persistencia real en PostgreSQL (tenants + ILK/ICH/aliases en primary; validar convergencia/restart en G5).
- [x] SHM `jsr-identity-*` propia (fase C implementada).
- [x] Sync primary/replica por socket identity (fase D implementada; métricas de convergencia en D4 pendientes).

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
- [x] B5. Persistir mutaciones de ILK/ICH/alias en PostgreSQL (primary):
  - `ILK_PROVISION/REGISTER/UPDATE` -> `identity_ilks`
  - alta/merge de canales -> `identity_ichs`
  - merge alias -> `identity_ilk_aliases`
- [x] B6. Cargar `identity_ilks`, `identity_ichs` e `identity_ilk_aliases` desde PostgreSQL al bootstrap del primary.

Salida:
- persistencia de identity en DB conectada al flujo runtime (sujeta a validación E2E de réplica/restart en G5).

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
  - ownership: backlog de diagnósticos/observabilidad (no cierre funcional de identity en este documento).

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
    - wrapper ejemplo para integradores `scripts/io_identity_example_e2e.sh` (naming `io.identity.example`)
  - [x] precondición de spec aplicada en plataforma:
    - `sy-identity` pasa a core requerido en instalación/manifiesto (`scripts/install.sh`)
    - bootstrap worker mínimo incluye `sy-identity` junto a `rt-gateway` y `sy-orchestrator`
  - [ ] integrar en runtimes IO reales (fuera de este repo) para:
    - lookup en SHM
    - miss -> `ILK_PROVISION`
    - usar `ilk_id` en `meta.src_ilk`
  - Nota: en este repo no queda trabajo técnico adicional para E1; el pendiente es de adopción en runtimes IO externos.
- [ ] E2. Frontdesk:
  - [x] helpers SDK para acciones frontdesk (`identity_system_call` / `identity_system_call_ok`) con fallback a primary.
  - [x] diags de referencia (`identity_provision_complete_diag`, `identity_merge_diag`) migrados a helpers SDK.
  - [x] workspace/scaffold para handoff externo disponible en repo:
    - `nodes/gov/common`
    - `nodes/gov/ai-frontdesk-gov`
    - alcance actual: solo `AI.frontdesk.gov` (sin segundo nodo `.gov` en esta fase)
  - [ ] integrar en runtime `AI.frontdesk` real (fuera de este repo):
    - completar registro vía `ILK_REGISTER` sobre ILK temporal
    - canal extra por `ILK_ADD_CHANNEL`
    - scaffold base disponible en este repo: `nodes/gov/ai-frontdesk-gov` (+ `nodes/gov/common`)
  - [ ] checklist de handoff `AI.frontdesk.gov` (owner externo, pendiente cuando entre al repo):
    - [ ] configuración de destino frontdesk en `hive.yaml`:
      - `government.identity_frontdesk: "AI.frontdesk.gov@motherbee"` (o nombre final acordado)
    - [ ] flujo de registro temporal -> completo:
      - consumir `meta.src_ilk` temporal
      - recolectar mínimos (`email`, `display_name`, `tenant_id`)
      - enviar `ILK_REGISTER` con `identity_system_call_ok`
      - verificar `ilk_id` devuelto == ILK temporal original
    - [ ] flujo de canal adicional + merge:
      - detectar match de persona existente (ej: por email)
      - enviar `ILK_ADD_CHANNEL` con `merge_from_ilk_id`
      - verificar convergencia alias old->canonical (durante TTL)
    - [ ] manejo de errores de negocio:
      - contemplar explícitamente `INVALID_TENANT`, `TENANT_PENDING`, `DUPLICATE_EMAIL`,
        `INVALID_REQUEST`, `UNAUTHORIZED_REGISTRAR`
      - no reintentar errores de validación (solo transitorios de red/routing)
    - [ ] gates E2E de cierre (cuando el nodo exista):
      - `BUILD_BIN=0 IDENTITY_PROVISION_COMPLETE_TARGET=\"SY.identity@sandbox\" IDENTITY_PROVISION_COMPLETE_FRONTDESK_NODE_NAME=\"AI.frontdesk.gov@sandbox\" bash scripts/identity_provision_complete_e2e.sh`
      - `BUILD_BIN=0 IDENTITY_MERGE_TARGET=\"SY.identity@worker-220\" IDENTITY_MERGE_FALLBACK_TARGET=\"SY.identity@sandbox\" IDENTITY_MERGE_FRONTDESK_NODE_NAME=\"AI.frontdesk.gov@sandbox\" bash scripts/identity_merge_alias_e2e.sh`
      - wrapper integrado: `BUILD_BIN=0 FRONTDESK_NODE_NAME=\"AI.frontdesk.gov@sandbox\" bash scripts/gov_frontdesk_identity_e2e.sh`
    - [ ] criterio de done:
      - ambos E2E en `status=ok`
      - sin overrides/manual workarounds de targets al cerrar
- [x] E3. Orchestrator:
  - [x] registro de nodos por `ILK_REGISTER` en `run_node` (pre-spawn):
    - usa relay system message hacia `SY.identity@<primary_hive>` (resuelto por `identity_primary_hive_id`)
    - en `SPAWN_NODE` remoto, propaga `identity_primary_hive_id` al worker para evitar resolver contra `SY.identity@<worker_hive>`
    - persiste `node_name -> ilk_id` en estado local orchestrator como cache/diagnóstico (no fuente de verdad)
    - modo estricto opcional por env `ORCH_IDENTITY_REGISTER_REQUIRED=true`
    - tenant resuelto desde `payload.tenant_id`, `payload.config.tenant_id` o `ORCH_DEFAULT_TENANT_ID`
  - [x] updates de metadata por `ILK_UPDATE` en `run_node` (delta explícito):
    - target fijo `SY.identity@<primary_hive>` (misma regla que `ILK_REGISTER`)
    - soporta `add_roles`, `remove_roles`, `add_capabilities`, `remove_capabilities`, `add_channels`, `identity_change_reason`
    - falla el spawn con `IDENTITY_UPDATE_FAILED` si se pidió delta y identity devuelve error
  - [x] hardening aplicado para alinear spec:
    - `sy_orchestrator` ya no usa `node_name -> ilk_id` local como fuente de verdad al resolver `ilk_id` inicial,
    - `SY.identity` canonicaliza `ILK_REGISTER` por `identification.node_name` y devuelve el ILK existente cuando corresponde.
- [x] E4. Merge temporal:
  - [x] crear alias `old->canonical` (`SY.identity` -> `ILK_ADD_CHANNEL` con `merge_from_ilk_id`)
  - [x] mantener alias por TTL (`merge_alias_ttl_secs`)
  - [x] cleanup al vencer (GC periódico + `AliasDelete` delta + soft-delete de ILK temporal)
  - [x] diag/E2E disponible: `src/bin/identity_merge_diag.rs` + `scripts/identity_merge_alias_e2e.sh`

Salida:
- flujo operativo end-to-end de alta humana y alta de nodos.

## 8) Fase F - OPA y canonicalización de ILK

- [x] F1. Exponer `data.identity` y `data.identity_aliases` desde reader SHM.
  - `SY.identity` ahora sincroniza snapshot completo (tenants/ilks/ichs/aliases) a `jsr-identity-*`.
  - `router` inyecta `identity` + `identity_aliases` en el bundle OPA al recargar policy.
- [x] F2. Canonicalizar `src_ilk` antes de resolver target.
  - Implementado en `router` como pre-resolve integrado:
    - lee alias desde SHM identity,
    - canonicaliza `meta.context.src_ilk` (si existe),
    - luego evalúa OPA con el ILK canonical.
- [x] F3. Ruteo de temporales a frontdesk.
  - Implementado en `router` como override pre-OPA:
    - si `registration_status=temporary`, fuerza target configurado en `hive.yaml`:
      `government.identity_frontdesk` (fallback: `AI.frontdesk@<hive_local>`).
- [x] F4. Validar que mensajes con ILK temporal mergeado sigan resolviendo por alias durante TTL.
  - Validado en tests de `router` (pre-resolve identity):
    - `apply_identity_pre_resolve_keeps_alias_canonical_during_ttl`
    - `apply_identity_pre_resolve_ignores_alias_after_ttl`

Salida:
- decisiones L3 consistentes sin perder mensajes en ventana de merge.

## 9) Fase G - E2E y criterios de aceptación

- [x] G1. E2E provisión:
  - ICH desconocido -> `ILK_PROVISION` -> ILK temporal real -> ruta a frontdesk.
  - Avance: diag+script integrados:
    - `src/bin/identity_provision_complete_diag.rs` (incluye verificación de route a `AI.frontdesk`)
    - `scripts/identity_provision_complete_e2e.sh`
- [x] G2. E2E complete:
  - `ILK_REGISTER` completa ILK temporal y reasigna tenant.
  - Avance: validación integrada en `identity_provision_complete_diag`:
    - convergencia `registration_status=complete` + `tenant_id` esperado en SHM identity,
    - bloqueo de segunda reasignación (`INVALID_TENANT_TRANSITION`) para ILK ya completo.
- [x] G3. E2E add_channel + merge:
  - nuevo ICH -> temporal -> `ILK_ADD_CHANNEL` -> alias activo -> convergencia a canonical.
  - Avance: script dedicado `scripts/identity_merge_alias_e2e.sh` (convergencia old->canonical y opcional wait/cleanup por TTL).
- [x] G4. E2E node registration:
  - spawn de `AI.*`/`WF.*` con ILK persistente por `node_name`.
  - Avance: script dedicado `scripts/identity_node_registration_e2e.sh`
    - valida `payload.identity.register.status=ok`
    - valida persistencia de `ilk_id` entre restart del mismo `node_name`
  - Validado también tras restart de `sy-identity` primary (persistencia de tenant/flujo de registro estable).
  - Nota: la idempotencia ya no depende del map local de orchestrator; la canonicalización por `node_name` vive en `SY.identity`.
- [x] G5. E2E replica:
  - full sync en worker nuevo + deltas runtime.
  - [x] hardening de bootstrap worker:
    - `add_hive` escribe `identity.sync.upstream` en `hive.yaml` remoto.
  - [x] diag+runner implementados:
    - `src/bin/identity_replica_sync_diag.rs`
    - `scripts/identity_replica_sync_e2e.sh`
  - [x] corrida de cierre en entorno multi-hive real (worker recién agregado o reiniciado)
- [x] G6. Pruebas negativas:
  - [x] diag+runner base implementados:
    - `src/bin/identity_negative_diag.rs`
    - `scripts/identity_negative_e2e.sh`
  - [x] actor no autorizado (`UNAUTHORIZED_REGISTRAR`)
  - [x] IDs mal formados (`INVALID_REQUEST`)
  - [x] tenant inexistente (`INVALID_TENANT`)
  - [x] duplicado de email (`DUPLICATE_EMAIL`)
  - [x] duplicado de ICH por unique (`DUPLICATE_ICH`)
  - [x] colisión de `node_name` cubierta por canonicalización/idempotencia (G4), no se espera error de duplicado en flujo normal
  - [x] corrida final de cierre con casos de duplicado

Salida:
- gate de aceptación identity v2 completo.

## 10) Alineación documental al cerrar implementación

- [x] DOC1. `docs/02-protocolo.md` (mensajes identity v2 + delimitación vs CONFIG_CHANGED).
- [x] DOC2. `docs/03-shm.md` (región identity v2 real: layout dinámico, aliases, hash mapping ICH->ILK).
- [x] DOC3. `docs/SY_nodes_spec.md` (identity fuera de CONFIG_CHANGED + sync por socket primary/replica).
- [x] DOC4. `docs/07-operaciones.md` (excepción DB ownership de identity + bloque `identity.*` en `hive.yaml`).
- [x] DOC5. `README.md` (estado de implementación de identity + toolbox SDK identity).
