# Migracion de offload text/v1 a SDK (NodeSender::send)

## Objetivo

Mover la logica de normalizacion/offload de `text/v1` (inline -> `content_ref` cuando supera limite) desde `io-common` al SDK de Fluxbee, aplicandola en `NodeSender::send` para cobertura inmediata en la mayoria de nodos que ya usan SDK.

Alcance de esta etapa:
- enforcement automatico en `send` para `payload.type == "text"` (contrato `text/v1`),
- mantener compatibilidad con mensaje ya normalizado (`content_ref` valido),
- evitar tocar router.

No alcance de esta etapa:
- normalizacion global de payloads no `text/v1`,
- cambios en router.

---

## Principios de implementacion

1. Punto unico de enforcement: `NodeSender::send`.
2. Contrato canonico: mantener envelope `Message` y normalizar solo `payload` de texto.
3. Idempotencia: si el payload ya viene como `content_ref`, no reprocesar.
4. Rollout seguro: incluir switch de escape temporal para desactivar auto-offload si aparece regresion.
5. `io-common` pasa a delegar en SDK para evitar logica duplicada.

---

## Plan de tareas (tests al final)

## Fase 1 - Diseno tecnico en SDK

- [x] Definir modulo de normalizacion en `crates/fluxbee_sdk` reutilizable desde `NodeSender::send`.
- [x] Definir estructura de config efectiva de offload en SDK:
  - [x] `max_message_bytes` (default contrato).
  - [x] `message_overhead_bytes`.
  - [x] path/config blob runtime.
- [x] Definir precedence de configuracion (runtime/env/default) sin romper comportamiento actual.
- [x] Definir switch de escape temporal (`disable_auto_blob`) para rollback controlado.

Entregable:
- API interna de normalizacion + decision de config documentada en codigo.

## Fase 2 - Enforcement en NodeSender::send

- [x] Integrar normalizacion justo antes de serializar (`serde_json::to_vec`).
- [x] Aplicar solo cuando `payload.type == "text"`.
- [x] Mantener passthrough total para payloads no texto.
- [x] Mantener idempotencia para `content_ref` existente.
- [x] Agregar logs operativos de bajo ruido en SDK:
  - [x] `debug`: decision (`offload_to_blob`, bytes estimados, limite).
  - [x] `info`: solo cuando hay offload real (`blob_name`, `size`).
  - [x] `warn`: fallo de normalizacion + codigo canonico.

Entregable:
- `send` con enforcement activo por default.

## Fase 3 - Ajuste de io-common (correccion de duplicacion)

- [x] Reemplazar normalizacion propia de inbound en `io-common` por delegacion al camino SDK (o dejarla en modo compatibilidad sin duplicar decision).
- [x] Evitar doble procesamiento `io-common` + `NodeSender::send`.
- [x] Mantener resolucion outbound (`content_ref` -> texto) en `io-common` donde corresponde por canal.
- [x] Revisar `io-slack` e `io-sim` para que no conserven offload paralelo ad-hoc.

Entregable:
- una sola fuente real de decision de offload (`send`), sin drift entre capas.

## Fase 4 - Migracion operacional y docs

- [x] Actualizar docs core/IO para dejar explicito que el enforcement esta en SDK (`send`).
- [x] Actualizar docs de IO indicando que `io-common` ya no decide offload por su cuenta.
- [x] Actualizar runbooks de prueba (`io-sim` y `io-slack`) con expectativas nuevas.
- [x] Incluir nota de limitaciones actuales de canal (ej. Slack) en salida exterior.

Entregable:
- documentacion alineada a la arquitectura real post-migracion.

## Fase 5 - Tests (ultimo)

- [ ] Unit tests SDK:
  - [ ] inline bajo limite (sin offload).
  - [ ] inline sobre limite (offload a `content_ref`).
  - [ ] `content_ref` preexistente (idempotente).
  - [ ] payload invalido (error canonico).
- [ ] Tests de integracion IO:
  - [ ] `io-sim` verifica que mensaje grande sale como `content_ref` usando `send`.
- [ ] Smoke E2E:
  - [ ] IO -> AI con texto grande.
  - [ ] verificacion de logs de decision y no regresion en respuestas.
- [ ] Test de escape hatch:
  - [ ] con `disable_auto_blob=true`, confirmar passthrough.

Entregable:
- suite verde con cobertura minima de contrato + regresion.

---

## Riesgos y mitigacion

1. Riesgo: impacto global inesperado por cambiar `send`.
- Mitigacion: escape hatch temporal + logs de decision + rollout gradual.

2. Riesgo: doble offload por coexistencia de logicas.
- Mitigacion: fase explicita de limpieza en `io-common`.

3. Riesgo: diferencias de configuracion por nodo/runtime.
- Mitigacion: precedence unica en SDK y defaults canonicos.

4. Riesgo: canales externos con limites menores al contrato interno.
- Mitigacion: documentar fallback por canal y mantener esta decision fuera de esta etapa.

---

## Criterio de cierre

Se considera cerrado cuando:
- [ ] `NodeSender::send` aplica offload `text/v1` por default.
- [ ] `io-common` no duplica la decision de offload.
- [ ] Docs reflejan el nuevo ownership (SDK send-path).
- [ ] Tests (SDK + integracion + smoke) pasan.
