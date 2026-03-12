# Nodo IO â€“ Slack (EspecificaciÃ³n MVP)
> Adaptation note for `json-router`:
> This spec is imported and kept as functional baseline.
> If a section conflicts with router v1.16+ docs in this repo, follow `docs/io/README.md` precedence.

## 1. Objetivo

El **Nodo IO Slack** es el adaptador entre Slack y el sistema interno (router + nodos).
Para el MVP, su objetivo es:

- **Inbound:** recibir mensajes Ãºnicamente a travÃ©s de **menciones al bot** (`app_mention`).
- **Outbound:** enviar mensajes a Slack usando la **Slack Web API** (`chat.postMessage`).

Este documento **extiende** la especificaciÃ³n base de Nodos IO y define Ãºnicamente
las decisiones y necesidades **especÃ­ficas de Slack**.

---

## 2. Supuestos del MVP

- Existe **una sola instancia activa** del Nodo IO Slack por app/workspace.
- Se acepta downtime si el nodo cae.
- Los Nodos IO **NO** manejan base de datos ni Redis; por lo tanto:
  - deduplicaciÃ³n y sessionizaciÃ³n son **en memoria** (*best-effort*),
  - retries/outbox outbound son **en memoria** (*best-effort*),
  - tras reinicio se pierde el estado tÃ©cnico.

> Nota: para evitar inconsistencias con necesidades futuras de durabilidad, la lÃ³gica interna del IO deberÃ­a usar una **interfaz de persistencia** (MVP: InMemory) tal como se define en `io-common.md`.

---

## 3. Inbound: recepciÃ³n de mensajes desde Slack

### 3.1 Mecanismo de recepciÃ³n

El Nodo IO Slack **DEBE** soportar uno de los siguientes modos:

#### OpciÃ³n A â€“ Socket Mode (recomendado para MVP)
- ConexiÃ³n WebSocket usando **App-Level Token** (`xapp-...`).
- Scope requerido: `connections:write`.
- No requiere endpoint HTTP pÃºblico.

#### OpciÃ³n B â€“ Events API (HTTP)
- Endpoint HTTPS pÃºblico.
- Soporte de `url_verification` (responder el `challenge` en plaintext).
- ValidaciÃ³n de firma usando el **Signing Secret** de Slack.

La elecciÃ³n del modo **NO afecta** el contrato interno del IO.

---

### 3.2 Evento soportado: `app_mention`

El Nodo IO Slack **SOLO** procesa eventos:
- `type = "app_mention"`

Campos relevantes del evento Slack:
- `user`: ID del usuario que menciona al bot.
- `channel`: ID del canal donde ocurriÃ³ la menciÃ³n.
- `text`: contenido del mensaje.
- `ts`: timestamp del mensaje.
- `thread_ts`: timestamp del hilo (si aplica).
- `event_id` / `event_ts`: identificadores del evento.

Otros eventos **DEBEN** ser ignorados.

---

## 4. DeduplicaciÃ³n inbound (Slack)

Slack puede reenviar eventos si:
- no recibe ACK,
- el ACK es lento,
- o hay errores transitorios.

### Clave de deduplicaciÃ³n recomendada (en orden de preferencia)
1. `event_id` (si estÃ¡ presente)
2. `(team_id, channel, ts)` como surrogate estable

### ImplementaciÃ³n (sin DB/Redis)
- CachÃ© en memoria con TTL.
- LÃ­mite por tamaÃ±o: `max_entries`/`max_bytes`.
- Eviction (TTL-LRU o drop-oldest).
- MÃ©tricas: dedup_hits, dedup_misses, evictions.

### Regla operativa
- El ACK a Slack **DEBE** enviarse lo antes posible, sin esperar respuesta del router ni de identidad.

> Nota: tras restart del nodo, se pierde el dedup y pueden ocurrir duplicados hacia el router.

---

## 5. SessionizaciÃ³n (Slack)

### Objetivo
Agrupar mÃºltiples mensajes consecutivos del mismo usuario en el mismo canal/hilo
en un Ãºnico turno lÃ³gico.

### Reglas MVP
- **PUEDE** aplicarse sessionizaciÃ³n para Slack.
- Clave sugerida:
```
(channel, user, thread_ts || channel)
```
- Ventana de tiempo configurable (ej: 2â€“5 segundos).
- Si el nodo se reinicia durante la ventana, el MVP **PUEDE** aceptar pÃ©rdida
de agrupamiento (mensajes procesados separados).

---

## 6. Identidad (L3) y despacho al router

### 6.1 Requisito
El Nodo IO Slack **DEBE** intentar resolver `meta.src_ilk` por SHM (`jsr-identity`).

- Si hay hit: incluir `meta.src_ilk`.
- Si hay miss: **enviar igualmente** el mensaje al Router con `meta.src_ilk = null`.

Esto implementa la estrategia recomendada de L3: **DesvÃ­o a Onboarding (Reprocesamiento)**.

### 6.2 IntegraciÃ³n con identidad (no bloqueante)
El Nodo IO Slack **PUEDE** emitir un aviso *fire-and-forget* para registrar/linkear al usuario en `SY.identity`, pero:

- **NO** debe bloquear el ACK,
- **NO** debe retener el mensaje en memoria â€œpendiente de identidadâ€,
- **NO** debe esperar un â€œreadyâ€ de `SY.identity`.

El control de flujo (espera/reinyecciÃ³n) es responsabilidad de `WF.onboarding` y del Router.

---

## 7. NormalizaciÃ³n hacia el router

AdemÃ¡s de `meta.src_ilk` (o `null`), el IO Slack **DEBE** adjuntar el bloque estandarizado `meta.context.io` (contrato IO Context), incluyendo `entrypoint` (workspace), `conversation` (channel/thread) y `reply_target` (channel + thread_ts).

Por cada evento vÃ¡lido (`app_mention`), el Nodo IO Slack **DEBE** construir un
`Message` del protocolo interno con:

### routing
- `routing.dst = null`
- `routing.ttl`: valor estÃ¡ndar del sistema
- `routing.trace_id`: nuevo o propagado

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

> Nota: si el Router inyecta `meta.ctx_window`, el IO Slack **DEBE** aceptarlo pero **NUNCA** debe serializarlo/reenviarlo a Slack (es informaciÃ³n interna).

### payload
```json
{
  "type": "text",
  "content": "<texto del mensaje sin la menciÃ³n al bot>",
  "raw": { "slack_event": "..." }
}
```

---

## 8. Outbound: envÃ­o de mensajes a Slack

### 8.1 API utilizada
- Slack Web API: `chat.postMessage`
- Token requerido: **Bot Token** (`xoxb-...`)
- Scope mÃ­nimo: `chat:write`

### 8.2 ResoluciÃ³n de destino

El Nodo IO Slack **DEBE**:
- Enviar el mensaje al `channel` indicado.
- Si `thread_ts` estÃ¡ presente, responder en el mismo hilo.
- Caso contrario, publicar en el canal.

---

## 9. Outbox e idempotencia outbound

### Modelo MVP (sin DB/Redis)
- Outbox/cola en memoria con estados:
  - `PENDING`, `SENDING`, `SENT`, `FAILED`, `DEAD`
- Reintentos con backoff y lÃ­mite configurable.
- LÃ­mites obligatorios: `max_pending`, `max_inflight`, `max_age_ms`.

### Idempotencia (Slack)
Para el MVP:
- La idempotencia outbound se garantiza por tracking en memoria del outbox.
- No se debe reenviar un mensaje marcado como `SENT` mientras el proceso estÃ© vivo.
- En caso de duda (timeout sin respuesta), se reintenta respetando backoff.

> Nota: tras restart, se pierde el outbox y puede haber pÃ©rdida de outbound no enviado aÃºn.

---

## 10. Manejo de errores especÃ­ficos de Slack

Errores comunes a manejar:
- `rate_limited`
- `channel_not_found`
- `not_in_channel`
- `invalid_auth` / `token_revoked`

Reglas:
- Errores de rate limit â†’ retry con backoff.
- Errores de permisos/configuraciÃ³n â†’ `DEAD` + auditorÃ­a.

---

## 11. ConfiguraciÃ³n mÃ­nima requerida

- `SLACK_APP_TOKEN` (xapp)
- `SLACK_BOT_TOKEN` (xoxb)
- `SLACK_SIGNING_SECRET` (si se usa Events API)



### AclaraciÃ³n â€“ Identidad L3 (v1.16)

El Nodo IO **NO debe esperar ni consumir mensajes sÃ­ncronos de `SY.identity`**.

La resoluciÃ³n de identidad se realiza **exclusivamente mediante lectura de SHM**
(`jsr-identity-*`, vÃ­a `jsr-identity` / `jsr-identity-client`).

Cuando no se puede resolver `src_ilk`, el IO **DEBE enviar el mensaje al Router con**
`meta.src_ilk = null` y permitir que el Router (OPA) desvÃ­e el flujo a onboarding.

Esta definiciÃ³n reemplaza cualquier mecanismo previo de espera, buffer o relay
de identidad en el Nodo IO.



### AclaraciÃ³n â€“ meta.ctx vs meta.context

SegÃºn la especificaciÃ³n del Router v1.16:

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

