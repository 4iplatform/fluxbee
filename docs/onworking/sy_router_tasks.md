# SY.router - Estado actual vs spec (v1.16+)

Documento consolidado de trabajo para router:
- LSA/WAN/topología remota.
- Resolver OPA y contrato de routing.

## Bloque A - LSA/WAN (migrado)
### Router LSA/SLA Full Review (Motherbee/Worker)

Date: 2026-02-18  
Scope: router WAN+LSA path, SHM `jsr-lsa-*`, and admin/orchestrator remote projections.

## Goal

Assess whether the observed remote router UUID (`00000000-0000-0000-0000-000000000000`) is an isolated issue or part of a broader LSA gap, and define a complete worklist.

## Executive conclusion

The UUID issue is **not isolated**.  
It is one symptom of a broader gap in the remote-topology model:

- remote router identity is missing from current LSA contract/SHM projection.
- remote node/router projection in orchestrator is lossy and partially ignores LSA status bits.
- LSA trust and sequence handling need hardening for restart/security scenarios.

## Evidence from current code

1) Remote router UUID is synthetic (nil)
- `src/bin/sy_orchestrator.rs:1000` uses `Uuid::nil()` in `remote_routers_for_hive`.
- This is because `LsaSnapshot` remote hive entries do not carry remote router UUID.

2) LSA protocol payload has no remote gateway identity fields
- `crates/jsr_client/src/protocol.rs:162` (`LsaPayload`) contains `hive/seq/timestamp/nodes/routes/vpns`.
- No `router_id` / `router_name` in payload.

3) SHM LSA remote hive entry has no remote gateway identity
- `src/shm/mod.rs:241` (`RemoteHiveEntry`) stores hive id, seq/time, counters, flags only.
- No remote router UUID/name field.

4) Orchestrator remote-node projection ignores stale/deleted flags
- `src/bin/sy_orchestrator.rs:941-959` includes all nodes for a hive index.
- `src/bin/sy_orchestrator.rs:962-975` hardcodes `"status":"active"` and uses hive `last_updated` as `connected_at`.

5) LSA auth/trust binding gap
- `src/router/mod.rs:1776-1785` accepts `MSG_LSA` and applies payload by `payload.hive`.
- No strict binding between authenticated WAN peer (`peer_hello.hive_id`) and `payload.hive`.

6) Sequence restart risk
- `src/router/mod.rs:1429-1433` rejects if `payload.seq <= last_seq`.
- On remote restart, sequence can reset to small values and be dropped until surpassing previous high watermark.

7) WAN readiness check is presence-only
- `src/bin/sy_orchestrator.rs:2433-2470` (`wait_for_wan`) checks hive presence in LSA.
- It does not enforce freshness/non-stale for success criteria.

## Impact

- Admin `/hives/{id}/routers` cannot expose a real remote router UUID today.
- Remote node/router status from admin API can look healthier than real LSA state.
- Potential stale or spoofed LSA edge cases can pollute remote topology.
- Restart behavior can produce long-lived partial visibility if seq handling is not reset-aware.

## Worklist (onworking)

## Status update (after P2 implementation)

- Remote router identity is now propagated end-to-end:
  - WAN LSA payload includes `router_id` / `router_name`.
  - SHM LSA remote hive entry includes router UUID/name.
  - Admin/orchestrator remote routers projection now reads real UUID/name from LSA state.
- SHM compatibility was handled with `LSA_VERSION` bump (`1 -> 2`) and region recreation on header mismatch.
- Remaining closure is `P3` (tests + operational validation matrix).

## P0 - Data correctness for admin/orchestrator view

- [x] Make orchestrator remote node projection LSA-flag aware:
  - Skip `FLAG_STALE` hives and stale/deleted nodes.
  - Return status based on flags (`active/stale/deleted`) instead of hardcoded `active`.
- [x] Make `wait_for_wan` freshness-aware:
  - Require matching hive not stale and within `HEARTBEAT_STALE_MS` window.
- [x] Keep current nil UUID behavior explicitly documented until protocol/model extension lands.
  - Nota operativa agregada en `docs/onworking/sy_admin_tasks.md` (sección routers).

Acceptance:
- `/hives/{id}/nodes` and `/hives/{id}/routers` reflect stale states correctly.
- `add_hive` WAN success depends on fresh LSA, not only presence.

## P1 - LSA trust and sequence hardening

- [x] Bind incoming LSA to authenticated WAN session:
  - Track peer hive per TCP session and reject payload hive mismatch.
- [x] Add sequence reset policy:
  - [x] Option A: per-peer/session epoch to allow restart reset.
  - Option B: reset `last_seq` when peer reconnect is detected.
- [x] Add explicit logs/counters for rejected LSA causes:
  - [x] `hive_mismatch`, `stale_seq`, `parse_error`, `unauthorized`.

Acceptance:
- Remote restart does not leave LSA permanently frozen.
- LSA from mismatched hive is rejected deterministically.

## P2 - Remote router identity model completion

- [x] Extend LSA model to carry remote gateway identity:
  - Protocol: added `router_id` / `router_name` to `LsaPayload`.
  - SHM: extended `RemoteHiveEntry` with remote router UUID/name (versioned layout).
- [x] Update orchestrator `/hives/{id}/routers` to return real UUID from LSA state.
- [x] Add migration/compatibility strategy for SHM version bump.
  - `LSA_VERSION` bumped `1 -> 2`.
  - Existing `/jsr-lsa-*` regions with old header are treated as invalid and recreated.
  - Compatibility mode for old peers: if LSA payload omits router identity, router falls back to authenticated WAN HELLO identity for projection.

Acceptance:
- `/hives/{id}/routers` no longer returns nil UUID.
- Remote router identity remains stable across refresh cycles.

## P3 - Tests and operational validation

- [x] Unit tests:
  - [x] LSA apply with stale seq/restart seq.
  - [x] Hive mismatch rejection.
  - [x] Flag projection for orchestrator API.
- [x] Integration tests:
  - [x] two-hive WAN bootstrap/disconnect/reconnect/stale transition covered with admin WAN scripts (`scripts/admin_wan_stale_recovery_e2e.sh`, `scripts/admin_wan_integration_suite.sh`).
  - [x] add/remove hive with LSA freshness gate covered (`scripts/admin_wan_integration_suite.sh`: requires `payload.wan_connected=true` on add and `NOT_FOUND` after remove).
- [x] E2E checks:
  - [x] admin endpoints return real remote router UUID after P2.
  - [x] add_hive/list_nodes/list_routers/router-cycle/storage-cycle validated via `scripts/admin_nodes_routers_storage_e2e.sh`.
  - [x] stale transitions visible through API (`scripts/admin_wan_stale_recovery_e2e.sh`: `alive -> stale -> alive` validated on `worker-220`).

Implemented tests:
- `src/router/mod.rs`:
  - `lsa_rejects_hive_mismatch`
  - `lsa_sequence_resets_on_new_session_epoch`
  - `lsa_uses_peer_identity_when_payload_identity_missing`
- `src/bin/sy_orchestrator.rs`:
  - `remote_routers_projection_uses_real_uuid_and_name`
  - `remote_node_projection_reports_status_from_flags`

Operational integration script prepared:
- `scripts/admin_wan_stale_recovery_e2e.sh`
  - validates `alive -> stale -> alive` via admin API against a real worker hive.
  - validates remote router UUID remains non-nil after recovery.
- `scripts/admin_wan_integration_suite.sh`
  - validates `add_hive` with WAN freshness gate (`wan_connected=true`), stale/recovery cycle, and `remove_hive` closure (`NOT_FOUND` post-delete).

## Suggested delivery order

1. P0 (fast correctness wins in API + bootstrap behavior).  
2. P1 (safety and resilience).  
3. P2 (model/protocol/SHM extension).  
4. P3 (test matrix closure).

## Notes

- Existing docs still reference older naming in some sections (`json-router` paths); keep this review focused on LSA behavior first.
- If remote router UUID appears as `0000...` after P2, treat it as deployment/version drift between mother/worker (not expected steady-state behavior).

## Bloque B - OPA/Resolve follow-up (migrado)
### OPA/Router Follow-up (post NATS + orchestrator E2E)

Date: 2026-02-21  
Scope: resolver OPA en router (`Destination::Resolve`) + consistencia de contrato de protocolo.

## Contexto del incidente

En `orchestrator_runtime_update_spawn_e2e`:

- Router recibe mensajes `system` con `dst=Resolve`.
- Router responde `UNREACHABLE reason=OPA_ERROR`.
- Log router: `opa resolve failed: json error: key must be a string at line 1 column 2`.

Esto indica falla técnica de parseo en el resolver OPA, no un `deny` explícito de policy.

## Hallazgos contra especificación

1) Contrato `dst` del protocolo:
- `docs/02-protocolo.md` define `routing.dst` como `UUID | "broadcast" | null`.
- Ejemplos de la misma spec en sección 7.8 usan `dst: "SY.orchestrator@motherbee"` (nombre L2), lo cual contradice 2.1.

2) Contrato OPA:
- `docs/04-routing.md` define salida OPA como JSON con `target`.
- Router ante error de resolver devuelve `OPA_ERROR` (esperado), pero la causa observada es parseo de formato, no decisión de policy.

3) Implementación actual:
- `src/opa.rs` prioriza `opa_json_dump` y deja `opa_value_dump` como fallback.
- El parseo no-JSON ahora se reporta explícitamente como error de parseo de resultado OPA (no como `deny` de policy).

## Checklist de cierre

### A. Robustez técnica OPA resolver
- [x] Revisar y definir orden de dumps en `src/opa.rs` (`opa_json_dump` preferido, fallback controlado).
- [x] Agregar manejo explícito para parseo no JSON (error diferenciable vs policy deny).
- [x] Agregar tests unitarios para `parse_target_from_result` y para flujo con dump no JSON.
- [x] Agregar logs de diagnóstico mínimos en resolver (fuente de dump usada + causa resumida).

### B. Consistencia contrato protocolo (docs + implementación)
- [x] Unificar spec de `routing.dst` en `docs/02-protocolo.md`:
  - [x] Se admite nombre L2 en `dst` y quedó documentado explícitamente.
  - [x] Router alineado para resolver `dst` string por UUID o por nombre L2 (FIB directo, sin OPA).
- [x] Revisar ejemplos operativos de mensajes `system` para evitar ambigüedad (`SY.admin` vs actores externos).
  - [x] `docs/02-protocolo.md` (7.8) aclara origen operativo por mensaje.
  - [x] `docs/07-operaciones.md` (4.9.6) usa ejemplo completo (`routing` + `meta.type` + `meta.msg`).
  - [x] Regla operativa explícita: para control-plane de orchestrator usar `dst` por nombre L2 (`SY.orchestrator@<hive>`), no `dst=null`/`Resolve`.

### C. Cobertura operativa
- [x] Incorporar caso negativo en E2E: `UNREACHABLE/<reason>` debe ser explícito y no timeout opaco.
  - `orch_system_diag` ahora soporta `ORCH_EXPECT_SPAWN_UNREACHABLE_REASON=<reason>` y falla explícitamente por mismatch (no timeout opaco).
- [x] Incorporar caso positivo de `Destination::Resolve` para mensajes `system` de control plane permitidos por policy.
  - `orch_system_diag` ahora soporta `ORCH_ROUTE_MODE=resolve` y envía `routing.dst=Resolve` + `meta.target`.
- [x] Agregar chequeo previo de salud OPA (status/version en SHM) antes de tests de resolve.
  - helper nuevo: `opa_shm_diag` (`/jsr-opa-<hive>`), integrado en `scripts/orchestrator_runtime_update_spawn_e2e.sh` cuando `ORCH_ROUTE_MODE=resolve`.
- [x] Hardening adicional de seguridad: validar origen permitido para `SPAWN_NODE`/`KILL_NODE` en `SY.orchestrator` (allowlist explícita), además de policy/router.
  - `sy_orchestrator` ahora valida origen por nombre L2 resolviendo `routing.src` (UUID) contra SHM del router.
  - Allowlist configurable por `ORCH_SYSTEM_ALLOWED_ORIGINS` (default: `SY.admin,WF.orch.diag`, expandido a `@<hive>` si no viene sufijo).
  - Si origen no permitido, responde `*_RESPONSE` con `status=error`, `error_code=FORBIDDEN` (sin timeout opaco).
  - Recomendación operación productiva: fijar `ORCH_SYSTEM_ALLOWED_ORIGINS=SY.admin` (dejar `WF.orch.diag` sólo para pruebas controladas).

### D. Pendiente estructural ya existente
- [x] Completar carga de `data` bundle en router cuando policy lo requiera (marcado en `docs/09-router-status.md`).
  - Router intenta leer `data bundle` en `/var/lib/fluxbee/opa/current/data.json` en cada `OPA_RELOAD`.
  - Si existe y no está vacío, lo aplica vía `opa_eval_ctx_set_data`; si no existe, mantiene fallback a `data` embebido en WASM.
  - Si el archivo no se puede leer, se reporta en log y no bloquea reload (fallback embebido).
  - Si el JSON del bundle es inválido, el reload falla explícitamente (`OPA_ERROR`) para evitar policy/data inconsistente.

## Comandos de validación (Checklist C)

Caso positivo (`Destination::Resolve` + control-plane):

```bash
TARGET_HIVE="worker-220" \
ORCH_ROUTE_MODE="resolve" \
BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

Caso negativo (esperando `UNREACHABLE` explícito por reason):

```bash
TARGET_HIVE="worker-220" \
ORCH_ROUTE_MODE="resolve" \
ORCH_SEND_RUNTIME_UPDATE=0 \
ORCH_SEND_KILL=0 \
ORCH_EXPECT_SPAWN_UNREACHABLE_REASON="OPA_NO_TARGET" \
BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

Nota:
- con la policy actual observada en `sandbox` (2026-02-22), el reason esperado es `OPA_NO_TARGET`.
- `OPA_ERROR` queda para errores técnicos del resolver OPA (parseo/ejecución), no para ausencia de target en policy.

Caso hardening de origen (esperando `FORBIDDEN` desde `SY.orchestrator`):

```bash
TARGET_HIVE="worker-220" \
ORCH_ROUTE_MODE="unicast" \
ORCH_DIAG_NODE_NAME="WF.unauthorized" \
ORCH_SEND_RUNTIME_UPDATE=0 \
ORCH_SEND_KILL=0 \
ORCH_EXPECT_SPAWN_ERROR_CODE="FORBIDDEN" \
BUILD_BIN=0 \
bash scripts/orchestrator_runtime_update_spawn_e2e.sh
```

## Notas de decisión

- No tocar policy/OPA rules fuera del contrato de `dst` y origen permitido para `SPAWN_NODE/KILL_NODE`.
- Priorizar corrección de robustez del resolver (A) antes de ampliar permisos de policy.

## Estado actual (2026-02-22)

- El flujo de orchestrator para `RUNTIME_UPDATE` / `SPAWN_NODE` / `KILL_NODE` quedó estable con `dst` por nombre L2 (FIB directo, sin pasar por `Resolve`+OPA para control-plane).
- El E2E con worker real cerró `status=ok` en `SPAWN_NODE_RESPONSE` y `KILL_NODE_RESPONSE`.
- El bloqueo observado al final no fue OPA/router sino sync de runtimes con permisos remotos, resuelto en `sy_orchestrator` con staging en `/tmp` + promoción con `sudo`.
- Hardening de origen en `sy_orchestrator` activo: acciones de sistema sólo se aceptan desde orígenes en allowlist.

## Criterio operativo acordado para mensajes `system` (orchestrator)

- `RUNTIME_UPDATE`:
  - origen: actor de control-plane autorizado (p.ej. `SY.admin` o tooling de diagnóstico como `WF.orch.diag`).
  - destino: `routing.dst = "SY.orchestrator@<hive>"`.
- `SPAWN_NODE` y `KILL_NODE`:
  - origen esperado: `SY.admin` (tooling de diagnóstico solo para E2E controlado).
  - destino: `routing.dst = "SY.orchestrator@<hive>"`.
- No usar `routing.dst = null` (`Destination::Resolve`) para estos mensajes de control-plane en operación normal.
- Nota de seguridad:
  - además del control en router/OPA, `SY.orchestrator` valida origen permitido para `RUNTIME_UPDATE`/`SPAWN_NODE`/`KILL_NODE`.
  - default operativo: `SY.admin@<hive>` y `WF.orch.diag@<hive>` (ajustable con `ORCH_SYSTEM_ALLOWED_ORIGINS`).
