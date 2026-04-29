# Linked Helper + Fluxbee
## Definiciones, dudas abiertas y puntos de dolor

> Documento de trabajo para ordenar definiciones ya acordadas, dudas abiertas y puntos de dolor respecto de la integración entre Linked Helper, un adaptador técnico y Fluxbee.
>
> Esta versión actualiza el documento previo y reemplaza conclusiones que ya quedaron obsoletas por definiciones más recientes.

---

## 1. Objetivo del análisis

El problema no consiste solamente en "conectar Linked Helper" a Fluxbee.

El caso analizado tiene varias particularidades:

- Linked Helper no expone una API pública adecuada para operar de forma directa.
- Existe un adaptador técnico que conoce cómo leer y escribir sobre la base utilizada por Linked Helper.
- En el dominio real no alcanza con identificar únicamente al remitente externo.
- También importa el destinatario interno o representante automatizado, porque de él dependen idioma, huso horario, credenciales, operadores, reglas comerciales y otras configuraciones de negocio.
- Además del mensaje entrante, pueden existir otros hechos relevantes: alta o actualización de avatares, alertas operativas por avatar, edición o cancelación futura de mensajes, etc.

Por lo tanto, el análisis debe contemplar no solo el ingreso de mensajes sino también:

- identidad,
- contexto conversacional,
- responsabilidades del adaptador,
- semántica específica del canal,
- seguridad y protocolo de comunicación,
- y el posible impacto sobre cognition.

---

## 2. Definiciones fijadas o bastante encaminadas

### 2.1. Existirá un nodo `IO.linkedhelper`

Queda definido, para esta línea de trabajo, que la integración se hará mediante un nodo específico `IO.linkedhelper`.

Esto implica que:

- no se seguirá, por ahora, el camino de evolucionar simplemente `IO.api`,
- el canal Linked Helper tendrá una semántica propia,
- y el nodo será responsable de traducir el protocolo privado `adaptador ↔ nodo` al protocolo interno de Fluxbee.

### 2.2. El servicio intermedio actúa como adaptador técnico

Se toma como definición que el servicio intermedio es el componente que conoce la realidad técnica de Linked Helper.

Sus responsabilidades incluyen, en principio:

- conocer cómo acceder a la PC/red donde corre Linked Helper,
- conocer la estructura de las tablas/bases utilizadas por Linked Helper,
- saber cómo extraer información relevante,
- saber cómo insertar/actualizar/borrar mensajes pendientes,
- absorber cambios internos de Linked Helper sin trasladar ese acoplamiento a Fluxbee.

Consecuencia:

- Fluxbee no debería depender directamente de la estructura interna de Linked Helper ni de la topología de red/máquinas donde se ejecuta.

### 2.3. Fluxbee no debería tocar directamente la DB de Linked Helper

Queda asentado que operaciones como:

- insertar mensajes,
- actualizar mensajes pendientes,
- borrar/cancelar mensajes pendientes,
- leer estados internos de LH,

pertenecen al adaptador técnico y no a Fluxbee.

Lo que sí puede ocurrir es que Fluxbee reciba eventos relacionados con esos hechos y luego emita resultados que el adaptador materializará en LH.

### 2.4. El adaptador es dueño del dato del avatar

Queda definido que el adaptador es el dueño operativo de la información del avatar.

Eso implica que:

- detecta nuevos avatares,
- detecta cambios sobre avatares existentes,
- y comunica esos hechos al nodo `IO.linkedhelper`, que a su vez resuelve las acciones necesarias dentro de Fluxbee.

El adaptador mantiene además un mapping entre avatar externo e identidad registrada en Fluxbee para poder emitir eventos coherentes.

### 2.5. El avatar puede modelarse como un ILK enriquecido

La interpretación actual más convincente es que el avatar puede modelarse como un **ILK con información adicional**, en lugar de inventar una categoría enteramente nueva.

En esta lectura, el avatar sería una identidad del sistema con datos como, por ejemplo:

- nombre,
- id externo,
- mail,
- compañía,
- tenant,
- idioma,
- huso horario,
- tono,
- metadata comercial u operativa.

Todavía no está cerrado qué tipo de ILK sería, pero sí que no necesariamente hace falta una nueva abstracción distinta de ILK.

### 2.6. El contacto externo terminará siendo una identidad del sistema

La dirección conceptual más consistente es que el contacto externo también termine cargado como una identidad más del sistema.

No necesariamente debe ser el adaptador quien lo gestione directamente, pero sí parece razonable asumir que el contacto:

- no es solo texto pasajero,
- sino una identidad externa con continuidad conversacional.

### 2.7. El mail no es requisito estructural para el contacto externo

Queda bastante encaminado que el contacto externo no debe depender del mail para existir en Fluxbee.

Esto es importante porque en LinkedIn / Linked Helper el mail puede no estar disponible.

La identidad del contacto deberá poder resolverse con:

- nombre u otros datos básicos,
- tenant,
- y algún identificador externo de canal suficientemente estable.

### 2.8. El identificador externo del contacto debe ser compuesto

En Linked Helper los ids de contacto pueden repetirse entre distintas bases/cuentas.

Por eso, para evitar colisiones, parece necesario trabajar con un identificador externo compuesto, por ejemplo combinando:

- avatar o cuenta de destino,
- identificador del contacto en Linked Helper,
- eventualmente tenant o base de origen.

El `contact person id` nativo de LH puede viajar como dato útil de referencia, pero no debería tomarse por sí solo como identidad canónica global.

### 2.9. Si un avatar deja de usarse, el adaptador puede simplemente dejar de emitir eventos

No parece necesario, al menos en una primera etapa, modelar un lifecycle complejo en Fluxbee cuando un avatar deja de existir operativamente.

Una lectura posible es:

- el adaptador deja de enviar eventos para ese avatar,
- Fluxbee conserva la identidad como residuo histórico o registro inactivo,
- y más adelante se decide si vale la pena una desactivación formal.

### 2.10. La comunicación `adaptador ↔ IO.linkedhelper` será por HTTP + HTTPS

Queda definido que, para esta etapa, la comunicación entre adaptador y nodo `IO.linkedhelper` se hará por:

- HTTP como protocolo de aplicación,
- HTTPS para proteger el canal en tránsito.

Por ahora no se seguirá el camino de TCP ni gRPC.

### 2.11. La autenticación mínima se hará con una installation key única por adaptador

Queda definido que cada instalación/adaptador tendrá una credencial propia.

No se usará una clave fija y compartida para todos.

Esto permite al menos:

- identificar cada instalación,
- revocar una instalación puntual,
- y preparar el terreno para rotaciones futuras.

### 2.12. El adaptador inicia siempre la comunicación

El mecanismo de intercambio definido hasta ahora es de polling iniciado siempre por el adaptador.

No habrá webhooks desde Fluxbee hacia el adaptador.

El flujo conceptual es:

- el adaptador hace un poll periódico,
- si tiene eventos, los envía,
- si no tiene eventos, envía un heartbeat,
- el nodo responde con lo que tenga pendiente para ese adaptador,
- o con un heartbeat si no hay nada más que devolver.

### 2.13. El heartbeat no es solo keepalive

El heartbeat cumple dos funciones:

- indicar que el adaptador sigue vivo,
- y actuar como mecanismo de retiro de respuestas pendientes.

Es decir, el heartbeat forma parte del mecanismo normal de intercambio, no es solamente una señal vacía de salud.

### 2.14. El nodo mantiene una cola de respuestas pendientes por adaptador

Queda definido que `IO.linkedhelper` es responsable de mantener y administrar una cola de respuestas/salidas pendientes por adaptador.

Cuando un adaptador inicia un intercambio, el nodo determina qué resultados pendientes corresponden a ese adaptador y los devuelve en la respuesta.

### 2.15. Los items enviados por el adaptador son eventos hacia Fluxbee

Los elementos del batch no son comandos ya resueltos, sino eventos producidos por el adaptador.

Fluxbee es quien determina qué hacer con esos eventos según su tipo y metadata.

Ejemplos:

- un mensaje de contacto puede terminar en IA,
- un alta de avatar puede terminar en identity/frontdesk,
- una alerta puede terminar en un workflow, log, Slack o mail.

### 2.16. Existen bloqueos por avatar, nunca por adaptador global

Queda definido que ciertos eventos pueden ser bloqueantes para un avatar particular.

Ejemplos claros ya identificados:

- alta de avatar,
- actualización de avatar.

Todavía no está cerrado el listado completo ni el alcance exacto del bloqueo, pero sí que:

- un avatar no bloquea a otro,
- y el bloqueo se acota al ámbito de ese avatar.

### 2.17. Para `conversation_message`, resolver o crear el contacto es pre-requisito

Queda definido que, para poder avanzar con la automatización de una conversación, el nodo debe primero resolver o crear correctamente el ILK del contacto externo.

Si eso falla por indisponibilidad de identity, frontdesk u otro servicio interno:

- el nodo no debe avanzar con la automatización de esa conversación,
- debe devolver ese caso como error para ese evento,
- y el adaptador decidirá luego si reintenta, alerta o ambas cosas.

### 2.18. `avatar_update` será, en v1, un snapshot completo

Para la primera versión, `avatar_update` se modelará igual que `avatar_create` en términos de estructura de datos, diferenciándose por el tipo de evento.

No se intentará definir todavía un mecanismo de patch parcial.

### 2.19. Las respuestas del nodo al adaptador se modelarán con tres tipos principales

Para v1, las respuestas conceptuales del nodo al adaptador serán:

- `ack`
- `result`
- `heartbeat`

`error` no será un mensaje aparte, sino un `result` con `status = error`.

### 2.20. `result` tendrá `status` y `result_type`

Queda bastante encaminado que `result` tendrá al menos:

- `status = success | error`
- `result_type`
- `payload` opcional
- `error_code` opcional
- `error_message` opcional
- `retryable` opcional

La decisión sobre qué hacer con ese resultado recae en el adaptador.

### 2.21. En resultados conversacionales no debe asumirse texto obligatorio

Para casos como `conversation_processed`, no debe asumirse que siempre existirá un `payload.message` textual.

Debe contemplarse que el contenido saliente pueda ser:

- texto,
- adjuntos,
- o una combinación de ambos.

---

## 3. Dudas abiertas

### 3.1. Falta `ILK_UPDATE` en el contrato actual de Identity

Se detectó que, en la documentación actual revisada, existen operaciones como:

- `ILK_CREATE`
- `ILK_LINK`
- `ILK_DELETE`
- `ILK_GRADUATE`

pero no aparece una operación formal `ILK_UPDATE`.

Dado que el adaptador será dueño del dato del avatar y debe poder informar cambios, este punto debe elevarse al equipo de core.

### 3.2. ¿Qué tipo de ILK será el avatar?

Si el avatar es un ILK con metadata adicional, todavía queda por resolver **qué tipo de ILK corresponde**.

Las dos alternativas principales que siguen abiertas son:

#### A. `human/internal`

El avatar se modela como una persona interna virtualizada o representada.

#### B. `agent`

El avatar se modela como un agente automatizado que responde formalmente dentro del sistema.

#### Por qué importa

Porque esta decisión afecta:

- cómo se interpreta la identidad,
- cómo se configura el prompting,
- qué parte del comportamiento vive en metadata,
- y si se utiliza o no el mecanismo de modules/degrees.

### 3.3. ¿Cómo se define el prompting del avatar?

Este punto sigue explícitamente abierto.

Todavía no está cerrado si el prompting del avatar debería vivir:

- en metadata/configuración simple,
- o mediante modules/degrees,
- o en alguna combinación de ambos.

### 3.4. ¿Quién define modules/degrees si finalmente aplican?

En caso de adoptar ese modelo, sigue pendiente responder:

- quién define los modules,
- quién compone los degrees,
- quién decide qué degree corresponde a cada avatar,
- y si esto es razonable para esta integración o demasiado complejo para una primera etapa.

### 3.5. ¿Quién da de alta el contacto externo?

Aunque ya parece razonable que el contacto termine teniendo ILK propio, no está completamente cerrado quién ejecuta ese alta en términos de responsabilidad interna.

Alternativas todavía abiertas:

- que el adaptador lo haga explícitamente,
- que lo resuelva el nodo `IO.linkedhelper`,
- o que exista una capa más explícita de consulta/alta hacia identity.

### 3.6. Política de reintentos, timeouts y respuestas tardías

Esto sigue completamente abierto.

Preguntas todavía sin cerrar:

- cuánto espera el adaptador una respuesta final,
- si reintenta automáticamente,
- cuándo alerta,
- qué pasa si una respuesta llega tarde,
- qué pasa si ya hubo una nueva consolidación para esa misma conversación,
- y cómo se evita entrar en estados ambiguos.

### 3.7. Modelo exacto de correlación

Está claro que el adaptador debe mantener ids trackeables y que el nodo debe devolverlos en sus respuestas.

Lo que todavía no está definido con precisión es:

- si habrá solo `event_id`,
- si existirá además `request_id`,
- o si habrá otra combinación de identificadores.

### 3.8. Qué eventos son bloqueantes además de los ya conocidos

Hoy se conocen algunos eventos bloqueantes claros, pero no está cerrado el listado exhaustivo.

Tampoco está completamente definido qué cosas bloquean exactamente dentro del avatar más allá del procesamiento conversacional.

### 3.9. Semántica exacta del heartbeat

Aunque ya quedó definido que el heartbeat no es solo keepalive, todavía falta decidir si además debería llevar información adicional como:

- versión de protocolo,
- cantidad de cuentas activas,
- resumen de backlog,
- o cualquier otra información operativa útil.

### 3.10. ¿Hace falta informar inserciones manuales, ediciones o cancelaciones a Fluxbee?

Este punto quedó explícitamente pendiente y no debería ignorarse.

Preguntas asociadas:

- si Fluxbee propuso una respuesta y luego se editó, ¿debe enterarse?
- si se cancela una respuesta pendiente, ¿debe quedar trazado?
- si hubo un mensaje manual que Fluxbee no vio generar, ¿debe informarse igual para mantener contexto?
- si hubo intervención humana externa, ¿cómo impacta en cognition?

### 3.11. ¿Cómo afecta esto a cognition?

No está resuelto qué debería considerar Fluxbee como “verdad conversacional” cuando ocurren situaciones como:

- propuesta automática,
- edición posterior,
- cancelación,
- inserción manual,
- respuesta real enviada distinta a la propuesta,
- intervención humana por fuera del flujo principal.

### 3.12. ¿Debe contemplarse explícitamente el caso de errores propios del adaptador?

Hasta ahora las alertas observadas son mayormente por avatar.

Aun así, podría ser necesario contemplar también errores propios del adaptador como proceso, aunque todavía no haya una taxonomía clara de esos casos.

### 3.13. ¿Qué campos mínimos exactos llevará cada payload v1?

Ya hay una dirección conceptual bastante clara para:

- `heartbeat`
- `avatar_create`
- `avatar_update`
- `conversation_message`

pero todavía falta cerrar el contrato exacto de campos mínimos y opcionales en cada uno.

---

## 4. Puntos de dolor identificados

### 4.1. En este dominio el destinatario importa tanto como el remitente

No alcanza con identificar al contacto externo.

También importa quién es el representante automatizado que recibe/interpreta la conversación, porque de esa identidad dependen cuestiones como:

- idioma,
- huso horario,
- tenant/empresa,
- credenciales para integraciones,
- operadores asociados,
- reglas comerciales,
- tono o estilo esperado.

Esto vuelve insuficiente cualquier modelo demasiado simplificado de “remitente + texto”.

### 4.2. El problema de identidad y el problema conversacional están muy entrelazados

Decidir cómo se modela el avatar afecta:

- tipo de ILK,
- prompting,
- routing,
- alta/sync,
- bloqueo por avatar,
- contrato entre adaptador y nodo,
- y posible impacto sobre cognition.

No son decisiones aisladas.

### 4.3. Existe tensión entre simplicidad operativa y riqueza del contrato

Un contrato simple facilita integración y debugging.

Un contrato más rico facilita trazabilidad, contexto, semántica de canal e impacto sobre cognition.

Todavía no está resuelto cuál de las dos dimensiones debe priorizarse en futuras iteraciones.

### 4.4. El contacto externo puede escalar mucho

Si cada contacto externo termina siendo un ILK, aparece una cuestión de volumen, alta automática y mapping estable.

Esto no significa que sea incorrecto, pero sí que hay que pensarlo como una decisión con impacto operativo.

### 4.5. Los ids externos de LH no son suficientes por sí solos

El contacto no puede identificarse de manera confiable solo con el id nativo de Linked Helper si ese id se repite entre distintas bases/cuentas.

Eso obliga a definir una clave externa compuesta y consistente.

### 4.6. El canal no se reduce a “mensaje entrante”

Aunque el caso más evidente es el mensaje nuevo, el canal también necesita contemplar hechos como:

- alta de avatar,
- actualización de avatar,
- alertas operativas,
- y posiblemente, más adelante, ediciones, cancelaciones o inserciones manuales.

Esto refuerza la necesidad de una semántica de canal propia.

### 4.7. Hay riesgo de submodelar al avatar

Si el avatar se reduce a un dato trivial de configuración, se corre el riesgo de perder:

- su identidad operativa,
- sus diferencias reales de comportamiento,
- su relación con tenant/credenciales,
- y su importancia como destino de la conversación.

### 4.8. Hay riesgo de sobremodelar al avatar

Si se lo convierte demasiado pronto en una entidad extremadamente específica o compleja, puede agregarse una capa nueva innecesaria cuando quizás un ILK enriquecido alcanzaba.

### 4.9. La verdad conversacional puede divergir de la propuesta original

Aunque todavía no esté resuelto, existe un punto de dolor fuerte cuando:

- Fluxbee propone algo,
- un humano lo edita o cancela,
- o se inserta contenido manual por fuera del flujo principal.

Eso puede afectar contexto, trazabilidad y cognition.

### 4.10. La política de respuesta tardía puede volver ambiguo el estado de una conversación

Si una conversación consolidada no recibe resultado dentro de cierto tiempo y luego llegan nuevos mensajes del mismo contacto, puede aparecer ambigüedad entre:

- la respuesta vieja pendiente,
- la nueva consolidación,
- y la decisión correcta del adaptador.

Esto sigue abierto y probablemente requerirá diseño adicional.

---

## 5. Temas especialmente sensibles que no deberían perderse

### 5.1. Impacto de edición/cancelación/manual sobre la verdad conversacional

Debe quedar marcado como pendiente de primer orden.

No es un detalle menor, porque define qué conserva o no conserva Fluxbee cuando la realidad final difiere de lo originalmente propuesto.

### 5.2. Separación entre identidad estable y datos del evento

Conviene evitar mezclar en el mismo lugar:

- datos estables del avatar/contacto,
- y datos situacionales de la conversación/evento.

Aunque no esté resuelta la forma exacta, esta separación conceptual ayuda a evitar modelos confusos.

### 5.3. La decisión `agent` vs `human/internal` no es decorativa

Esta decisión condiciona directamente:

- si el prompting va por modules/degrees,
- cómo se interpreta el avatar dentro de Fluxbee,
- qué responsabilidades recaen en identity,
- y qué tan “formal” se vuelve el representante automatizado dentro del sistema.

### 5.4. La seguridad mínima ya quedó encaminada, pero su operación futura no debe olvidarse

Ya se definió:

- HTTP + HTTPS,
- installation key única por adaptador.

Pero siguen pendientes preguntas prácticas como:

- cómo se almacena esa key en el adaptador,
- cómo se rota,
- cómo se revoca,
- y cómo se maneja un reemplazo o reinstalación.

---

## 6. Síntesis del estado actual

Hoy puede decirse, de manera bastante prudente, lo siguiente:

1. **Linked Helper quedará encapsulado detrás de un adaptador técnico.**
2. **La integración usará un nodo específico `IO.linkedhelper`.**
3. **Fluxbee no debería acoplarse a la DB ni a la lógica interna de Linked Helper.**
4. **El adaptador será dueño del dato del avatar y mantendrá el mapping operativo necesario.**
5. **El avatar probablemente pueda modelarse como un ILK enriquecido.**
6. **El contacto externo probablemente también termine siendo una identidad del sistema.**
7. **La ausencia de email en LinkedIn no debería impedir la creación del contacto como ILK.**
8. **La comunicación `adaptador ↔ nodo` será por HTTP + HTTPS con installation key única por instalación.**
9. **El intercambio será iniciado siempre por el adaptador, mediante polling/heartbeat.**
10. **El nodo mantendrá una cola de respuestas pendientes por adaptador.**
11. **Existen bloqueos por avatar, nunca bloqueos globales por adaptador.**
12. **Para procesar una conversación, resolver o crear el contacto externo es un pre-requisito.**
13. **Las respuestas del nodo al adaptador se modelarán con `ack`, `result` y `heartbeat`, con `error` como status dentro de `result`.**
14. **Siguen abiertos puntos sensibles como `ILK_UPDATE`, el tipo exacto de ILK del avatar, prompting, retries, cognition y la verdad conversacional.**

---

## 7. Próximos pasos sugeridos para el análisis

Sin entrar todavía en implementación completa, los próximos temas a cerrar podrían ser:

1. actualizar el documento de decisiones/core para pedir `ILK_UPDATE`,
2. cerrar el contrato mínimo exacto de payloads v1,
3. definir con mayor precisión las categorías de `result_type`,
4. listar eventos bloqueantes conocidos por avatar,
5. definir política mínima de timeout / retry / alerta del lado adaptador,
6. y luego sí bajar esto a una lista de trabajo técnica para:
   - `IO.linkedhelper`,
   - cambios/core necesarios,
   - y adaptador mínimo compatible.
