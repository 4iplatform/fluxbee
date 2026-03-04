# Lista de tareas - Desarrollo Nodo IO Slack (MVP)

Este documento deriva tareas a partir de:
- `docs/nodos_io_spec.md`
- `docs/io-common.md` (implementado para MVP en `crates/io-common`)
- `docs/io-nodo-slack.md`
- `docs/router-docs/*` (protocolo, framing, `node_client`, identidad L3/ILK y SHM)
- `docs/sy-identity-options.md` (análisis de estrategias; v1.16 recomienda onboarding router-driven)
- `docs/pending-items.md` (histórico; algunas ideas quedaron obsoletas con v1.16)

## Supuestos y restricciones (según specs)

- Este repo ya contiene implementaciones MVP de `router-client`, `router-protocol`, `io-common` e `io-slack`.
- v1.16: la identidad se resuelve **best-effort** por lectura de SHM (`jsr-identity-*`). El IO **NO** espera a `SY.identity` ni retiene mensajes: ante MISS/Unavailable, **forwardea igual** al router con `meta.src_ilk = null` (Router+OPA manejan onboarding/reinyección).
- MVP operativo: single-instance por workspace/destinatarios; sin DB/Redis en el IO (estado técnico en memoria, best-effort). Si aparece contradicción con “outbox durable” del spec general, se resuelve como decisión explícita (ver tareas).

## Entregable principal

- [ ] Implementar y dejar operativo `IO.slack.<identificador>@<isla>` (ver naming en `docs/router-docs/01-arquitectura.md`) con:
  - inbound: `app_mention` → `Message` interno → router
  - outbound: router → `chat.postMessage`
  - identidad: `meta.src_ilk` **best-effort** (hit por SHM; si no se puede resolver, `meta.src_ilk = null` y se forwardea igual)

---

## Milestone 0 - Aterrizaje y decisiones base

- [x] Confirmar stack Rust y estructura del repo (workspace con crates `io-slack`, `io-common`, `router-client`, `router-protocol`).
- [x] Definir naming L2 del nodo: `IO.slack.<team_id>@<isla>` donde `<team_id>` es el “workspace id” de Slack (campo `team_id` en Events API / Socket Mode). Si se soporta Enterprise Grid multi-workspace, revisar si conviene `enterprise_id` o `enterprise_id.team_id`.
- [x] Definir modo de recepción Slack para MVP:
  - [x] Socket Mode para inbound (recomendado en `docs/io-nodo-slack.md`)
  - [ ] Events API HTTP (post-MVP / alternativa)
- [x] Decidir “durabilidad” mínima para estado técnico: **solo memoria** (sin DB/Redis y sin persistencia a disco en el IO).
  - [x] Estado en memoria (se pierde en restart): dedup inbound, session buffer, outbox outbound, idempotency keys, pending request/response (trace_id).
  - [x] UUID del nodo: se provee por config/env (no se persiste a disco). Implicancia: si cambia entre reinicios, el router lo verá como “otro nodo”.
- [x] Definir contrato de mensajes outbound hacia Slack (qué espera recibir el IO desde el router): campos mínimos y dónde viven.
  - [x] Transporte: inbound por Socket Mode; outbound por Slack Web API HTTP (`chat.postMessage`).
  - [x] Destino (delivery) en `meta.context.io (TENTATIVO).reply_target` (contrato IO Context): `kind="slack_post"`, `address="<slack_channel_id>"`, `params.thread_ts?`, `params.workspace_id="<team_id>"`.
  - [x] Contexto IO mínimo también en `meta.context.io (TENTATIVO)`: `channel="slack"`, `entrypoint/slack_workspace`, `sender/slack_user`, `conversation/slack_channel`, `message.id`.
  - [x] Contenido en `payload`: `type="text"`, `content="<texto>"` (requerido), `blocks=[...]` (opcional, passthrough a Slack si viene).

## Milestone 1 - Conectividad al router (node_client en Rust)

Basado en `docs/router-docs/02-protocolo.md`:
framing length-prefix (u32 big-endian) + handshake HELLO/ANNOUNCE + reconexión.

- [x] Implementar framing:
  - [x] `writeFrame(msg)` (JSON → bytes, header u32be + payload) (`crates/router-client/src/framing.rs:18`)
  - [x] `readFrames()` (buffer incremental → parse frames → JSON) (`crates/router-client/src/framing.rs:43`)
  - [x] Límite: rechazar/defender mensajes > 64KB inline (`crates/router-client/src/framing.rs:9`)
- [x] Implementar handshake:
  - [x] Enviar `HELLO` (`meta.type="system"`, `meta.msg="HELLO"`, `routing.ttl=1`) (`crates/router-client/src/handshake.rs:35`).
  - [x] Esperar `ANNOUNCE` y parsear `vpn_id` / `router_name` (`crates/router-client/src/handshake.rs:70`).
- [x] Implementar modelo split sender/receiver:
  - [x] TX queue con capacidad fija (configurable; usar 256) y backpressure (`crates/router-client/src/lib.rs:148`).
  - [x] RX queue con capacidad fija (configurable; usar 256) (`crates/router-client/src/lib.rs:149`).
  - [x] Loop RX (socket → queue) y loop TX (queue → socket) (`crates/router-client/src/lib.rs:310`).
- [x] Implementar reconexión automática con backoff, y el comportamiento requerido:
  - [x] Al reconectar, vaciar TX queue (como el spec) y exponer señal de desconexión al nodo (`NodeReceiver.is_connected()` / `NodeSender.send()` falla) (`crates/router-client/src/lib.rs:91`, `crates/router-client/src/lib.rs:44`, `crates/router-client/src/lib.rs:350`).
- [x] Definir UUID del nodo (sin persistencia a disco):
  - [x] Cargar `node_uuid` desde config/env al iniciar (ejemplo: `crates/router-client/examples/smoke.rs:1`).
  - [x] Documentar la implicancia operativa si el UUID cambia (nuevo “nodo” para el router).
- [x] Implementar `WITHDRAW` en shutdown limpio (`crates/router-client/src/lib.rs:51`, `crates/router-client/src/lib.rs:357`).
- [x] Agregar “smoke test” local (ejemplo): `crates/router-client/examples/smoke.rs:1`.

## Milestone 1.1 - Harness local (sin router real) (opcional)

Para probar `io-slack` sin depender del router real y sin interferir con su desarrollo.

- [x] Implementar un `router-stub` local (HELLO/ANNOUNCE + loopback echo) en `crates/router-stub/src/main.rs:1`.

## Milestone 2 - `io-common` (subconjunto MVP)

Basado en `docs/io-common.md` + `docs/nodos_io_spec.md`. Objetivo: reutilizar en futuros IOs y encapsular lo repetible.

### 2.1 Inbound reliability (MVP)
- [ ] Implementar deduplicación inbound en memoria:
  - [x] TTL configurable + límites (por entries; `max_bytes` post-MVP) + política de eviction (`crates/io-common/src/reliability.rs:1`).
  - [ ] Métricas: `dedup_hits`, `dedup_misses`, `evictions`, `drops`.
- [ ] Implementar patrón “ACK rápido” (el adaptador del canal devuelve ACK sin esperar router/identidad).
- [x] ~~Implementar buffer acotado “pendiente de identidad” (`crates/io-common/src/pending_identity.rs:1`)~~ (obsoleto v1.16: el IO no retiene mensajes por identidad).
- [ ] (v1.16) En MISS/Unavailable de identidad: forward al router con `meta.src_ilk = null` (sin buffer) + métrica/log `identity_miss`.

- [x] Mover orquestación inbound (dedup + identidad + buffer) a `io-common` con `InboundProcessor` (`crates/io-common/src/inbound.rs:1`).

### 2.2 Identidad (lookup-only + SHM)
- [ ] Definir interfaz `IdentityResolver` (consumida por IO Slack) como **lookup-only**:
  - [ ] `lookup(channel_type, external_id) -> Option<src_ilk>` (best-effort; no debe bloquear ni retener mensajes).
- [ ] Implementar `ShmIdentityResolver` (read-only) para `jsr-identity-<island>`:
  - [ ] resolver `(channel_type, external_id) -> ich -> ilk` según `docs/router-docs/03-shm.md` + `docs/router-docs/11-context.md`.
  - [ ] si SHM no está disponible: devolver `None` (no error hard).
- [ ] Mantener `MockIdentityResolver` / `DisabledIdentityResolver` solo para dev:
  - [ ] `mock`: simula SHM con hits determinísticos o tabla en memoria.
  - [ ] `disabled`: siempre `None` (permite testear onboarding desde router con `src_ilk=null`).

### 2.2.1 IO Context (meta.context.io (TENTATIVO))
- [ ] Implementar utilidades para construir/validar `meta.context.io (TENTATIVO)` (contrato IO Context):
  - [x] Helper Slack inbound `slack_inbound_io_context(...)` (`crates/io-common/src/io_context.rs:1`)
  - [x] `extract_reply_target(...)` / `extract_slack_post_target(...)` (`crates/io-common/src/io_context.rs:1`)
  - [ ] helpers para propagar `meta.context.io (TENTATIVO)` inbound → outbound

### 2.X Helpers de mensaje (io-common)
- [ ] Centralizar construcción de `Message` para IO:
  - [x] Builder inbound user → router (`crates/io-common/src/router_message.rs:1`)

### 2.3 Outbound reliability (MVP)
- [ ] Implementar outbox en memoria (best-effort) con:
  - [ ] estados mínimos `PENDING/SENDING/SENT/FAILED/DEAD`
  - [ ] retry con backoff + clasificación de errores (retryable / non-retryable / rate_limited / auth_error)
  - [ ] límites: `max_pending`, `max_inflight`, `max_age_ms`
- [ ] Definir claramente qué se considera “idempotencia” por proceso (clave interna) para Slack outbound.

### 2.4 Lifecycle + observabilidad (base común)
- [ ] Implementar estados `STARTING/READY/DRAINING/STOPPED` y manejo de SIGTERM.
- [ ] Estandarizar logs estructurados (incluyendo `trace_id`).
- [ ] Exponer métricas estándar (al menos counters/gauges básicos; definir formato/exporter).

### 2.5 Nota de durabilidad (para evitar confusiones)
- [ ] Documentar explícitamente que el MVP es **best-effort por proceso**: no hay garantía de dedup/outbox/idempotencia tras restart.
- [ ] Dejar hooks claros para post-MVP: backend de storage opcional (Redis/Postgres) fuera del IO o en otro componente, sin cambiar la API pública.

## Milestone 3 - IO Slack inbound (Slack → router)

Basado en `docs/io-nodo-slack.md`.

- [ ] Implementar receptor Slack:
  - [x] Socket Mode: conexión WS con `xapp-*` (`crates/io-slack/src/main.rs:1`).
  - [ ] (Alternativa) Events API HTTP: `url_verification` + validación de firma con Signing Secret.
- [x] Filtrar únicamente eventos `type="app_mention"`; ignorar el resto (`crates/io-slack/src/main.rs:1`).
- [x] ACK inmediato a Slack (sin esperar router/identidad) (`crates/io-slack/src/main.rs:1`).
- [ ] Implementar dedup Slack:
  - [x] key preferida: `event_id`; fallback `(team_id, channel, ts)` (`crates/io-slack/src/main.rs:1`, `crates/io-common/src/reliability.rs:1`).
- [ ] (Opcional) Implementar sessionización Slack:
  - [ ] clave sugerida: `(channel, user, thread_ts || channel)`
  - [ ] ventana configurable (2–5s).
- [ ] Normalizar a `Message` interno:
  - [x] `routing.dst = null`, `routing.ttl` estándar, `routing.trace_id` nuevo/propagado (`crates/io-slack/src/main.rs:1`).
  - [x] `meta.type = "user"` (`crates/io-slack/src/main.rs:1`).
  - [ ] Resolver `meta.src_ilk` best-effort por SHM:
    - `channel="slack"`
    - `external_id = <slack user id>`
    - si no se resuelve: enviar igual con `meta.src_ilk = null`
  - [x] Poblar `meta.context.io (TENTATIVO)` (contrato IO Context) con:
    - `channel="slack"`
    - `entrypoint: { kind: "slack_workspace", id: "<team_id>" }`
    - `sender: { kind: "slack_user", id: "<user>" }`
    - `conversation: { kind: "slack_channel", id: "<channel>", thread_id: "<thread_ts?>" }`
    - `message: { id: "<event_id|ts>" }`
    - `reply_target: { kind: "slack_post", address: "<channel>", params: { thread_ts: "<thread_ts?>", workspace_id: "<team_id>" } }`
  - [x] `payload.type="text"`, `payload.content` sin la mención al bot, `payload.raw` con evento Slack (`crates/io-slack/src/main.rs:1`).
- [x] Enviar el `Message` al router usando el `node_client` (`crates/io-slack/src/main.rs:1`).

## Milestone 4 - IO Slack outbound (router → Slack)

- [x] Definir/implementar parser del mensaje outbound desde router (qué campos vive dónde) (`crates/io-slack/src/main.rs:1`).
- [ ] Implementar envío Slack Web API:
  - [x] `chat.postMessage` con `xoxb-*`, scope `chat:write` (`crates/io-slack/src/main.rs:1`).
  - [x] Resolver destino:
    - [x] `channel` desde `meta.context.io (TENTATIVO).reply_target.address` (`crates/io-slack/src/main.rs:1`)
    - [x] `thread_ts` desde `meta.context.io (TENTATIVO).reply_target.params.thread_ts` (si existe) (`crates/io-slack/src/main.rs:1`)
- [ ] Integrar outbox/retry/backoff:
  - [ ] manejar `rate_limited` (retry), `invalid_auth/token_revoked` (DEAD), `not_in_channel`, `channel_not_found`.
- [ ] Métricas/logs de entrega: `outbound_sent`, `outbound_failed`, latencias, tamaño de outbox.

## Milestone 5 - Config, operación y calidad

- [ ] Configuración mínima (env/config file):
  - [ ] Slack: `SLACK_APP_TOKEN`, `SLACK_BOT_TOKEN`, `SLACK_SIGNING_SECRET` (si HTTP)
  - [ ] Router: endpoint/socket, `island_id`, `node_name`, `ttl_default`
  - [ ] Límites: dedup TTL, session window, outbox backoff, caps de colas
  - [ ] Identidad (v1.16): nombre SHM `jsr-identity-<island>` (derivado de `island_id`) + `IDENTITY_MODE=shm|mock|disabled`
- [ ] (Opcional) Soportar `CONFIG_CHANGED` / responder `CONFIG_RESPONSE` para un `subsystem` `io.slack` si se define (ver `docs/nodos_io_spec.md` + `docs/router-docs/02-protocolo.md`).
- [ ] Healthchecks:
  - [ ] estado conexión router (`is_connected`)
  - [ ] estado conexión Slack (socket/http)
  - [ ] estado `READY/DRAINING`
- [ ] Tests (mínimos recomendados):
  - [ ] framing (read/write, chunking, multi-frame)
  - [ ] dedup TTL/eviction
  - [ ] identidad: MISS/Unavailable no bloquea (forward con `src_ilk=null`)
  - [ ] outbox retry/backoff + clasificación de errores
- [ ] Documentación operativa:
  - [ ] cómo crear Slack App (eventos, scopes, tokens)
  - [ ] cómo configurar y ejecutar el nodo
  - [ ] troubleshooting (rate limits, auth, reconexión router)

---

## Dudas abiertas (para no bloquear el desarrollo)

Estas dudas impactan directamente el formato de external_id/SHM y la integración con onboarding (Router/OPA).

- [ ] Formato final de `external_id` en Slack (solo `user` vs `team_id:user`).
- [ ] Si el IO debe emitir algún aviso *fire-and-forget* para registro/link (o si se elimina por completo esa responsabilidad del IO).
- [ ] Reinyección por onboarding: conservación de `trace_id` y campos mínimos que deben sobrevivir.
