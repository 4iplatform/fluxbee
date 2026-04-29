# Especificación técnica — Protocolo y mecanismo `adapter ↔ IO.linkedhelper`

> Documento actualizado para reflejar el mecanismo vigente del canal `adapter ↔ IO.linkedhelper`.

---

# 1. Alcance

Este documento cubre:

- transporte y seguridad mínima;
- sentido de la comunicación;
- polling/beacon;
- cola de resultados;
- batching;
- familias de respuestas;
- cambios de configuración devueltos por beacon;
- y estado mínimo durable del canal.

No cubre todavía:

- shape exacto de todos los payloads;
- schema JSON definitivo;
- política final de retries/timeouts;
- wire format alternativo;
- detalle interno nodo ↔ router;
- ni API final de config / status.

---

# 2. Transporte y seguridad

## 2.1. Transporte
El canal `adapter ↔ IO.linkedhelper` se implementa sobre:

- HTTP
- protegido por HTTPS

## 2.2. Motivo de la elección
Se descartan, por ahora:

- TCP con protocolo propio;
- gRPC;
- protobuf.

Se prioriza:
- menor complejidad operativa;
- implementación más simple;
- seguridad mínima razonable con certificado.

## 2.3. Autenticación mínima
Cada instalación del adapter tendrá una **installation key** única.

## 2.4. Pendientes de seguridad
Siguen abiertos:

- almacenamiento local de la key;
- rotación/revocación;
- mecanismo exacto de autenticación HTTP;
- posible seguridad adicional futura.

---

# 3. Topología y despliegue inicial

## 3.1. Instancia única del nodo
En la primera etapa, el nodo `IO.linkedhelper`:

- levanta una sola instancia;
- escucha en un puerto específico a definir;
- y no se contempla multi-instancia inicialmente.

## 3.2. Consecuencia
Esto simplifica:
- routing externo hacia el nodo;
- coordinación del estado local;
- persistencia inicial del estado de canal;
- y devolución de resultados/config al adapter correcto.

---

# 4. Sentido de la comunicación

## 4.1. La comunicación siempre la inicia el adapter
No hay webhooks ni push directo desde Fluxbee al adapter.

## 4.2. Polling / beacon
El adapter realiza polls periódicos hacia el nodo.

En cada intercambio:
- si tiene eventos, los envía;
- si no tiene eventos, envía heartbeat.

## 4.3. El heartbeat no es solo keepalive
También sirve para:

- retirar resultados pendientes;
- recibir ILKs ya listos;
- recibir cambios de configuración por ICH;
- sostener el canal de retorno.

---

# 5. Batch y cola de resultados

## 5.1. Batch por request
Un request puede contener múltiples eventos de:

- distintos avatares;
- distintas cuentas;
- distintos tipos de evento.

## 5.2. Respuesta del nodo
La respuesta puede devolver:

- `ack`
- `result`
- `heartbeat`

y mezclar elementos pendientes de distintos eventos previos del mismo adapter.

## 5.3. Cola por adapter
El nodo mantiene una cola o conjunto de resultados pendientes por adapter y los entrega en el siguiente beacon/poll.

## 5.4. Entrega diferida
Las respuestas a eventos enviados en un mismo poll no tienen por qué volver juntas.
Pueden llegar en polls posteriores.

---

# 6. Naturaleza de los eventos enviados por el adapter

Los items enviados por el adapter son **eventos** hacia Fluxbee, no comandos ya resueltos.

El adapter informa hechos observados.
Fluxbee decide qué hacer con ellos según:

- tipo de evento;
- metadata;
- estado del sistema;
- routing;
- servicios internos.

Ejemplos:
- avatar detectado;
- mensaje conversacional;
- alerta operativa.

---

# 7. Familias de mensajes / eventos vigentes o encaminados

## 7.1. `heartbeat`
Sigue siendo un tipo conceptual válido a nivel mecanismo.

## 7.2. `avatar_create`
Sigue vigente, pero ahora su semántica es:

- reportar descubrimiento del avatar;
- permitir que el nodo cree el ILK provisorio;
- sin habilitar todavía automatización en el adapter.

## 7.3. `conversation_message`
Sigue vigente, con una restricción nueva:
- no debe emitirse para un avatar sin ILK definitivo/utilizable y sin automatización LH habilitada por ICH.

## 7.4. `system_alert`
Sigue siendo relevante para estados y alertas no conversacionales del canal.

## 7.5. Evento futuro: `avatar_update`
Se reconoce como posible evento futuro, pero no forma parte del mínimo v1.

---

# 8. Gating por avatar

## 8.1. Principio general
Existen bloqueos por avatar.
Un avatar no bloquea a otro.

## 8.2. Casos claros hoy
- avatar nuevo sin ILK definitivo;
- avatar con ILK aún no `complete`;
- ICH LH con automatización desactivada.

## 8.3. Consecuencia
Mientras un avatar no esté listo:
- el adapter no debe enviar mensajes conversacionales de ese avatar;
- el nodo no debe devolverle ILK provisorio;
- y el canal queda en espera hasta promoción/habilitación.

## 8.4. Pendiente
Sigue abierto el catálogo exacto de eventos bloqueantes por avatar.

---

# 9. Cambios de configuración devueltos por beacon

## 9.1. Beacon como canal de configuración
Además de resultados, el beacon debe poder devolver al adapter:

- ILK de avatar ya listo;
- automatización LH habilitada/deshabilitada para un ICH;
- otros cambios de configuración futuros del canal.

## 9.2. Caso importante: ICH LH auto-detectado
Los ICHs de Linked Helper auto-detectados nacen con automatización desactivada.
Cuando ese flag cambia, el nodo debe devolvérselo al adapter por beacon.

## 9.3. Efecto en el adapter
Cuando recibe automatización desactivada para el ICH LH de un avatar:
- deja de buscar/reportar mensajes para IA de ese avatar;
- puede seguir reportando estados, alertas y eventos no conversacionales.

## 9.4. Colapso de cambios por ICH
Para cambios de configuración/estado por ICH que estén pendientes de entrega al adapter, el nodo debería mantener el **último estado relevante**.

Ejemplo:
- `disabled`
- luego `enabled`

No conviene devolver ambos cambios si el segundo ya pisa al primero.
Salvo que más adelante se defina explícitamente otro esquema con versionado o timestamps, para esta etapa debería prevalecer el último estado conocido por ICH.

---

# 10. Respuestas del nodo

## 10.1. `ack`
Indica recepción/aceptación para procesamiento.

## 10.2. `result`
Se mantiene el modelo conceptual:

- `status = success | error`
- `result_type`
- `payload` opcional
- `error_code` opcional
- `error_message` opcional
- `retryable` opcional

### Regla importante
`error` no es una familia aparte; es un `result` con `status = error`.

## 10.3. `heartbeat`
Se usa cuando no hay nada más que devolver o como respuesta mínima a un poll vacío.

## 10.4. Nuevos resultados especialmente relevantes
Con el nuevo reparto aparecen como importantes, por ejemplo:

- avatar listo / ILK completo;
- automatización LH habilitada/deshabilitada para un ICH;
- errores de resolución/promoción.

---

# 11. Estado durable mínimo del nodo

El nodo debería persistir de forma durable el estado mínimo de coordinación del canal, incluyendo al menos:

- mapping `installation_id / adapter_id ↔ installation_key` o referencia equivalente;
- mapping `installation_id ↔ avatares descubiertos`;
- mapping `avatar externo ↔ ILK provisorio/definitivo`;
- mapping `ILK/ICH ↔ installation_id`;
- lista de ILKs provisorios pendientes de promoción;
- estado conocido de automatización por ICH para LH;
- cambios pendientes de entregar al adapter;
- cola durable o reconstruible de resultados por adapter.

## 11.1. Motivo
La persistencia no es solo para “no perder datos”, sino para poder:

- seguir monitoreando ILKs pendientes;
- asociar correctamente cambios a la instalación adecuada;
- y devolver siempre la información al adapter correcto.

---

# 12. Config vs status/state (distinción conceptual)

## 12.1. Data plane
El beacon/poll debe transportar:
- eventos del adapter;
- resultados;
- y cambios incrementales relevantes para que el adapter siga operando.

## 12.2. Control plane
La consulta administrativa del estado del nodo no debería mezclarse sin más con el beacon.

Hay dos necesidades distintas:

### Config propia del nodo
Ejemplos:
- puerto;
- features del canal;
- installation keys / adapters registrados;
- límites o switches propios del nodo.

### Estado / pending del canal
Ejemplos:
- ILKs provisorios pendientes;
- ICHs LH deshabilitados;
- mappings installation ↔ avatar ↔ ILK/ICH;
- cambios pendientes.

## 12.3. Pendiente
Sigue abierto si esto se expondrá por:
- `get config` / `set config`,
- otro mecanismo de status/state,
- o una combinación.

Pero conceptualmente conviene no mezclar “config del nodo” con “estado operativo multi-tenant del canal” sin distinguirlos.

---

# 13. Qué sigue vigente y qué cambia

## 13.1. Sigue vigente
- HTTP + HTTPS
- installation key por adapter
- polling/beacon iniciado siempre por el adapter
- heartbeat como retiro de pendientes
- cola de resultados por adapter
- familias `ack` / `result` / `heartbeat`
- `error` como status de `result`

## 13.2. Cambia o se reencuadra
- `avatar_create` ya no implica habilitación inmediata;
- `conversation_message` no sale hasta tener ILK definitivo y automatización habilitada;
- el beacon ahora también transporta cambios de configuración;
- el adapter ya no consume ILKs provisorios;
- `avatar_update` sale del mínimo v1.

---

# 14. Pendientes / cajas negras

## 14.1. `get config` / `set config`
No está definido:
- si la habilitación/deshabilitación por ICH entra por `set config`;
- si se ve por `get config`;
- cómo listar ILKs/ICHs provisorios;
- cuánto estado operativo conviene devolver desde ahí.

## 14.2. Payloads exactos
Sigue pendiente cerrar el schema exacto de cada tipo de mensaje con este nuevo reparto.

## 14.3. Retries / timeouts
La política fina sigue abierta y no bloquea este documento.

---

# 15. Síntesis

El canal `adapter ↔ IO.linkedhelper` queda actualmente modelado como:

- HTTP/HTTPS;
- instancia única del nodo en un puerto fijo a definir;
- polling iniciado siempre por el adapter;
- heartbeat como retiro de pendientes y canal de config;
- cola de resultados por adapter;
- cambios de ICH colapsados por último estado;
- estado durable mínimo del canal;
- automatización controlada por ICH del canal LH.
