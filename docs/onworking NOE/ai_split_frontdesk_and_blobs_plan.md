# Plan Integrado - Split AI/Frontdesk + Blobs en AI Nodes

Fecha: 2026-03-31
Estado: propuesta para ejecución por etapas

## 1. Objetivo

Resolver dos lí­neas crí­ticas de trabajo en AI Nodes sin retrabajo:

1. separar `AI.common` y `SY.frontdesk.gov` como runtimes con ownership y responsabilidad distinta;
2. integrar contrato de blobs (`text/v1`: `content_ref` + `attachments`) en AI Nodes.

Este plan asume la directriz actual:
- `SY.frontdesk.gov` no debe depender de `nodes/ai/common`;
- frontdesk/gov debe apoyarse en `nodes/gov/common` + SDKs.

## 2. Estrategia de orden (recomendada)

Orden recomendado:

1. **Primero split AI/frontdesk** (frontera de arquitectura y deploy).
2. **Después blobs** (sobre capa estable, evitando implementaciones duplicadas).

Motivo:
- Si se implementan blobs antes del split, se corre riesgo alto de duplicar o mover código luego.
- Si se fija primero la frontera, blobs se implementa una vez en la capa compartida correcta.

## 3. Alcance por capa

### 3.1 `AI.common` (familia AI no-gov)

- Runtime base de nodos `AI.*` comunes.
- Sin tools/flujo gov.
- Sin flags de modo `gov`.

### 3.2 `SY.frontdesk.gov` (familia gov)

- Runtime especializado para frontdesk.
- Prompt/flujo funcional propio de frontdesk.
- Tools de identidad requeridas para frontdesk (`ILK_REGISTER`, `ILK_ADD_CHANNEL`, etc., según contrato vigente).

### 3.3 Compartido

- SDKs (`fluxbee_sdk`, `fluxbee_ai_sdk`).
- `nodes/gov/common` como shared layer de gov.
- `nodes/ai/common` queda para familia AI común (no-gov).

## 4. Backlog propuesto

## Fase A - Congelar frontera de arquitectura

- [x] A1. Documentar oficialmente la frontera:
  - `AI.common` no consume `nodes/gov/common`.
  - `SY.frontdesk.gov` no consume `nodes/ai/common`.
- [x] A2. Declarar deprecado el uso de `AI_NODE_MODE=gov` para distinguir runtimes.
- [x] A3. Definir contrato de prompts frontdesk:
  - prompts/versionado ligados al runtime `SY.frontdesk.gov` (no mutación ad-hoc por chat).

## Fase B - Split de runtime/deploy (sin cambiar features)

- [x] B1. Separar publicación/deploy:
  - quitar dependencia funcional de `--mode default|gov` en scripts de AI.
  - publicar `AI.common` y `SY.frontdesk.gov` por su propio paquete/comportamiento.
- [x] B2. Quitar código gov del runner común:
  - remover registro de tools gov en `AI.common`.
  - remover wiring de bridge de identidad gov en `AI.common`.
- [ ] B3. Mantener comportamiento actual en frontdesk:
  - [x] B3.a. Forzar runtime `SY.frontdesk.gov` a perfil gov por defecto (sin depender del flag para seleccionar perfil).
  - [x] B3.b. Migrar lógica compartible hacia `nodes/gov/common` y eliminar duplicación residual.
- [ ] B4. Tests de no-regresión del split:
  - `AI.common` sin capacidad gov.
  - `SY.frontdesk.gov` mantiene capacidades de identidad esperadas.

## Fase C - Integración de blobs en AI Nodes

- [x] C1. Extender input path de AI para `text/v1` completo:
  - `content` inline,
  - `content_ref` (resolver blob),
  - `attachments[]` (resolver blobs).
- [x] C2. Integrar `fluxbee_sdk::blob` sin redefinir contrato local.
  - centralizado en `fluxbee_ai_sdk::text_payload::build_model_input_from_payload` para evitar duplicación en runners.
- [x] C3. Aplicar límites de tamaño/MIME y códigos canónicos:
  - `BLOB_NOT_FOUND`,
  - `BLOB_IO_ERROR`,
  - `BLOB_TOO_LARGE`,
  - `unsupported_attachment_mime` (si corresponde).
- [x] C4. Definir salida AI con blobs:
  - cuando contenido supere inline, offload a blob + `content_ref`.
  - si hay adjuntos generados, publicar + `attachments[]`.
- [x] C5. Tests de contrato:
  - parsing `text/v1` con `attachments/content_ref`,
  - errores blob canónicos,
  - retry path con `resolve_with_retry` donde aplique.

## Fase D - Limpieza final y docs

- [x] D1. Remover remanentes `mode=gov` en docs/runbooks AI.
- [x] D2. Actualizar runbooks de deploy:
  - `AI.common` y `SY.frontdesk.gov` como caminos separados.
- [x] D3. Actualizar spec AI con estado final de blobs implementado.
- [ ] D4. Cerrar checklist de handoff frontdesk (si owner externo sigue vigente).

## 5. Criterios de done por lí­nea

Split AI/frontdesk cerrado cuando:
- `AI.common` no contiene ni expone lógica gov;
- `SY.frontdesk.gov` funciona sin depender de `nodes/ai/common`;
- deploy/publicación no requieren `mode=gov` para distinguir runtimes.

Blobs AI cerrados cuando:
- AI consume `text/v1` con `content_ref/attachments`;
- usa `fluxbee_sdk::blob` end-to-end;
- devuelve errores canónicos `BLOB_*` y pasa tests de contrato.

## 6. Orden de ejecución sugerido (operativo)

1. Fase A completa
2. Fase B completa
3. Fase C completa
4. Fase D completa

Notas:
- evitar mezclar B y C en el mismo PR grande;
- priorizar PRs pequeños con validación de contrato por etapa.
