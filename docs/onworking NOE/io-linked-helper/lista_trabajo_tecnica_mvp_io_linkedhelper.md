# Lista de trabajo técnica – MVP `IO.linkedhelper`

> Objetivo: definir una lista de trabajo técnica concreta y secuenciada para iniciar el MVP de integración entre el adapter de Linked Helper y Fluxbee.
>
> Esta versión ya incorpora decisiones operativas provisorias para reducir ambigüedad y poder arrancar implementación.

Documento de referencia vigente para definiciones y dudas:

- `docs/onworking NOE/io-linked-helper/definiciones_dudas_puntos_dolor_lh_fluxbee.md`

No existe un archivo separado `definiciones_dudas_puntos_dolor_lh_fluxbee_actualizado.md` dentro del repo. Si aparece esa referencia en otro lugar, debe entenderse como referencia al archivo anterior.

---

## 1. Alcance del MVP

El MVP apunta a validar el circuito:

1. el adapter se autentica y se comunica con `IO.linkedhelper` por `HTTP + HTTPS`,
2. el adapter envía eventos en batch por polling periódico,
3. el nodo mantiene cola de resultados pendientes por adapter,
4. el nodo resuelve/crea identidades necesarias en Fluxbee,
5. el nodo traduce eventos relevantes al protocolo interno de Fluxbee,
6. Fluxbee produce resultados,
7. el nodo devuelve esos resultados al adapter en las siguientes respuestas,
8. el adapter consume el resultado y ejecuta la acción local que corresponda.

Fuera de alcance del MVP:

- update/delete de mensajes pendientes en Linked Helper,
- adjuntos con procesamiento completo,
- política cerrada de reintentos/timeouts,
- partial update de avatar,
- definición final `agent` vs `human/internal` para avatar,
- prompting formal por `modules/degrees`,
- semántica completa de edición/cancelación/manual sobre cognition.

---

## 2. Checklist de avance

Este checklist es el tablero operativo del MVP. Se marca a medida que avancemos.

### 2.1. Definiciones y contrato

- [x] Confirmar documento de referencia vigente de Linked Helper
- [x] Fijar tipo provisional de ILK para avatar
- [x] Fijar `external_id` canónico compuesto del contacto LH
- [x] Dejar `ILK_UPDATE` como dependencia pendiente de core
- [x] Congelar contrato HTTP v1 `adapter ↔ IO.linkedhelper`
- [x] Congelar tipos de eventos y tipos de respuesta del MVP

### 2.2. Nodo `IO.linkedhelper`

- [x] Crear skeleton del nodo `IO.linkedhelper`
- [x] Definir configuración mínima del nodo
- [x] Implementar autenticación mínima por adapter
- [x] Implementar parsing y validación de batch
- [x] Implementar `heartbeat`
- [x] Implementar cola de pendientes por adapter

Nota de robustez del MVP:

- la primera versión de la cola de pendientes puede implementarse en memoria;
- eso **no es durable** y implica pérdida de pendientes si el proceso reinicia;
- antes de considerar esta parte como producción, debe reemplazarse por persistencia local durable.

Nota de gap funcional diferido:

- para contactos externos sin `email`, la provisión mínima de identidad sigue siendo viable;
- lo que queda pendiente de revisar más adelante es la interacción con policy/routing cuando esa identidad permanezca en estado `temporary`;
- este gap no bloquea `avatar_create`, pero sí puede impactar la automatización conversacional futura.

### 2.3. Identidad de avatar y contacto

- [x] Implementar `avatar_create`
- [ ] Diseñar mapping local de avatar
- [ ] Implementar `avatar_update` hasta donde no bloquee core
- [ ] Implementar resolución/alta de contacto externo
- [ ] Definir metadata mínima del contacto

### 2.4. Flujo interno Fluxbee

- [ ] Implementar traducción de `conversation_message`
- [ ] Definir estrategia inicial de routing interno
- [ ] Definir mecanismo interno de retorno de resultados
- [ ] Implementar primer `result` conversacional útil

### 2.5. Reglas operativas y validación

- [ ] Implementar gating mínimo por avatar
- [ ] Definir comportamiento ante evento bloqueado
- [ ] Agregar logging mínimo del nodo
- [ ] Cubrir pruebas de canal y happy path

---

## 3. Definiciones operativas vigentes para arrancar

### 3.1. Tipo provisional de ILK para avatar

Para el MVP, el avatar se tratará provisoriamente como una identidad de tipo `human`.

Justificación operativa:

- evita introducir una categoría nueva en esta etapa,
- permite representar al avatar como identidad direccionable en Fluxbee,
- y deja abierta una revisión posterior si el tipo más correcto termina siendo otro.

Nota:

- esta definición **no debe considerarse final**,
- y debe revisarse más adelante con el equipo de core/identity.

### 3.2. Clave externa canónica del contacto Linked Helper

Para el MVP, el `external_id` canónico del contacto se define como:

- `<adapter_avatar_id>_<lh_contact_id>`

Ejemplos:

- avatar adapter `1` + contacto LH `66` → `1_66`
- avatar adapter `2` + contacto LH `333` → `2_333`

Reglas:

- el primer componente es el identificador del avatar dentro del adapter,
- el segundo componente es el identificador nativo del contacto en Linked Helper,
- el `lh_contact_id` por sí solo no es canónico,
- el nodo debe conservar además los campos separados dentro de metadata útil.

### 3.2.b. Gap diferido para contactos sin email

Para el MVP, se asume esta separación:

- el `avatar_create` debe venir suficientemente completo como para poder completar registro vía frontdesk;
- el contacto externo de `conversation_message` puede no traer `email`.

Implicancia:

- el contacto podrá provisionarse de forma temporal usando el pipeline IO/identity ya existente;
- pero la automatización posterior puede requerir revisión de policy/routing si ese contacto no alcanza estado `complete`.

### 3.3. `ILK_UPDATE` en core

La necesidad de update formal de ILK sigue siendo una dependencia del equipo de core.

Para esta línea de trabajo:

- no intervenimos en core por ahora,
- si `avatar_update` queda bloqueado por esta ausencia, se pospone esa parte y se sigue con el resto del backlog,
- el nodo debe quedar estructurado para enchufar esa operación cuando exista un camino oficial.

### 3.4. Wire format v1

Los drafts actuales ya alcanzan para arrancar un contrato v1 implementable.

Lo que falta no es el concepto general sino bajar el draft a:

- schema JSON estable,
- campos obligatorios/opcionales,
- códigos de error mínimos,
- ejemplos de request/response.

### 3.5. Retry y deduplicación

Retry/dedup queda diferido para una etapa posterior, salvo que aparezca como bloqueo directo de implementación.

---

## 4. Secuencia concreta de implementación

Esta es la secuencia recomendada para construir el MVP sin abrir frentes innecesarios.

### 3.1. Fase 0 — Contrato implementable

#### 3.1.1. Congelar el contrato HTTP v1 `adapter ↔ IO.linkedhelper`

Bajar el draft actual a un contrato concreto con:

- endpoint de ingreso,
- headers de autenticación,
- envelope request/response,
- shape de batch,
- shape de `ack`,
- shape de `result`,
- shape de `heartbeat`,
- correlación por item.

Entrega esperada:

- documento de contrato v1 listo para implementar,
- ejemplos JSON de request/response.

#### 3.1.2. Congelar los tipos de eventos del MVP

Dejar cerrados para implementación:

- `heartbeat`
- `avatar_create`
- `avatar_update`
- `conversation_message`

Y cerrar para `result` al menos:

- `status`
- `result_type`
- `payload`
- `error_code`
- `error_message`
- `retryable`

### 3.2. Fase 1 — Esqueleto del nodo

#### 3.2.1. Crear el nodo `IO.linkedhelper`

Implementar el skeleton inicial:

- crate/bin nuevo,
- bootstrap mínimo,
- surface HTTP,
- config dinámica/control-plane,
- schema de nodo no configurado/configurado,
- contrato de configuración del adapter.

#### 3.2.2. Definir configuración mínima del nodo

La primera versión debería contemplar al menos:

- listen address/port,
- adapters permitidos,
- installation keys,
- tenant por adapter o regla equivalente,
- destino por defecto o estrategia de routing inicial,
- timeouts básicos del canal HTTP.

#### 3.2.3. Implementar autenticación mínima por adapter

El nodo debe:

- identificar `adapter_id`,
- validar installation key,
- rechazar requests no autenticados,
- exponer metadatos mínimos para observabilidad.

### 3.3. Fase 2 — Ingreso del canal externo

#### 3.3.1. Implementar parsing y validación de batch

El nodo debe:

- recibir múltiples items,
- validar envelope,
- validar tipos conocidos,
- aislar errores por item cuando sea posible,
- generar estructura interna normalizada por evento.

#### 3.3.2. Implementar `heartbeat`

Debe poder:

- aceptar heartbeat vacío,
- responder heartbeat sin novedades,
- usar heartbeat como retiro de pendientes.

#### 3.3.3. Implementar cola de pendientes por adapter

Resolver almacenamiento en memoria para el MVP, con interfaz interna que después permita persistencia si hace falta.

El nodo debe:

- encolar resultados por `adapter_id`,
- devolverlos en el siguiente poll,
- preservar correlación por item,
- no mezclar adapters.

### 3.4. Fase 3 — Identidad de avatar

#### 3.4.1. Implementar `avatar_create`

Con tipo provisional `human`, el nodo debe:

- validar snapshot,
- resolver alta en identity/core,
- registrar mapping local necesario,
- emitir `ack`,
- encolar `result` de éxito o error.

#### 3.4.2. Diseñar el mapping local de avatar

Definir qué persistimos localmente para cada avatar, al menos:

- `adapter_id`
- `adapter_avatar_id`
- `tenant_id`
- `avatar_ilk`
- metadata mínima útil
- estado de bloqueo si aplica

#### 3.4.3. Implementar `avatar_update`

Hacer la bajada técnica hasta donde alcance sin tocar core:

- recibir y validar full snapshot,
- resolver avatar existente,
- si existe operación soportada en core, aplicarla,
- si no existe, devolver error explícito y seguir con el resto del backlog.

### 3.5. Fase 4 — Identidad del contacto externo

#### 3.5.1. Implementar resolución/alta de contacto

Ante `conversation_message`, resolver o crear el contacto externo usando:

- `tenant_id`
- canal `linkedhelper`
- `external_id = <adapter_avatar_id>_<lh_contact_id>`

#### 3.5.2. Definir metadata mínima del contacto

Guardar lo necesario para continuidad conversacional, sin depender de email:

- nombre si existe,
- `adapter_avatar_id`,
- `lh_contact_id`,
- campos auxiliares del adapter si vienen,
- referencias de conversación/hilo si existen.

### 3.6. Fase 5 — Traducción al protocolo interno de Fluxbee

#### 3.6.1. Implementar traducción de `conversation_message`

El nodo debe construir un mensaje interno con:

- `meta.src_ilk` del contacto,
- `meta.dst_ilk` o metadata equivalente del avatar,
- `ich`/carrier de canal coherente con Linked Helper,
- metadata de hilo o conversación,
- payload textual inicial.

#### 3.6.2. Definir estrategia inicial de routing

Cerrar para el MVP cómo se resuelve el destino interno:

- por `dst_node` fijo configurado,
- o por policy/routing posterior.

La recomendación para arrancar es:

- `dst_node` fijo por adapter o por configuración del nodo,
- sin abrir todavía un frente extra de resolución sofisticada.

### 3.7. Fase 6 — Retorno de resultados al adapter

#### 3.7.1. Definir el mecanismo interno de retorno

El nodo necesita capturar la salida que luego devolverá al adapter.

Para el MVP, definir un camino simple y explícito:

- recepción de respuesta final desde el flujo interno,
- transformación a `result`,
- encolado por `adapter_id`.

#### 3.7.2. Implementar primer `result` conversacional útil

Soportar al menos:

- texto de salida,
- metadata correlacionable,
- `status=success`,
- error conversacional simple cuando falle el flujo interno.

### 3.8. Fase 7 — Gating por avatar

#### 3.8.1. Implementar bloqueo mínimo por avatar

Al menos para:

- `avatar_create` en curso,
- `avatar_update` en curso.

#### 3.8.2. Definir comportamiento ante evento bloqueado

Cuando llegue un evento bloqueado:

- no debe procesarse,
- debe devolverse `ack` y/o `result` de error según contrato,
- debe quedar claro si es `retryable`.

### 3.9. Fase 8 — Observabilidad y validación

#### 3.9.1. Logging mínimo del nodo

Agregar logs claros para:

- autenticación,
- recepción de batch,
- cantidad de items,
- avatar/contact resolution,
- enqueue/dequeue de resultados,
- errores por item.

#### 3.9.2. Pruebas de canal y happy path

Cubrir como mínimo:

- auth válida/inválida,
- heartbeat vacío,
- heartbeat con pendientes,
- `avatar_create`,
- `conversation_message` con contacto nuevo,
- generación de `result` conversacional.

---

## 5. Bloques de trabajo por área

### 4.1. Core / contratos / decisiones mínimas

#### 2.1.1. Confirmar y documentar el contrato v1 adapter ↔ `IO.linkedhelper`

Definir de manera implementable:

- endpoint HTTP/HTTPS de ingreso,
- autenticación por installation key,
- formato request/response batch,
- tipos de mensaje saliente del adapter:
  - `heartbeat`
  - `avatar_create`
  - `avatar_update`
  - `conversation_message`
- tipos de respuesta nodo → adapter:
  - `ack`
  - `result`
  - `heartbeat`
- envelope común mínimo:
  - `adapter_id`
  - ids correlacionables por item,
  - `type`,
  - `status` y `result_type` cuando aplique.

#### 2.1.2. Incorporar soporte de actualización de ILK en core/identity

Hoy existe base documental para:

- `ILK_CREATE`
- `ILK_LINK`
- `ILK_DELETE`
- `ILK_GRADUATE`

Se requiere definir e implementar un mecanismo equivalente a **`ILK_UPDATE`** o la alternativa oficial que el core considere válida para actualizar:

- metadata,
- nombre,
- canales,
- datos relevantes del avatar.

#### 2.1.3. Definir criterio mínimo de tipo de ILK para avatar

No hace falta cerrar la decisión final de arquitectura del avatar, pero sí definir un criterio operativo provisional para el MVP, de modo que el nodo pueda:

- crear avatares,
- actualizarlos,
- enlazarlos al tenant,
- y usarlos como destino conversacional.

La decisión puede quedar explícitamente marcada como provisoria.

#### 2.1.4. Definir clave externa canónica para contacto LH

Definir la forma del `external_id` que permita resolver o crear el contacto externo sin colisiones.

Debe contemplar al menos:

- origen `linkedhelper`,
- referencia al avatar o cuenta operativa,
- identificador nativo del contacto dentro de LH.

Debe quedar claro que el `contact_person_id` de LH por sí solo no alcanza como identificador canónico.

---

### 4.2. Nodo `IO.linkedhelper`

#### 2.2.1. Crear el nuevo nodo IO

Implementar el esqueleto del nodo `IO.linkedhelper`, incluyendo:

- bootstrap/configuración básica,
- integración con runtime de Fluxbee,
- registro en inventory/config si aplica,
- surface HTTP para recibir requests del adapter.

#### 2.2.2. Implementar autenticación mínima del adapter

El nodo debe:

- recibir `adapter_id` + installation key,
- validar credenciales,
- rechazar requests inválidos,
- dejar preparada la estructura para rotación/revocación futura.

#### 2.2.3. Implementar recepción batch + parsing de mensajes

El nodo debe poder:

- recibir múltiples items por request,
- validar envelope,
- validar tipos conocidos,
- rechazar items inválidos sin comprometer necesariamente todo el batch,
- generar `ack` y/o `result` según corresponda.

#### 2.2.4. Implementar cola de resultados pendientes por adapter

El nodo debe mantener una cola lógica de salidas por adapter para que, en cada poll/heartbeat:

- devuelva resultados pendientes,
- preserve correlación por item,
- no mezcle resultados entre adapters,
- pueda responder vacío con `heartbeat` si no hay nada disponible.

#### 2.2.5. Implementar soporte de `heartbeat`

El nodo debe:

- aceptar requests de heartbeat,
- usar heartbeat como mecanismo de retiro de pendientes,
- responder con resultados si los hay,
- responder con `heartbeat` si no hay nada para devolver.

#### 2.2.6. Implementar `avatar_create`

El nodo debe:

- recibir snapshot completo del avatar,
- validar datos mínimos,
- resolver alta en identity/core,
- devolver `ack` y luego `result` con ILK creado o error,
- registrar mapping necesario para uso posterior.

#### 2.2.7. Implementar `avatar_update`

Para el MVP se asume `full snapshot update`.

El nodo debe:

- recibir snapshot completo,
- validar que el avatar exista o definir política de fallback,
- ejecutar actualización en identity/core,
- devolver `ack` y luego `result` con éxito o error.

#### 2.2.8. Implementar resolución/alta de contacto externo

Ante `conversation_message`, el nodo debe:

- resolver el contacto externo por canal + external_id + tenant,
- si no existe, crearlo,
- no depender del email como requisito obligatorio,
- devolver error si no puede resolver/crear el `src_ilk`.

#### 2.2.9. Implementar traducción de `conversation_message` a mensaje interno Fluxbee

El nodo debe construir el mensaje interno con lo mínimo esperado por el data plane:

- `src_ilk` del contacto,
- `dst_ilk` del avatar si corresponde,
- `conversation_id` o referencia equivalente,
- `context.channel = linkedhelper`,
- metadata externa relevante.

#### 2.2.10. Implementar gating/bloqueo por avatar

El nodo debe aplicar bloqueo por avatar cuando corresponda, al menos para:

- `avatar_create` pendiente,
- `avatar_update` pendiente.

Debe quedar preparado para extender la lista de casos bloqueantes.

El bloqueo:

- afecta solo a ese avatar,
- no bloquea a otros avatares del mismo adapter.

#### 2.2.11. Implementar resultados nodo → adapter

El nodo debe poder devolver:

- `ack`
- `result`
- `heartbeat`

Y dentro de `result`, soportar al menos:

- `status = success | error`
- `result_type`
- `payload` opcional
- `error_code` opcional
- `error_message` opcional
- `retryable` opcional

#### 2.2.12. Implementar primer resultado conversacional útil

Para `conversation_message`, el nodo debe ser capaz de devolver al adapter un resultado que represente la salida conversacional producida por Fluxbee.

Ese resultado no debe asumir obligatoriamente texto puro; debe quedar preparado para representar contenido saliente más general.

---

### 4.3. Adapter mínimo compatible

#### 2.3.1. Incorporar cliente HTTP/HTTPS al nodo

El adapter debe:

- iniciar la comunicación con `IO.linkedhelper`,
- autenticarse con installation key,
- manejar polling periódico,
- enviar batchs,
- consumir la respuesta del nodo.

#### 2.3.2. Generar ids correlacionables por item

El adapter debe generar y conservar ids por item/evento para poder:

- correlacionar `ack`,
- correlacionar `result`,
- identificar eventos pendientes,
- detectar ausencia de respuesta.

#### 2.3.3. Mantener mapping avatar externo ↔ ILK

Una vez resuelto el avatar, el adapter debe conservar el mapping necesario para:

- enviar `conversation_message` con referencia válida al avatar,
- identificar cuándo un avatar quedó bloqueado o pendiente de alta/update.

#### 2.3.4. Enviar `heartbeat` cuando no haya eventos

El adapter debe soportar el patrón:

- si hay eventos, envía batch,
- si no hay eventos, envía heartbeat,
- en ambos casos consume resultados pendientes del nodo.

#### 2.3.5. Enviar `avatar_create` y `avatar_update`

El adapter debe detectar:

- nuevos avatares,
- cambios de datos relevantes,

y emitir el evento correspondiente antes de permitir procesamiento conversacional para ese avatar cuando aplique.

#### 2.3.6. Enviar `conversation_message`

El adapter debe:

- consolidar mensajes como hoy corresponda,
- incluir referencia al avatar,
- incluir información suficiente del contacto para permitir resolución/alta,
- incluir identificador de conversación/hilo si existe,
- incluir contenido consolidado.

#### 2.3.7. Consumir `result.status=error`

Sin cerrar aún la política fina de retries, el adapter debe quedar preparado para:

- registrar el error,
- decidir reintento o alerta,
- no avanzar automáticamente cuando el nodo devuelve error bloqueante para esa conversación/avatar.

---

### 4.4. Integración con flujo AI

#### 2.4.1. Validar routing del mensaje interno desde `IO.linkedhelper`

Comprobar que el mensaje construido por el nodo:

- routea correctamente por OPA,
- resuelve tenant vía identidad,
- llega al handler AI esperado o al destino definido por política.

#### 2.4.2. Validar retorno de salida conversacional al nodo

Definir el mecanismo mínimo para que el nodo pueda recibir o recuperar el resultado que debe devolver al adapter.

No hace falta resolver aún edición/cancelación/manual; solo el camino feliz de respuesta conversacional.

---

## 5. Pruebas mínimas a contemplar

### 3.1. Canal y autenticación

- adapter válido puede hacer poll
- adapter inválido es rechazado
- heartbeat sin pendientes responde heartbeat
- heartbeat con pendientes devuelve resultados

### 3.2. Avatar

- alta exitosa de avatar
- update exitoso de avatar
- error de alta/update bien serializado hacia adapter
- bloqueo por avatar respetado

### 3.3. Contacto

- alta automática de contacto sin email
- lookup correcto por external id compuesto
- error si identity/frontdesk no permite resolver/crear contacto

### 3.4. Conversación

- `conversation_message` routea correctamente
- se obtiene resultado conversacional
- el resultado vuelve al adapter en poll posterior si no estuvo disponible inmediatamente

---

## 4. Dudas abiertas que no deben bloquear el MVP

Estas cuestiones deben quedar registradas, pero no impedir la primera implementación:

- decisión final `agent` vs `human/internal` para avatar,
- prompting formal via `modules/degrees`,
- `partial update` de avatar,
- tratamiento completo de adjuntos,
- update/delete de mensajes pendientes,
- timeouts y política de retries,
- respuestas tardías y reconciliación,
- impacto de edición/cancelación/manual sobre cognition,
- taxonomía final de alertas por avatar vs adapter,
- revocación/rotación completa de installation key.

---

## 5. Orden sugerido de trabajo

### Paso 1
Core:
- definir `ILK_UPDATE` o alternativa oficial,
- definir external id compuesto,
- confirmar criterio provisional para tipo de ILK del avatar.

### Paso 2
Nodo:
- skeleton de `IO.linkedhelper`,
- autenticación mínima,
- parsing batch,
- cola por adapter,
- heartbeat.

### Paso 3
Nodo + identity:
- `avatar_create`,
- `avatar_update`,
- resolución/alta de contacto.

### Paso 4
Nodo + AI:
- traducción de `conversation_message`,
- routing interno,
- devolución de resultado conversacional al adapter.

### Paso 5
Adapter:
- cliente HTTP/HTTPS,
- polling,
- ids correlacionables,
- consumo de `ack/result/heartbeat`,
- emisión de `avatar_create`, `avatar_update` y `conversation_message`.

---

## 6. Resultado esperado del MVP

Al finalizar el MVP debería ser posible demostrar, al menos, el siguiente caso completo:

1. el adapter se autentica contra `IO.linkedhelper`,
2. detecta un avatar nuevo y lo da de alta,
3. detecta un contacto nuevo y un mensaje consolidado,
4. el nodo resuelve/crea identidad del contacto,
5. el nodo routea el mensaje al flujo interno de Fluxbee,
6. Fluxbee produce una salida conversacional,
7. el nodo guarda el resultado para ese adapter,
8. el adapter lo retira en un poll/heartbeat posterior,
9. el adapter queda en condiciones de ejecutar la acción local correspondiente.
