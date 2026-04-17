# IO Response Envelope + AI Runtime Integration Spec

Estado: propuesta técnica para implementación  
Audiencia: desarrollo runtime AI, desarrollo de nodos IO, arquitectura  
Alcance inicial: `IO.api` + `SY.frontdesk.gov`  
Dirección de diseño: usar capacidades ya existentes en OpenAI Agents SDK / Responses path antes que inventar un mecanismo propio distinto

Estado de implementación del primer corte:

- runtime AI compartido con lectura de `meta.context.response_envelope` y `meta.context.final_response_contract`
- traducción a `OutputSchemaSpec`
- integración con OpenAI Responses `json_schema`
- fallback textual controlado cuando no aplica structured output real
- helpers en `io-common`
- adopción validada en `IO.api -> SY.frontdesk.gov`

Regla de implementación para runtime AI:
- las nuevas capacidades del runtime AI deben portarse incrementalmente desde `agents-sdk-python` disponible en este repo;
- no se debe rediseñar una superficie paralela si ya existe una capacidad relevante en el SDK Python;
- el port debe tomar solo el subconjunto necesario para el problema actual, sin intentar copiar toda la superficie del SDK de una vez.

---

## 1. Propósito

Definir un mecanismo común para que un nodo `IO.*` pueda **declarar opcionalmente el formato de respuesta que necesita** de un nodo `AI.*`, sin:

- convertir esa necesidad en texto embebido dentro del mensaje del usuario;
- acoplar el contrato a payloads custom exclusivos como `frontdesk_result`;
- depender únicamente de prompt engineering manual dentro de `behavior.instructions`.

La idea es acercar el runtime AI de Fluxbee a un modelo más rico, compatible con el enfoque del OpenAI Agents SDK:

- instrucciones base del agente;
- modificación del input inmediatamente antes del model call;
- structured outputs / output schema cuando aplique.

---

## 2. Problema que resuelve

Hoy el runner AI compartido tiene una instrucción base del nodo (`behavior.instructions`) y un input del usuario. Ese esquema es suficiente para conversación general, pero queda corto cuando un IO necesita una respuesta con **shape de máquina** para operar determinísticamente.

Caso inicial:

- `IO.api` en `subject_mode = explicit_subject` puede requerir una regularización síncrona de identidad antes de devolver HTTP.
- Hoy eso se resolvió con contratos específicos `frontdesk_handoff` / `frontdesk_result`.
- Ese camino funciona, pero introduce un contrato custom específico que otros IO presentes o futuros también tendrían que conocer si alguna vez interactúan con frontdesk.

La dirección propuesta es:

1. los IO pueden, cuando lo necesiten, adjuntar un `response_envelope` en metadata;
2. el runtime AI puede consumir esa metadata usando capacidades equivalentes a las del Agents SDK;
3. el nodo AI produce una salida acorde al envelope solicitado;
4. el IO consumidor decide cómo parsearla y qué hacer con ella.

---

## 3. Alcance

### 3.1 En alcance

- capacidad opcional en nodos `IO.*` para adjuntar `response_envelope`;
- soporte en el runtime AI compartido para leer esa metadata;
- integración con capacidades equivalentes del OpenAI Agents SDK;
- primer uso en `IO.api -> SY.frontdesk.gov`.

### 3.2 Fuera de alcance

- reemplazar `text/v1` como contrato general de los nodos AI;
- definir un envelope universal para todos los requests entre AI/WF/IO;
- definir un registry global de schemas arbitrarios;
- soportar de entrada todos los proveedores LLM no OpenAI con la misma profundidad;
- backward compatibility exhaustiva con `frontdesk_result` como prioridad de producto.

---

## 4. Principios de diseño

### 4.1 Opt-in

El envelope es **opcional**.

- si un `IO.*` no necesita respuesta con formato específico, no envía nada;
- si un emisor sí lo necesita para el hop actual, envía `meta.context.response_envelope`.

### 4.2 Metadata, no contenido del usuario

La necesidad de formato de salida es metadata operativa del transporte / consumer. No es parte del mensaje del usuario.

Por lo tanto:

- no debe ir concatenada al texto del usuario;
- no debe modelarse como si fuera una parte natural de la conversación;
- debe viajar en `meta.context.*`.

### 4.2.1 Dos carriers distintos

La capacidad necesita distinguir dos cosas:

- el contrato de respuesta del **hop actual**;
- el contrato esperado para la **respuesta final** de la operación completa.

Por lo tanto se definen dos carriers distintos:

- `meta.context.response_envelope`
- `meta.context.final_response_contract`

Regla:

- `response_envelope` no alcanza para representar simultáneamente el contrato del hop actual y el contrato final;
- esos dos roles no deben mezclarse en un único campo.

### 4.3 Runtime AI compartido, no hack por nodo

La capacidad debe vivir en el **SDK/runtime compartido de nodos AI**, no como hack exclusivo de `SY.frontdesk.gov`.

Frontdesk es solo el primer consumidor.

### 4.4 Portar capacidades existentes antes que inventar otras

La implementación debe priorizar portar o reflejar capacidades que ya existen en el OpenAI Agents SDK / Responses path:

- modificación del input inmediatamente antes del model call;
- structured outputs / output schema;
- separación conceptual entre instrucciones base y preparación del input de llamada.

Fuente práctica de referencia para ese port:

- la carpeta `agents-sdk-python` incluida en este repo;
- su código fuente debe usarse como referencia primaria para comportamiento y shape de capacidades nuevas del runtime AI.

### 4.5 Envelope chico y estructural

El envelope debe describir el **shape mínimo** de salida requerido.

No debe convertirse en:

- mini lenguaje de negocio;
- prompt largo explicativo;
- contrato de workflow completo;
- carrier de políticas HTTP complejas.

---

## 5. Modelo funcional

## 5.1 Lado IO

Un `IO.*` puede adjuntar en el mensaje interno:

```json
{
  "meta": {
    "context": {
      "response_envelope": {
        "kind": "json_object_v1",
        "strict": true,
        "required": ["success", "human_message"],
        "properties": {
          "success": {
            "type": "boolean",
            "description": "true if the operation completed successfully"
          },
          "error_code": {
            "type": "string",
            "description": "machine-readable error code when success=false"
          },
          "human_message": {
            "type": "string",
            "description": "brief human-readable message"
          }
        }
      },
      "final_response_contract": {
        "kind": "json_object_v1",
        "strict": true,
        "required": ["success", "human_message"],
        "properties": {
          "success": {
            "type": "boolean"
          },
          "error_code": {
            "type": "string"
          },
          "human_message": {
            "type": "string"
          }
        }
      }
    }
  }
}
```

### Reglas

- el envelope es metadata opcional;
- `response_envelope` y `final_response_contract` deben ser pequeños, estables y determinísticos;
- el IO que lo emite es responsable de que coincida con lo que realmente espera parsear;
- no se asume que todos los nodos del sistema lo soporten.

### Semántica de `response_envelope`

- `meta.context.response_envelope` define el shape esperado para la respuesta del receptor directo del mensaje actual.
- es un contrato hop-by-hop;
- no debe propagarse automáticamente a mensajes intermedios, handoffs, tool-calls ni delegaciones internas;
- si un nodo necesita un shape específico de un hop siguiente, debe construir un nuevo `response_envelope` para ese hop.

### Semántica de `final_response_contract`

- `meta.context.final_response_contract` define el shape esperado para la respuesta final de la operación original;
- no reemplaza a `response_envelope`;
- puede preservarse localmente o reenviarse como contexto pasivo;
- no obliga al hop siguiente a responder con ese shape;
- no debe usarse para describir coreografía interna, retries ni reglas de negocio intermedias.

### Regla de propagación

- `response_envelope` no se reenvía automáticamente;
- `final_response_contract` puede conservarse durante la orquestación si el nodo necesita recomponer la respuesta final;
- copiar `meta.context` completo hacia hops intermedios sin filtrar estos campos se considera incorrecto.

---

## 5.2 Lado AI runtime

Cuando el runtime AI recibe un mensaje:

1. resuelve `behavior.instructions` como instrucción base del nodo;
2. lee `meta.context.response_envelope` si existe;
3. si el envelope existe y el runtime soporta la capacidad:
   - agrega una instrucción dinámica por turno, inmediatamente antes del model call;
   - y, cuando aplique, configura structured output / output schema equivalente;
4. si no existe envelope, ejecuta el camino normal.

### Importante

Esto **no reemplaza** `behavior.instructions`.

- `behavior.instructions` sigue siendo la instrucción base del nodo;
- `response_envelope` agrega una restricción / expectativa **por turno**;
- el mensaje del usuario sigue viajando como input del usuario.

---

## 6. Qué traer del OpenAI Agents SDK

## 6.1 Capacidades a portar / reflejar

### A. Modificación del model input inmediatamente antes de la llamada

Portar una capacidad equivalente a `RunConfig.call_model_input_filter`.

Objetivo:

- permitir que el runtime ajuste instrucciones e input **por turno**, a partir de metadata del mensaje;
- evitar que todo se resuelva reescribiendo `behavior.instructions`;
- evitar meter la instrucción dentro del texto del usuario.

#### Uso en Fluxbee

El runtime AI debe poder:

- leer `meta.context.response_envelope`;
- transformarlo en una instrucción dinámica breve;
- aplicarla justo antes del model call.

#### Referencia primaria en `agents-sdk-python`

La referencia concreta a portar para esta fase es:

- `agents.run_config.ModelInputData`
- `agents.run_config.CallModelData`
- `agents.run_config.RunConfig.call_model_input_filter`
- `agents.run_internal.turn_preparation.maybe_filter_model_input(...)`

Semántica relevante del SDK Python:

- el runner prepara primero `instructions` + `input items`;
- luego invoca un hook opcional de filtrado inmediatamente antes del model call;
- ese hook puede reescribir instrucciones e inputs ya preparados;
- el hook recibe contexto de run y agente activo;
- si el hook no existe, el request preparado sigue por el camino normal.

#### Gap actual del port Rust

Hoy el port Rust todavía no tiene esa capa explícita.

Estado actual:

- `crates/fluxbee_ai_sdk/src/agent.rs` construye el request directamente en `Agent::build_request(...)`;
- `crates/fluxbee_ai_sdk/src/llm.rs` expone `LlmRequest { system, input, input_parts, ... }`;
- el camino actual no tiene un objeto intermedio equivalente a `ModelInputData`;
- el camino actual no tiene un hook equivalente a `call_model_input_filter`.

Conclusión práctica para esta fase:

- el primer objetivo no es inventar una API nueva desde cero;
- el primer objetivo es introducir en Rust una capa intermedia equivalente a `ModelInputData` y un hook previo al model call compatible con esa semántica.
- este gap es esperado y forma parte explícita del cambio a implementar en esta fase.

---

### B. Structured outputs / output schema

Portar soporte equivalente a `output_schema` / structured outputs.

Objetivo:

- cuando el envelope pida JSON objeto estructurado, no depender solo de una instrucción textual;
- pedir al proveedor una salida estructurada real cuando el modelo/provider lo permita;
- reducir casos de JSON inválido, incompleto o adornado con texto extra.

#### Uso en Fluxbee

Para `response_envelope.kind = json_object_v1`:

- si el provider/path soporta schema estructurado, usarlo;
- si no lo soporta, usar fallback textual controlado (ver sección 8).

#### Referencia primaria en `agents-sdk-python`

La referencia concreta a portar para esta fase es:

- `agents.agent_output.AgentOutputSchemaBase`
- `agents.agent_output.AgentOutputSchema`
- `agents.models.openai_responses.OpenAIResponsesModel.get_response_format(...)`

Semántica relevante del SDK Python:

- el SDK distingue plain text vs output estructurado;
- el schema JSON se construye como objeto explícito;
- el modo estricto se preserva en el schema;
- el provider OpenAI Responses recibe ese schema como `text.format = { type: "json_schema", ... }`;
- la salida del modelo se valida después contra el schema antes de tratarla como output válido.

#### Gap actual del port Rust

Hoy el port Rust no tiene todavía esa cadena completa.

Estado actual:

- `crates/fluxbee_ai_sdk/src/llm.rs` expone `LlmRequest` sin campo para output schema / response format;
- `OpenAiResponsesClient::generate(...)` arma el body solo con `model`, `input`, `max_output_tokens`, `temperature`, `top_p`;
- el cliente actual espera texto libre de vuelta (`output_text` o primer bloque textual);
- no existe validación post-respuesta contra schema;
- no existe abstracción equivalente a `AgentOutputSchema`.

Conclusión práctica para esta fase:

- no alcanza con agregar una instrucción dinámica que diga "devolvé JSON";
- para structured outputs reales hay que portar tres piezas en conjunto:
  - representación interna del schema;
  - traducción del schema al request de OpenAI Responses;
  - validación de la respuesta recibida contra ese schema.

### 6.1.10 Objetivo concreto de Fase 3

Para este trabajo, Fase 3 queda acotada a soportar structured outputs reales para el caso:

- `kind = json_object_v1`
- proveedor OpenAI Responses

No hace falta en esta fase:

- soporte general para cualquier provider;
- múltiples dialectos de schema;
- tipos complejos fuera del subconjunto ya cerrado en Fase 1;
- una abstracción pública general tan amplia como la del SDK Python.

Lo que sí debe quedar listo:

- una abstracción interna mínima de output schema;
- la traducción de `response_envelope` a esa abstracción;
- el mapeo a `response_format` para OpenAI Responses;
- validación explícita de la salida antes de devolverla al caller.

### 6.1.11 Extensión mínima de `LlmRequest`

Para soportar structured outputs reales, `LlmRequest` debe poder transportar una restricción de salida además del input.

#### Cambio mínimo recomendado

Agregar a `LlmRequest` un campo opcional equivalente a output schema / response format.

Forma conceptual mínima:

`output_schema: Option<OutputSchemaSpec>`

Donde `OutputSchemaSpec` representa:

- `name: String`
- `schema: Value`
- `strict: bool`

Regla:

- el campo debe ser opcional;
- si es `None`, el request sigue el camino actual de texto libre;
- si está presente, el cliente/provider puede intentar structured outputs reales.

#### Por qué esta forma y no otra

No conviene en esta fase guardar directamente el payload específico de OpenAI Responses dentro de `LlmRequest`.

Conviene separar:

- representación interna neutral del schema;
- traducción provider-specific al body HTTP.

Eso permite:

- mantener el SDK ordenado;
- testear mejor la traducción;
- no acoplar todo el modelo interno al shape del provider.

#### Relación con `response_envelope`

`response_envelope` no debe entrar crudo en `LlmRequest`.

Orden correcto:

1. `response_envelope`
2. traducción a `OutputSchemaSpec`
3. `LlmRequest.output_schema`
4. traducción provider-specific a `response_format`

#### Regla de compatibilidad

La incorporación de `output_schema` en `LlmRequest` no debe cambiar el contrato de los callers actuales:

- los callers que no usen schema no tienen que setear nada nuevo;
- el mock debe poder seguir funcionando con requests sin schema;
- los tests actuales del camino de texto libre deben seguir siendo válidos con cambios mínimos o nulos.

#### Primer target provider

En esta fase, la única traducción obligatoria es:

- `OutputSchemaSpec` -> `text.format.json_schema` para OpenAI Responses

No hace falta en esta fase:

- imponer que todos los providers soporten schema;
- agregar fallbacks provider-specific más allá del caso OpenAI actual.

### 6.1.12 Abstracción interna mínima de schema estructurado

Dentro de `fluxbee_ai_sdk` debe existir una abstracción interna reutilizable para representar output schemas estructurados, sin acoplarla directamente ni al carrier `response_envelope` ni al payload específico de OpenAI.

#### Objetivo

Separar tres niveles:

1. contrato externo pedido por el emisor (`response_envelope`);
2. representación interna reusable del SDK (`OutputSchemaSpec`);
3. traducción provider-specific (`response_format` de OpenAI Responses).

#### Estructura conceptual mínima

`OutputSchemaSpec`

- `name: String`
- `schema: Value`
- `strict: bool`

Responsabilidad:

- encapsular el schema JSON ya listo para validación/traducción;
- ser la unidad que viaja dentro de `LlmRequest`;
- no depender del nombre del campo original en metadata.

#### Helpers mínimos recomendados

`OutputSchemaSpec::new(...)`

- construye la instancia validando shape básico;

`OutputSchemaSpec::json_schema()`

- devuelve el schema JSON interno;

`OutputSchemaSpec::strict()`

- devuelve si debe usarse modo estricto;

`OutputSchemaSpec::name()`

- devuelve el nombre lógico del output.

No hace falta en esta fase una jerarquía compleja equivalente a `AgentOutputSchemaBase`.

#### Validación mínima de la abstracción

`OutputSchemaSpec` debe garantizar al menos:

- que `name` no esté vacío;
- que `schema` sea un objeto JSON;
- que `strict` preserve el valor pedido por el carrier;
- que la instancia sea serializable hacia el provider sin reinterpretaciones ambiguas.

#### Relación con el envelope

`OutputSchemaSpec` no reemplaza a `response_envelope`.

Regla:

- `response_envelope` es metadata externa de integración;
- `OutputSchemaSpec` es la forma interna lista para runtime/provider;
- la traducción entre ambos debe vivir en una capa explícita del SDK.

#### Relación con validación de salida

La misma abstracción debe servir como base para:

- construir `response_format` para el provider;
- validar la respuesta estructurada recibida.

Eso evita tener dos descripciones distintas del mismo output esperado.

#### Alcance deliberadamente acotado

En esta fase no hace falta:

- soportar múltiples clases de output schema;
- soportar wrapping automático de tipos arbitrarios;
- soportar Pydantic-like adapters o reflection avanzada;
- replicar toda la API de `AgentOutputSchema` del SDK Python.

La prioridad es tener una pieza interna mínima, estable y testeable para el caso `json_object_v1`.

### 6.1.13 Traducción `response_envelope -> OutputSchemaSpec`

Para esta fase, la única traducción obligatoria es:

- `response_envelope.kind = "json_object_v1"` -> `OutputSchemaSpec`

#### Objetivo

Convertir el carrier externo, pequeño y estable, en un schema JSON interno listo para:

- ser enviado al provider;
- y validar la respuesta devuelta.

#### Regla de traducción

Dado este carrier:

```json
{
  "kind": "json_object_v1",
  "strict": true,
  "required": ["success", "human_message"],
  "properties": {
    "success": { "type": "boolean" },
    "error_code": { "type": "string" },
    "human_message": { "type": "string" }
  }
}
```

la traducción a `OutputSchemaSpec` debe generar conceptualmente:

```json
{
  "name": "final_output",
  "schema": {
    "type": "object",
    "properties": {
      "success": { "type": "boolean" },
      "error_code": { "type": "string" },
      "human_message": { "type": "string" }
    },
    "required": ["success", "human_message"],
    "additionalProperties": false
  },
  "strict": true
}
```

#### Reglas específicas de la traducción

- `kind` selecciona la estrategia de traducción;
- en v1, solo `json_object_v1` es válido;
- `strict` se copia a `OutputSchemaSpec.strict`;
- `required` se copia al `schema.required`;
- `properties` se traduce directamente a `schema.properties`;
- el schema generado debe ser siempre `type = "object"`;
- en v1 debe usarse `additionalProperties = false` para mantener el contrato chico y determinístico.

#### Nombre del schema

En esta fase, el nombre interno recomendado es:

- `final_output`

Regla:

- puede ser fijo en esta primera implementación;
- no hace falta derivarlo dinámicamente del nodo o del carrier;
- si más adelante hiciera falta un nombre configurable, debe agregarse explícitamente y no inferirse de forma ambigua.

#### Tipos de campo

La traducción debe respetar el subconjunto cerrado en Fase 1:

- `string`
- `boolean`
- `integer`
- `number`

Y mapearlos sin reinterpretación.

Regla:

- si el carrier pide un tipo fuera de ese subconjunto, la traducción falla con `invalid_response_contract`.

#### `enum`

Si `properties.<field>.enum` existe:

- debe copiarse al campo JSON Schema correspondiente;
- solo se admite para propiedades `string`;
- si aparece sobre otro tipo, la traducción debe fallar con `invalid_response_contract`.

#### Campos opcionales

Los campos presentes en `properties` pero ausentes de `required`:

- deben incluirse igualmente en `schema.properties`;
- no deben aparecer en `schema.required`.

#### `final_response_contract`

En esta fase, `final_response_contract` reutiliza exactamente la misma traducción estructural que `response_envelope`.

La diferencia no está en el schema generado, sino en cuándo y para qué se aplica.

#### Error canónico de traducción

Si el carrier no puede traducirse a `OutputSchemaSpec`, el error canónico sigue siendo:

- `invalid_response_contract`

Casos típicos:

- `kind` no soportado;
- `required` inconsistente con `properties`;
- tipo no soportado;
- `enum` inválido;
- shape no compatible con objeto top-level flat de v1.

#### Resultado esperado de Fase 3.3

Al cerrar esta fase, el SDK debe tener una función explícita y testeable equivalente a:

- `response_envelope_to_output_schema_spec(...) -> Result<OutputSchemaSpec>`

Esa función debe ser:

- determinística;
- libre de side effects;
- independiente del provider;
- reusable tanto para `response_envelope` como para `final_response_contract`.

### 6.1.14 Integración con `OpenAiResponsesClient`

Una vez que existe `OutputSchemaSpec`, el siguiente paso es integrarlo con el cliente actual de OpenAI Responses del port Rust.

#### Objetivo

Hacer que `OpenAiResponsesClient` pueda:

- seguir operando en modo texto libre cuando no hay schema;
- y usar structured outputs reales cuando `LlmRequest.output_schema` esté presente.

#### Cambio mínimo en el body del request

Hoy el cliente arma el body con:

- `model`
- `input`
- `max_output_tokens`
- `temperature`
- `top_p`

Con structured outputs, debe agregar además el bloque equivalente a:

```json
{
  "text": {
    "format": {
      "type": "json_schema",
      "name": "final_output",
      "schema": { ... },
      "strict": true
    }
  }
}
```

Regla:

- si `request.output_schema` es `None`, el bloque `text.format` no se envía;
- si `request.output_schema` está presente, el cliente debe construir `text.format` a partir de ese valor;
- la traducción provider-specific debe vivir dentro del cliente OpenAI, no en la capa del carrier.

#### Responsabilidad de la traducción provider-specific

Conviene introducir un helper explícito, por ejemplo:

- `build_openai_response_format(output_schema: &OutputSchemaSpec) -> Value`

Responsabilidad:

- traducir `OutputSchemaSpec` al shape exacto esperado por OpenAI Responses;
- mantener esa lógica fuera del armado general de `PreparedModelInput`.

#### Lectura de la respuesta

Cuando el request se envía con output schema:

- el cliente no debe tratar la salida simplemente como texto libre "cualquiera";
- debe extraer el bloque textual JSON devuelto por Responses;
- y pasarlo a la validación post-respuesta contra el mismo `OutputSchemaSpec`.

Regla:

- el cliente OpenAI puede seguir leyendo `output_text` o el primer contenido textual utilizable;
- pero el resultado no debe considerarse válido hasta pasar la validación estructurada.

#### Compatibilidad

La integración no debe romper el camino actual:

- requests sin schema siguen por el mismo flujo;
- tests actuales del cliente sin structured output deben seguir siendo válidos;
- el mock debe tolerar tanto requests con schema como sin schema.

#### Resultado esperado de Fase 3.4

Al cerrar esta fase, `OpenAiResponsesClient` debe poder:

1. recibir `LlmRequest` con o sin `output_schema`;
2. traducir `output_schema` a `text.format.json_schema` cuando exista;
3. seguir usando texto libre cuando no exista;
4. dejar la validación estructurada lista para la fase siguiente.

### 6.1.15 Fallback cuando no hay structured outputs reales

El runtime debe tener un fallback explícito cuando el provider/path elegido no soporte schema estructurado real para ese request.

#### Objetivo

Evitar dos problemas:

- fallar innecesariamente cuando el nodo todavía puede intentar cumplir el contrato por instrucción;
- degradar de forma silenciosa sin dejar trazabilidad.

#### Regla principal

Si existe `output_schema` pero el provider/path no puede usar structured outputs reales:

- el runtime puede degradar a un camino textual controlado;
- esa degradación debe ser explícita;
- no debe cambiar el carrier ni el contrato pedido por el caller;
- no debe inyectarse la instrucción dentro del user text.

#### Forma del fallback

El fallback debe consistir en:

1. no enviar `text.format.json_schema` al provider;
2. traducir `OutputSchemaSpec` a una instrucción dinámica breve y estable;
3. agregar esa instrucción al bloque de instrucciones dinámicas del turno;
4. seguir el model call como texto libre;
5. validar después la respuesta contra el mismo `OutputSchemaSpec`.

#### Regla de instrucción dinámica

La instrucción de fallback debe:

- decir que la respuesta final debe ser JSON válido;
- pedir que no haya markdown fences ni texto extra fuera del JSON;
- pedir que el objeto cumpla exactamente los campos requeridos por el schema;
- mantenerse breve y determinística.

No debe:

- copiar descripciones largas del carrier;
- meter lógica de negocio adicional;
- duplicar el texto completo del schema si no hace falta.

#### Criterio para activar el fallback

El fallback debe activarse cuando ocurra alguna de estas condiciones:

- el provider no soporte structured outputs;
- el path actual del cliente no soporte `text.format.json_schema`;
- la implementación de esta fase todavía no tenga disponible la traducción provider-specific para ese request.

No debe activarse cuando:

- el provider/path sí soporta structured outputs reales y el schema es válido.

#### Trazabilidad

El runtime debe dejar trazabilidad suficiente para distinguir:

- request con structured outputs reales;
- request degradado a fallback textual;
- request sin contract estructurado desde el inicio.

No hace falta en esta fase definir el formato final de logs/métricas, pero sí la obligación de distinguir esos caminos.

#### Compatibilidad

El fallback no debe cambiar el comportamiento de requests sin envelope/schema.

Solo aplica cuando:

- el caller pidió output estructurado;
- pero el runtime no puede usar schema real para ese turno.

### 6.1.16 Validación de la respuesta estructurada

Antes de devolver la respuesta al nodo consumidor, el runtime debe validar la salida del modelo contra el mismo `OutputSchemaSpec` usado para el request o para el fallback.

#### Objetivo

Garantizar que el caller reciba:

- un objeto compatible con el contract pedido;
- o un error explícito y trazable si el modelo no cumplió el shape esperado.

#### Regla principal

Si el turno usó `output_schema` o su fallback textual equivalente:

- la salida no debe considerarse válida solo porque el modelo devolvió texto;
- debe pasar primero por validación estructurada contra `OutputSchemaSpec`.

#### Etapas de validación

La validación debe ocurrir en este orden:

1. extraer el bloque textual utilizable de la respuesta del provider;
2. obtener un candidato JSON;
3. parsear ese candidato como JSON;
4. validar que el valor parseado sea compatible con `OutputSchemaSpec.schema`;
5. recién entonces devolver el resultado al caller.

#### Reglas mínimas de extracción del candidato JSON

En el camino de fallback textual, el runtime puede necesitar normalizar la salida antes de validar.

Se admite extraer el candidato JSON usando reglas conservadoras como:

- si la respuesta completa ya es un objeto JSON, usarla tal cual;
- si viene envuelta en markdown fences, remover fences;
- si hay texto residual antes/después y puede localizarse un único objeto JSON bien delimitado, usar ese objeto;
- si no puede identificarse un candidato claro, la validación falla.

Regla:

- esta extracción es un paso técnico de robustez;
- no debe convertirse en un parser permisivo que "adivina" salidas semánticamente ambiguas.

#### Alcance de la validación en v1

Para el subconjunto `json_object_v1`, la validación mínima debe verificar:

- que el valor raíz sea un objeto;
- que existan todos los campos listados en `required`;
- que cada propiedad presente tenga el tipo declarado;
- que `enum`, si existe, se cumpla;
- que no existan propiedades extra cuando el schema generado usa `additionalProperties = false`.

#### Un único schema para request y response

La validación debe usar exactamente el mismo `OutputSchemaSpec` que se utilizó para:

- construir `response_format`, o
- construir la instrucción dinámica de fallback.

Regla:

- no debe haber una segunda representación ad hoc del contract para validar la respuesta;
- el runtime debe evitar drift entre lo que pide y lo que valida.

#### Resultado de una validación exitosa

Si la validación pasa:

- el runtime obtiene un valor JSON estructurado confiable;
- ese valor es el que debe devolver al nodo consumidor;
- el nodo consumidor no debería tener que revalidar shape básico del contract si confía en el runtime.

#### Resultado de una validación fallida

Si la validación falla:

- el runtime no debe devolver esa salida como si fuera válida;
- debe producir un error explícito;
- ese error debe ser distinguible de errores de transporte o de provider.

### 6.1.17 Error canónico para salida estructurada inválida

Cuando el modelo devuelve una salida que no puede validarse contra `OutputSchemaSpec`, el runtime debe usar un error canónico distinto de:

- `invalid_response_contract` (contract pedido inválido);
- errores HTTP/transporte/provider;
- errores internos no relacionados con schema.

#### Código canónico recomendado

- `invalid_structured_output`

#### Semántica

Este error significa:

- el contract pedido era válido;
- el runtime pudo intentar aplicar structured outputs reales o fallback textual;
- pero la salida devuelta por el modelo no cumplió el schema esperado.

#### Casos cubiertos

`invalid_structured_output` aplica cuando ocurre cualquiera de estos casos:

- la respuesta no contiene JSON parseable;
- la respuesta contiene JSON pero no es un objeto válido para `json_object_v1`;
- faltan campos requeridos;
- algún campo tiene tipo incorrecto;
- `enum` no se cumple;
- aparecen propiedades extra no admitidas;
- no puede extraerse un candidato JSON inequívoco del texto devuelto.

#### Lo que no cubre

No debe usarse `invalid_structured_output` cuando:

- el contract pedido ya era inválido antes del model call;
- el provider rechazó el request;
- hubo timeout, error de red o error interno del cliente;
- el runtime decidió no usar schema porque no había contract estructurado.

#### Información recomendada asociada al error

Sin definir todavía el shape exacto del error wire-visible, el runtime debería poder conservar internamente al menos:

- `code = "invalid_structured_output"`
- detalle resumido del fallo (`json_parse_error`, `missing_required_field`, `type_mismatch`, etc.)
- si el turno usó structured outputs reales o fallback textual

#### Regla de manejo

Cuando aparece este error:

- el runtime no debe devolver el payload inválido como éxito;
- el caller debe poder distinguir que el problema fue incumplimiento del schema por parte del modelo;
- el error debe ser trazable para debugging y tuning del prompt/schema.

### 6.1.18 Extracción segura desde `Message.meta.context`

En el protocolo actual del repo, `response_envelope` y `final_response_contract` viven dentro de `meta.context`, que hoy está tipado como `Option<Value>` y no como campos top-level dedicados.

#### Objetivo

Evitar que cada caller del runtime haga parsing ad hoc de `meta.context` y termine reproduciendo reglas inconsistentes.

#### Regla principal

La extracción de ambos carriers debe centralizarse en helpers explícitos del SDK.

Helpers recomendados:

- `extract_response_envelope(meta: &Meta) -> Result<Option<Value>>`
- `extract_final_response_contract(meta: &Meta) -> Result<Option<Value>>`

O equivalentemente helpers que devuelvan tipos ya parseados/validados.

#### Reglas de extracción

La extracción debe seguir estas reglas:

- si `meta.context` es `None`, ambos valores son `None`;
- si `meta.context` no es un objeto JSON, la extracción falla con `invalid_response_contract`;
- si la clave no existe, el valor es `None`;
- si la clave existe pero su valor no es un objeto JSON válido para v1, la extracción falla con `invalid_response_contract`;
- la extracción no debe reinterpretar nombres alternativos ni aliases implícitos en esta fase.

#### Claves canónicas

En esta fase, las únicas claves canónicas son:

- `meta.context.response_envelope`
- `meta.context.final_response_contract`

No se debe aceptar automáticamente:

- `meta.context.io.response_envelope`
- aliases legacy
- nested locations ambiguas

salvo que una migración futura lo agregue explícitamente.

#### Integración con `ModelCallContext`

La construcción de `ModelCallContext` debe usar esos helpers y no leer `meta.context` directamente.

Orden recomendado:

1. leer `Message.meta`;
2. extraer carriers estructurados con helpers;
3. poblar `ModelCallContext` con esos valores;
4. pasar `ModelCallContext` al filtro pre-call.

#### Beneficio adicional

Este mismo punto central de extracción también sirve para:

- aplicar la regla de no propagación automática;
- validar temprano el shape del carrier;
- mantener simetría entre lectura para runtime y lectura para `io-common`.

### 6.1.19 Regla de preservación en replies

El SDK ya sanitiza parte de `meta.context` al construir replies.

Con la introducción de estos carriers, debe quedar explícito que:

- `response_envelope` no debe propagarse automáticamente al reply del hop siguiente;
- `final_response_contract` solo debe preservarse si el nodo orquestador lo necesita para recomponer la respuesta final;
- esa preservación no debe depender de copiar `meta.context` crudo.

Dirección recomendada:

- la lógica de reply debe filtrar explícitamente estos carriers;
- y la preservación de `final_response_contract`, cuando corresponda, debe ser una decisión explícita del runtime/nodo, no un efecto colateral del clone de `Meta`.

---

### C. Modelo interno de input más rico que `system + user_text`

Actualizar el runtime interno para que pueda representar:

- instrucciones base del nodo;
- instrucciones dinámicas por turno;
- input del usuario;
- input parts / multimodal;
- constraint de salida.

No es necesario copiar exactamente cada clase del SDK Python, pero sí acercar el port Rust a esa arquitectura, para no quedar atado a una sola string `system` más un único `input` plano.

### 6.1.1 Objetivo concreto de Fase 2

Para este trabajo, Fase 2 queda acotada a introducir una preparación explícita del input del modelo con esta estructura conceptual mínima:

- instrucciones base del nodo;
- instrucciones dinámicas por turno;
- input textual principal;
- input parts preparados;
- metadata suficiente para aplicar filtros pre-call.

No hace falta en esta fase:

- portar sessions;
- portar handoffs;
- portar guardrails;
- portar toda la semántica del runner Python.

Lo que sí debe quedar listo:

- un paso explícito de "preparar model input";
- un hook previo al model call para transformar ese input preparado;
- compatibilidad con el camino actual cuando no haya filtro ni envelope.

### 6.1.2 Propuesta mínima de port Rust para Fase 2

La propuesta mínima para el port Rust es introducir una capa intermedia pequeña y explícita, sin intentar copiar toda la superficie del runner Python.

#### Estructuras conceptuales mínimas

`PreparedModelInput`

- `instructions_base: Option<String>`
- `instructions_dynamic: Vec<String>`
- `input_text: Option<String>`
- `input_parts: Vec<Value>`

Semántica:

- `instructions_base` representa la instrucción estable del nodo;
- `instructions_dynamic` agrega restricciones por turno derivadas de metadata/runtime;
- `input_text` preserva el input textual principal cuando exista;
- `input_parts` representa el input ya preparado para el proveedor.

`ModelCallContext`

- `agent_name: String`
- `message_meta: Meta`
- `response_envelope: Option<Value>`
- `final_response_contract: Option<Value>`

Semántica:

- no es el mensaje completo del router;
- contiene solo el contexto mínimo necesario para decidir transformaciones pre-call.

`ModelInputFilter`

- hook opcional interno del runtime;
- recibe `PreparedModelInput` + `ModelCallContext`;
- devuelve un `PreparedModelInput` modificado;
- corre inmediatamente antes de materializar `LlmRequest`.

#### Regla de composición

El request al modelo debe construirse en este orden conceptual:

1. resolver instrucciones base del nodo;
2. preparar input textual y/o parts;
3. derivar instrucciones dinámicas por turno desde metadata relevante;
4. aplicar `ModelInputFilter` si existe;
5. recién entonces materializar `LlmRequest`.

#### Regla de compatibilidad

Si no hay envelope ni filtro:

- `instructions_dynamic = []`;
- `input_parts` puede seguir el camino actual;
- el comportamiento final debe ser equivalente al runtime actual.

#### Inserción recomendada en el port actual

En el estado actual del repo, la inserción natural es entre:

- la lógica que hoy resuelve el input y llama a `Agent::build_request(...)`;
- y la materialización final de `LlmRequest`.

Dirección recomendada:

- dejar de construir `LlmRequest` directamente dentro de `Agent::build_request(...)`;
- mover primero a una función explícita de preparación;
- usar una segunda función para convertir `PreparedModelInput` en `LlmRequest`.

### 6.1.3 Punto exacto de inserción en el repo actual

El relevamiento del repo muestra tres superficies distintas:

#### A. Camino simple de `Agent`

Archivos relevantes:

- `crates/fluxbee_ai_sdk/src/agent.rs`
- `crates/fluxbee_ai_sdk/src/llm.rs`

Estado actual:

- `Agent::run_text(...)` llama directo a `self.build_request(...)`;
- `Agent::build_request(...)` materializa `LlmRequest` en forma inmediata;
- ese camino hoy no tiene etapa explícita de preparación intermedia.

Conclusión:

- este es el punto más directo donde introducir `PreparedModelInput -> LlmRequest`;
- `Agent::build_request(...)` no debería seguir siendo el lugar donde se mezclan resolución de instrucciones y materialización final del request.

#### B. Camino principal del runner con function calling

Archivos relevantes:

- `crates/fluxbee_ai_sdk/src/function_calling.rs`
- `crates/fluxbee_ai_sdk/src/immediate_memory.rs`

Estado actual:

- el runner arma primero `FunctionLoopItem`s con `build_initial_items(...)`;
- ese camino ya tiene una etapa previa de preparación lógica del input;
- luego el modelo de function calling consume `FunctionModelTurnRequest`, no `LlmRequest` directo.

Conclusión:

- para el caso del runner AI moderno, este camino es el más importante funcionalmente;
- la preparación tipo `PreparedModelInput` debe calzar antes de materializar el pedido al proveedor/modelo efectivo;
- la nueva capa no debe romper el armado previo de items (`SystemText`, `UserText`, `UserContentParts`, etc.), sino operar encima de ese input ya preparado.

#### C. Superficies secundarias que también construyen `LlmRequest`

Archivo relevante:

- `crates/fluxbee_ai_sdk/src/summary_refresh.rs`

Estado actual:

- `refresh_conversation_summary(...)` construye `LlmRequest` directo;
- es una superficie auxiliar del SDK, no el camino principal del runtime de nodos.

Conclusión:

- no debe ser el punto inicial del cambio;
- pero una vez introducida la nueva preparación común, conviene evaluar si esta superficie debe migrar al mismo patrón para no dejar dos formas incompatibles de armar requests.

### 6.1.4 Decisión para Fase 2.3

La decisión recomendada para esta fase es:

1. tomar como punto principal de inserción el camino `Agent` / request simple y el camino de model turn usado por el runner;
2. introducir una preparación explícita reutilizable antes de `LlmRequest`;
3. dejar `summary_refresh` como superficie secundaria a alinear después, salvo que la implementación común ya permita adoptarla sin costo alto.

### 6.1.5 Objetivo mínimo del refactor

Al terminar esta fase, el repo debería distinguir claramente entre:

- preparación del input del modelo;
- filtrado pre-call;
- materialización final del request al proveedor.

Eso debe quedar visible en la estructura del SDK, no solo como una convención implícita.

### 6.1.6 Hook interno equivalente a `call_model_input_filter`

El port Rust debe introducir un hook interno explícito, inspirado en `RunConfig.call_model_input_filter`, pero acotado al runtime actual del repo.

#### Objetivo del hook

Permitir que el runtime modifique el input ya preparado inmediatamente antes del model call, usando metadata del mensaje Fluxbee.

Casos de uso iniciales:

- agregar instrucciones dinámicas derivadas de `response_envelope`;
- agregar instrucciones dinámicas derivadas de `final_response_contract`;
- aplicar pequeñas transformaciones de input por turno sin reescribir la instrucción base del nodo.

#### Forma mínima recomendada

El hook debe recibir:

- `PreparedModelInput`
- `ModelCallContext`

Y debe devolver:

- `PreparedModelInput`

Semántica:

- recibe input ya preparado, no payload crudo del canal;
- corre inmediatamente antes de materializar `LlmRequest`;
- puede modificar instrucciones e input ya preparados;
- no debe mutar estado global ni depender de side effects implícitos.

#### Interfaz conceptual recomendada

`PreparedModelInputFilter`

- hook opcional del runtime;
- firma conceptual:
  - `fn apply(prepared: PreparedModelInput, ctx: &ModelCallContext) -> Result<PreparedModelInput>`

No es obligatorio que la primera versión exponga esta interfaz como extensión pública general del SDK.

En v1 puede vivir como:

- helper interno del runtime AI;
- o callback interno inyectado por el runner/nodo.

#### Regla de composición del hook

El hook no reemplaza la preparación base del input.

Orden obligatorio:

1. preparar input base;
2. construir `ModelCallContext`;
3. aplicar filtro si existe;
4. validar el resultado del filtro;
5. materializar `LlmRequest`.

#### Regla de validación del resultado

Después del hook, el runtime debe garantizar al menos:

- que siga existiendo una forma válida de input para el proveedor;
- que las instrucciones dinámicas no queden duplicadas de forma accidental;
- que el filtro no deje un request vacío salvo que el caller lo permita explícitamente;
- que errores del hook sean trazables y fallen de forma explícita.

#### Regla de compatibilidad

Si no hay hook:

- el runtime sigue por el camino actual;
- el comportamiento debe ser equivalente al existente hoy.

Si hay hook pero no hay metadata relevante:

- el hook debe comportarse como no-op.

### 6.1.7 Primer filtro concreto a implementar

Para este trabajo, el primer filtro concreto del runtime debe ser el que traduce:

- `meta.context.response_envelope`
- y, cuando corresponda, `meta.context.final_response_contract`

a instrucciones dinámicas por turno y/o restricciones de output.

Regla:

- el filtro no debe decidir lógica de negocio del nodo;
- solo debe preparar restricciones de input/output para el model call.

### 6.1.8 Acceso a metadata del mensaje Fluxbee antes del model call

Para que el hook pre-call sea útil en el runtime real, debe poder leer metadata del mensaje Fluxbee antes de materializar el request al modelo.

#### Principio

La metadata relevante del mensaje debe llegar al hook por una estructura de contexto acotada, no por acceso implícito a un `Message` global ni por lectura lateral desde otras capas.

#### Metadata mínima requerida en esta fase

`ModelCallContext` debe poder exponer, como mínimo:

- `meta.type`
- `meta.src_ilk`
- `meta.dst_ilk`
- `meta.ich`
- `meta.thread_id`
- `meta.thread_seq`
- `meta.priority`
- `meta.context.response_envelope`
- `meta.context.final_response_contract`

Regla:

- el hook no necesita recibir `routing` ni `payload` completos para este caso;
- si más adelante hacen falta otros campos, deben agregarse de forma explícita.

#### Aclaración importante: metadata disponible no implica metadata visible para el modelo

La metadata anterior debe estar disponible para el runtime y para el hook pre-call, pero eso no implica que toda esa metadata se inyecte al modelo o al prompt.

Regla:

- `ModelCallContext` es contexto interno del runtime;
- no es equivalente al input final visible para el modelo;
- solo la metadata necesaria para un caso concreto debe proyectarse a instrucciones dinámicas, input parts o restricciones de output.

Ejemplos:

- `response_envelope` puede traducirse a instrucciones dinámicas y/o schema de salida;
- `final_response_contract` puede usarse para orientar la respuesta final cuando el nodo orquesta múltiples hops;
- `thread_id`, `src_ilk`, `dst_ilk`, `ich`, `thread_seq`, `priority` o `meta.type` no deben entrar al modelo automáticamente solo por estar presentes en el carrier.

#### Fuente de esa metadata

En el runtime de nodos AI, la fuente primaria es el `Message` recibido por `AiNode::on_message(...)`.

La etapa previa al model call debe:

1. leer el `Message` de entrada;
2. extraer la metadata mínima relevante;
3. construir `ModelCallContext`;
4. pasar ese contexto al hook.

#### Regla de encapsulación

El hook no debe depender de:

- leer `meta.context` sin tipado desde cualquier lado del código;
- reparsear el `Message` completo por su cuenta;
- conocer detalles del transporte router/socket.

La resolución de metadata relevante debe ocurrir una sola vez antes del hook.

#### Helpers recomendados

Conviene introducir helpers explícitos para:

- extraer `response_envelope` desde `meta.context`;
- extraer `final_response_contract` desde `meta.context`;
- construir `ModelCallContext` desde `Message`.

Eso evita que cada nodo o cada caller rearme su propia lógica de extracción.

#### Regla de faltantes

Si alguno de esos campos no está presente:

- `ModelCallContext` debe usar `Option`;
- la ausencia de esos campos no debe romper el model call por sí sola;
- solo debe romper si una capacidad concreta los requiere como obligatorios para ese turno.

#### Objetivo de Fase 2.5

Al cerrar esta fase, el SDK debe tener un camino claro y único para:

- tomar un `Message` Fluxbee;
- derivar contexto tipado de model call;
- entregar ese contexto al filtro pre-call;
- seguir con el flujo normal si no hay metadata relevante.

### 6.1.9 Compatibilidad con el camino actual sin envelope

La introducción de `PreparedModelInput`, `ModelCallContext` y el hook pre-call no debe romper el comportamiento actual del runtime cuando el mensaje no trae `response_envelope` ni `final_response_contract`.

#### Regla principal

Si no hay metadata nueva relevante:

- el runtime debe preparar el model input de forma equivalente al camino actual;
- el hook debe comportarse como no-op;
- el `LlmRequest` materializado debe ser semánticamente equivalente al que hoy genera el runtime.

#### Compatibilidad esperada por superficie

Para el camino simple de `Agent`:

- `instructions_base` debe seguir resolviéndose desde `Agent.instructions`;
- si no hay instrucciones dinámicas, el request final debe seguir usando solo esa base;
- `input_text` e `input_parts` deben mantener el comportamiento actual.

Para el runner con function calling:

- los `FunctionLoopItem` ya construidos no deben cambiar de semántica por este refactor;
- la nueva capa debe actuar sobre el input ya preparado, no alterar retroactivamente la lógica del loop;
- si no hay metadata nueva, el model turn debe comportarse igual que hoy.

Para superficies auxiliares como `summary_refresh`:

- la migración al patrón nuevo puede diferirse;
- mientras tanto no debe quedar bloqueada por el refactor principal.

#### Criterio de validación de compatibilidad

Debe poder verificarse que:

- un mensaje sin envelope sigue produciendo la misma clase de request al proveedor;
- un mensaje sin envelope sigue produciendo la misma conducta observable del nodo;
- el cambio agrega capacidad nueva, no cambia silenciosamente el contrato de los casos existentes.

---

## 6.2 Qué no traer como prioridad

No hace falta en este cambio:

- handoffs avanzados;
- guardrails framework completo;
- persistencia de run state del Agents SDK;
- streaming event framework completo;
- toda la superficie del Python SDK si no aporta al problema concreto.

La prioridad es traer el subconjunto que resuelve bien:

- instrucciones dinámicas por turno;
- structured output;
- ensamblado limpio del request al modelo.

---

## 7. Shape formal v1

## 7.1 Shape canónico de `response_envelope`

```json
{
  "kind": "json_object_v1",
  "strict": true,
  "required": ["success", "human_message"],
  "properties": {
    "success": {
      "type": "boolean",
      "description": "true if the operation completed successfully"
    },
    "error_code": {
      "type": "string",
      "description": "machine-readable error code when success=false"
    },
    "human_message": {
      "type": "string",
      "description": "brief human-readable message"
    }
  }
}
```

## 7.1.1 Shape canónico de `final_response_contract`

En v1, `final_response_contract` reutiliza exactamente el mismo shape estructural que `response_envelope`.

Regla:

- cambia su semántica de uso;
- no cambia su shape base en esta primera versión.

## 7.2 Reglas de validación v1

### Obligatorios del envelope

- `kind`
- `required`
- `properties`

### Recomendados

- `strict`

### Dentro de `properties.<field>`

- `type`
- `description` corta
- `enum` opcional

Reglas adicionales:

- `kind` debe ser `json_object_v1`;
- `strict`, si está presente, debe ser booleano;
- `required` debe ser un array de strings no vacío;
- `properties` debe ser un objeto JSON no vacío;
- todo nombre listado en `required` debe existir en `properties`;
- no se permiten claves repetidas ni nombres vacíos;
- `properties.<field>.description` es opcional pero, si existe, debe ser corta y estable;
- si `enum` existe, todos sus valores deben ser strings.

## 7.3 Restricciones del modelo v1

En v1 el contrato queda deliberadamente acotado a un shape fácil de validar y traducir a structured outputs.

Reglas:

- el shape base es siempre un objeto JSON top-level;
- v1 asume top-level flat object;
- no se permiten nested objects arbitrarios;
- no se permiten arrays como tipo de campo en este primer corte;
- no se permiten `oneOf`, `anyOf`, `allOf`, `patternProperties` ni reglas avanzadas equivalentes;
- no se permite describir múltiples variantes de payload en un mismo contract.

## 7.4 Tipos permitidos por propiedad en v1

Tipos soportados en `properties.<field>.type`:

- `string`
- `boolean`
- `integer`
- `number`

Reglas:

- `string` puede usar `enum`;
- `boolean` no usa `enum` en v1;
- `integer` y `number` no usan límites (`minimum`, `maximum`) en v1;
- cualquier tipo fuera de este subconjunto debe considerarse inválido para esta versión.

## 7.5 Política para campos opcionales

Reglas:

- un campo presente en `properties` pero ausente en `required` se considera opcional;
- en v1, opcional significa que el campo puede omitirse;
- en v1, opcional no significa que el campo pueda emitirse como `null`;
- si el campo está presente en la respuesta, debe respetar el tipo declarado en `properties.<field>.type`;
- el runtime debe intentar preservar ese carácter opcional en el schema traducido al proveedor;
- el consumer no debe asumir presencia de campos opcionales salvo que su lógica lo controle explícitamente.

## 7.6 Error canónico ante contract inválido

Si `response_envelope` o `final_response_contract` no validan el shape v1, el runtime o consumer debe tratarlo como error de contrato inválido.

Código canónico recomendado para este primer corte:

- `invalid_response_contract`

Semántica:

- el contrato recibido no cumple el shape admitido por esta versión;
- no debe degradarse silenciosamente a un comportamiento implícito;
- el error debe quedar trazable en logs/observabilidad.

## 7.7 Qué no meter en v1

No meter todavía:

- descripciones largas;
- ejemplos extensos;
- reglas HTTP detalladas por código;
- nested schemas complejos si no son necesarios;
- lógica de retry;
- reglas conversacionales;
- políticas de negocio profundas.

---

## 8. Modo de degradación / MVP

Para un MVP, se permite una degradación explícita:

- si el runtime AI todavía no soporta structured outputs reales;
- o si el provider/path elegido no los soporta en esa llamada;
- el runtime puede traducir el envelope a una instrucción dinámica textual.

### Regla importante

Ese fallback textual debe seguir siendo:

- instrucción adicional de sistema;
- no concatenación dentro del mensaje del usuario;
- no pseudo-system message metido en `text/v1.content`.

### Forma esperada del MVP

1. `IO.*` envía `meta.context.response_envelope`;
2. el runtime AI lo lee;
3. si no puede aplicar schema estructurado real, lo transforma en instrucción adicional equivalente;
4. el nodo responde según el envelope.

Este fallback es aceptable como MVP, pero no debe congelar el diseño a largo plazo.

---

## 9. Integración inicial recomendada: `IO.api -> SY.frontdesk.gov`

## 9.1 Objetivo

Usar esta capacidad como reemplazo del contrato custom actual `frontdesk_handoff` / `frontdesk_result`.

## 9.2 Dirección propuesta

### En `IO.api`

Cuando el request requiere regularización síncrona de identidad:

- construir el mensaje a frontdesk por el camino normal del nodo AI;
- agregar `meta.context.response_envelope`;
- preservar `meta.context.final_response_contract` si el nodo va a orquestar hops intermedios antes de responder;
- esperar una respuesta que respete ese envelope;
- parsearla y mapearla a la respuesta HTTP.

### En `SY.frontdesk.gov`

- dejar de depender de `frontdesk_result` como contrato propio obligatorio;
- usar el runtime AI común mejorado para producir la salida conforme al envelope recibido;
- si no hay envelope, responder por el camino conversacional normal del nodo.

## 9.3 Envelope sugerido para el caso frontdesk

```json
{
  "kind": "json_object_v1",
  "strict": true,
  "required": ["success", "human_message"],
  "properties": {
    "success": {
      "type": "boolean",
      "description": "true if the ILK is already permanent or was upgraded to permanent"
    },
    "error_code": {
      "type": "string",
      "description": "machine-readable error code when success=false"
    },
    "human_message": {
      "type": "string",
      "description": "brief human-readable message"
    }
  }
}
```

## 9.4 Simplificación deliberada para MVP

En este primer corte se acepta sacrificar granularidad fina de HTTP status.

Mapeo sugerido inicial:

- `success = true` -> success path de `IO.api`
- `success = false` -> error path
- `error_code` -> logging / observabilidad / evolución posterior
- `human_message` -> body humano del error o instrucción al caller

No intentar en v1 resolver todo el árbol de códigos HTTP finos por `error_code`.

---

## 10. Comportamiento cuando un nodo no soporta envelope

## 10.1 MVP recomendado

Mientras el soporte se implementa por etapas, se acepta:

- si el runtime AI soporta envelope estructurado real, usarlo;
- si solo soporta augmentación de instrucciones, usar eso;
- si el nodo target no soporta nada todavía, no usar este mecanismo con ese target en producción.

## 10.2 Regla de rollout

No asumir soporte universal desde el día uno.

La capacidad debe considerarse habilitada solo para nodos/runtimes explícitamente migrados.

---

## 11. Cambios esperados por capa

## 11.1 Runtime / SDK AI

Debe incorporar:

- lectura de `meta.context.response_envelope`;
- preparación de instrucciones dinámicas por turno;
- integración con structured outputs / output schema equivalente;
- fallback textual controlado para MVP;
- separación conceptual entre instrucción base del nodo y restricción dinámica de salida.

## 11.2 `IO.common`

Debe incorporar:

- helper para escribir / leer `meta.context.response_envelope`;
- helper para escribir / leer `meta.context.final_response_contract`;
- validación mínima del shape del envelope;
- helpers de parseo del resultado cuando un IO lo use.

## 11.3 `IO.api`

Debe incorporar:

- decisión de cuándo necesita envelope;
- construcción del envelope para el caso sync de identidad;
- parseo del resultado recibido;
- mapping al contrato HTTP.

## 11.4 `SY.frontdesk.gov`

Debe incorporar:

- eliminación progresiva de la dependencia contractual en `frontdesk_result`;
- uso del runtime AI común mejorado;
- policy/prompting orientado a cumplir el envelope cuando exista.

---

## 12. Qué no debe pasar

No debe adoptarse como solución canónica:

1. meter la instrucción de formato dentro del texto del usuario;
2. tratar el envelope como mensaje conversacional;
3. convertir `response_envelope` en un mini lenguaje arbitrario;
4. redefinir `behavior.instructions` como carrier de reglas por turno;
5. exigir que todos los nodos IO o AI soporten esto de inmediato;
6. mezclar `reply_target` con `response_envelope`.

---

## 13. Decisiones explícitas

1. El contrato hop-by-hop viaja en `meta.context.response_envelope`.
2. Es opcional y opt-in por nodo IO.
3. La capacidad vive en el runtime AI compartido, no en un hack por nodo.
4. Se prioriza portar capacidades ya existentes del OpenAI Agents SDK / Responses path.
5. El fallback MVP permitido es instrucción dinámica adicional de sistema, no prompt injection en user text.
6. La primera aplicación es `IO.api -> SY.frontdesk.gov`.
7. El objetivo de esa primera aplicación es reemplazar `frontdesk_handoff` / `frontdesk_result`, no convivir indefinidamente con ellos como diseño final.
8. `meta.context.final_response_contract` preserva el objetivo final de la operación cuando hay hops intermedios.
9. `response_envelope` no se propaga automáticamente.

---

## 14. Sugerencias de implementación para el equipo

Estas no son una lista cerrada de tareas ni implican orden obligatorio, pero sí delimitan el trabajo necesario:

### A. Revisar el port Rust actual del runner AI

Confirmar:

- dónde se resuelven hoy `behavior.instructions`;
- dónde se construye el request al modelo;
- dónde conviene insertar el equivalente a `call_model_input_filter`.

### B. Portar el subconjunto útil del Agents SDK

Prioridad sugerida:

1. hook equivalente a modificación del model input antes del call;
2. soporte de structured outputs / output schema;
3. request interno más rico que `system + input`.

### C. Definir el shape exacto de `response_envelope` v1

Mantenerlo pequeño y estable.

### D. Implementar helpers en `IO.common`

Para:

- set/get del envelope en metadata;
- validación mínima;
- parseo del resultado.

### E. Migrar `IO.api` y `SY.frontdesk.gov`

Objetivo:

- `IO.api` envía envelope;
- frontdesk responde conforme al envelope;
- `IO.api` usa ese resultado para su flujo síncrono.

### F. Actualizar documentación oficial

Actualizar al menos:

- spec de `IO.api`;
- spec de frontdesk gov;
- spec del runtime/SDK AI si corresponde;
- ejemplos de config / contract docs.

---

## 15. Criterio final

El valor de esta propuesta no es “pedir JSON por prompt”, sino introducir una frontera más limpia entre:

- **lo estable del nodo AI** (`behavior.instructions`), y
- **lo dinámico que un IO necesita en ese turno** (`response_envelope`).

Si se implementa correctamente:

- mejora claridad;
- reduce contratos custom por nodo;
- evita mezclar instrucciones del sistema con el texto del usuario;
- y deja a Fluxbee más cerca del modelo operativo que ya existe en el Agents SDK de OpenAI.
