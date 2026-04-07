# IO Slack + Blob - Estado consolidado

Fecha: 2026-04-06
Estado: parcial, con implementacion base cerrada

## Objetivo

Tener `IO.slack` alineado con `text/v1` canonico:

- `content`
- `content_ref`
- `attachments[]`

usando helpers compartidos y sin decisiones ad-hoc por adapter para el offload de texto grande.

## Estado actual

### Cerrado

- `IO.slack` descarga adjuntos inbound desde Slack, los promueve a blob y envia `BlobRef` al router.
- `IO.slack` resuelve `content_ref` y `attachments[]` en outbound hacia Slack.
- el offload de texto `text/v1` por tamano ya no depende de `IO.slack`.
- `IO.slack` envia al router por `NodeSender::send`, por lo que hereda la normalizacion/offload del SDK.
- `IO.slack` ya tiene una allowlist efectiva propia para:
  - imagenes
  - PDF
  - texto simple/markdown/json
  - office comun (`csv`, `doc/docx`, `xls/xlsx`, `ppt/pptx`)

### Aclaracion importante

El ownership actual del offload `content` vs `content_ref` es:

- `fluxbee_sdk::NodeSender::send`

No es ownership actual de:

- `IO.slack`
- `io-common::InboundProcessor`

`io-common` sigue siendo responsable de:

- helpers `text_v1_blob`
- validaciones comunes
- resolucion outbound de blobs

## Pendientes reales

- tests de integracion adicionales de `IO.slack`
- smoke Linux real para payload `text/v1` sobre `64KB`
- cierre documental final de adopcion para futuros adapters IO

## Nota de alcance

Que `IO.slack` este alineado con blobs no implica:

- que acepte todos los formatos que Slack pueda transportar
- ni que cualquier adapter futuro herede automaticamente esta disciplina

Eso sigue dependiendo de:

- la policy efectiva del adapter
- y de que el adapter use `NodeSender::send` como camino obligatorio de envio al router
