# Estado de Adjuntos AI vs Agents SDK

Fecha: 2026-04-01  
Alcance: `AI.common` / `AI.frontdesk.gov` + `fluxbee_ai_sdk` (ingesta y envío a OpenAI Responses).

## Objetivo de este documento

Dejar explícito, contra el modelo de `openai/openai-agents-python`, qué está implementado hoy y qué falta en nodos AI para procesamiento de archivos/adjuntos.

## Contrato objetivo (Agents SDK / Responses)

Tipos de content de usuario relevantes:

1. `input_text`
2. `input_image`
3. `input_file`

Variantes de transporte esperables:

1. `input_image.image_url`
2. `input_image.file_id`
3. `input_image.detail`
4. `input_file.file_data`
5. `input_file.file_id`
6. `input_file.file_url`
7. `input_file.filename`

## Estado actual de implementación (AI)

### Soportado hoy

1. `input_text`: soportado.
2. `input_image.image_url`: soportado (data URL base64).
3. `input_image.detail`: soportado (default `auto`, override en SDK).
4. `input_file.file_data`: implementado en SDK (enviar base64 desde blob local).
5. `input_file.filename`: implementado.
6. multimodal con tools activadas: soportado (ya no degrada imagen a texto en function-calling).

### Parcial / con incidencia abierta

1. `input_file.file_data` con PDF:
   - estado: implementado pero con error observado en provider (`400 invalid_value` sobre `file_data`).
   - conclusión: falta ajustar formato final exacto del campo para compatibilidad completa.

### No implementado aún

1. `input_image.file_id`: no implementado.
2. `input_file.file_id`: no implementado.
3. `input_file.file_url`: no implementado.
4. estrategia de upload previo a `/files` + referenciado por `file_id`: no implementada en nodos AI.

## Compatibilidad por clase de archivo (hoy)

1. Texto (`text/plain`, `text/markdown`, `application/json`):
   - camino actual: texto embebido en contexto.
   - estado: funcional.
2. Imagen (`image/png`, `image/jpeg`, `image/webp`):
   - camino actual: `input_image.image_url`.
   - estado: funcional, validado en E2E.
3. PDF (`application/pdf`):
   - camino actual: `input_file.file_data`.
   - estado: en progreso (error 400 actual).
4. Otros binarios (docx, xlsx, etc.):
   - camino actual: mapeados a `input_file.file_data`.
   - estado: sin validación E2E formal.

## Evidencia concreta más reciente

1. Imagen + tools activadas: respuesta correcta sobre contenido visual.
2. PDF: provider devolvió `400 invalid_value` para `input[...].content[...].file_data`.

## Qué falta para “alineado a Agents SDK”

1. Cerrar formato final de `input_file.file_data` para que PDF funcione estable.
2. Incorporar estrategia `file_id` (upload y reuso) para `input_image`/`input_file`.
3. Incorporar `file_url` cuando el flujo lo requiera.
4. Definir política de selección de transporte por tamaño/tipo:
   - cuándo usar `file_data`,
   - cuándo promover a `file_id`,
   - cuándo usar `file_url`.
5. Ejecutar batería E2E por tipo:
   - png/jpg/webp
   - pdf
   - json/txt
   - al menos un binario no-PDF.

## Nota separada (no bloqueante para capacidad de archivo, pero importante)

`IO.slack` hoy no renderiza `payload.type="error"` y puede mostrar `invalid_text_payload`.  
Esto afecta UX de error, pero no cambia el estado real de soporte de formatos del agente.
