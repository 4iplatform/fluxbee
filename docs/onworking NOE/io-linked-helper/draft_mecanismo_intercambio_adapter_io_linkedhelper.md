# Draft — Mecanismo de intercambio entre Adapter y nodo `IO.linkedhelper`

## 1. Objetivo

Este documento resume el mecanismo conceptual de intercambio entre el **adapter externo** y el nodo **`IO.linkedhelper`** dentro de Fluxbee.

El foco de este draft está puesto únicamente en:

- la mecánica general de comunicación,
- la dirección del intercambio,
- la noción de batch,
- la relación entre eventos, respuestas y heartbeats,
- la existencia de bloqueos por avatar,
- y las dudas abiertas que todavía no conviene cerrar.

No forma parte del alcance de este documento:

- el wire format concreto,
- la estructura final del payload,
- el detalle de timeouts/reintentos,
- ni la definición exhaustiva de todos los tipos de mensajes.

---

## 2. Principios generales del intercambio

### 2.1. La comunicación siempre es iniciada por el adapter

El **adapter** es quien abre la comunicación hacia `IO.linkedhelper`.

No existe comunicación espontánea desde Fluxbee o desde el nodo hacia el adapter fuera de una conexión iniciada por este último.

En otras palabras:

- no hay webhooks desde Fluxbee hacia el adapter,
- no hay push directo hacia el adapter,
- el adapter es quien periódicamente consulta y envía información,
- y la respuesta del nodo es el único momento en que Fluxbee puede devolverle información al adapter.

### 2.2. El intercambio funciona como un mecanismo de polling bidireccional

El adapter realiza polls periódicos hacia `IO.linkedhelper`.

En cada intercambio pueden ocurrir dos cosas al mismo tiempo:

- el adapter **envía eventos** hacia Fluxbee,
- y el nodo **devuelve respuestas o acciones pendientes** destinadas a ese adapter.

Esto implica que el canal funciona como un **polling bidireccional**, aunque la apertura de la comunicación esté siempre del lado del adapter.

---

## 3. Heartbeat

### 3.1. El heartbeat no es solo keepalive

El heartbeat no debe entenderse únicamente como una señal de “estoy vivo”.

En este mecanismo, el heartbeat cumple además una función operativa:

- permitir que el adapter retire respuestas pendientes,
- permitir que el nodo devuelva acciones o resultados acumulados,
- y mantener activo el flujo aun cuando el adapter no tenga nuevos eventos para enviar.

### 3.2. Comportamiento esperado del heartbeat

El mecanismo general es el siguiente:

#### Caso A: el adapter tiene eventos para enviar

1. El adapter detecta que tiene uno o más eventos para enviar.
2. Envía esos eventos a `IO.linkedhelper`.
3. El nodo recibe los eventos.
4. En la respuesta, el nodo devuelve:
   - ACKs,
   - resultados ya disponibles,
   - errores,
   - acciones pendientes para ese adapter,
   - o heartbeat si no hay nada para devolver.

#### Caso B: el adapter no tiene eventos para enviar

1. El adapter envía un heartbeat.
2. El nodo recibe el heartbeat.
3. Si tiene respuestas o acciones pendientes para ese adapter, las devuelve.
4. Si no tiene nada para devolver, responde con heartbeat / sin novedades.

### 3.3. Definición provisional

El heartbeat debe considerarse parte del mecanismo normal de intercambio y **no** una excepción o mensaje accesorio.

---

## 4. Naturaleza de los elementos enviados por el adapter

### 4.1. Los elementos del batch son eventos hacia Fluxbee

Los ítems enviados por el adapter deben entenderse como **eventos** producidos por el mundo externo, no como acciones ya resueltas.

El adapter informa hechos o situaciones observadas. Luego, **Fluxbee determina qué hacer con ellos** en función de su metadata, su tipo y las reglas de routing aplicables.

### 4.2. Consecuencias de esta definición

Esto implica que:

- el adapter no decide el camino interno final dentro de Fluxbee,
- el adapter no necesita conocer el destino exacto del evento,
- el adapter reporta el hecho observado,
- Fluxbee decide si eso debe pasar por identidad, IA, workflows, canales de alerta, logging, etc.

### 4.3. Ejemplos

- Un **mensaje nuevo de contacto hacia avatar** se envía como evento; Fluxbee decide cómo procesarlo y a qué nodos o flujos derivarlo.
- Un **alta de avatar** se envía como evento; Fluxbee decide cómo resolver la identidad y qué acciones ejecutar.
- Una **alerta** se envía como evento; Fluxbee decide si debe loguearse, alertarse, escalarse o procesarse por un workflow.

---

## 5. Batch de eventos

### 5.1. Un request puede contener múltiples eventos

El adapter puede manejar múltiples cuentas de Linked Helper y múltiples avatares.

Por lo tanto, un único intercambio puede contener varios eventos en un mismo payload.

### 5.2. El batch puede mezclar distintos avatares y distintos tipos de eventos

Dentro de un mismo request, el adapter puede enviar eventos de:

- distintos avatares,
- distintos contactos,
- distintas cuentas de Linked Helper,
- y distintos tipos funcionales.

Ejemplo conceptual:

- avatar A: mensaje consolidado con contacto 1,
- avatar A: mensaje consolidado con contacto 2,
- avatar B: actualización de ILK,
- avatar C: alta de ILK.

### 5.3. Restricción: solo se incluyen eventos no bloqueados para ese avatar

El batch puede contener múltiples eventos de un mismo avatar **siempre que ese avatar no se encuentre bloqueado para ese tipo de evento**.

---

## 6. Respuesta del nodo al adapter

### 6.1. La respuesta puede contener múltiples elementos

La respuesta del nodo al adapter no debe pensarse como una única respuesta monolítica asociada al request completo.

Puede contener múltiples elementos heterogéneos, por ejemplo:

- ACKs,
- resultados finales,
- errores,
- acciones pendientes para ejecutar en el adapter,
- heartbeat / sin novedades.

### 6.2. Las respuestas no tienen por qué llegar todas juntas

Aunque varios eventos hayan sido enviados en un mismo request, sus respuestas no tienen por qué volver todas en la misma respuesta.

Algunas pueden:

- ser reconocidas inmediatamente,
- resolverse más adelante,
- o recién devolverse en heartbeats/polls posteriores.

Por lo tanto, la respuesta del canal debe entenderse como un mecanismo de **entrega diferida y acumulada** de resultados o acciones pendientes.

### 6.3. Categorías provisionales de respuesta

Sin entrar aún en detalle de implementación, se identifican al menos las siguientes categorías de respuesta:

#### a. ACK

Indica que el nodo recibió y aceptó un evento para procesamiento.

No implica resolución final.

#### b. Resultado final

Indica que el procesamiento asociado a un evento previo ya concluyó.

Ejemplos:

- alta de avatar resuelta con ILK asignado,
- actualización de avatar aplicada,
- respuesta conversacional lista,
- resolución final de una acción.

#### c. Acción para ejecutar en el adapter

Indica que el adapter debe realizar alguna acción concreta en el mundo externo.

Ejemplos:

- enviar mensaje al contacto,
- actualizar mensaje pendiente,
- borrar/cancelar mensaje,
- ejecutar alguna acción específica sobre Linked Helper.

#### d. Error

Indica que ocurrió algún problema de validación, resolución o ejecución.

#### e. Heartbeat / sin novedades

Indica que el nodo no tiene elementos pendientes para devolver en ese intercambio.

---

## 7. Cola de respuestas del nodo

### 7.1. El nodo mantiene una cola de salidas por adapter

El nodo `IO.linkedhelper` es responsable de mantener y administrar su **cola de respuestas/salidas pendientes por adapter**.

Esto incluye, según corresponda:

- respuestas a eventos previos,
- acciones destinadas a ese adapter,
- errores,
- elementos pendientes aún no retirados.

### 7.2. El nodo decide qué devolver en cada poll

Cada vez que un adapter inicia un intercambio, el nodo evalúa qué elementos pendientes le corresponden y los devuelve en la respuesta.

La asignación de esos elementos depende del vínculo entre:

- el adapter,
- las cuentas de Linked Helper que administra,
- y los avatares asociados.

### 7.3. Definición provisional

El polling del adapter y la cola del nodo forman conjuntamente el mecanismo de entrega de respuestas y acciones de retorno.

---

## 8. Correlación entre eventos y respuestas

### 8.1. Necesidad de correlación

Dado que:

- un request puede contener múltiples eventos,
- las respuestas pueden ser parciales,
- y algunas pueden llegar en polls posteriores,

resulta obligatorio contar con un mecanismo de correlación entre cada evento enviado y las respuestas asociadas.

### 8.2. Definición provisional

Por ahora se asume que:

- el adapter debe generar algún identificador correlacionable por evento,
- el nodo debe preservarlo o devolver una referencia equivalente,
- y esa referencia debe permitir al adapter rastrear el estado de cada evento enviado.

### 8.3. Pendiente

Queda abierto definir:

- si habrá un `event_id`,
- si también habrá `request_id`,
- si se usará correlación por avatar,
- o alguna combinación de estos elementos.

---

## 9. Bloqueos por avatar

### 9.1. Existen acciones bloqueantes por avatar

Se define que ciertas acciones pueden bloquear temporalmente el avance de otros eventos del mismo avatar.

Ejemplos actualmente conocidos:

- alta de avatar,
- actualización de avatar.

### 9.2. El bloqueo no es global al adapter

Un avatar bloqueado **no bloquea** al resto de los avatares administrados por el mismo adapter.

Esto significa que:

- los bloqueos están contenidos por avatar,
- el adapter puede seguir procesando otros avatares aunque uno esté bloqueado,
- y el batch puede seguir incluyendo eventos de otros avatares no bloqueados.

### 9.3. Lo que todavía no está definido

Aún no está completamente definido:

- qué otros tipos de eventos son bloqueantes,
- qué alcance exacto tiene el bloqueo dentro del avatar,
- si bloquea solo conversaciones,
- o si también bloquea otros tipos de eventos del mismo avatar.

### 9.4. Definición provisional

Se fija como regla inicial que el bloqueo es **por avatar** y nunca global al adapter.

---

## 10. Tipos de eventos identificados hasta ahora

La lista actual de eventos identificados desde el adapter hacia Fluxbee incluye, al menos:

- `heartbeat`
- `avatar_ilk_create`
- `avatar_ilk_update`
- `contact_message`
- `system_alert`

Esta lista no debe considerarse cerrada.

### 10.1. Heartbeat

Se utiliza cuando el adapter no tiene otros eventos para enviar, pero necesita sostener el intercambio y retirar respuestas pendientes.

### 10.2. Alta de avatar / ILK

Se utiliza cuando el adapter detecta un avatar nuevo cuya identidad debe ser resuelta en Fluxbee.

### 10.3. Actualización de avatar / ILK

Se utiliza cuando el adapter detecta cambios relevantes en la información del avatar y necesita sincronizarlos.

### 10.4. Mensaje nuevo de contacto hacia avatar

Se utiliza cuando el adapter detecta una nueva unidad conversacional a procesar desde el contacto externo hacia el avatar.

### 10.5. Alerta de sistema

Se utiliza para informar situaciones relevantes detectadas por el adapter, generalmente asociadas a un avatar.

Ejemplos:

- instancia de avatar frenada,
- error persistente,
- ausencia prolongada de respuesta a eventos previos,
- otros estados anómalos que deban ser tratados por Fluxbee.

---

## 11. Alertas de sistema

### 11.1. Foco actual en alertas por avatar

Hasta el momento, las alertas observadas o esperadas se asocian principalmente a un avatar.

Aunque una falla de Linked Helper pueda impactar varios avatares al mismo tiempo, operativamente suele expresarse como alertas por cada avatar afectado.

### 11.2. Posible extensión futura

Se deja contemplada la posibilidad de que existan también alertas globales del adapter, aunque hoy no se tenga un catálogo claro de ellas.

### 11.3. Definición provisional

El modelo actual se centra en alertas por avatar, sin excluir futuras alertas del adapter como entidad propia.

---

## 12. Dudas abiertas

### 12.1. Correlación exacta

Pendiente definir la estructura exacta de correlación entre eventos y respuestas.

### 12.2. Política de ACK

Pendiente definir:

- si todos los eventos deben recibir ACK,
- en qué momento se devuelve,
- y qué implica exactamente cada ACK.

### 12.3. Política de errores

Pendiente definir:

- taxonomía de errores,
- si son finales o reintentables,
- y cómo deben correlacionarse.

### 12.4. Política de reintentos y timeouts

Pendiente definir:

- si el adapter puede reenviar eventos sin respuesta,
- cuánto espera antes de alertar,
- y cómo manejar respuestas tardías.

### 12.5. Casos de consolidación y respuestas tardías

Pendiente definir cómo se resuelven escenarios donde:

- un mensaje consolidado queda sin respuesta dentro del timeout,
- luego llega nueva actividad del mismo contacto,
- y eventualmente arriba una respuesta vieja.

### 12.6. Catálogo exacto de eventos bloqueantes

Pendiente definir qué acciones adicionales deben considerarse bloqueantes por avatar.

---

## 13. Síntesis

El mecanismo acordado hasta este punto puede resumirse así:

- la comunicación es siempre iniciada por el adapter,
- el intercambio funciona como polling bidireccional,
- el heartbeat forma parte del mecanismo normal de retiro de pendientes,
- el adapter envía batchs de eventos,
- los eventos son hechos observados por el adapter y Fluxbee determina qué hacer con ellos,
- el nodo mantiene una cola de respuestas/salidas por adapter,
- las respuestas pueden llegar en el mismo intercambio o en polls posteriores,
- hay bloqueo por avatar y no por adapter completo,
- y todavía quedan abiertas varias decisiones finas sobre correlación, ACK, errores, timeouts y reintentos.
