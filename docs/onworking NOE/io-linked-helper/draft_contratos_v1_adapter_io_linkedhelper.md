# Draft v1 — Contratos y mecanismo de intercambio `adapter ↔ IO.linkedhelper`

## 1. Objetivo

Este documento consolida las definiciones actuales para una primera versión (v1) del intercambio entre:

- el **adapter** externo que interactúa con Linked Helper,
- y el nodo **`IO.linkedhelper`** dentro de Fluxbee.

El objetivo de esta v1 es fijar:

- el mecanismo general de comunicación,
- los tipos mínimos de mensajes salientes del adapter,
- los tipos mínimos de respuestas del nodo,
- las reglas conceptuales mínimas de correlación,
- y las dudas abiertas que no deben bloquear el avance.

Este documento **no** busca cerrar todavía:

- el wire format definitivo,
- el detalle de timeouts y retries,
- la política completa de deduplicación,
- ni todos los tipos de payloads futuros.

---

## 2. Alcance y contexto

### 2.1. Canal considerado

Este draft cubre exclusivamente la capa privada:

- **adapter ↔ `IO.linkedhelper`**

No cubre en detalle:

- el protocolo interno **nodo ↔ router Fluxbee**,
- ni el formato final interno con el que `IO.linkedhelper` traducirá cada evento al modelo Fluxbee.

### 2.2. Responsabilidad del adapter

El adapter:

- conoce cómo leer y escribir sobre el mundo Linked Helper,
- es el dueño operativo del dato del avatar en el entorno externo,
- mantiene el mapping entre avatar externo e ILK del avatar registrado en Fluxbee,
- detecta eventos y los envía al nodo,
- y luego actúa en función de los resultados que devuelve el nodo.

### 2.3. Responsabilidad del nodo `IO.linkedhelper`

El nodo:

- recibe eventos del adapter,
- los traduce al modelo Fluxbee,
- resuelve o deriva el procesamiento necesario dentro de Fluxbee,
- mantiene una cola de respuestas/salidas por adapter,
- y devuelve al adapter los resultados pendientes que le correspondan.

---

## 3. Protocolo y seguridad del canal

### 3.1. Transporte

Para la v1, el canal se define sobre:

- **HTTP + HTTPS**

La elección responde a una combinación de:

- simplicidad de implementación,
- menor costo operativo inicial,
- facilidad para agregar seguridad mínima con certificado,
- y menor complejidad respecto de un protocolo TCP propio.

### 3.2. Sentido de la comunicación

La comunicación es siempre iniciada por el **adapter**.

No hay comunicación espontánea desde Fluxbee hacia el adapter.

En otras palabras:

- **no hay webhooks**,
- **no hay push directo desde Fluxbee**,
- el adapter hace polling periódico.

### 3.3. Seguridad mínima acordada

La base de seguridad acordada para esta capa es:

- **HTTPS** para proteger el canal en tránsito,
- **clave de instalación única por adapter/instalación** para autenticación mínima.

### 3.4. Clave de instalación

Se asume un esquema de credencial única por instalación del adapter.

Eso permite, al menos conceptualmente:

- identificar instalaciones concretas,
- validar que la llamada proviene de un adapter conocido,
- revocar una instalación puntual,
- y dejar abierta la puerta a rotación futura.

### 3.5. Dudas abiertas de seguridad

Quedan abiertas, pero ya identificadas, estas preguntas:

- dónde y cómo se almacena localmente la clave de instalación,
- cómo será la rotación o revocación de esa clave,
- si habrá más adelante autenticación adicional aparte de la installation key.

---

## 4. Mecanismo general de intercambio

## 4.1. Polling periódico

El adapter realiza intercambios periódicos con el nodo.

En cada ciclo:

- si tiene eventos para enviar, envía un batch con esos eventos,
- si no tiene eventos, envía un heartbeat.

En la respuesta, el nodo:

- devuelve lo que tenga pendiente para ese adapter,
- o bien responde con heartbeat si no tiene nada para devolver.

### 4.2. Heartbeat

El heartbeat **no** se considera solamente un keepalive.

También cumple la función de:

- retirar respuestas pendientes,
- sostener el canal de retorno desde el nodo hacia el adapter,
- y permitir que el nodo responda aunque el adapter no tenga nuevos eventos salientes.

### 4.3. Batch por request

El adapter puede enviar múltiples items en un mismo request.

Esos items pueden:

- pertenecer a distintos avatares,
- pertenecer a distintas cuentas LH administradas por el mismo adapter,
- y mezclar distintos tipos de evento.

Ejemplo conceptual:

- Avatar A: conversación con contacto 1
- Avatar A: conversación con contacto 2
- Avatar B: update de avatar
- Avatar C: create de avatar

### 4.4. Batch en la respuesta

El nodo puede devolver en una misma respuesta:

- ACKs,
- resultados finales,
- o heartbeat,

mezclando resultados correspondientes a distintos eventos previos del mismo adapter.

Las respuestas a eventos de un mismo batch original **no tienen por qué volver juntas**.

Pueden volver:

- en la respuesta inmediata,
- o en heartbeats/polls posteriores.

### 4.5. Cola de respuestas por adapter

El nodo `IO.linkedhelper` es responsable de:

- mantener una cola o conjunto de resultados pendientes por adapter,
- determinar qué resultados corresponden a cada adapter,
- y devolverlos cuando ese adapter abra un nuevo intercambio.

Esta cola es responsabilidad del nodo, no del router ni del adapter.

---

## 5. Correlación

### 5.1. Principio general

Cada item enviado por el adapter debe incluir un identificador correlacionable.

Las respuestas del nodo deben poder referenciar el evento original correspondiente.

### 5.2. Definición todavía abierta

Todavía no está cerrado el modelo exacto de correlación.

Queda abierto definir si la correlación usará, por ejemplo:

- solo `event_id`,
- `request_id + event_id`,
- o algún esquema combinado.

### 5.3. Definición mínima para avanzar

Para la v1, se deja asentado que:

- cada item del adapter debe tener un id propio,
- y cada respuesta del nodo debe poder referenciar el item al que corresponde.

---

## 6. Bloqueos por avatar

### 6.1. Alcance del bloqueo

Existen acciones/eventos bloqueantes a nivel avatar.

Está definido que:

- un avatar puede quedar bloqueado por ciertos eventos propios,
- pero **un avatar no bloquea a otro**,
- y el adapter puede seguir procesando eventos de otros avatares aunque uno esté bloqueado.

### 6.2. Casos conocidos

Hoy se consideran al menos como candidatos obvios a bloqueo:

- `avatar_create`
- `avatar_update`

### 6.3. Dudas abiertas

Queda abierto definir:

- qué otros eventos podrían ser bloqueantes,
- qué alcance exacto tiene el bloqueo dentro del avatar,
- y qué tipos de mensajes del avatar quedan impedidos mientras exista ese bloqueo.

---

## 7. Naturaleza de los eventos del adapter

Los items que salen del adapter hacia el nodo se consideran:

- **eventos del adapter hacia Fluxbee**.

No son comandos ya resueltos.

El adapter informa hechos, y Fluxbee determina qué hacer con ellos según:

- tipo de evento,
- metadata,
- estado del sistema,
- routing,
- y servicios internos involucrados.

Ejemplos:

- un mensaje de conversación puede terminar procesado por IA y devolver una respuesta,
- un alta de avatar puede terminar en identity/frontdesk,
- una alerta puede terminar en un workflow, logueo o notificación a otro canal.

---

## 8. Tipos mínimos de mensajes adapter → nodo (v1)

Para la v1 se definen cuatro tipos mínimos.

Todos los items deben tener id propio, incluyendo heartbeat.

## 8.1. `heartbeat`

### Objetivo

Permitir al adapter:

- indicar que sigue operativo,
- mantener vivo el intercambio,
- y retirar pendientes cuando no tiene nuevos eventos para enviar.

### Campos mínimos propuestos

- `id`
- `adapter_id`
- `timestamp`

### Campos opcionales futuros

Queda abierta la posibilidad de agregar más adelante campos resumidos como:

- versión de protocolo,
- cantidad de cuentas manejadas,
- cantidad de avatares activos,
- backlog local,
- u otros datos de estado.

---

## 8.2. `avatar_create`

### Objetivo

Notificar la detección/alta de un avatar externo que debe quedar registrado en Fluxbee.

### Criterio general

Para la v1, se lo considera un **snapshot completo** del avatar.

### Campos mínimos propuestos

- `id`
- `adapter_id`
- `external_avatar_id`
- referencia de tenant/empresa
- `name`
- `email` (si existe)
- `metadata`

### Metadata sugerida inicial

Dentro de `metadata` podrían vivir, por ejemplo:

- `timezone`
- `language`
- `tone`
- otros atributos adicionales del avatar

### Nota

No se cierra todavía cómo se integrará esto con prompting, degree/modules o tipo exacto de ILK del avatar.

---

## 8.3. `avatar_update`

### Objetivo

Notificar una actualización del avatar externo ya existente.

### Criterio general acordado

Para la v1, `avatar_update` se modela igual que `avatar_create`:

- mismo shape,
- distinto tipo de evento,
- snapshot completo.

### Justificación

Esto evita en v1 la complejidad de:

- partial updates,
- semántica ambigua de campos faltantes,
- y dudas sobre si un campo ausente implica borrar o mantener.

### Definición abierta futura

Se deja abierta la posibilidad de una v2 con update parcial/patch.

---

## 8.4. `conversation_message`

### Objetivo

Representar una comunicación consolidada desde un contacto externo hacia un avatar.

### Condición importante

El adapter envía este evento usando ya el **ILK del avatar**, porque mantiene el mapping avatar externo ↔ ILK del avatar registrado.

### Estructura conceptual mínima

#### Identidad del avatar

- `id`
- `adapter_id`
- `avatar_ilk`
- opcionalmente `external_avatar_id`

#### Identidad del contacto

Debe incluir suficiente información para que el nodo pueda:

- resolver o crear el ILK del contacto externo si aún no existe.

Campos mínimos sugeridos:

- `contact_name`
- `contact_external_composite_id`
- `contact_lh_person_id` (como referencia de origen, no como identidad canónica única)
- `contact_metadata` opcional

### Nota importante sobre identidad del contacto

El `contact_lh_person_id` de Linked Helper:

- puede ser útil para trazabilidad y debugging,
- pero **no** debe asumirse como identificador único global,
- porque puede variar según el avatar/cuenta/base de origen.

Por eso se propone un `external_composite_id` del contacto como referencia más estable del lado canal.

#### Contexto conversacional

- `conversation_external_id`
- timestamps o referencias de origen si aplican
- ids de mensajes fuente si se considera útil para trazabilidad futura

#### Contenido

- contenido consolidado del mensaje
- adjuntos opcionales

### Adjuntos

Para la v1:

- se recomienda dejar lugar lógico en el contrato para adjuntos,
- aunque su procesamiento efectivo no se considere todavía obligatorio.

### Pre-requisito para automatización

El procesamiento conversacional depende de que el nodo pueda resolver o crear correctamente el ILK del contacto externo (`src_ilk`).

Si no puede hacerlo por fallas internas (por ejemplo identity/frontdesk no disponibles), el nodo:

- no debe avanzar con la automatización de esa conversación,
- debe responder con error para ese evento,
- y el adapter decidirá luego si reintenta, alerta o ambas cosas.

### Sobre email del contacto

El email del contacto externo **no se considera obligatorio** para su alta como ILK.

Si existe, puede enviarse como metadata adicional.

---

## 8.5. `system_alert`

### Estado actual

Se reconoce conceptualmente este tipo de evento, pero **no se lo incluye aún como payload mínimo completamente cerrado de la v1**.

### Observación actual

Hasta ahora, las alertas observadas se entienden principalmente como alertas por avatar.

De todos modos, se deja contemplada la posibilidad de que existan en el futuro errores propios del adapter.

### Definición pendiente

Quedan abiertas:

- la forma exacta del payload,
- sus subtipos,
- y su tratamiento fino dentro de Fluxbee.

---

## 9. Respuestas mínimas nodo → adapter (v1)

Para la v1 se propone una familia corta de respuestas top-level:

- `ack`
- `result`
- `heartbeat`

## 9.1. `ack`

### Objetivo

Indicar que:

- el evento fue recibido,
- aceptado para procesamiento,
- pero todavía no hay resolución final.

### Campos mínimos sugeridos

- `response_id`
- `adapter_id`
- `event_id`
- `type = ack`

### Observación

No se considera obligatorio emitir `ack` para heartbeat si no aporta valor operativo.

---

## 9.2. `result`

### Objetivo

Representar el resultado operativo actual de un evento previo.

Ese resultado puede ser:

- exitoso,
- fallido,
- o implicar datos con los que el adapter decidirá qué hacer a continuación.

### Definición conceptual importante

El nodo devuelve **resultados**; no se fuerza en v1 la idea de “instrucción”.

La decisión de qué hacer con ese resultado corresponde al adapter.

### Campos mínimos propuestos

- `response_id`
- `adapter_id`
- `event_id`
- `type = result`
- `status = success | error`
- `result_type`
- `payload` opcional
- `error_code` opcional
- `error_message` opcional
- `retryable` opcional

### Criterio general

- `error` no es una categoría top-level separada,
- sino un `result` con `status = error`.

### Ejemplos de `result_type` exitosos

- `avatar_created`
- `avatar_updated`
- `conversation_processed`
- `no_action_required`

### Ejemplos de `result_type` con error

- `avatar_create_failed`
- `avatar_update_failed`
- `contact_ilk_resolution_failed`
- `conversation_processing_failed`
- `invalid_payload`

### Nota importante sobre resultados conversacionales

Para `conversation_processed`, **no debe asumirse que exista siempre un campo textual `message` obligatorio**.

El payload debería permitir representar contenido saliente más general, incluyendo por ejemplo:

- texto solo,
- adjuntos solos,
- o texto + adjuntos.

En otras palabras, el resultado conversacional debe pensarse como contenido saliente general y no únicamente como string de mensaje.

---

## 9.3. `heartbeat`

### Objetivo

Indicar que:

- el nodo no tiene nada más para devolver en ese intercambio,
- o responder a un poll/heartbeat sin pendientes.

### Campos mínimos sugeridos

- `response_id`
- `adapter_id`
- `type = heartbeat`
- `timestamp`

---

## 10. Qué puede volver según el tipo de evento

## 10.1. Frente a `heartbeat`

El nodo puede devolver:

- `heartbeat`
- o cualquier combinación pendiente de `ack` / `result` correspondiente a ese adapter.

## 10.2. Frente a `avatar_create`

Puede devolver:

- `ack`
- luego `result` exitoso con ILK resuelto/registrado
- o `result` con `status = error`

## 10.3. Frente a `avatar_update`

Puede devolver:

- `ack`
- luego `result` exitoso confirmando aplicación
- o `result` con `status = error`

## 10.4. Frente a `conversation_message`

Puede devolver:

- `ack`
- luego `result` exitoso con salida conversacional procesada
- o `result` con `status = error`

### Nota importante

Si no se puede resolver/crear el ILK del contacto externo, el nodo debe responder el evento con error y no avanzar con la automatización conversacional.

## 10.5. Frente a `system_alert`

Queda abierto con más detalle, pero conceptualmente podría devolver:

- `ack`
- luego `result`
- o `result` con error

---

## 11. Definiciones cerradas hasta ahora

### Canal y seguridad

- El canal adapter ↔ nodo será sobre **HTTP + HTTPS**.
- Se utilizará una **installation key** única por adapter/instalación.

### Mecanismo general

- La comunicación siempre la inicia el adapter.
- No hay webhooks desde Fluxbee al adapter.
- El adapter hace polling periódico.
- Si tiene eventos, los envía.
- Si no tiene eventos, envía heartbeat.
- El heartbeat también funciona como mecanismo para retirar respuestas pendientes.
- El nodo mantiene una cola de respuestas/salidas por adapter.

### Batching

- El adapter puede enviar múltiples eventos de múltiples avatares/cuentas en el mismo request.
- El nodo puede devolver múltiples respuestas en el mismo response.
- Las respuestas a un mismo batch no tienen por qué volver juntas.

### Bloqueos

- Existen bloqueos por avatar.
- Un avatar no bloquea a otro.
- `avatar_create` y `avatar_update` son casos conocidos candidatos a bloqueo.

### Eventos mínimos v1

- `heartbeat`
- `avatar_create`
- `avatar_update`
- `conversation_message`

### Respuestas mínimas v1

- `ack`
- `result`
- `heartbeat`

### Actualización de avatar

- `avatar_update` tendrá, en v1, el mismo shape que `avatar_create`.
- Se lo trata como snapshot completo, no patch parcial.

### Contacto externo

- El email del contacto no es obligatorio para su alta como ILK.
- El procesamiento conversacional requiere poder resolver/crear el ILK del contacto.
- Si eso falla, no debe avanzar la automatización de esa conversación.

---

## 12. Dudas abiertas / pendientes

### Identidad y avatares

- tipo exacto de ILK del avatar (`agent` vs `human/internal`)
- prompting del avatar
- si prompting terminará acoplado a degree/modules u otro mecanismo
- existencia formal de `ILK_UPDATE` en core

### Correlación

- modelo exacto de ids (`event_id`, `request_id`, etc.)
- reglas de correlación para respuestas tardías

### Bloqueos

- lista exacta de eventos bloqueantes por avatar
- alcance preciso del bloqueo dentro del avatar

### Resultados y retries

- política de timeout
- política de reintentos
- tratamiento de respuestas tardías
- qué hacer ante resultados vencidos o conversaciones que evolucionan mientras se espera un resultado

### Seguridad

- dónde se almacena la installation key en el adapter
- rotación/revocación de la installation key
- si habrá autenticación adicional futura

### Alertas

- shape exacto de `system_alert`
- catálogo de alertas por avatar
- catálogo de alertas globales del adapter (si existieran)

### Adjuntos

- forma exacta del payload de adjuntos
- si habrá soporte efectivo en la v1 o solo contrato preparado

---

## 13. Próximos pasos sugeridos

Con este draft, los siguientes pasos naturales serían:

1. definir el shape exacto del envelope HTTP request/response,
2. cerrar el contrato mínimo de cada payload v1,
3. definir el shape exacto del `result.payload` para los casos mínimos,
4. decidir el mecanismo interno del nodo para resolver/crear ILKs,
5. y recién después avanzar con timeouts, retries y semántica operativa fina.
