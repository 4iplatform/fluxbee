# Especificación técnica — Relay global para Nodos IO

**Estado:** propuesta normativa para implementación  
**Fecha:** 2026-04-09  
**Audiencia:** equipo de desarrollo de `io-common`, `IO.*`, `fluxbee_sdk` y runtime/ops  
**Ámbito:** todos los nodos `IO.*` del sistema

---

## 0. Objetivo

Definir un mecanismo de **relay/sessionización global** para todos los nodos `IO.*`, implementado en `io-common`, cuya función es **consolidar fragmentos inbound del mismo turno físico del canal en un único mensaje canónico** antes de enviarlo al router.

Esta especificación busca eliminar ambigüedades sobre:

- dónde vive el relay,
- qué problema resuelve,
- cómo se identifica una sesión de relay,
- cuándo se consolida y cuándo no,
- cómo se integra con identity, blob y router,
- qué se configura por nodo,
- cómo debe evolucionar hacia un backend durable sin rediseñar el contrato.

---

## 1. Decisiones cerradas

### 1.1 Ubicación del mecanismo

El relay pasa a ser un componente **global y obligatorio de `io-common`**.

Consecuencias:

- ningún adapter `IO.*` debe implementar su propia sessionización ad hoc;
- `IO.slack` debe migrar su `SlackSessionizer` al mecanismo común;
- futuros `IO.*` deben integrarse al relay común desde el inicio.

### 1.2 Alcance funcional

El relay:

- actúa **antes** del envío al router;
- actúa **antes** de cognition;
- no modifica cognition;
- no forma parte de la memoria cognitiva;
- no es storage core;
- no es historia conversacional durable;
- no reemplaza `thread_id`, `thread_seq`, `src_ilk`, `ich` ni `blob_ref`.

### 1.3 Activación por nodo

Todos los nodos `IO.*` pasan por el relay.

Regla cerrada:

- `relay_window_ms > 0` => relay activo con espera/consolidación.
- `relay_window_ms = 0` => passthrough inmediato, sin espera.

Es decir, el relay es universal como mecanismo, pero su ventana puede ser cero cuando la naturaleza del canal no justifique buffering.

### 1.4 Responsabilidad arquitectónica

El relay resuelve un único problema:

> consolidar múltiples fragmentos inbound que pertenecen al mismo turno físico del canal en un solo `payload text/v1` canónico antes de publicar al router.

No debe usarse para:

- almacenar historia de conversación,
- reinyectar conversaciones antiguas,
- construir memoria de largo plazo,
- realizar deducciones semánticas,
- sustituir colas de negocio,
- persistir estado de negocio del interlocutor.

### 1.5 Backend de storage

La interfaz del relay se define desde v1 con **backend abstracto**.

Backends previstos:

- `memory`
- `durable`

Decisión de implementación inicial:

- la primera implementación puede arrancar con `memory`, alineada con el estado actual de `io-common` y `IO.slack`;
- la especificación deja normado cómo debe verse `durable` para que la migración posterior no cambie el contrato del adapter.

### 1.6 Rehidratación

La rehidratación de un backend durable **no** es replay histórico.

Regla cerrada:

- solo se rehidratan sesiones **recientes, activas y no vencidas**;
- sesiones expiradas o viejas se descartan;
- el relay no debe retomar buffers “antiguos” (por ejemplo, de días anteriores).

---

## 2. Relación con el resto del sistema

### 2.1 Con cognition

No hay cambios en cognition.

Cognition solo debe ver el mensaje ya consolidado por el IO. El relay existe totalmente fuera de su dominio.

### 2.2 Con router

El router no participa del relay.

El router recibe el mensaje ya consolidado y continúa su flujo normal:

- routing,
- asignación de `thread_seq`,
- enriquecimiento cognitivo,
- entrega al destino.

### 2.3 Con identity

Identity sigue operando por el pipeline ya adoptado en IO:

- `lookup`
- `provision_on_miss`
- `forward`

El relay no reemplaza identity.

La integración correcta es:

1. el adapter construye/normaliza el fragmento inbound;
2. el relay agrupa por `RelayKey`;
3. al `flush`, se invoca el pipeline de identity/forward del inbound processor;
4. el mensaje consolidado sale con `meta.src_ilk` resuelto o `null` en degraded fallback.

### 2.4 Con blobs y adjuntos

Los adjuntos siguen el contrato canónico del sistema.

Reglas:

- el relay puede acumular metadatos de adjuntos y/o referencias ya normalizadas;
- el relay no inventa un contrato paralelo para binarios;
- el `flush` debe producir un `text/v1` compatible con `NodeSender::send`;
- la decisión `content` vs `content_ref` sigue perteneciendo al SDK.

---

## 3. Modelo conceptual

### 3.1 Entidades

#### 3.1.1 `RelayKey`

Identificador determinístico de la sesión de relay.

Propiedades obligatorias:

- lo define el adapter del canal;
- debe ser estable para todos los fragmentos que representen el mismo turno físico;
- no debe depender de heurísticas semánticas;
- no debe requerir acceso al router para construirse.

#### 3.1.2 `RelayFragment`

Unidad mínima de ingreso al relay.

Representa un fragmento individual recibido desde el canal externo.

Contenido mínimo esperado:

- `relay_key`
- `fragment_id` externo o derivado para dedup
- `received_at`
- `content_text` opcional
- `attachments` opcionales
- `raw_payload` opcional para trazabilidad
- `io_context`
- `identity_input`
- `flush_hints` opcionales (por ejemplo `final=true`)

#### 3.1.3 `RelaySession`

Estado acumulado de una sesión abierta.

Campos normativos:

- `relay_key`
- `opened_at`
- `last_fragment_at`
- `deadline_at`
- `expires_at`
- `version`
- `seen_fragment_ids`
- `fragment_count`
- `byte_count`
- `contents[]`
- `attachments[]`
- `raws[]` opcional
- `io_context` base
- `identity_input` base

#### 3.1.4 `AssembledTurn`

Resultado del `flush`.

Debe poder convertirse de forma determinística en un único mensaje interno Fluxbee.

---

## 4. Contrato de `io-common`

## 4.1 Interfaces obligatorias

### 4.1.1 `RelayPolicy`

Configuración del relay por nodo.

Campos mínimos:

- `enabled: bool` (derivado operativamente de `relay_window_ms > 0`)
- `relay_window_ms: u64`
- `stale_session_ttl_ms: u64`
- `rehydrate_grace_ms: u64`
- `max_open_sessions: usize`
- `max_fragments_per_session: usize`
- `max_bytes_per_session: usize`
- `flush_on_attachment: bool`
- `flush_on_explicit_final: bool`
- `flush_on_max_fragments: bool`
- `flush_on_max_bytes: bool`
- `storage_mode: memory | durable`
- `raw_capture_mode: none | bounded`

### 4.1.2 `RelayStore`

Backend abstracto del relay.

Operaciones mínimas:

- `load_session(relay_key)`
- `upsert_session(session)`
- `delete_session(relay_key)`
- `list_rehydratable(now)`
- `count_open_sessions()`
- `evict_expired(now)`

### 4.1.3 `RelayBuffer`

Orquestador principal del mecanismo.

Operaciones mínimas:

- `handle_fragment(fragment) -> RelayDecision`
- `flush_session(relay_key, reason) -> Option<AssembledTurn>`
- `flush_expired(now) -> Vec<AssembledTurn>`
- `rehydrate_on_startup(now)`
- `drain_on_shutdown(mode)`

### 4.1.4 `RelayDecision`

Resultados posibles al ingresar un fragmento:

- `Hold`
- `FlushNow(AssembledTurn)`
- `DropDuplicate`
- `RejectCapacity`
- `DropExpired`

---

## 5. Responsabilidades de adapter vs `io-common`

### 5.1 Responsabilidades del adapter `IO.*`

Cada adapter debe:

- parsear el evento externo;
- decidir si el evento produce uno o más `RelayFragment`;
- construir el `RelayKey`;
- extraer `fragment_id` para dedup best-effort;
- proveer `io_context`;
- proveer `identity_input`;
- indicar hints de flush si el canal los tiene (`final`, `thread_break`, etc.).

### 5.2 Responsabilidades de `io-common`

`io-common` debe:

- deduplicar por `fragment_id` dentro de la sesión;
- abrir/cerrar sesiones;
- extender deadlines;
- aplicar límites;
- ejecutar timers de flush;
- ensamblar el turno final;
- invocar el inbound processor común;
- entregar el mensaje final al router.

### 5.3 Regla cerrada sobre el `RelayKey`

`io-common` **no** debe intentar inferir por sí mismo la semántica del canal.

Regla:

- el adapter define el `RelayKey`;
- `io-common` solo lo trata como identificador opaco.

Esto evita meter conocimiento de Slack/API/WhatsApp/Email dentro de la capa común.

---

## 6. Reglas normativas de sesión

### 6.1 Apertura

Una sesión se abre cuando llega un fragmento cuyo `RelayKey` no existe activo en el store.

### 6.2 Extensión de ventana

Cada nuevo fragmento válido de una sesión activa:

- actualiza `last_fragment_at`;
- recalcula `deadline_at = now + relay_window_ms`;
- incrementa `version`.

### 6.3 Deduplicación

La deduplicación del relay es **best-effort por sesión**.

Reglas:

- si el `fragment_id` ya fue visto en la sesión, el fragmento se descarta con `DropDuplicate`;
- la dedup del relay no reemplaza la dedup inbound general del IO si esta también existe;
- el set `seen_fragment_ids` debe estar acotado por la vida de la sesión.

### 6.4 Cierre

Una sesión se cierra cuando:

- se ejecuta un `flush` exitoso,
- expira por `stale_session_ttl_ms`,
- es descartada por política operacional,
- el nodo entra en shutdown y la política de drain así lo decide.

---

## 7. Reglas normativas de flush

## 7.1 Causas válidas de flush

El relay debe soportar estas razones de flush:

- `window_elapsed`
- `explicit_final`
- `attachment_boundary`
- `max_fragments`
- `max_bytes`
- `manual`
- `shutdown`
- `rehydrate_expiry`

### 7.2 Flush por ventana

Regla base:

- si no llega un nuevo fragmento antes de `deadline_at`, la sesión debe consolidarse y flushear.

### 7.3 Flush explícito

Si el adapter marca `flush_hints.final=true` y `flush_on_explicit_final=true`, la sesión debe flushear inmediatamente.

### 7.4 Flush por adjunto

Si `flush_on_attachment=true`, el ingreso de un adjunto puede forzar flush inmediato.

Regla práctica:

- usar esta opción en canales donde un attachment suele representar cierre natural del turno;
- desactivarla cuando el canal envía texto+adjunto como partes normales del mismo turno.

### 7.5 Flush por límites

Si se alcanza:

- `max_fragments_per_session`, o
- `max_bytes_per_session`

la política define si se flushea o si se deja de acumular.

Decisión normativa recomendada:

- preferir **flush inmediato** por límite en lugar de seguir aceptando fragmentos silenciosamente.

### 7.6 Ensamblado del contenido

Regla cerrada:

- el ensamblado del contenido debe ser **determinístico y simple**;
- v1 no introduce semántica ni compresión inteligente.

Baseline recomendado:

- concatenación ordenada por llegada,
- separador configurable por canal o default simple (`"\n"`).

### 7.7 Mensaje resultante

El `flush` debe producir un único mensaje Fluxbee compatible con el contrato `text/v1`.

Debe incluir:

- `payload.type = "text"`
- `payload.content` o `content_ref` vía SDK
- `attachments[]` cuando corresponda
- `meta.src_ilk` resuelto o `null`
- `meta.context.io.*` ya estandarizado
- metadata de relay bajo `meta.context.io.relay.*`

---

## 8. Metadata de relay

El mensaje consolidado debe dejar trazabilidad explícita de que pasó por relay.

Bloque recomendado:

```json
{
  "meta": {
    "context": {
      "io": {
        "relay": {
          "applied": true,
          "reason": "window_elapsed",
          "parts": 3,
          "window_ms": 2500,
          "opened_at": "...",
          "last_fragment_at": "..."
        }
      }
    }
  }
}
```

Campos mínimos:

- `applied: bool`
- `reason`
- `parts`
- `window_ms`

Campos opcionales:

- `opened_at`
- `last_fragment_at`
- `dropped_duplicates`
- `dropped_over_limit`

Regla:

- esta metadata es solo de observabilidad/trazabilidad;
- no reemplaza `thread_id` ni otro carrier canónico.

---

## 9. Configuración por nodo

## 9.1 Campos mínimos de configuración

Todo adapter `IO.*` que use `io-common` debe exponer al menos:

```yaml
relay:
  window_ms: 0
  stale_session_ttl_ms: 30000
  rehydrate_grace_ms: 10000
  max_open_sessions: 1000
  max_fragments_per_session: 8
  max_bytes_per_session: 262144
  flush_on_attachment: false
  flush_on_explicit_final: true
  flush_on_max_fragments: true
  flush_on_max_bytes: true
  storage_mode: memory
  raw_capture_mode: bounded
```

### 9.2 Semántica de defaults

- `window_ms = 0` => relay sin buffering.
- `stale_session_ttl_ms` debe ser mayor o igual a `window_ms`.
- `rehydrate_grace_ms` solo aplica a `storage_mode=durable`.
- `raw_capture_mode=bounded` permite conservar una traza acotada para debug.

### 9.3 Validaciones normativas

Configuraciones inválidas deben rechazarse por `CONFIG_SET`.

Ejemplos inválidos:

- `window_ms < 0`
- `stale_session_ttl_ms < window_ms`
- `max_open_sessions = 0`
- `max_fragments_per_session = 0`
- `max_bytes_per_session = 0`
- `storage_mode` no soportado

---

## 10. Política de capacidad y backpressure

### 10.1 Límite de sesiones abiertas

Si el store alcanza `max_open_sessions`, el comportamiento debe ser explícito.

Decisión normativa de v1:

- el sistema **no** debe crecer sin límite;
- el comportamiento default recomendado es **fail-open controlado**:
  - si no puede abrir una nueva sesión, el fragmento se envía en passthrough inmediato cuando sea seguro hacerlo;
  - si eso no es posible, se rechaza con métrica/log canónico.

### 10.2 Límite de bytes y fragmentos

No se debe seguir aceptando acumulación silenciosa luego de superar límites.

Comportamiento recomendado:

- preferir `flush` inmediato al alcanzar el límite;
- si incluso el flush no es viable, registrar drop explícito.

### 10.3 Objetivo operacional

El relay debe ser siempre:

- acotado,
- observable,
- predecible,
- incapaz de generar crecimiento descontrolado del proceso.

---

## 11. Backend durable (normativa de diseño)

## 11.1 Propósito

El backend durable no existe para transformar el relay en cola histórica.

Existe solo para cubrir:

- restart cercano del proceso,
- crash recovery de ventana corta,
- rehidratación acotada de sesiones activas.

## 11.2 Requisitos funcionales

Un backend durable debe proveer:

- lookup por `RelayKey`
- update atómico por sesión
- scan de sesiones rehidratables
- expiración por TTL
- borrado definitivo post-flush
- reconstrucción de timers al boot

## 11.3 Restricción de dominio

El backend durable del relay debe ser:

- local por nodo,
- privado del nodo IO,
- desacoplado de cognition,
- desacoplado del router,
- desacoplado de storage core.

No debe usarse una base compartida del sistema como dependencia del relay.

## 11.4 Elección de tecnología

La especificación **no obliga** a una tecnología única.

Opciones válidas futuras:

- store local documental tipo LanceDB por nodo,
- SQLite local,
- otro store embebido equivalente.

La elección concreta debe respetar el contrato `RelayStore`.

---

## 12. Rehidratación en backend durable

## 12.1 Regla cerrada

Al boot, solo se rehidratan sesiones cuyo estado cumpla simultáneamente:

- no fueron flusheadas,
- `now <= expires_at`,
- `now <= deadline_at + rehydrate_grace_ms`.

Si cualquiera de estas condiciones no se cumple, la sesión se descarta.

## 12.2 No replay histórico

Regla explícita:

- un mensaje pendiente de hace horas o días **no** debe retomarse como relay activo;
- el relay no es un mecanismo de replay de backlog.

## 12.3 Comportamiento al descartar por antigüedad

Cuando una sesión durable se descarta por vieja:

- no se reinyecta al router,
- se elimina del store,
- se registra métrica/log de expiración no recuperable.

## 12.4 Reconstrucción post-rehydrate

Las sesiones rehidratadas deben:

- recrear `deadline_at` efectivo,
- reinstalar timers,
- conservar su contenido acumulado,
- flushear normalmente si la ventana expira.

---

## 13. Shutdown y drain

### 13.1 Requisito mínimo

En shutdown ordenado, `RelayBuffer` debe ejecutar una política explícita.

### 13.2 Modos permitidos

Se permiten dos modos de drain:

- `flush_open_sessions`
- `drop_open_sessions`

### 13.3 Recomendación v1

- `storage_mode=memory`: default recomendado `drop_open_sessions` en shutdown abrupto y `flush_open_sessions` solo si el tiempo de cierre lo permite.
- `storage_mode=durable`: persistir estado y dejar rehidratación al próximo boot, siempre bajo las reglas de expiración.

---

## 14. Observabilidad obligatoria

## 14.1 Logs estructurados

Eventos mínimos a loggear:

- apertura de sesión
- dedup hit
- extensión de deadline
- flush
- passthrough por capacidad
- drop por expiración
- rehidratación exitosa
- descarte por sesión vieja

## 14.2 Métricas mínimas

Contadores requeridos:

- `relay_sessions_opened_total`
- `relay_sessions_flushed_total{reason=...}`
- `relay_fragments_received_total`
- `relay_fragments_dedup_dropped_total`
- `relay_fragments_overlimit_dropped_total`
- `relay_sessions_capacity_rejected_total`
- `relay_sessions_rehydrated_total`
- `relay_sessions_expired_total`

Gauges requeridos:

- `relay_sessions_open_current`
- `relay_buffer_bytes_current`

Histogramas recomendados:

- `relay_flush_parts`
- `relay_session_lifetime_ms`
- `relay_assembly_bytes`

---

## 15. Integración con `IO.slack`

## 15.1 Cambio requerido

La implementación actual `SlackSessionizer` debe migrarse a `io-common`.

### 15.2 Resultado esperado

`IO.slack` debe pasar a:

- construir `RelayFragment`;
- definir `RelayKey` específico de Slack;
- delegar acumulación, deadline y flush al `RelayBuffer` común.

### 15.3 Regla de compatibilidad

El comportamiento observable de `IO.slack` no debe degradarse respecto del MVP actual:

- misma semántica base de ventana,
- misma dedup por sesión,
- mismos límites de sesiones/fragmentos,
- mismo resultado final: un solo mensaje consolidado enviado al router.

---

## 16. Integración con futuro `IO.api.*`

## 16.1 Obligación

Todo nodo `IO.api.*` debe usar el relay global de `io-common`.

## 16.2 Definición del `RelayKey` para API

El `RelayKey` del adapter API debe construirse con material determinístico del canal/API.

Baseline recomendado:

- `source_system`
- `external_user_id`
- `conversation_key` o `thread_hint` si existe
- fallback determinístico cuando no exista thread nativo

Regla:

- el adapter API no debe usar heurísticas semánticas para agrupar;
- si el cliente externo provee una clave de conversación, debe priorizarse;
- si no la provee, el adapter debe documentar la derivación usada.

---

## 17. Plan de implementación

## 17.1 Fase 1 — Introducción del mecanismo común

Implementar en `io-common`:

- `RelayPolicy`
- `RelayStore`
- `RelayBuffer`
- `InMemoryRelayStore`
- ensamblado simple de turnos
- métricas/logs
- metadata `meta.context.io.relay.*`

## 17.2 Fase 2 — Migración de `IO.slack`

- reemplazar `SlackSessionizer` por el relay común;
- mantener config equivalente;
- validar no-regresión funcional.

## 17.3 Fase 3 — Adopción por nuevos adapters

- `IO.api.*`
- otros `IO.*` futuros

## 17.4 Fase 4 — Backend durable

Agregar:

- `DurableRelayStore`
- `rehydrate_on_startup`
- expiración por TTL
- tests de restart cercano

Sin cambiar:

- contrato del adapter,
- `RelayKey`,
- `RelayFragment`,
- semántica de flush.

---

## 18. Reglas de compatibilidad y no ambigüedad

### 18.1 Lo que el relay sí es

- un buffer técnico de consolidación inbound,
- local al nodo IO,
- configurable por nodo,
- pre-router,
- pre-cognition,
- acotado y observable.

### 18.2 Lo que el relay no es

- memoria cognitiva,
- cola durable de negocio,
- historial conversacional,
- storage compartido del sistema,
- ownership de `thread_id`,
- ownership de identidad.

### 18.3 Regla final de lectura

Si una implementación duda entre conservar más tiempo una sesión vieja o descartarla, la respuesta correcta del diseño es:

- **descartarla**.

El relay está diseñado para consolidación de ventana corta, no para recuperación de conversaciones antiguas.

---

## 19. Checklist de implementación

### `io-common`

- [ ] agregar `RelayPolicy`
- [ ] agregar `RelayStore`
- [ ] agregar `RelayBuffer`
- [ ] implementar `InMemoryRelayStore`
- [ ] definir metadata `meta.context.io.relay.*`
- [ ] agregar métricas y logs normativos
- [ ] tests de dedup, flush y capacidad

### `IO.slack`

- [ ] reemplazar `SlackSessionizer` por `RelayBuffer`
- [ ] mapear config existente a `RelayPolicy`
- [ ] mantener comportamiento observable actual
- [ ] validar envío consolidado al router

### futuros `IO.api.*`

- [ ] definir `RelayKey` del canal/API
- [ ] emitir `RelayFragment`
- [ ] usar relay común
- [ ] documentar derivación de key

### durable v2

- [ ] implementar `DurableRelayStore`
- [ ] agregar rehidratación acotada
- [ ] agregar descarte por expiración
- [ ] validar restart cercano

---

## 20. Criterio de aceptación

La implementación se considera alineada con esta spec cuando:

1. todos los `IO.*` pasan por un relay común de `io-common`;
2. `relay_window_ms=0` produce passthrough inmediato;
3. `relay_window_ms>0` consolida fragmentos en un único `text/v1`;
4. cognition no requiere cambios ni conocimiento del relay;
5. el relay opera con límites y métricas explícitas;
6. el diseño deja backend durable posible sin cambiar el contrato del adapter;
7. una sesión vieja no se rehidrata ni se reinyecta;
8. `IO.slack` deja de tener sessionización privada y usa la común.

