# Nodo IO - Slack (Especificación MVP)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Objetivo

El **Nodo IO Slack** es el adaptador entre Slack y el sistema interno (router + nodos).
Para el MVP, su objetivo es:

- **Inbound:** recibir mensajes únicamente a través de **menciones al bot** (`app_mention`).
- **Outbound:** enviar mensajes a Slack usando la **Slack Web API** (`chat.postMessage`).

Este documento **extiende** la especificación base de Nodos IO y define únicamente
las decisiones y necesidades **específicas de Slack**.

---

## 2. Supuestos del MVP

- Existe **una sola instancia activa** del Nodo IO Slack por app/workspace.
- Se acepta downtime si el nodo cae.
- Los Nodos IO **NO** manejan base de datos ni Redis; por lo tanto:
  - deduplicación y sessionización son **en memoria** (*best-effort*),
  - retries/outbox outbound son **en memoria** (*best-effort*),
  - tras reinicio se pierde el estado técnico.

> Nota: para evitar inconsistencias con necesidades futuras de durabilidad, la lógica interna del IO debería usar una **interfaz de persistencia** (MVP: InMemory) tal como se define en `io-common.md`.

---

## 3. Inbound: recepción de mensajes desde Slack

### 3.1 Mecanismo de recepción

El Nodo IO Slack **DEBE** soportar uno de los siguientes modos:

#### Opción A - Socket Mode (recomendado para MVP)
- Conexión WebSocket usando **App-Level Token** (`xapp-...`).
- Scope requerido: `connections:write`.
- No requiere endpoint HTTP público.

#### Opción B - Events API (HTTP)
- Endpoint HTTPS público.
- Soporte de `url_verification` (responder el `challenge` en plaintext).
- Validación de firma usando el **Signing Secret** de Slack.

La elección del modo **NO afecta** el contrato interno del IO.

---

### 3.2 Evento soportado: `app_mention`

El Nodo IO Slack **SOLO** procesa eventos:
- `type = "app_mention"`

Campos relevantes del evento Slack:
- `user`: ID del usuario que menciona al bot.
- `channel`: ID del canal donde ocurrió la mención.
- `text`: contenido del mensaje.
- `ts`: timestamp del mensaje.
- `thread_ts`: timestamp del hilo (si aplica).
- `event_id` / `event_ts`: identificadores del evento.

Otros eventos **DEBEN** ser ignorados.

---

## 4. Deduplicación inbound (Slack)

Slack puede reenviar eventos si:
- no recibe ACK,
- el ACK es lento,
- o hay errores transitorios.

### Clave de deduplicación recomendada (en orden de preferencia)
1. `event_id` (si está presente)
2. `(team_id, channel, ts)` como surrogate estable

### Implementación (sin DB/Redis)
- Caché en memoria con TTL.
- Límite por tamaño: `max_entries`/`max_bytes`.
- Eviction (TTL-LRU o drop-oldest).
- Métricas: dedup_hits, dedup_misses, evictions.

### Regla operativa
- El ACK a Slack **DEBE** enviarse lo antes posible, sin esperar respuesta del router ni de identidad.

> Nota: tras restart del nodo, se pierde el dedup y pueden ocurrir duplicados hacia el router.

---

## 5. Sessionización (Slack)

### Objetivo
Agrupar múltiples mensajes consecutivos del mismo usuario en el mismo canal/hilo
en un único turno lógico.

### Reglas MVP
- **PUEDE** aplicarse sessionización para Slack.
- Clave sugerida:
```
(channel, user, thread_ts || channel)
```
- Ventana de tiempo configurable (ej: 2-5 segundos).
- Si el nodo se reinicia durante la ventana, el MVP **PUEDE** aceptar pérdida
de agrupamiento (mensajes procesados separados).

---

## 6. Identidad (L3) y despacho al router

### 6.1 Requisito
El Nodo IO Slack **DEBE** usar el flujo compartido de `io-common` para identidad:

1. `lookup` por SHM (`jsr-identity-*`)
2. si hay miss/error: `provision_on_miss` via `SY.identity` (`ILK_PROVISION`)
3. si provision falla o timeout: forward con `meta.src_ilk = null`

- Si hay hit: incluir `meta.src_ilk`.
- Si hay miss y provision exitoso: incluir `meta.src_ilk` provisionado.
- Si hay miss y provision no exitoso: **enviar igualmente** con `meta.src_ilk = null`.

Esto implementa la estrategia recomendada de L3: **Desvío a Onboarding (Reprocesamiento)**.

### 6.2 Integración con identidad (ACK no bloqueante)
El Nodo IO Slack usa `io-common::FluxbeeIdentityProvisioner` para el paso de provision.
Regla operativa:

- **SIEMPRE** debe ACKear a Slack primero.
- Después del ACK puede ejecutar lookup/provision en el flujo inbound.
- No hay espera de una “sesión de identidad”; si no hay `src_ilk`, forward con `null`.

El control de flujo (espera/reinyección) es responsabilidad de `WF.onboarding` y del Router.

---

## 7. Normalización hacia el router

Además de `meta.src_ilk` (o `null`), el IO Slack **DEBE** adjuntar el bloque estandarizado `meta.context.io` (contrato IO Context), incluyendo `entrypoint` (workspace), `conversation` (channel/thread) y `reply_target` (channel + thread_ts).

Por cada evento válido (`app_mention`), el Nodo IO Slack **DEBE** construir un
`Message` del protocolo interno con:

### routing
- `routing.dst = null`
- `routing.ttl`: valor estándar del sistema
- `routing.trace_id`: nuevo o propagado

Interpretación operativa:
- `routing.dst = null` implica `resolve` en el router (baseline de producción).
- Un destino unicast fijo solo debe usarse como override de pruebas/controladas.

### meta
- `meta.type = "user"`
- `meta.src_ilk`: ILK representando al usuario de Slack (humano externo) **o null**
- `meta.context.io`:
```json
{
  "io": {
    "channel": "slack",
    "entrypoint": { "kind": "slack_workspace", "id": "<team_id>" },
    "sender": { "kind": "slack_user", "id": "<user>" },
    "conversation": { "kind": "slack_channel", "id": "<channel>", "thread_id": "<thread_ts si existe>" },
    "message": { "id": "<event_id|ts>" },
    "reply_target": {
      "kind": "slack_post",
      "address": "<channel>",
      "params": { "thread_ts": "<thread_ts si existe>", "workspace_id": "<team_id>" }
    }
  }
}
```

> Nota: si el Router inyecta `meta.ctx_window`, el IO Slack **DEBE** aceptarlo pero **NUNCA** debe serializarlo/reenviarlo a Slack (es información interna).

### payload
```json
{
  "type": "text",
  "content": "<texto del mensaje sin la mención al bot>",
  "raw": { "slack_event": "..." }
}
```

### 7.1 Regla de offload por tamaño (delegada a SDK `send`)
- `IO.slack` construye payload `text/v1` base (`content` + `attachments` cuando corresponda).
- La decisión de dejar inline o convertir a `content_ref` por límite de tamaño **la toma `fluxbee_sdk::NodeSender::send`**.
- Configuración relevante del enforcement SDK:
  - defaults internos del SDK (`max_message_bytes=64KB`, `message_overhead_bytes=2048`).
- Objetivo: evitar divergencias entre adapters IO y mantener un comportamiento único para `text/v1`.

Nota operativa:
- `io.blob.*` en control-plane del nodo puede seguir siendo útil para validaciones/políticas del adapter, pero no gobierna por sí solo el auto-offload en `NodeSender::send`.

### 7.2 Estado de validación y límite del canal Slack
- El mecanismo canónico de offload (`content` -> `content_ref` al superar límite) se valida de forma determinística con `io-sim`, porque no depende de límites del proveedor externo.
- En `IO.slack` se valida integración end-to-end del adapter (ingreso/salida, adjuntos, contrato `text/v1`), pero no es un canal confiable para probar payloads de texto extremos.
- Referencia oficial Slack (`chat.postMessage`): Slack recomienda textos cortos y **trunca mensajes por encima de 40.000 caracteres**.
  - Fuente: https://docs.slack.dev/reference/methods/chat.postMessage/ (sección "Truncating content").

---

## 8. Outbound: envío de mensajes a Slack

### 8.1 API utilizada
- Slack Web API: `chat.postMessage`
- Token requerido: **Bot Token** (`xoxb-...`)
- Scope mínimo: `chat:write`

### 8.2 Resolución de destino

El Nodo IO Slack **DEBE**:
- Enviar el mensaje al `channel` indicado.
- Si `thread_ts` está presente, responder en el mismo hilo.
- Caso contrario, publicar en el canal.

### 8.3 Política de contenido largo (estado actual)

✅ **Implementado hoy:**
- Si el payload inbound a `io-slack` trae `content_ref` de texto, `io-slack` resuelve el blob y lo envía como texto por `chat.postMessage`.
- Los `attachments[]` del payload se envían como archivos usando el flujo externo de Slack files API.

⚠️ **Limitación conocida (hoy):**
- Para texto muy largo, este comportamiento puede chocar con el límite de `chat.postMessage` (Slack trunca por encima de 40.000 caracteres).
- En consecuencia, si un nodo upstream (por ejemplo AI) devuelve texto muy grande via `content_ref`, hoy `io-slack` puede terminar publicando texto truncado si no viene además como attachment explícito.

🧩 **TBD / a normar para Slack:**
- Definir fallback canónico cuando `resolved.text` excede límite de Slack:
  - subir texto completo como archivo/snippet y no truncar,
  - opcionalmente agregar notice configurable/localizable,
  - o chunking controlado si se adopta esa estrategia.

Referencia oficial de límite:
- https://docs.slack.dev/reference/methods/chat.postMessage/ ("Truncating content").

---

## 9. Outbox e idempotencia outbound

### Modelo MVP (sin DB/Redis)
- Outbox/cola en memoria con estados:
  - `PENDING`, `SENDING`, `SENT`, `FAILED`, `DEAD`
- Reintentos con backoff y límite configurable.
- Límites obligatorios: `max_pending`, `max_inflight`, `max_age_ms`.

### Idempotencia (Slack)
Para el MVP:
- La idempotencia outbound se garantiza por tracking en memoria del outbox.
- No se debe reenviar un mensaje marcado como `SENT` mientras el proceso esté vivo.
- En caso de duda (timeout sin respuesta), se reintenta respetando backoff.

> Nota: tras restart, se pierde el outbox y puede haber pérdida de outbound no enviado aún.

---

## 10. Manejo de errores específicos de Slack

Errores comunes a manejar:
- `rate_limited`
- `channel_not_found`
- `not_in_channel`
- `invalid_auth` / `token_revoked`

Reglas:
- Errores de rate limit -> retry con backoff.
- Errores de permisos/configuración -> `DEAD` + auditoría.

---

## 11. Configuración mínima requerida

- `SLACK_APP_TOKEN` (xapp)
- `SLACK_BOT_TOKEN` (xoxb)
- `SLACK_SIGNING_SECRET` (si se usa Events API)

Modo spawn/orchestrator (compliance path):
- El nodo puede leer configuración desde `config.json` de spawn:
  - ruta canónica: `/var/lib/fluxbee/nodes/IO/<node_name>/config.json`
- Tokens soportados en config:
  - `slack.app_token`, `slack.bot_token`
  - `slack.app_token_ref`, `slack.bot_token_ref` (`env:VAR`)
- Precedencia:
  - env explícito (`SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN`) sobre config de spawn.
- Contrato de spawn activo:
  - requiere `FLUXBEE_NODE_NAME` inyectado por orchestrator
  - sin fallback legacy de `NODE_NAME`/`ISLAND_ID`
  - sin path override legacy de config de spawn

## 12. Gaps conocidos (estado actual)

- Frontera core vs node control-plane:
  - `PUT .../config` (core) mantiene `config.json` administrado por orchestrator.
  - hot reload runtime/business de `IO.slack` se aplica por `CONFIG_SET` (control-plane del nodo).
  - `CONFIG_CHANGED` se trata como señal informativa de infraestructura; no reemplaza `CONFIG_SET` en v1.

- Fallback de configuración y resiliencia de proceso:
  - hoy, si faltan credenciales (`slack.app_token`/`slack.bot_token`) el proceso termina con `exit 1`.
  - comportamiento esperado (pendiente): no crash; reportar estado/config error y seguir atendiendo control-plane.

- Bloqueante operativo (prioridad alta):
  - hoy el runtime trata falta de config/secrets como fatal de arranque (`exit 1`), lo que provoca restart loop en systemd.
  - responsabilidad de `IO.slack`: este caso debe pasar a modo degradado/no-ready, sin terminar proceso.
  - estado esperado (pendiente): levantar el runtime, publicar estado de error de config y permanecer controlable por orchestrator/admin.



### Aclaración - Identidad L3 (v1.16+)

El Nodo IO Slack no implementa un protocolo de sesión con Identity.
Usa lookup por SHM + provision on miss en el pipeline compartido de `io-common`.

La resolución primaria es por SHM (`jsr-identity-*`).
Cuando hay miss/error, el provisioner puede enviar `ILK_PROVISION` a `SY.identity`.

Si no se obtiene `src_ilk` (fallo/timeout), el IO **DEBE** enviar al Router con
`meta.src_ilk = null` para degradación controlada/onboarding.

Esta definición reemplaza cualquier modo legacy o lógica de identidad ad-hoc en el nodo Slack.



### Aclaración - meta.ctx vs meta.context

Según la especificación del Router v1.16:

- `meta.ctx` representa el **Contexto Conversacional (CTX)**
- `meta.context` se reserva exclusivamente para **datos adicionales evaluados por OPA**

`meta.context` **NO** debe usarse para almacenar historia, turns ni contexto conversacional.



## Retries y deduplicación

✅ **NORMATIVO (HOY, Socket Mode)**:
- La deduplicación se hace por un `message_id` estable (preferentemente `event_id`; fallback `ts`) dentro de la ventana del sessionizer.
- No se depende de headers `X-Slack-Retry-*` (propios de HTTP Events API), dado que el modo activo es Socket Mode.

🧩 **A ESPECIFICAR**:
- Si a futuro se habilita ingestión por HTTP Events API, se deberá incorporar manejo explícito de `X-Slack-Retry-*` y una política de dedupe consistente.



## Configuración en caliente (sin reinicio)

✅ **NORMATIVO**:
- El nodo Slack **debe** poder actualizar credenciales/config sin depender de un restart del orchestrator, usando Control Plane (`CONFIG_SET`).
- Precedencia sugerida:
  1) Override en memoria recibido por `CONFIG_SET` (si existe)
  2) Config local (YAML/env)
- En reinicio, el override en memoria se pierde; si no hay config local, el nodo puede quedar `FAILED_CONFIG (missing_secret)` hasta recibir `CONFIG_SET` nuevamente.




