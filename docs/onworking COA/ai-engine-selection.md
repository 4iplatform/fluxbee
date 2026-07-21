# OpenAI + Anthropic engine selection — relevamiento y tareas

**Estado:** implementación principal terminada; validación integral y documentación
operativa en curso.

**Relevado contra:** `main` local al 2026-07-21 (`4ebb28c`).

**Alcance:** SDK AI en Rust, consumidores AI dentro de nodos `SY.*`, nodos dinámicos
`AI.*`, selección de credenciales en `SY.vault`, contratos de configuración y pruebas.

## 1. Pedido operativo

El comportamiento buscado tiene dos caminos distintos:

1. Los nodos `SY.*` que usan AI toman un proveedor por defecto del `hive.yaml`.
   Si el campo no existe, deben continuar usando OpenAI, como hoy. No debe haber una
   selección de proveedor ni modelo independiente por cada `SY.*`. El modelo de cada
   proveedor también se configura en `hive.yaml`.
2. Un nodo dinámico `AI.*` selecciona su proveedor mediante la key de Vault indicada
   en su comando/configuración. La key ya tiene `metadata.resource_type=openai` o
   `anthropic`; el nodo no debe pedir además otro selector de proveedor que pueda
   contradecir esa metadata. El modelo se configura en el mismo `CONFIG_SET`, no en
   la key.
3. Las credenciales continúan viviendo exclusivamente en `SY.vault`.
4. OpenAI y Anthropic deben implementarse dentro de `fluxbee-ai-sdk`, en Rust. No se
   agrega otro runtime, sidecar ni SDK en otro lenguaje.

Fuera de alcance de este cambio: nuevos proveedores además de OpenAI/Anthropic,
balanceo de keys, cambios al routing, cambios cognitivos y rediseño general de Vault.

## 2. Baseline relevado antes de implementar

Al iniciar esta tarea, el soporte Anthropic **no estaba implementado**. Había buenas abstracciones iniciales,
pero los wire formats de OpenAI todavía atraviesan el SDK y los consumidores.

| Área | Estado actual | Brecha |
| --- | --- | --- |
| AI SDK | `LlmClient` y `FunctionCallingModel` son traits; sólo existen `OpenAiResponsesClient` y `OpenAiFunctionCallingModel` | Falta provider enum/factory, cliente Anthropic y neutralizar historia, tools y attachments |
| Vault SDK | `ResourceType::Anthropic` ya existe | Los consumidores resuelven sólo `ResourceType::Openai`; `resolve_resource` elige por tipo, no por key explícita |
| `hive.yaml` | No tiene un default AI canónico | Hay que agregarlo y mantener fallback OpenAI |
| `SY.admin` | Executor OpenAI directo, modelo default `gpt-5.4-mini` | Debe usar el default del hive y el SDK común |
| `SY.architect` | OpenAI directo, tools y multimodal; además lee el legacy `ai_providers.openai` | Debe migrar al default único del hive |
| `SY.cognition` | OpenAI directo para semantic tagger y narrative summarizer; default `gpt-4.1-mini` | Tiene gates OpenAI-only y config por-SY que contradice el objetivo |
| `SY.frontdesk.gov` | Es un system node, aunque su binario deriva del runner AI | Debe heredar el default de `SY.*`; no pertenece al mecanismo de selección de los `AI.*` dinámicos |
| `AI.*` dinámicos | `behavior.provider` existe en el JSON efectivo pero se ignora; sólo `kind=openai_chat` funciona | Deben recibir una key de Vault, derivar proveedor de su metadata y construir el runtime adecuado |

## 2.1 Estado implementado al 2026-07-21

- El SDK Rust expone `AiProvider`, `HiveAiConfig`, factories neutrales y adapters
  OpenAI Responses + Anthropic Messages.
- El contenido multimodal y la historia de function calling son neutrales; cada
  adapter produce su propio wire format.
- Anthropic implementa texto, JSON schema, imágenes/documentos, tools multi-turn,
  errores con request id y retries acotados para transporte/408/409/429/5xx.
- `AI.*` acepta solamente `behavior.kind=ai_chat` con `vault_key` y `model`; lee la
  key exacta y deriva el provider de `metadata.resource_type`.
- `SY.admin`, `SY.architect`, `SY.cognition` y `SY.frontdesk.gov` usan el provider y
  model del hive. Sin sección `ai`, el efectivo es OpenAI + `gpt-5.5`.
- Los overrides provider/model por `CONFIG_SET` en consumidores SY se rechazan.
- Los ejemplos canónicos de `hive.yaml` incluyen el default OpenAI y el orchestrator
  lo propaga al hive generado para workers.

## 3. Evidencia por componente

### 3.1 SDK AI en Rust

`crates/fluxbee_ai_sdk/src/llm.rs` contiene:

- `LlmClient::generate/generate_stream`, que sí es agnóstico;
- `OpenAiResponsesClient`, fijado a `/v1/responses`;
- `OpenAiFunctionCallingModel`, acoplado a `previous_response_id`,
  `function_call` y `function_call_output` de OpenAI;
- `LlmRequest.input_parts: Option<Vec<Value>>`, cuyo tipo parece neutral pero recibe
  bloques OpenAI (`input_text`, `input_image`, `input_file`).

`crates/fluxbee_ai_sdk/src/text_payload.rs` expone
`build_openai_user_content_parts*`. Los tres caminos con attachments
(`SY.architect`, `AI.*` y `SY.frontdesk.gov`) lo llaman antes de entrar al modelo.

La abstracción de tools tampoco alcanza todavía para Anthropic. El runner persiste
el `response_id` de OpenAI dentro de `FunctionToolResult`; Anthropic exige reconstruir
la conversación con un bloque assistant `tool_use` seguido por un bloque user
`tool_result`. Hoy `FunctionLoopItem` no conserva el bloque assistant tool-use.

Conclusión: no alcanza con agregar un segundo `impl LlmClient`. Primero hay que hacer
provider-neutral el modelo interno de input y continuación de tools; después cada
adapter serializa su propio wire format.

Referencias externas verificadas para implementar el adapter:

- Anthropic Messages usa `POST /v1/messages`, `x-api-key`, `anthropic-version`,
  `system` top-level y mensajes user/assistant.
- Tools usan `input_schema`, respuestas `tool_use` y continuación `tool_result`.
- Structured output actual usa `output_config.format` con `type=json_schema` en los
  modelos que declaran esa capability.

Documentación oficial:

- <https://platform.claude.com/docs/en/api/messages/create>
- <https://platform.claude.com/docs/en/agents-and-tools/tool-use/handle-tool-calls>
- <https://platform.claude.com/docs/en/build-with-claude/structured-outputs>

#### SDKs Anthropic disponibles y criterio de port

Anthropic no publica actualmente un SDK oficial para Rust. Sus SDKs oficiales son
Python, TypeScript, C#, Go, Java, PHP y Ruby. Existen crates Rust comunitarios, pero
los relevados son incompletos, work-in-progress o no demuestran cobertura mantenida
de Messages + structured output + tools + multimodal. No conviene incorporar uno
como dependencia central de Fluxbee.

El código de los SDKs oficiales es público y fue revisado sin copiarlo al repo. La
revisión quedó fijada a estas revisiones para no diseñar contra una referencia móvil:

- `anthropics/anthropic-sdk-go@0ce94bd` (v1.58.0): referencia principal para tipos
  Messages, headers, errores, request id y política de retries;
- `anthropics/anthropic-sdk-typescript@3e9a2e1`: referencia principal para el tool
  runner y armado de `tool_result` paralelo.

Funciones/comportamientos concretos a portar al SDK Rust de Fluxbee, adaptados a sus
traits existentes:

- headers `anthropic-version`, `x-api-key`, `content-type` y `accept`;
- request/response types mínimos de Messages, content blocks y usage;
- envelope de error + `request-id` sin incluir la key o payload sensible;
- clasificación retryable: error de conexión, 408, 409, 429 y 5xx, respetando
  `x-should-retry`;
- `retry-after-ms` / `retry-after`, backoff exponencial acotado y jitter;
- conservación del assistant message completo que contiene `tool_use`;
- ejecución paralela de tool calls y respuesta user con todos los `tool_result`,
  incluido `is_error`;
- `output_config.format` para JSON schema;
- mappings de texto, imagen y documento que Fluxbee realmente usa.

No se debe portar el SDK generado completo, sus APIs beta, Bedrock/Vertex, batches,
managed agents ni su capa de environment credentials. Fluxbee ya tiene `reqwest`,
timeouts, Vault y abstracciones propias; el port debe ser angosto y auditable.

### 3.2 Vault y selección de key

`crates/fluxbee_sdk/src/vault.rs` ya conoce `openai` y `anthropic`. La metadata de
una entrada contiene:

- `resource_type` (identifica el proveedor);
- `tenant_id`;
- `ilk` opcional;
- descripción y tags.

No contiene un campo canónico de modelo.

El método usado hoy por los consumidores es
`VaultClient::resolve_resource(resource_type, tenant, timeout)`. Aplica la precedencia
dedicated ILK -> pool del tenant -> pool root, toma el recurso más reciente y devuelve
sólo el `value`; descarta key, metadata y versión.

Esto sirve para los `SY.*`: el default del hive determina `ResourceType` y luego se
descubre la credencial correspondiente. No sirve para el nuevo contrato de `AI.*`,
porque allí el operador debe seleccionar una **key concreta**. Para ese caso hay que
usar/encapsular `VaultClient::get(key)` y validar que la metadata recuperada:

- sea visible para el tenant/ILK según las reglas de Vault;
- tenga `resource_type` exactamente `openai` o `anthropic`;
- tenga un value con `api_key` no vacío;
- sea la misma key que se muestra, redacted, en status/`CONFIG_GET`.

Este cambio es una excepción intencional al Model D' actual: el documento
`sy_vault_model_d.md` dice que los consumidores no guardan refs y descubren por tipo.
La credencial sigue en Vault, pero `AI.*` sí necesita persistir el identificador no
secreto de la key seleccionada. Hay que actualizar esa documentación.

### 3.3 Configuración del hive

No existe una sección AI canónica en `config/hive.yaml` ni en
`packaging/hive.yaml.example`. Tampoco existe en el `HiveFile` compartido de
`src/config/mod.rs`.

Además, los binarios declaran varios `HiveFile` privados. Los consumidores afectados
parsean el YAML por separado (`SY.admin`, `SY.architect`, `SY.cognition`), por lo que
agregar el campo sólo al router config no lo propaga automáticamente.

Contrato canónico cerrado:

```yaml
ai:
  default_provider: anthropic # openai | anthropic
  providers:
    openai:
      model: "gpt-5.5"
    anthropic:
      model: "<anthropic-model-id>"
```

Reglas:

- sección ausente -> provider `openai` y model `gpt-5.5`;
- `default_provider` ausente -> `openai`;
- con sección `ai` presente, el provider seleccionado debe tener `model` no vacío;
- cambiar `default_provider` selecciona el bloque y modelo de ese provider;
- valor vacío o desconocido -> error de configuración, sin fallback silencioso;
- no contiene secrets ni una key de Vault;
- es la única fuente de provider y model para los consumidores `SY.*`;
- `max_tokens`, timeouts y límites funcionales pueden seguir siendo propios de cada
  operación; no son selectores de provider/model.

Conviene definir `AiProvider` y `HiveAiConfig` en un crate compartido y hacer que los
parsers locales reutilicen esa forma, evitando strings y defaults duplicados.

### 3.4 Consumidores `SY.*`

El inventario actual es:

- `SY.admin`: executor con function calling. Resuelve OpenAI del pool root, cachea
  `OpenAiResponsesClient` y refresca sólo ante broadcast `resource_type=openai`.
- `SY.architect`: chat y varios subagentes con function calling, structured output y
  attachments. Resuelve OpenAI del pool root y refresca sólo OpenAI. Mezcla config de
  nodo con el legacy `hive.ai_providers.openai`.
- `SY.cognition`: semantic tagger y narrative summarizer de texto. Resuelve OpenAI
  del pool root. Expone `config.semantic_tagger.provider/model` y rechaza proveedores
  distintos de OpenAI.
- `SY.frontdesk.gov`: system node listado en `hive.yaml`; usa OpenAI, tools,
  attachments y resolución desde el root tenant. Por semántica debe seguir el default
  `SY.*`, no el selector por instancia de `AI.*`.

Los cuatro deben derivar provider y credencial del default del hive. Sus listeners
`VAULT_SECRET_CHANGED`, readiness, status, errores y contratos deben interesarse por
el provider efectivo, no quedar hardcoded a `openai`.

La configuración no secreta específica de la función puede permanecer (timeouts,
thresholds, catálogo, límites). Deben eliminarse del código y de los contratos los
overrides por-SY de provider y model (`ai_providers.openai`,
`semantic_tagger.provider/model` y equivalentes).

### 3.5 Nodos dinámicos `AI.*`

`ai-generic` tiene hoy dos formas de config (YAML legacy y documento efectivo), pero
el camino administrado relevante es `CONFIG_SET` + JSON persistido. Su contrato real:

- sólo permite `behavior.kind=echo|openai_chat`;
- materializa `behavior.provider=openai`, pero el dispatcher no lee ese campo;
- exige `behavior.model`;
- resuelve OpenAI por `resource_type`, no por key;
- no escucha cambios de Vault: resuelve la key en cada llamada;
- rechaza secretos inline, correctamente.

El nuevo contrato no debe aceptar dos fuentes de verdad (`provider` + key). La key
seleccionada es configuración no secreta y su metadata decide el provider.

Contrato nuevo cerrado:

```json
{
  "behavior": {
    "kind": "ai_chat",
    "vault_key": "tenant-support-claude",
    "model": "<model-id>"
  }
}
```

Reglas estrictas, sin compatibilidad alpha:

- el único behavior LLM nuevo es `ai_chat`; `openai_chat` se elimina;
- `vault_key` y `model` son obligatorios para `ai_chat`;
- `behavior.provider` se elimina y se rechaza si aparece; la metadata de la key es
  autoritativa;
- persistir sólo `vault_key`, nunca el valor secreto;
- `CONFIG_GET` devuelve key, provider derivado, modelo efectivo y disponibilidad,
  sin devolver la credencial;
- cambiar key o model requiere otro `CONFIG_SET` con `config_version` mayor;
- configs persistidas con el contrato anterior no se migran ni se aceptan: quedan en
  `FAILED_CONFIG` hasta recibir un `CONFIG_SET` nuevo válido.

## 4. Diseño de implementación propuesto

### 4.1 Tipos compartidos

Crear en el SDK Rust:

- `AiProvider::{OpenAi, Anthropic}` con parseo estricto y mapeo a `ResourceType`;
- `AiCredential { provider, api_key, vault_key?, version? }` sólo en memoria;
- factory de cliente de generación y factory de function-calling;
- errores provider-neutral (`missing_ai_api_key`, auth, rate limit, timeout,
  unsupported capability), conservando detalle del provider.

### 4.2 Input y tools provider-neutral

Reemplazar `Vec<Value>` como contrato interno por tipos propios, por ejemplo:

- texto;
- imagen con MIME + bytes/data source;
- documento con MIME, nombre y bytes/texto;
- historial assistant con texto y tool calls;
- resultados de tools con `call_id`, contenido e `is_error`.

Los adapters OpenAI/Anthropic convierten esos tipos a su JSON. Esto evita que cada
consumer tenga ramas provider-specific y permite probar el mapping sin red.

El estado de continuación debe dejar de depender de `previous_response_id`. OpenAI
puede seguir aprovechándolo internamente, pero el runner necesita conservar una
historia neutral suficiente para Anthropic.

### 4.3 Resolución de credenciales

- `SY.*`: `hive.ai.default_provider` -> provider config/model -> `ResourceType` ->
  `resolve_resource`.
- `AI.*`: `behavior.vault_key` -> `get(key)` -> metadata `resource_type` -> provider;
  `behavior.model` permanece independiente de la key.
- Ambos extraen `api_key` según el contrato vigente de valores AI en Vault (object o
  bare string); esto no habilita compatibilidad de configuración de nodos.
- Status y broadcast usan el provider efectivo.

### 4.4 Sin duplicar lógica en consumidores

La resolución provider + key + cliente debería vivir en helpers compartidos. No se
deben crear cuatro copias nuevas de `resolve_anthropic_api_key` junto a las copias
OpenAI actuales.

## 5. Backlog de implementación

Las decisiones de contrato y el fallback OpenAI están cerrados en §6. El model de
Anthropic se declara explícitamente en `hive.yaml` cuando ese provider se selecciona.

### Fase A — contratos y configuración

- [x] `AIENG-A1` Cerrar provider/model/key y política de compatibilidad (§6).
- [x] `AIENG-A2` Agregar `AiProvider` compartido, parseo, serde y mapping a
  `ResourceType`; tests de valores válidos/inválidos.
- [x] `AIENG-A3` Agregar `HiveAiConfig { default_provider, providers }`, validación
  del model seleccionado y fallback `openai` + `gpt-5.5` cuando la sección completa
  no existe.
- [x] `AIENG-A4` Integrar la sección en todos los parsers afectados y en
  `config/hive.yaml` / `packaging/hive.yaml.example`.
- [x] `AIENG-A5` Eliminar `ai_providers.openai`,
  `semantic_tagger.provider/model`, `behavior.provider`, `openai_chat` y sus caminos
  muertos; rechazar configs viejas sin aliases de compatibilidad.

### Fase B — SDK Anthropic en Rust

- [x] `AIENG-B1` Crear tipos provider-neutral para content parts y attachments.
- [x] `AIENG-B2` Adaptar OpenAI a esos tipos sin regresiones.
- [x] `AIENG-B3` Hacer provider-neutral la historia del loop de tools.
- [x] `AIENG-B4` Implementar `AnthropicMessagesClient` con headers, body, error
  parsing, usage, request id y respuesta text; portar la política de retry relevante
  del SDK oficial Go.
- [x] `AIENG-B5` Implementar structured output Anthropic o fallback explícito por
  capability; validar con el mismo `OutputSchemaSpec` local.
- [x] `AIENG-B6` Implementar tool-use Anthropic, incluidos parallel tool calls,
  `tool_result`, `is_error` y orden de bloques.
- [x] `AIENG-B7` Implementar attachments Anthropic (texto, imágenes y documentos
  soportados) con errores explícitos para MIME/capability no soportados.
- [x] `AIENG-B8` Exponer factories provider-neutral y eliminar imports OpenAI de los
  consumidores.
- [ ] `AIENG-B9` Tests HTTP con servidor mock para payloads, headers, parsing,
  structured output, tools, errores y attachments de ambos providers.

### Fase C — Vault y selección de key

- [x] `AIENG-C1` Crear helper de credencial por provider para `SY.*` usando
  `resolve_resource`.
- [x] `AIENG-C2` Crear helper de credencial por key para `AI.*` usando `get`, con
  validación de metadata y autorización.
- [ ] `AIENG-C3` Exponer diagnóstico redacted (`vault_key`, provider, version,
  resolved/source) sin plaintext.
- [ ] `AIENG-C4` Cubrir key inexistente, tipo no AI, metadata ausente, credencial
  vacía, acceso denegado y rotación.

### Fase D — consumidores `SY.*`

- [x] `AIENG-D1` Migrar `SY.admin` al runtime/factory común.
- [x] `AIENG-D2` Migrar `SY.architect`, incluidos todos sus subagentes, tools,
  structured output y attachments.
- [x] `AIENG-D3` Migrar ambos pipelines de `SY.cognition` y retirar gates
  OpenAI-only.
- [x] `AIENG-D4` Migrar `SY.frontdesk.gov` como system node que hereda el hive.
- [x] `AIENG-D5` Hacer provider-aware `VAULT_SECRET_CHANGED`, refresh, readiness,
  status, `CONFIG_GET`, métricas y mensajes de error.
- [ ] `AIENG-D6` Tests de sección ausente -> OpenAI y sección Anthropic -> Anthropic
  para cada consumidor.

### Fase E — nodos dinámicos `AI.*`

- [x] `AIENG-E1` Agregar el selector de key al schema efectivo y persistencia de
  `CONFIG_SET`.
- [x] `AIENG-E2` Derivar provider exclusivamente de metadata Vault y construir el
  runtime con la factory.
- [x] `AIENG-E3` Reemplazar `openai_chat` por el contrato estricto `ai_chat`;
  actualizar contrato live, errores y redacción, y borrar ramas legacy.
- [x] `AIENG-E4` Mantener resolución por request o agregar refresh seguro; en ambos
  casos probar rotación/cambio de key sin filtrar secretos.
- [ ] `AIENG-E5` Probar chat simple, memoria inmediata, tools, outputs tipados y
  attachments con OpenAI y Anthropic.

### Fase F — documentación, operaciones y gate final

- [ ] `AIENG-F1` Actualizar `AI_nodes_spec.md`, `AI_Nodes_SDK_Spec_v1.md`,
  `node-config-control-plane-spec.md`, runbook, ejemplos y Model D'.
- [ ] `AIENG-F2` Actualizar ayuda/ejemplos de `SY.admin` y Archi para crear la key y
  asignarla al `AI.*`.
- [ ] `AIENG-F3` E2E: hive sin sección (OpenAI), hive Anthropic, cambio de key de un
  AI OpenAI->Anthropic, provider mismatch, key faltante y restore/rotation.
- [ ] `AIENG-F4` Ejecutar fmt, clippy/tests de crates afectados y el gate del repo.

## 6. Decisiones cerradas

### D1. Provider y model son conceptos separados

- La key de Vault determina acceso y proveedor mediante `metadata.resource_type`.
- La key no contiene ni determina el model.
- Para `SY.*`, provider y model viven en la configuración AI del `hive.yaml`.
- Para `AI.*`, `vault_key` y `model` viven en `behavior` y cambian por `CONFIG_SET`.
- Si la sección `ai` no existe, el fallback cerrado es provider `openai` y model
  `gpt-5.5`.

### D2. Selección de key en `AI.*`

Se reutiliza `CONFIG_SET` con `config.behavior.vault_key`. No se agrega un verbo de
control nuevo. Otro `CONFIG_SET` con versión mayor cambia key y/o model.

### D3. Sin backward compatibility

Fluxbee está en alpha. Se reemplazan los contratos y se elimina el código anterior:

- no alias de `openai_chat`;
- no discovery implícito por `resource_type` para `AI.*` sin `vault_key`;
- no `behavior.provider`;
- no overrides provider/model por nodo `SY.*`;
- no migración silenciosa de config persistida vieja.

El sistema debe quedar en un solo estado coherente, sin tareas de deprecación ni ramas
muertas pendientes.

## 7. Criterio de terminado

El trabajo estará completo cuando:

- omitir `ai` en `hive.yaml` conserve OpenAI en todos los consumidores `SY.*`;
- elegir Anthropic en el hive migre juntos a admin, architect, cognition y frontdesk;
- un `AI.*` derive OpenAI/Anthropic de la metadata de su key, sin secreto inline;
- ambos providers soporten los caminos realmente usados: texto, structured output,
  tools multi-turn, memoria inmediata y attachments;
- status/config/errores no digan OpenAI cuando el provider efectivo sea Anthropic;
- no queden payloads OpenAI construidos fuera del adapter OpenAI del SDK Rust;
- tests unitarios, integración HTTP y E2E cubran fallback, selección, rotación y
  degradación.
