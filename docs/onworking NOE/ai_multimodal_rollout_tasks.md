# AI Multimodal Rollout - Task List (AI.common first)

Objetivo: habilitar procesamiento multimodal en nodos AI de forma segura, empezando por `AI.common`, manteniendo compatibilidad con contrato Fluxbee (`text/v1`, `BlobRef`, errores canonicos).

## Principio de implementacion (obligatorio)

- [ ] P0.0 Basar la implementacion multimodal en OpenAI vigente (Agents SDK + Responses API), evitando diseno ad-hoc local cuando ya exista contrato estandar.
- [ ] P0.1 Cualquier desvio respecto al modelo OpenAI debe documentarse con motivo tecnico (compatibilidad Fluxbee, seguridad, costo o latencia).
- [ ] P0.2 Mantener separacion de capas:
  - Fluxbee define contrato de transporte/config/error canonico.
  - La adaptacion multimodal sigue el esquema OpenAI (input parts, retries, tool/use patterns).

## Fuente OpenAI obligatoria para esta fase

- [x] P0.3 Fuente normativa: `openai/openai-agents-python`.
- [x] P0.4 Alineacion minima con Agents SDK (segun `src/agents/items.py`):
  - `input_text`
  - `input_image` (`image_url | file_id`, y `detail`)
  - `input_file` (`file_data | file_url | file_id`, y `filename`)
- [ ] P0.5 Si el flujo Fluxbee actual degrada adjuntos a texto donde OpenAI espera parts estructurados, corregir antes de cerrar rollout.

## Alcance

- Incluye: `fluxbee_ai_sdk`, runners `AI.common` / `SY.frontdesk.gov` (wiring/config), `IO.slack` (UX de error outbound), docs AI.
- Excluye por ahora: cambios de producto gov especificos (solo dejar preparado para decision futura).

## Fase 0 - Definiciones de contrato (bloqueante)

- [x] M0.1 Cerrar spec: `AI.common` con `multimodal=true` por default.
- [x] M0.2 Cerrar spec: `SY.frontdesk.gov` default temporal (`false`) con override via config.
- [x] M0.3 Cerrar matriz MIME inicial:
  - aceptar en multimodal: `image/png`, `image/jpeg`, `image/webp`
  - mantener textuales: `text/plain`, `text/markdown`, `application/json`
  - `application/pdf`: pendiente de estrategia (`input_file` vs extractor)

## Fase 1 - Config/runtime flags por nodo

- [x] M1.1 Soportar `behavior.capabilities.multimodal` en config efectiva (`CONFIG_SET` + bootstrap/persisted).
- [x] M1.2 Aplicar defaults por runtime:
  - `AI.common`: `multimodal=true`
  - `SY.frontdesk.gov`: `multimodal=false`
- [x] M1.3 Exponer en `CONFIG_GET` el valor efectivo de `multimodal`.

## Fase 2 - SDK AI: input multimodal estructurado

- [x] M2.1 Extender `fluxbee_ai_sdk::text_payload` para producir estructura de input multimodal (no solo `String`).
- [x] M2.2 Mantener compatibilidad textual.
- [x] M2.3 Mantener errores canonicos (`unsupported_attachment_mime`, `BLOB_*`, `too_many_attachments`).
- [x] M2.4 Agregar tests de contrato en SDK AI para mezcla texto+imagen.

## Fase 3 - Adapter OpenAI multimodal

- [x] M3.1 Adaptar request builder OpenAI para incluir parts multimodales usando blobs resueltos.
- [x] M3.2 Mantener fallback textual cuando `multimodal=false`.
- [x] M3.3 Mapear errores del proveedor a errores canonicos del nodo cuando aplique.
- [x] M3.4 Tests de adapter (payload mixto y validacion de formato enviado).
- [x] M3.5 Corregir gap actual: con tools habilitadas (function-calling), la imagen no puede entrar solo como texto ensamblado.
- [x] M3.6 Extender function-calling para soportar usuario multimodal (parts) sin perder tools.
- [x] M3.7 Soportar `detail` en `input_image` (default configurable y override futuro).
- [x] M3.8 Incorporar `input_file` en adapter para adjuntos no imagen cuando corresponda al modelo.

## Fase 4 - Runner wiring

- [x] M4.1 `AI.common`: usar camino multimodal del SDK segun config efectiva.
- [x] M4.2 `SY.frontdesk.gov`: mismo wiring, respetando default/override.
- [x] M4.3 Evitar logica duplicada fuera del SDK AI.
- [x] M4.4 Regla explicita:
  - si hay adjunto no textual y `multimodal=true`, no degradar a texto en modo tools
  - garantizar envio de `input_image` / `input_file` al provider
- [x] M4.5 Mantener compatibilidad textual pura para casos sin adjuntos o `multimodal=false`.

## Fase 5 - IO/Slack UX de errores

- [ ] M5.1 En outbound Slack, si `payload.type="error"`, renderizar mensaje funcional (`code` + texto amigable).
- [ ] M5.2 Evitar fallback generico `invalid_text_payload` cuando el error original ya es canonico.
- [ ] M5.3 Mantener logging tecnico completo para debug (sin exponer sensibles al usuario final).

## Fase 6 - Validacion E2E

- [ ] M6.1 E2E: Slack imagen -> AI.common responde util (sin `invalid_text_payload` generico).
- [ ] M6.2 E2E: Slack txt/json sigue funcionando.
- [ ] M6.3 E2E: MIME no permitido devuelve error canonico correcto.
- [ ] M6.4 E2E: nodo sigue `active/running` ante errores de adjuntos.
- [ ] M6.5 E2E: imagen + tools habilitadas (`src_ilk` presente) usa vision real, no inferencia textual del nombre de archivo.
- [ ] M6.6 E2E: adjunto no imagen recorre camino `input_file` cuando aplica.

## Fase 7 - Docs y rollout

- [ ] M7.1 Actualizar `docs/AI_nodes_spec.md` con estado final multimodal.
- [ ] M7.2 Actualizar runbook de deploy/config con ejemplos `multimodal=true/false`.
- [ ] M7.3 Definir rollout incremental (sandbox -> staging -> prod) y senales de observabilidad.

## Orden recomendado desde ahora

1. M3.5 + M3.6 (cerrar imagen real en function-calling)
2. M3.7 + M3.8 (alineacion completa de adjuntos con Agents SDK)
3. M4.x
4. M5.x
5. M6.x
6. M7.x

## Referencia obligatoria

- Repo base: https://github.com/openai/openai-agents-python
- Punto clave usado para contratos de adjuntos: `src/agents/items.py`
