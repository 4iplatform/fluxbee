# IO / AI Open Topics - Estado Actual y Backlog Consolidado

Fecha: 2026-04-06
Owner: NOE
Scope: temas abiertos sobre blobs/attachments y spawn/config en nodos `IO.*` y `AI.*`

## Objetivo

Consolidar en un solo lugar:

1. el estado actual real (docs + codigo),
2. las tareas que ya quedaron hechas,
3. las tareas que siguen abiertas para cerrar completamente los temas pendientes,
4. la relacion con documentos `onworking NOE` ya existentes y con la referencia local `agents-sdk-python`.

## Fuentes base tomadas para este consolidado

Core / contrato:

- `docs/02-protocolo.md`
- `docs/blob-annex-spec.md`
- `docs/node-spawn-config-spec.md`
- `docs/node-config-control-plane-spec.md`
- `docs/runtime-lifecycle-spec.md`
- `docs/io/nodos_io_spec.md`
- `docs/io/io-common.md`
- `docs/AI_nodes_spec.md`

Onworking NOE relevantes:

- `docs/onworking NOE/io_slack_blob_tasks.md`
- `docs/onworking NOE/io_text_over_limit_blob_enforcement_plan.md`
- `docs/onworking NOE/io_text_v1_sdk_send_enforcement_plan.md`
- `docs/onworking NOE/io_nodes_config_control_plane_migration_tasks.md`
- `docs/onworking NOE/ai_attachments_status.md`
- `docs/onworking NOE/ai_attachments_router_processing_plan.md`
- `docs/onworking NOE/ai_multimodal_rollout_tasks.md`
- `docs/onworking NOE/ai_nodes_v14_spawn_json_plan.md`
- `docs/onworking NOE/core_ai_node_state_cleanup_gap.md`
- `docs/onworking NOE/ai_chat_slack_blob_e2e.md`
- `docs/onworking NOE/t6_4_io_slack_e2e_steps.md`

Referencia local obligatoria para lectura de archivos / attachments en AI:

- `agents-sdk-python/src/agents/items.py`
- `agents-sdk-python/docs/realtime/guide.md`

## Referencia Agents SDK que queda congelada para attachments AI

Segun `agents-sdk-python/src/agents/items.py`, el modelo relevante a respetar en Fluxbee es:

- `input_text`
- `input_image`
  - `image_url`
  - `file_id`
  - `detail`
- `input_file`
  - `file_data`
  - `file_url`
  - `file_id`
  - `filename`

Interpretacion adoptada para Fluxbee:

- Fluxbee define transporte y persistencia local (`text/v1` + `BlobRef`).
- AI debe mapear ese contrato al modelo OpenAI vigente sin inventar formatos ad-hoc locales cuando ya exista modo canonico en Agents SDK / Responses.
- No todos los modos del Agents SDK son representables hoy con el transporte Fluxbee actual.
- Este backlog distingue explicitamente entre:
  - alcance actual cerrable del lado `IO/AI`
  - definiciones pendientes de `CORE`
  - modos del Agents SDK que hoy quedan fuera de scope implementable por contrato/transporte actual

Modos hoy compatibles con `BlobRef` local y por lo tanto dentro del scope actual de cierre en `IO/AI`:

- `input_text`
- `input_image.image_url`
- `input_image.detail`
- `input_file.file_data`
- `input_file.filename`

Nota de alcance E2E actual:

- que `AI` soporte un modo Agents SDK no implica que hoy llegue por todos los adapters `IO`
- validacion Linux real del 2026-04-06 mostro:
  - `png/jpg` llegan a `AI.common`
  - `pdf` llega a `AI.common`
  - inicialmente `xlsx` no llegaba a `AI.common` porque `IO.slack` lo descartaba inbound con `unsupported_attachment_mime`
- despues de esa validacion, `IO.slack` paso a definir una allowlist efectiva propia mas amplia para office comunes
- revalidacion Linux posterior del 2026-04-06 confirmo que `xlsx` ya llega a `AI.common` por Slack con esa allowlist actualizada
- por lo tanto, el cierre del bloque `AI` se interpreta como:
  - soporte correcto cuando el attachment ya llego a `AI` via `BlobRef`
  - no como garantia E2E por cualquier adapter/canal

Modos no cerrados hoy, separados por tipo de dependencia:

Pendientes dentro del scope actual `IO/AI`:

- `input_file.file_data` para `application/pdf` con compatibilidad real de provider
- tabla/politica explicita de seleccion entre `file_data` vs otros modos cuando exista mas de una opcion valida
- E2E por tipo de attachment usando el contrato Fluxbee actual

Fuera de scope actual de implementacion puramente `IO/AI` o diferidos por contrato/transporte:

- `input_image.file_id`
- `input_file.file_id`
- `input_file.file_url`

## Estado consolidado por tema

### 1. IO attachments / blobs

Estado actual:

- `IO.slack` ya puede recibir adjuntos externos, promoverlos a blob y enviar `BlobRef` al router.
- `IO.slack` ya puede recibir desde router `content_ref` y `attachments[]`, resolverlos y publicar texto + archivos en Slack.
- El offload automatico de texto `text/v1` sobre limite ya vive en `fluxbee_sdk::NodeSender::send`.
- `io-common` conserva la logica de resolucion outbound (`content_ref` -> texto y `attachments[]` -> paths) y validaciones de payload.

Estado real: `PARCIALMENTE CERRADO`

Lo que ya esta cerrado:

- pipeline funcional `Slack -> blob -> router`
- pipeline funcional `router -> blob -> Slack`
- ownership correcto del offload `content` vs `content_ref` en SDK send-path

Lo que falta para darlo por realmente cerrado:

- forzar como contrato que todo envio `text/v1` al router pase por `fluxbee_sdk::NodeSender::send` y no por caminos alternativos
- pruebas de contrato y smoke Linux/E2E pendientes
- extender el criterio a cualquier adapter IO nuevo, no solo `IO.slack`
- definir si el cleanup/reinstall limpio debe invalidar tambien el estado dinamico IO persistido

Documentos fuente:

- `docs/onworking NOE/io_slack_blob_tasks.md`
- `docs/onworking NOE/io_text_over_limit_blob_enforcement_plan.md`
- `docs/onworking NOE/io_text_v1_sdk_send_enforcement_plan.md`
- `docs/onworking NOE/io_nodes_config_control_plane_migration_tasks.md`

### 2. AI attachments / blobs

Estado actual:

- AI ya resuelve `content` y `content_ref`.
- AI ya resuelve `attachments[]` como blobs locales.
- Texto, imagen (`image/png`, `image/jpeg`, `image/webp`) y binarios no imagen ya tienen mapeo de SDK.
- En multimodal con tools, la imagen ya no se degrada a texto.
- El runner usa el SDK AI compartido para construir parts hacia OpenAI.

Estado real: `PARCIALMENTE CERRADO`

Soportado hoy y dentro del scope actual `AI`:

- `input_text`
- `input_image.image_url`
- `input_image.detail`
- `input_file.file_data` (`data:<mime>;base64,...`)
- `input_file.filename`

Tabla explicita de estado actual para attachments AI:

| Modo Agents SDK | Estado actual | Alcance |
| --- | --- | --- |
| `input_text` | soportado | `AI` |
| `input_image.image_url` | soportado | `AI` |
| `input_image.detail` | soportado | `AI` |
| `input_file.file_data` | soportado en general como `data:<mime>;base64,...`, con incidencia abierta para PDF | `AI` |
| `input_file.filename` | soportado junto con `file_data` | `AI` |
| `input_image.file_id` | no implementado | requiere definicion/contrato `CORE` |
| `input_file.file_id` | no implementado | requiere definicion/contrato `CORE` |
| `input_file.file_url` | no implementado | requiere definicion/contrato `CORE` |

Con incidencia abierta pero dentro del scope actual `AI`:

- compatibilidad estable de `input_file.file_data` para PDF segun provider/modelo

No implementado aun, separado por alcance:

Pendiente dentro del scope actual `AI`:

- politica de seleccion `file_data` vs `file_id` vs `file_url`
- bateria E2E completa por tipo

Bloqueante E2E actual fuera de `AI`:

- la policy efectiva inbound del adapter sigue siendo un posible cuello de botella E2E aunque `AI` soporte el modo Agents SDK
- `IO.slack` ya amplio su allowlist efectiva para office comunes (`csv`, `doc/docx`, `xls/xlsx`, `ppt/pptx`)
- queda pendiente validar E2E real por Slack con esa policy actualizada

Pendiente fuera de scope puramente `IO/AI` o dependiente de definicion de contrato/transporte:

- `input_image.file_id`
- `input_file.file_id`
- `input_file.file_url`

Limitacion actual adicional:

- los runners AI actuales responden texto y offload de texto largo a `content_ref`, pero no exponen todavia un flujo completo de adjuntos salientes generados por el runtime como parte del data-plane normal
- por lo tanto, casos de producto del tipo "claro, aca esta" + archivo adjunto generado por el agente no deben considerarse implementados hoy en `AI.*`, aunque `IO.slack` ya sepa enviar `attachments[]` salientes si los recibe

Lectura operativa para este backlog:

- el objetivo inmediato de `IO/AI` no es "implementar todos los modos del Agents SDK cueste lo que cueste"
- el objetivo inmediato es cerrar por completo los modos del Agents SDK que ya son representables desde el contrato Fluxbee actual (`content`, `content_ref`, `attachments[]` con `BlobRef`)
- cuando un modo requiera metadata que Router/Core no transporta hoy (`file_id`, `file_url` confiable, identidades remotas), se documenta como bloqueado por contrato y no como deuda local pura de `AI`

Documentos fuente:

- `docs/onworking NOE/ai_attachments_status.md`
- `docs/onworking NOE/ai_attachments_router_processing_plan.md`
- `docs/onworking NOE/ai_multimodal_rollout_tasks.md`

### 3. Spawn IO sin config / con config parcial / luego `CONFIG_SET`

Estado actual:

- core permite spawn con `config` omitido o parcial
- orchestrator mergea template de runtime + patch de request + `_system`
- `IO.slack` toma fallback desde `config.json` de orchestrator si no hay estado dinamico propio
- `CONFIG_SET` hace hot reload y persiste estado dinamico del nodo

Estado real: `FUNCIONAL, PERO NO CERRADO`

Lo que ya esta cerrado:

- bootstrap desde `config.json` managed
- estado `UNCONFIGURED` / `FAILED_CONFIG` / `CONFIGURED`
- `CONFIG_GET` / `CONFIG_SET` runtime-owned

Lo que sigue abierto:

- hoy IO soporta solo `apply_mode="replace"`; `merge_patch` sigue pendiente
- faltan tests de precedencia y lifecycle
- no esta cerrado el contrato de cleanup de estado dinamico en reinstall limpia

Documento fuente:

- `docs/onworking NOE/io_nodes_config_control_plane_migration_tasks.md`

### 4. Spawn AI sin config / con config parcial / luego `CONFIG_SET`

Estado actual:

- core permite spawn con `config` omitido o parcial
- runner AI ya puede bootear sin YAML usando `--node-name` / `FLUXBEE_NODE_NAME`
- si existe estado dinamico persistido, ese estado tiene precedencia
- si no existe, puede bootstrapear desde `config.json` de spawn
- si nada valido existe, queda `UNCONFIGURED`
- `CONFIG_SET` materializa/persiste config efectiva del nodo y hace hot reload

Estado real: `FUNCIONAL, PERO NO CERRADO`

Lo que ya esta cerrado:

- boot sin YAML
- fallback a spawn config
- persistencia de config efectiva dinamica
- hot reload por `CONFIG_SET`

Lo que sigue abierto:

- hoy AI soporta solo `apply_mode="replace"`; `merge_patch` sigue pendiente
- no esta cerrada la proteccion at-rest de secretos
- falta decidir si el primer JSON efectivo lo crea core en spawn o el nodo con primer `CONFIG_SET`
- sigue abierto el bug de precedence/lifecycle: estado dinamico local viejo puede sobrevivir a reinstall limpia

Documentos fuente:

- `docs/onworking NOE/ai_nodes_v14_spawn_json_plan.md`
- `docs/onworking NOE/core_ai_node_state_cleanup_gap.md`

## Tareas ya cerradas al consolidar este backlog

Se marcaron como cerradas en docs existentes las siguientes tareas porque ya coinciden con el codigo actual:

- `docs/onworking NOE/ai_multimodal_rollout_tasks.md`
  - `M4.1`
  - `M4.2`
  - `M4.3`
  - `M4.4`
  - `M4.5`
- `docs/onworking NOE/ai_attachments_router_processing_plan.md`
  - `D1`
  - `D2`
  - `D3`
  - `D4`

Motivo:

- ambos runners (`AI.common` y `SY.frontdesk.gov`) ya usan el mismo camino del SDK AI para resolver input, construir parts estructurados y mantener fallback textual/controlado

## Gaps que seguian repartidos y conviene explicitar aca

### Gap transversal A - lifecycle de estado dinamico en reinstall limpia

Aplica a AI con evidencia concreta y potencialmente tambien a IO.

Pregunta de arquitectura pendiente:

- quien limpia o invalida estado dinamico local cuando una operacion de core pretende reinstalar limpio una instancia

Criterio recomendado:

- responsabilidad primaria del core/orchestrator
- restart normal puede conservar estado
- delete/reinstall limpia no deberia heredarlo

Implicancia para este backlog:

- las tareas `CORE` de este tema son de definicion/implementacion fuera de `IO/AI`
- del lado `IO/AI` solo corresponde dejar documentado el impacto, agregar tests de precedencia propios cuando exista contrato estable y adaptar bootstrap/runtime a esa definicion

### Gap transversal B - `merge_patch`

La spec de IO y AI lo contempla, pero el codigo actual de runtime valida solo `replace`.

Impacto:

- el flujo funciona
- pero la promesa documental de patch parcial caliente no esta completa en runtime node-owned

### Gap transversal C - garantia para futuros nodos IO

Hoy el comportamiento esta bien cerrado para `IO.slack` + SDK.

Todavia falta volverlo garantia de arquitectura para cualquier adapter nuevo mediante:

- un unico send-path obligatorio via `fluxbee_sdk::NodeSender::send`
- tests compartidos por adapter
- checklist de adopcion en `nodes/io/common`

### Gap transversal D - attachments AI alineados a Agents SDK completos

El objetivo no es "leer algunos adjuntos".
El objetivo es:

- soportar todos los modos representables desde Fluxbee sin desviar el modelo OpenAI
- documentar como diferidos los modos no representables hoy
- cerrar provider-compat para PDF y definir politica de `file_data` vs `file_id` vs `file_url`

## Backlog consolidado propuesto

Convencion:

- `[ ] CORE` = tarea a documentar/escalar para que la resuelva o defina el equipo de core
- `[ ] IO` = tarea implementable o cerrable en nodos `IO.*`
- `[ ] AI` = tarea implementable o cerrable en nodos `AI.*` / SDK AI compartido
- `[ ] IO/AI` = tarea que realmente requiere cierre extremo a extremo entre ambos lados
- `[x]` = tarea ya cerrada

### Bloque 1 - Cerrar lifecycle y precedencia de config dinamica

Prioridad: P0

[ ] CORE: crear/actualizar un documento de handoff para core definiendo la semantica de:
  - restart normal
  - respawn operativo
  - reinstall limpia
  - `delete + reinstall`
  - `force recreate`
[ ] CORE: definir si una reinstall limpia debe limpiar o invalidar siempre el estado dinamico node-owned de `AI.*` y `IO.*`
[ ] CORE: implementar y validar el cleanup/invalidation del estado dinamico asociado a la instancia cuando aplique semantica de reinstall limpia
[ ] IO: una vez definido el contrato de core, agregar pruebas de precedencia en nodos `IO.*`:
  - persisted > spawn fallback > unconfigured en restart normal
  - reinstall limpia no hereda persisted viejo
[ ] AI: una vez definido el contrato de core, agregar pruebas de precedencia en nodos `AI.*`:
  - persisted > spawn fallback > unconfigured en restart normal
  - reinstall limpia no hereda persisted viejo
[ ] IO: reflejar en docs de nodos `IO.*` el comportamiento efectivo resultante y cualquier expectativa sobre bootstrap
[ ] AI: reflejar en docs de nodos `AI.*` el comportamiento efectivo resultante y cualquier expectativa sobre bootstrap

Documentos a referenciar:

- `docs/onworking NOE/core_ai_node_state_cleanup_gap.md`
- `docs/onworking NOE/io_nodes_config_control_plane_migration_tasks.md`

### Bloque 2 - Cerrar `CONFIG_SET` runtime-owned

Prioridad: P1

[x] IO: `replace` funciona hoy como camino estable actual
[x] AI: `replace` funciona hoy como camino estable actual
[x] IO: `CONFIG_SET` persiste en archivo dinamico node-owned y no en `config.json` de orchestrator
[x] AI: `CONFIG_SET` persiste en archivo dinamico node-owned y no en `config.json` de orchestrator
[ ] CORE: confirmar si la spec quiere mantener `merge_patch` como requisito obligatorio o bajarlo a diferido
[ ] AI: implementar `merge_patch` real en AI runtime si la definicion de spec se mantiene
[ ] IO: implementar `merge_patch` real en IO runtime comun si la definicion de spec se mantiene
[ ] IO: agregar tests de versionado en runtime `IO.*`:
  - create
  - update
  - stale
  - idempotencia
[ ] AI: agregar tests de versionado en runtime `AI.*`:
  - create
  - update
  - stale
  - idempotencia

Documentos a referenciar:

- `docs/onworking NOE/ai_nodes_v14_spawn_json_plan.md`
- `docs/onworking NOE/io_nodes_config_control_plane_migration_tasks.md`

### Bloque 3 - Cerrar IO blobs de forma transversal

Prioridad: P1

[x] IO: el offload automatico de texto `text/v1` sobre limite ya vive en `fluxbee_sdk::NodeSender::send` para mensajes `payload.type="text"` que cumplen contrato `text/v1`
[x] IO: documentar explicitamente que todo envio `text/v1` hacia router debe pasar por `fluxbee_sdk::NodeSender::send`
[x] IO: revisar nodos IO actuales y confirmar que no queda un send-path alternativo que saltee `NodeSender::send`
[x] IO: `fluxbee_sdk` ya tiene unit tests base de `NodeSender::send` / `send_normalization` para offload `text/v1`
[ ] IO: completar tests `io-common` / `IO.slack`
[x] IO: smoke Linux real para `>64KB` ya validado con `io-sim` (no con Slack, porque Slack no es canal valido para ese umbral)
[ ] IO: una vez fijado el contrato, convertir este comportamiento en requisito de adopcion para cualquier nuevo adapter IO

Documentos a referenciar:

- `docs/onworking NOE/io_text_v1_sdk_send_enforcement_plan.md`
- `docs/onworking NOE/io_text_over_limit_blob_enforcement_plan.md`
- `docs/onworking NOE/io_slack_blob_tasks.md`

### Bloque 4 - Cerrar attachments AI contra Agents SDK

Prioridad: P1

Desglose operativo del bloque:

- Subbloque 4.A = cerrable ya en `AI` con contrato Fluxbee actual
- Subbloque 4.B = cerrable en `AI`, pero dependiente de compatibilidad real del provider/modelo
- Subbloque 4.C = bloqueado por definicion/metadata de `CORE`
- Subbloque 4.D = evidencia E2E y cierre documental final

[x] AI: ya existe mapeo para `input_text`, `input_image.image_url`, `input_image.detail`, `input_file.file_data`, `input_file.filename`
[x] AI: agregar en este documento una tabla explicita de:
  - soportado ahora
  - pendiente dentro del scope actual
  - diferido por transporte/contrato Fluxbee
[x] AI 4.A.1: congelar por doc el baseline implementable ya con contrato Fluxbee actual:
  - `input_text`
  - `input_image.image_url`
  - `input_image.detail`
  - `input_file.file_data`
  - `input_file.filename`
[x] AI 4.A.2: documentar la politica de seleccion vigente dentro del scope actual:
  - textuales permitidos -> `input_text`
  - imagenes permitidas -> `input_image.image_url`
  - no imagen con `multimodal=true` -> `input_file.file_data`
[x] AI 4.A.3: agregar pre-validacion de `input_file` antes de enviar al provider
[x] AI 4.A.4: definir errores canonicos y comportamiento estable para adjuntos que no entren en la politica anterior
[x] AI 4.B.1: validar provider-compat inicial de `input_file.file_data` para PDF en `AI.common`
[ ] AI 4.B.1.b: cerrar compatibilidad estable de `input_file.file_data` para PDF segun matriz provider/modelo soportada
[ ] AI 4.B.2: definir fallback PDF si el provider/modelo no acepta el modo directo
[ ] AI 4.B.3: dejar criterio explicito para otros binarios que usen `input_file.file_data`:
  - aceptado
  - rechazado temprano
  - degradado de forma explicita
[ ] CORE: definir si Fluxbee va a representar de forma canonica metadata/identidades remotas necesarias para habilitar `file_id`
[ ] AI 4.C.1: implementar `file_id` para `input_image` / `input_file` solo si existe contrato Fluxbee suficiente para representarlo sin hacks ad-hoc
[ ] CORE: definir si Fluxbee va a admitir `file_url` como modo canonico confiable y bajo que modelo de trust/metadata
[ ] AI 4.C.2: evaluar e implementar `file_url` solo si existe esa definicion de contrato
[ ] AI 4.C.3: definir selector final de transporte entre `file_data` / `file_id` / `file_url` segun contrato final
[ ] AI 4.D.1: cerrar E2E por tipo en validacion directa de nodos `AI.*` cuando el attachment ya llega al runner:
  - cobertura representativa por familias, no exhaustiva por cada MIME posible
  - imagen
  - pdf
  - texto simple/estructurado cuando aplique
  - binario no imagen representativo
  - audio representativo (no probado)
  - mezcla de adjuntos
[x] IO 4.D.1.b: ampliar la allowlist efectiva inbound de `IO.slack` para MIME no imagen representables por `AI` via `input_file.file_data`
  - `csv`
  - `doc/docx`
  - `xls/xlsx`
  - `ppt/pptx`
[ ] IO/AI 4.D.1.c: completar E2E extremo a extremo por Slack una vez ampliada la allowlist inbound de `IO.slack`
  - `xlsx`: validado el 2026-04-06
  - `docx`: validado el 2026-04-06
  - audio: no probado
  - pendiente ampliar a otras clases representativas solo si se consideran relevantes
[ ] IO/AI 4.D.2: reflejar en docs canonicas y onworking el estado final por modo de attachment

Resultado esperado del bloque 4 si se cierra sin esperar cambios de `CORE`:

- AI queda completamente alineado con Agents SDK en todos los modos hoy representables desde Fluxbee
- PDF y otros `input_file.file_data` quedan con comportamiento determinista
- `file_id` y `file_url` quedan trazados como diferidos por contrato, no mezclados con deuda local ya resoluble

Documentos a referenciar:

- `docs/onworking NOE/ai_attachments_status.md`
- `docs/onworking NOE/ai_attachments_router_processing_plan.md`
- `docs/onworking NOE/ai_multimodal_rollout_tasks.md`

### Bloque 4 bis - Adjuntos salientes generados por AI

Prioridad: P1/P2 segun decision de producto

Principio rector para este bloque:

- tomar como referencia el modelo y ejemplos de `agents-sdk-python`
- en particular:
  - outputs estructurados de tool (`ToolOutputImage`, `ToolOutputFileContent`)
  - y el patron de `tool_use_behavior` / `ToolsToFinalOutputResult` que muestra que la capa de runtime decide cuando un tool output pasa a ser output final de usuario

Objetivo del bloque:

- habilitar casos de producto del tipo:
  - "claro, aca esta"
  - texto breve de acompanamiento
  - mas uno o mas archivos/imagenes adjuntos generados por el agente

Estado actual:

- el contrato Fluxbee ya soporta `payload.attachments[]` salientes
- `IO.slack` ya sabe publicar `attachments[]` si recibe `BlobRef`
- los runners `AI.*` actuales responden texto y/o `content_ref`, pero no exponen todavia un flujo general para artefactos salientes

Backlog propuesto:

[ ] AI 4bis.1: definir contrato interno de salida de behavior/runtime para artefactos user-facing
  - no inferir automaticamente "todo tool output archivo => adjunto al usuario"
  - exigir una decision explicita de runtime/behavior sobre que artefactos son final user deliverable
  - mantener alineacion conceptual con `tool_use_behavior` del Agents SDK
[ ] AI 4bis.2: definir tipo reusable en SDK AI para salida final con adjuntos
  - ejemplo de forma:
    - texto final
    - lista de artefactos generados (`bytes`, `mime`, `filename`)
  - esto debe convivir con el caso actual de salida solo texto
[ ] AI 4bis.3: implementar helper comun en SDK AI para materializar artefactos salientes a blob
  - `put_bytes`
  - `promote`
  - retorno de `BlobRef`
[ ] AI 4bis.4: implementar builder de respuesta `text/v1` con `attachments[]` salientes
  - texto breve + archivos
  - solo archivo si aplica
  - offload de texto largo a `content_ref` si hiciera falta
[ ] AI 4bis.5: integrar ese camino en runners `AI.common` y `SY.frontdesk.gov`
  - mantener backward compatibility del caso actual `String -> build_text_response(...)`
  - usar el camino nuevo solo cuando el behavior devuelva artefactos finales
[ ] AI 4bis.6: definir politica de fallback/error para generacion de artefactos
  - si el texto esta listo pero el archivo falla
  - si falla blob persist/promote
  - si hay multiple attachments y falla uno
[ ] AI 4bis.7: elegir un MVP de artefacto saliente
  - recomendado: un solo tipo primero (`pdf` o imagen)
  - no intentar cerrar de entrada PDF + XLSX + imagen + otros
[ ] IO/AI 4bis.8: validar E2E con `IO.slack`
  - texto + attachment saliente
  - attachment saliente sin texto si se decide soportarlo
  - error canonicamente visible si la generacion falla
[ ] AI/IO 4bis.9: actualizar docs canonicamente
  - que soporta el contrato
  - que esta implementado hoy
  - que tipos de artefactos salientes quedan dentro del MVP

Preguntas de decision minima antes de implementarlo:

- si el MVP sale solo para un tipo de artefacto o para varios
- si el primer MVP se apoya en:
  - tool local especifica de generacion
  - o behavior especifico que ya produzca el archivo
- si la salida minima requerida es:
  - texto + attachment
  - o tambien attachment-only

Riesgo principal a controlar:

- no filtrar al usuario archivos generados para razonamiento interno del agente
- la publicacion a usuario final debe ser una decision explicita del runtime/behavior, no una inferencia automatica por tipo de tool output

### Bloque 5 - Cerrar UX IO<->AI para errores y archivos

Prioridad: P2

[ ] IO: en `IO.slack`, renderizar bien `payload.type="error"` proveniente de AI
[ ] IO: evitar fallback generico `invalid_text_payload` cuando ya exista error canonico
[ ] IO/AI: completar runbooks E2E actuales para:
  - Slack -> AI con imagen
  - Slack -> AI con PDF
  - AI error canonico -> Slack

Documentos a referenciar:

- `docs/onworking NOE/ai_multimodal_rollout_tasks.md`
- `docs/onworking NOE/ai_chat_slack_blob_e2e.md`
- `docs/onworking NOE/t6_4_io_slack_e2e_steps.md`

### Bloque 6 - Cerrar higiene documental final

Prioridad: P2

[ ] IO: reflejar estado final real en:
  - `docs/io/io-common.md`
  - `docs/io/io-slack-deploy-runbook.md`
[ ] AI: reflejar estado final real en:
  - `docs/AI_nodes_spec.md`
  - `docs/ai-nodes-deploy-runbook.md`
[ ] IO/AI: bajar o eliminar `onworking` redundantes una vez absorbidos
[ ] IO/AI: mantener este archivo como indice consolidado hasta cierre total

## Items que estaban afuera o poco visibles y se agregan explicitamente

1. El riesgo de heredar estado dinamico viejo no es solo AI; tambien puede afectar IO si el lifecycle pretende ser limpio.
2. El soporte completo "alineado a Agents SDK" no se agota en aceptar blobs locales; incluye decidir y documentar los modos diferidos por transporte.
3. El envio saliente de adjuntos desde runners AI no esta cerrado como flujo funcional general, aunque el SDK ya tenga primitives para texto largo y parts estructurados de entrada.
4. La garantia "esto aplica a cualquier IO nuevo" todavia no existe como enforcement de arquitectura; hoy existe como implementacion correcta del camino actual.

## Orden recomendado para avanzar

Orden efectivo acordado actualmente:

1. Bloque 4
2. Bloque 3
3. Bloque 5
4. Bloque 6
5. Bloque 1
6. Bloque 2

Motivo del orden efectivo:

- se posterga `CORE` config/lifecycle para el final
- se prioriza cerrar primero attachments AI contra Agents SDK dentro del contrato Fluxbee actual
- despues se termina de blindar IO blobs y la UX E2E
- recien al final se vuelve sobre lifecycle/config compartido y temas de `merge_patch`

Orden originalmente recomendado:

1. Bloque 1
2. Bloque 2
3. Bloque 4
4. Bloque 3
5. Bloque 5
6. Bloque 6

Razon del orden original:

- primero hay que fijar semantica de lifecycle y config persistida para no cerrar attachments sobre una base operacional inconsistente
- luego cerrar el contrato runtime-owned de config
- despues completar attachments AI alineados a Agents SDK
- luego terminar de blindar IO blobs de forma transversal
