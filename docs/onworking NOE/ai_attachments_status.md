# Estado de Adjuntos AI vs Agents SDK

Fecha: 2026-04-06
Alcance: `AI.common` / `AI.frontdesk.gov` + `fluxbee_ai_sdk` (ingesta y envio a OpenAI Responses).

## Objetivo de este documento

Dejar explicito, contra el modelo de `openai/openai-agents-python`, que esta implementado hoy y que falta en nodos AI para procesamiento de archivos/adjuntos.

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

Nota de alcance:

- en el Agents SDK local usado como referencia, `input_file` expone `file_data`, `file_url` y `file_id`
- en este repo no aparece una lista exhaustiva de MIME "validos por Agents SDK" para `file_data`
- por eso, este documento separa:
  - modo implementado
  - evidencia E2E concreta por familias de archivo
  - y pendientes de validacion

## Estado actual de implementacion (AI)

### Soportado hoy

1. `input_text`: soportado.
2. `input_image.image_url`: soportado (data URL base64).
3. `input_image.detail`: soportado (default `auto`, override en SDK).
4. `input_file.file_data`: implementado en SDK como `data:<mime>;base64,...` desde blob local.
5. `input_file.filename`: implementado.
6. multimodal con tools activadas: soportado (ya no degrada imagen a texto en function-calling).
7. pre-validacion minima de adjuntos archivo antes de construir `input_file`: implementada.
   - valida `mime`
   - valida `filename` efectivo
8. rechazo de provider sobre params de attachment mapeado a error canonico mas especifico:
   - `provider_attachment_invalid_request`

### Parcial / con incidencia abierta

1. `input_file.file_data` con PDF:
   - estado: implementado con `data:application/pdf;base64,...`
   - conclusion: el formato local ya quedo alineado al camino usado por Agents SDK/examples, pero todavia falta validacion E2E estable contra provider/modelo antes de darlo por cerrado en una matriz mas amplia.

### No implementado aun

1. `input_image.file_id`: no implementado.
2. `input_file.file_id`: no implementado.
3. `input_file.file_url`: no implementado.
4. estrategia de upload previo a `/files` + referenciado por `file_id`: no implementada en nodos AI.
5. selector final `file_data` vs `file_id` vs `file_url`: no implementado.
   - queda sujeto a contrato Fluxbee/Core

## Compatibilidad por clase de archivo (hoy)

1. Texto (`text/plain`, `text/markdown`, `application/json`):
   - camino actual: texto embebido en contexto.
   - estado: funcional.
2. Imagen (`image/png`, `image/jpeg`, `image/webp`):
   - camino actual: `input_image.image_url`.
   - estado: funcional, validado en E2E.
3. PDF (`application/pdf`):
   - camino actual: `input_file.file_data`.
   - estado: implementado con formato canonico local.
   - evidencia actual:
     - validacion Linux del 2026-04-06 desde Slack hasta `AI.common` con respuesta util y sin `provider_invalid_request` ni `provider_attachment_invalid_request`.
   - pendiente:
     - cierre E2E estable contra la matriz provider/modelo soportada.
4. Otros binarios (docx, xlsx, etc.):
   - camino actual: `input_file.file_data`.
   - estado: mapeo implementado.
   - evidencia actual:
     - `xlsx` validado E2E por `Slack -> IO.slack -> AI.common` el 2026-04-06.
     - `docx` validado E2E por `Slack -> IO.slack -> AI.common` el 2026-04-06.
   - pendiente:
     - mantener cobertura representativa por familias de archivos no imagen, no exhaustiva por cada MIME posible.
5. Audio y otros adjuntos no imagen/no documento:
   - camino actual esperado: `input_file.file_data`.
   - estado: no probado en validacion E2E asentada en este documento al 2026-04-06.
   - pendiente:
     - cubrir al menos formatos de audio representativos y otros binarios relevantes segun el alcance real del canal/provider.

## Evidencia concreta mas reciente

1. Imagen + tools activadas: respuesta correcta sobre contenido visual.
2. El SDK AI ahora serializa `input_file.file_data` como `data:<mime>;base64,...` y no como base64 pelado.
3. Si el provider rechaza params de attachment, el runner devuelve `provider_attachment_invalid_request` en vez de caer solo en `provider_invalid_request`.
4. PDF llego a `AI.common` desde Slack y respondio sin errores canonicos de provider en validacion Linux del 2026-04-06.
5. `xlsx` llego a `AI.common` desde Slack en validacion Linux del 2026-04-06 despues de ampliar la allowlist efectiva de `IO.slack`.

## Limite actual de evidencia

Lo validado hasta ahora demuestra que el pipeline funciona para algunos tipos concretos, pero no cierra todavia toda la matriz de adjuntos representables.

Evidencia E2E concreta hoy:

1. imagen
2. PDF
3. `xlsx`
4. `docx`

Todavia faltan pruebas asentadas para clases adicionales, por ejemplo:

1. audio representativo (no probado)
2. alguna clase adicional de binario no imagen fuera de office si se la considera relevante
3. combinaciones de multiples adjuntos

## Que falta para "alineado a Agents SDK"

1. Validar E2E que `input_file.file_data` quede estable para PDF en los modelos/providers objetivo.
2. Incorporar estrategia `file_id` (upload y reuso) para `input_image`/`input_file` si Core define metadata suficiente.
3. Incorporar `file_url` cuando el flujo lo requiera y Core defina trust/metadata suficiente.
4. Definir politica de seleccion de transporte por tamano/tipo:
   - cuando usar `file_data`
   - cuando promover a `file_id`
   - cuando usar `file_url`
5. Ejecutar bateria E2E por tipo:
   - familias representativas, no una matriz exhaustiva de todos los MIME posibles
   - imagen
   - pdf
   - texto estructurado/simple cuando aplique
   - office no imagen (`xlsx`/`docx`) ya cubierto
   - audio representativo (no probado)
   - mezcla de adjuntos

## Que quedo explicitamente fuera de alcance local

1. `file_id` y `file_url` no se cierran solo desde AI si Fluxbee no transporta metadata suficiente para representarlos de forma canonica.
2. Esos modos deben documentarse como diferidos por contrato/Core, no como "rechazados por producto".

## Nota separada (no bloqueante para capacidad de archivo, pero importante)

`IO.slack` hoy no renderiza `payload.type="error"` y puede mostrar `invalid_text_payload`.
Esto afecta UX de error, pero no cambia el estado real de soporte de formatos del agente.
