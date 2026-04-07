# IO text/v1 > limite (64KB) - Estado y pendientes

Fecha: 2026-04-06
Estado: parcial, con ownership ya cerrado

## Objetivo

Garantizar que en nodos `IO.*` el manejo de `text/v1` con texto grande sea consistente y no dependa de implementaciones ad-hoc por adapter:

- si el payload inline supera el limite, convertir automaticamente a `content_ref`
- mantener contrato canonico `text/v1`
- centralizar el comportamiento en un solo send-path

## Ownership real actual

El ownership efectivo del offload por tamano ya quedo en:

- `fluxbee_sdk::NodeSender::send`, para mensajes `payload.type="text"` que cumplen contrato `text/v1`

Consecuencia:

- `io-common` no debe decidir por su cuenta `content` vs `content_ref`
- los adapters `IO.*` no deben reimplementar esa decision
- `io-common` queda para:
  - validaciones comunes de `text/v1`
  - helpers de construccion de payload base
  - resolucion outbound de `content_ref` / `attachments`

## Estado actual

### Cerrado

- `NodeSender::send` normaliza `text/v1` antes de serializar.
- el corte real usa `BlobToolkit::build_text_v1_payload_with_limit(...)` dentro de esa normalizacion.
- `send_normalization` ya tiene tests unitarios base para:
  - inline bajo limite
  - offload sobre limite
  - `content_ref` preexistente
- `IO.slack` envia al router via `sender.send(msg)`.
- `IO.sim` envia al router via `sender.send(msg)`.
- el smoke Linux real para `>64KB` ya fue validado con `io-sim`.
- `docs/io/io-common.md`, `docs/io/io-slack.md` y `docs/io/io-slack-deploy-runbook.md` ya reflejan que el ownership del offload esta en SDK.

### Abierto

- mas tests de integracion `io-common` / `IO.slack`
- volver este comportamiento requisito explicito para cualquier adapter IO nuevo

## Nota operativa de validacion

La validacion `>64KB` debe considerarse cerrada cuando se hace con `io-sim`.

Motivo:

- Slack no es un canal valido para ese umbral, porque `chat.postMessage` tiene limites propios y trunca texto antes de servir como prueba confiable del contrato interno.
- Por eso, la evidencia correcta para este punto es:
  - test unitario del SDK
  - smoke Linux con `io-sim`

## Riesgo residual

El riesgo principal ya no es `IO.slack`, sino:

- adapters IO futuros que construyan mensajes y no usen `NodeSender::send`
- documentacion vieja que siga sugiriendo ownership en `io-common`

## Criterio de cierre

Este tema queda realmente cerrado cuando:

1. hay smoke Linux `>64KB`
2. no quedan docs activas que atribuyan el offload a `io-common`
3. el requisito "todo envio `text/v1` al router pasa por `NodeSender::send`" queda asentado como contrato de adopcion para `IO.*`
