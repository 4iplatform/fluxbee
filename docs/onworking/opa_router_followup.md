# OPA/Router Follow-up (post NATS + orchestrator E2E)

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
- `src/opa.rs` usa `opa_value_dump` antes de `opa_json_dump`.
- Si `opa_value_dump` retorna formato no JSON estricto, el parseo JSON falla y se cae en `OPA_ERROR`.

## Checklist de cierre

### A. Robustez técnica OPA resolver
- [x] Revisar y definir orden de dumps en `src/opa.rs` (`opa_json_dump` preferido, fallback controlado).
- [ ] Agregar manejo explícito para parseo no JSON (error diferenciable vs policy deny).
- [ ] Agregar tests unitarios para `parse_target_from_result` y para flujo con dump no JSON.
- [ ] Agregar logs de diagnóstico mínimos en resolver (fuente de dump usada + causa resumida).

### B. Consistencia contrato protocolo (docs + implementación)
- [x] Unificar spec de `routing.dst` en `docs/02-protocolo.md`:
  - [x] Se admite nombre L2 en `dst` y quedó documentado explícitamente.
  - [x] Router alineado para resolver `dst` string por UUID o por nombre L2 (FIB directo, sin OPA).
- [x] Revisar ejemplos operativos de mensajes `system` para evitar ambigüedad (`SY.admin` vs actores externos).
  - [x] `docs/02-protocolo.md` (7.8) aclara origen operativo por mensaje.
  - [x] `docs/07-operaciones.md` (4.9.6) usa ejemplo completo (`routing` + `meta.type` + `meta.msg`).
  - [x] Regla operativa explícita: para control-plane de orchestrator usar `dst` por nombre L2 (`SY.orchestrator@<hive>`), no `dst=null`/`Resolve`.

### C. Cobertura operativa
- [ ] Incorporar caso negativo en E2E: `UNREACHABLE/OPA_ERROR` debe ser explícito y no timeout opaco.
- [ ] Incorporar caso positivo de `Destination::Resolve` para mensajes `system` de control plane permitidos por policy.
- [ ] Agregar chequeo previo de salud OPA (status/version en SHM) antes de tests de resolve.
- [ ] Hardening adicional de seguridad: validar origen permitido para `SPAWN_NODE`/`KILL_NODE` en `SY.orchestrator` (allowlist explícita), además de policy/router.

### D. Pendiente estructural ya existente
- [ ] Completar carga de `data` bundle en router cuando policy lo requiera (marcado pendiente en `docs/09-router-status.md`).

## Notas de decisión (antes de implementar)

- No tocar policy/OPA rules hasta cerrar decisión de contrato de `dst` y de origen permitido para `SPAWN_NODE/KILL_NODE`.
- Priorizar corrección de robustez del resolver (A) antes de ampliar permisos de policy.

## Estado actual (2026-02-21)

- El flujo de orchestrator para `RUNTIME_UPDATE` / `SPAWN_NODE` / `KILL_NODE` quedó estable con `dst` por nombre L2 (FIB directo, sin pasar por `Resolve`+OPA para control-plane).
- El E2E con worker real cerró `status=ok` en `SPAWN_NODE_RESPONSE` y `KILL_NODE_RESPONSE`.
- El bloqueo observado al final no fue OPA/router sino sync de runtimes con permisos remotos, resuelto en `sy_orchestrator` con staging en `/tmp` + promoción con `sudo`.

## Criterio operativo acordado para mensajes `system` (orchestrator)

- `RUNTIME_UPDATE`:
  - origen: actor de control-plane autorizado (p.ej. `SY.admin` o tooling de diagnóstico como `WF.orch.diag`).
  - destino: `routing.dst = "SY.orchestrator@<hive>"`.
- `SPAWN_NODE` y `KILL_NODE`:
  - origen esperado: `SY.admin` (tooling de diagnóstico solo para E2E controlado).
  - destino: `routing.dst = "SY.orchestrator@<hive>"`.
- No usar `routing.dst = null` (`Destination::Resolve`) para estos mensajes de control-plane en operación normal.
- Nota de seguridad:
  - hoy `SY.orchestrator` procesa por `meta.msg` sin validar explícitamente nombre de origen; el control de acceso depende de router/OPA.
  - queda marcado hardening adicional para validar origen en `SY.orchestrator`.
