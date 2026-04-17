# IO Response Envelope + AI Runtime Integration Spec

Estado: propuesta técnica para implementación  
Audiencia: desarrollo runtime AI, desarrollo de nodos IO, arquitectura  
Alcance inicial: `IO.api` + `SY.frontdesk.gov`  
Dirección de diseño: usar capacidades ya existentes en OpenAI Agents SDK / Responses path antes que inventar un mecanismo propio distinto

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
