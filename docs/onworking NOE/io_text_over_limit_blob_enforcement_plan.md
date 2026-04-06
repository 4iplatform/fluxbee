# IO text/v1 > limite (64KB) - Plan de implementacion

## Objetivo

Garantizar que en nodos IO el manejo de `text/v1` con texto grande sea consistente y no dependa de implementaciones ad-hoc por adapter:

- si el payload inline supera el limite (default `64KB`), convertir automaticamente a `content_ref` (blob),
- mantener contrato canonico `text/v1`,
- centralizar el comportamiento en `io-common` para que quede reusable y obligatorio en la practica.

## Estado de avance

- [x] Fase 1 (enforcement funcional en `io-common`) implementada.
- [x] Fase 2 (alineacion `io-slack` para delegar decision `content` vs `content_ref`) implementada.
- [x] Documentación base actualizada (`docs/io/io-common.md`, `docs/io/io-slack.md`, `docs/io/io-slack-deploy-runbook.md`).
- [x] Documentos onworking alineados (`io_slack_blob_tasks.md`, `t6_4_io_slack_e2e_steps.md`).
- [ ] Validación en entorno Linux (cargo check/tests + smoke >64KB).

---

## Estado actual (base)

- El SDK ya soporta offload automatico (`build_text_v1_payload_with_limit`).
- `io-common` ya tiene helpers (`text_v1_blob.rs`) para:
  - construir inbound (`content` vs `content_ref`),
  - resolver outbound (`content_ref` -> texto),
  - validar limites/mime/adjuntos.
- `IO.slack` usa esos helpers.
- Riesgo actual: que futuros/adicionales adapters IO no usen este camino y queden inconsistentes.

---

## Plan por fases

## Fase 0 - Contrato y decision tecnica (corta)

1. Definir que la normalizacion de `text/v1` en inbound IO es responsabilidad de `io-common` (no de cada adapter).
2. Definir que todo adapter IO debe pasar por ese paso comun antes de `process_inbound`.
3. Definir politica de errores:
   - errores tecnicos de blob/limite => codigos canonicos (`BLOB_*`, `invalid_text_payload`, etc.),
   - sin panics ni bypass silencioso.

Entregable:
- criterio escrito y aprobado en docs IO.

## Fase 1 - Refactor minimo en io-common (enforcement funcional)

1. Agregar en `io-common` un paso comun de normalizacion inbound `text/v1`:
   - input: payload candidato (`serde_json::Value`) + config blob,
   - output: payload normalizado (`content` inline o `content_ref` si excede).
2. Integrar ese paso en el pipeline inbound comun (previo a forward al router).
3. Mantener passthrough para payloads no `text`.
4. Evitar doble procesamiento si un adapter ya envio `content_ref` valido.

Criterio de aceptacion:
- el offload por tamaño se aplica desde capa comun sin logica duplicada por adapter.

## Fase 2 - Ajuste de adapters IO (alineacion)

1. `IO.slack`:
   - simplificar builder local para delegar normalizacion a `io-common`,
   - mantener solo mapeo de evento Slack -> payload base.
2. `IO.sim`:
   - mantener modo de simulacion manual,
   - agregar opcion de probar camino comun (casos >64KB) para test funcional.
3. Validar que no haya otros adapters IO con bypass.

Criterio de aceptacion:
- adapters IO no deciden por su cuenta la regla de offload por tamaño.

## Fase 3 - Logging operativo (claro, no invasivo)

Agregar logs estructurados de bajo ruido en el paso comun:

1. Evento de decision por mensaje (`debug`):
   - `text_v1_normalized=true`,
   - `offload_to_blob=true|false`,
   - `inline_bytes`,
   - `estimated_total_bytes`,
   - `max_message_bytes`,
   - `has_attachments`,
   - `trace_id`, `node_name`.
2. Evento de offload efectivo (`info` solo cuando offload=true):
   - `content_ref.blob_name`,
   - `content_ref.size`,
   - `reason="message_over_limit"`.
3. Evento de error (`warn`):
   - `canonical_code`,
   - `error_detail` resumido,
   - `trace_id`, `node_name`.

Regla de ruido:
- no loggear contenido de usuario,
- no loggear blobs completos,
- no repetir logs por retries internos salvo fallo final.

## Fase 4 - Tests

1. `io-common`:
   - texto chico queda inline,
   - texto grande offload a `content_ref`,
   - payload invalido -> error canonico,
   - limite configurable (`max_message_bytes`, `message_overhead_bytes`).
2. `io-slack`:
   - inbound con texto > limite termina en `content_ref`,
   - outbound resuelve `content_ref` correctamente.
3. E2E smoke:
   - mensaje real >64KB desde IO -> AI (o destino configurable),
   - verificar que no falle por limite y llegue por blob.

---

## Actualizacion de documentacion (obligatoria)

1. `docs/io/io-common.md`
   - documentar normalizacion comun de `text/v1`,
   - documentar regla de offload automatico por limite.
2. `docs/io/io-slack.md` (o doc operativo equivalente)
   - aclarar que `IO.slack` delega offload a `io-common`,
   - listar knobs de config `io.blob.*`.
3. `docs/onworking NOE/t6_4_io_slack_e2e_steps.md`
   - agregar check explicito para caso >64KB y verificacion de `content_ref`.
4. (Opcional recomendado) `docs/onworking NOE/io_slack_blob_tasks.md`
   - agregar nota de “enforcement comun” para evitar drift futuro.

---

## Orden recomendado de ejecucion

1. Fase 0 (contrato).
2. Fase 1 (enforcement en `io-common`).
3. Fase 3 (logging comun) en paralelo con Fase 1/2.
4. Fase 2 (alineacion adapters).
5. Fase 4 (tests).
6. Actualizacion de documentacion.

---

## Riesgos y mitigacion

- Riesgo: doble offload o comportamiento distinto entre adapters.
  - Mitigacion: un unico punto comun de normalizacion + tests de regresion.
- Riesgo: exceso de logs.
  - Mitigacion: decision en `debug`, offload en `info` solo cuando aplica, errores en `warn`.
- Riesgo: cambios de contrato no detectados.
  - Mitigacion: tests de contrato `text/v1` y validacion e2e con payload >64KB.
