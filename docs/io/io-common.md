# io-common - Especificación técnica
> Adaptation note for `json-router`:
> In this repo, identity flow follows Router-driven onboarding (v1.16+):
> IO performs `lookup -> provision_on_miss -> forward`.
> On provision failure/timeout, it forwards with `meta.src_ilk = null` (degraded fallback).
> Any older wording suggesting synchronous wait/buffering for identity should be treated as superseded.

> Operational note:
> If lookup logs show `EACCES` (permission denied) when reading identity SHM, this is an environment permissions issue.
> See `docs/io/README.md` ("Known Issue / Troubleshooting") for diagnosis and operational fix.



## Control Plane unificado (IO Nodes ↔ AI Nodes)

✅ **NORMATIVO**:
- IO Nodes adoptan el mismo patrón de Control Plane que AI Nodes:
  - `PING/STATUS` para observabilidad mínima,
  - `CONFIG_GET/CONFIG_SET/CONFIG_RESPONSE` para gestión de configuración en caliente.
- Los mensajes de Control Plane se distinguen por `meta.type in {"system","admin"}` + `meta.msg`.

🧩 **A ESPECIFICAR**:
- Catálogo global de comandos system/admin (cuando Fluxbee lo formalice en protocolo/base).



## Adaptación temporal al core/SDK (campos L3 e identidad)

> ✅ **Canónico (protocolo Fluxbee):** existen campos L3 dedicados para identidad y contexto conversacional (por ejemplo `src_ilk/dst_ilk`, `ich/ctx`, `ctx_seq`, `ctx_window`).  
> ⚠️ **Estado actual (core/SDK):** el SDK no expone todos los campos L3 dedicados, pero `src_ilk` sí se transporta canónicamente en `meta.src_ilk`. `meta.context` queda para metadata adicional.

✅ **NORMATIVO (HOY, implementación compatible):**
- IO Nodes **MUST** usar `meta.src_ilk` como carrier canónico de identidad de origen.
- La metadata específica del canal (Slack/WhatsApp/Email) debe vivir bajo `meta.context.io.*`.
- Mientras dure la transición, campos L3 no tipados (por ejemplo `dst_ilk`) pueden transportarse en `meta.context`.

⚠️ **ADVERTENCIA:**
- El uso de `meta.context` como carrier es temporal para campos no tipados. `meta.src_ilk` ya es canónico y no debe duplicarse en `meta.context`.

🧩 **Estado actual del core:**
- `ctx_seq/ctx_window` no forman parte del happy path canónico del repo.
- si aparecen en payloads viejos, deben tratarse como metadata legacy/opaca.



## 0. Objetivo

`io-common` es una librería (o conjunto de librerías por lenguaje) que encapsula la
lógica **crítica y repetible** de los Nodos IO.

Su objetivo es:
- evitar duplicación de lógica entre IOs,
- reducir errores en deduplicación, retries e idempotencia,
- estandarizar operación, observabilidad y lifecycle,
- permitir evolucionar de un MVP single-instance a una arquitectura escalable.

`io-common` **no** es consciente del canal (Slack, WhatsApp, email, etc.).
Los detalles específicos quedan en cada Nodo IO.

---

## 1. Principios de diseño

- **Agnóstico de canal**: no conoce APIs externas.
- **Sin persistencia local**: `io-common` **NO** asume DB/Redis dentro de los IO. Todo estado técnico es **en memoria**.
- **Best-effort por proceso**: deduplicación, sessionización y reintentos funcionan mientras el proceso está vivo; tras restart pueden perderse.
- **Compatible con el protocolo**: no asume campos fuera de `routing/meta/payload`.
- **Acotado y observable**: todo buffer/caché debe tener TTL, límites y métricas para evitar crecimiento sin control.
- **Evolutivo**: el diseño deja ganchos para incorporar persistencia (DB/Redis) fuera del IO si el sistema lo habilita en el futuro.

---

## 2. Responsabilidades de io-common

### 2.1 Inbound reliability
- Deduplicación inbound **en memoria** con TTL y límites (cap por entries/bytes).
- Helpers para patrón "ACK rápido":
  - el ACK al proveedor **no** espera respuesta del router,
  - el procesamiento downstream ocurre asincrónicamente.
- Cola/buffer **acotado** de mensajes "pendientes" (p.ej. esperando identidad) con timeouts.

### 2.1.1 Normalización `text/v1` + offload a blob (canónico IO)
- El punto de enforcement de offload `text/v1` (inline -> `content_ref`) está en `fluxbee_sdk::NodeSender::send`.
- `io-common` **DEBE** delegar esa decisión al SDK y **NO** duplicar lógica de offload por tamaño.
- Los adapters IO **NO** deben reimplementar la decisión `content` vs `content_ref`; solo deben mapear evento externo -> payload base + adjuntos.
- Las validaciones comunes de `text/v1` (shape, límites, conteo, resolución de blobs) viven en `io-common`.
- La policy efectiva de MIME aceptados debe ser definida por cada adapter IO según sus capacidades y el canal externo, y `io-common` debe aplicarla de forma uniforme.
- Un default de `io-common` puede servir como baseline técnico, pero no debe reemplazar una decisión explícita del adapter cuando el canal soporta más o menos tipos.

Regla operativa para MIME:
- si Fluxbee puede transportar un attachment y el canal/adapter tiene capacidad razonable de aceptarlo, el adapter debería incluir ese MIME en su policy efectiva
- si un MIME queda afuera, debe ser por limitación concreta del canal/adaptation o por decisión explícita documentada, no por un default implícito accidental

Regla corta de adopcion para futuros `IO.*`:
- si el adapter produce mensajes de usuario textuales hacia el router, debe usar el contrato canonico `text/v1`
- esos mensajes deben enviarse al router por `fluxbee_sdk::NodeSender::send`
- el adapter no debe reimplementar por su cuenta la decision `content` vs `content_ref`
- si el adapter necesita otro tipo de payload que no sea `text/v1`, no hereda automaticamente el auto-offload y debe definirse explicitamente su contrato/tamano antes de asumir comportamiento equivalente

Configuración operativa actual del enforcement en SDK:
- La normalización en `NodeSender::send` usa defaults internos del SDK.
- No depende de variables de entorno para decidir offload en esta etapa.

Logging operativo (bajo ruido):
- `debug`: decisión de normalización (`offload_to_blob`, bytes estimados, límite, adjuntos).
- `info` solo cuando hubo offload a blob.
- `warn` en error de normalización con código canónico.
- Nunca loggear contenido de usuario ni bytes de blobs.

Validación recomendada:
- Prueba determinística de payloads grandes (`>64KB` default) con `io-sim`.
- En `io-slack`, usar pruebas de integración de adapter y contrato, considerando límites propios del canal.

### 2.2 Outbound reliability
- Outbox **en memoria** (best-effort) con:
  - cola acotada,
  - reintentos con backoff,
  - timeouts y estados mínimos (`PENDING/SENDING/SENT/FAILED/DEAD`) solo para operación interna.
- Idempotencia outbound **por proceso** mediante claves internas en memoria (si se requiere).
- Worker/dispatcher en memoria para procesar el outbox.
- **Nota**: al no haber persistencia, tras restart puede perderse el outbox y, por ende, mensajes outbound que no hayan sido enviados todavía.

### 2.2.2 Política de salida a canal externo (contenido grande)

✅ **NORMATIVO (baseline IO):**
- Los adapters IO **DEBEN** respetar límites del canal externo destino.
- Los adapters IO **NO DEBEN** truncar contenido de forma silenciosa.
- Frente a límite de canal, el adapter **DEBE** usar una estrategia explícita de entrega alternativa (por ejemplo archivo/adjunto/chunking), y dejar trazabilidad en logs/métricas.

🧩 **TBD por canal:**
- Cada canal define estrategia concreta de fallback para payloads largos:
  - `text` directo,
  - archivo/adjunto,
  - chunking en múltiples mensajes,
  - híbrido (resumen + adjunto).

✅ **NORMATIVO UX (multilenguaje):**
- No usar notices hardcodeados en un idioma fijo.
- Si se envía mensaje de aviso por fallback, debe ser configurable/localizable por nodo.
- Default recomendado cuando no hay localización: entregar contenido completo sin texto de aviso fijo.

### 2.2.1 Identity Resolution Client (SY.identity)

`io-common` **DEBE** proveer identidad como pipeline compartido para todos los IO:

1. `lookup(channel, external_id)` sobre SHM de identidad (`jsr-identity-<island>`).
2. Si lookup da miss/error y `provision_on_miss=true`, llamar `ILK_PROVISION` contra `SY.identity`.
3. Si provision devuelve ILK, forward con `meta.src_ilk=<ilk>`.
4. Si provision falla o timeout, forward con `meta.src_ilk=null` (degraded fallback).

Configuración operativa:
- `ISLAND_ID`
- `IDENTITY_TARGET` (default `SY.identity@<ISLAND_ID>`)
- `IDENTITY_TIMEOUT_MS`

Propiedades requeridas:
- no bloquea ACK al proveedor externo
- no mantiene sesiones sin límite esperando identidad
- logs/counters para hit/miss/error/provision/fallback

### 2.3 Sessionización (opcional)
- Agrupamiento de fragmentos en turnos.
- Ventanas configurables.
- Implementación local (MVP) o durable (post-MVP).

### 2.4 Retry y errores
- Clasificación de errores (retryable / non-retryable / rate_limited).
- Circuit breaker opcional.
- Políticas configurables por IO.

### 2.5 Lifecycle
- Estados: STARTING / READY / DRAINING / STOPPED.
- Manejo de SIGTERM.
- Flush de outbox en shutdown.

### 2.6 Observabilidad
- Logs estructurados comunes.
- Métricas estándar.
- Propagación de `trace_id`.

---

## 3. Capa 3: Identidad (IdentityResolver)

Para garantizar coherencia entre adaptadores IO, `io-common` expone un contrato
de identidad centrado en inbound:
- resolver `src_ilk` por `(channel, external_id)` cuando sea posible,
- provisionar en miss si está habilitado,
- degradar a `src_ilk=null` cuando no se logra resolver.

### 3.1 Interfaz del Resolver (Contrato)
El contrato operativo en runtime está compuesto por:

- `IdentityResolver::lookup(channel, external_id) -> Option<src_ilk>`
- `IdentityProvisioner::provision(input) -> Option<src_ilk>`
- `InboundProcessor` como orquestador del flujo (lookup/provision/fallback)

### 3.2 Flujo Inbound (Canal -> Router)
Cada vez que el Nodo IO recibe un mensaje o evento desde el exterior:
1. **Extracción:** El Nodo IO extrae el ID del proveedor (ej: `U12345`).
2. **Lookup:** `lookup(channel, external_id)`.
3. **Provision on miss (opcional por config):** si hay miss/error, intentar `ILK_PROVISION`.
4. **Forward al Router:** con `meta.src_ilk=<ilk>` si hubo éxito, o `null` en fallback.

### 3.3 Outbound
La resolución de destino outbound es por `meta.context.io.reply_target` (contrato IO Context).
No depende de un resolver de identidad adicional en `io-common`.

---

## Fuera de alcance de io-common

- Validación de firmas de proveedores.
- Autenticación OAuth.
- Extracción de IDs específicos de canal.
- Decisiones de routing u OPA.
- Gestión de secretos.

---

## 4. Arquitectura interna

### 4.1 Interfaces principales

**InboundAdapter (por canal)**
- parse_event(raw) -> InboundEvent
- dedup_key(event) -> DedupKey
- session_key(event) -> SessionKey?
- to_internal_message(event|turn) -> Message

**OutboundAdapter (por canal)**
- build_outbound(message) -> OutboxItem
- send(outbox_item) -> SendResult

**ErrorClassifier**
- classify(error) -> retryable | non_retryable | rate_limited | auth_error

**Storage**
- dedup_store
- outbox_store
- session_store (opcional)

---

---

## 4.X Contrato de metadata IO (meta.context.io (TENTATIVO))

`io-common` **DEBE** proveer utilidades para construir y validar el bloque estandarizado
`meta.context.io (TENTATIVO)` (ver también la spec de Nodos IO). El objetivo es que todos los IO produzcan/consuman
una metadata homogénea, minimizando excepciones por canal.

Elementos clave:
- `entrypoint`: identidad/cuenta del negocio (p.ej. número A vs B).
- `reply_target`: parámetros mínimos para responder (contrato clave).
- `message.id`: ID externo para dedup best-effort.
- `conversation`: identificador externo (y thread si aplica).

`io-common` **PUEDE** proveer helpers tipo:
- `build_io_context_inbound(...)`
- `extract_reply_target(outbound_message)`
- `propagate_io_context(inbound -> outbound)`

> Nota: el contenido exacto por canal se modela vía `reply_target.kind` y `reply_target.params`.


## 5. Estado y almacenamiento (dentro del IO)

Bajo las directivas actuales, los Nodos IO **NO** manejan base de datos ni Redis.
Por lo tanto, `io-common` implementa almacenamiento **en memoria** para:

- `dedup_cache` (TTL + cap)
- `session_buffer` (ventanas + caps)
- `outbox_queue` (cola + caps)

### 5.1 Reglas obligatorias de acotamiento

`io-common` **DEBE** exponer configuración para limitar memoria y evitar crecimiento sin control:

- `dedup.ttl_ms`, `dedup.max_entries`, `dedup.max_bytes`
- `session.window_ms`, `session.max_sessions`, `session.max_fragments`, `session.max_bytes`
- `outbox.max_pending`, `outbox.max_inflight`, `outbox.max_age_ms`
- políticas de expulsión (eviction) y flush forzado

`io-common` **DEBE** exponer métricas por:
- evictions,
- drops (por límites),
- size actual de caches/colas,
- timeouts.

---

## 6. Opciones futuras: persistencia y aceleración fuera del IO

Si en el futuro el sistema permite persistencia para IO, hay dos caminos típicos:

- **Postgres** (fuente de verdad) para dedup/outbox/mapeos (idempotencia) con auditoría.
- **Redis** como acelerador (cache TTL, buffers temporales, rate limiting).

Estas opciones **NO** están habilitadas dentro del IO bajo las directivas actuales, pero el diseño
de `io-common` debe mantener puntos de extensión para incorporarlas (por ejemplo, una interfaz `StorageBackend`).
 (post-MVP)

### 6.1 Usos recomendados
- Cache de dedup (SETNX + TTL).
- Buffer temporal de sessionización.
- Rate limiting distribuido.

### 6.2 No recomendado como fuente de verdad
- Outbox durable.
- Auditoría final.

### 6.3 Modelo híbrido
- Redis fast-path + Postgres truth.
- Fallback a Postgres ante misses.

---

## 7. Decisiones abiertas

### 7.1 DB por IO vs schema por IO
- DB por IO: más aislamiento, más ops.
- Schema por IO: menos ops, menos aislamiento.

### 7.2 Sessionización obligatoria u opcional
- Obligatoria en chat: menos ruido downstream.
- Opcional: MVP más simple.

### 7.3 Estrategia de claiming del outbox
- SELECT FOR UPDATE SKIP LOCKED.
- Leases con lock_until.
- Broker externo.

### 7.4 Idempotencia outbound
- Clave interna.
- Soporte nativo del canal (si existe).

---

## 8. Si no se usa io-common

Cada Nodo IO debe implementar:
- deduplicación durable,
- outbox y retries,
- backoff,
- lifecycle y draining,
- métricas y logging,
- sessionización (si aplica).

Riesgos:
- inconsistencia entre canales,
- bugs de duplicación,
- alta deuda técnica.

Mitigación mínima:
- checklist de conformidad + tests de contrato.

---

## 9. Entregables de io-common

- Migraciones SQL versionadas.
- API pública estable.
- Métricas estándar.
- Documentación de integración.


---

## Anexo A - Esquema sugerido si se habilita Postgres (post-MVP)

> Este anexo es **informativo** y **no aplica** mientras los IO no puedan usar DB/Redis.

### A.1 Tabla `io_dedup` (inbound)

- `dedup_key TEXT PRIMARY KEY`
- `first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()`
- `expires_at TIMESTAMPTZ NOT NULL`
- `meta JSONB NULL`

Operación: `INSERT ... ON CONFLICT DO NOTHING` (conflict => DUPLICATE).

### A.2 Tabla `io_outbox` (outbound)

- `id UUID PRIMARY KEY`
- `state TEXT NOT NULL` (`PENDING/SENDING/SENT/FAILED/DEAD`)
- `attempts INT NOT NULL DEFAULT 0`
- `next_attempt_at TIMESTAMPTZ NOT NULL`
- `locked_by TEXT NULL`
- `lock_until TIMESTAMPTZ NULL`
- `idempotency_key TEXT UNIQUE NOT NULL`
- `destination JSONB NOT NULL`
- `payload JSONB NOT NULL`
- `provider_msg_id TEXT NULL`
- `last_error TEXT NULL`
- `created_at TIMESTAMPTZ NOT NULL DEFAULT now()`
- `updated_at TIMESTAMPTZ NOT NULL DEFAULT now()`

Índices: `(state, next_attempt_at)`, `(lock_until)`.

### A.3 Tabla `io_session_buffer` (sessionización)

- `session_key TEXT PRIMARY KEY`
- `buffer JSONB NOT NULL`
- `window_deadline TIMESTAMPTZ NOT NULL`
- `updated_at TIMESTAMPTZ NOT NULL`


> **Outbox (aclaración):** El outbox de los nodos IO **no es durable**. Su estado vive únicamente en memoria y puede perderse ante reinicios. Sin embargo, **se mantiene el mecanismo ya establecido de TTL, retries y ventana de vida máxima**, tras la cual los mensajes pendientes se descartan.

---

## Aclaraciones v1.16+ - Alineacion con Fluxbee

### Identidad L3

El Nodo IO no debe bloquear ACK inbound esperando identidad.

Pipeline normativo:
1. lookup en SHM (`jsr-identity-<island>`)
2. si hay miss/error: `ILK_PROVISION` contra `SY.identity` (provision_on_miss)
3. si provision falla/timeout: forward degradado con `meta.src_ilk = null`

`src_ilk` debe ir en `meta.src_ilk`; `meta.context` queda para metadata adicional (incluyendo `meta.context.io.*`).
`thread_id` debe ir en `meta.thread_id` (carrier canónico v2).
`meta.context.thread_id` fue removido del pipeline IO y no debe usarse.

---

### Contexto (`ctx_window`)

`ctx_window` lo inyecta Router. IO puede recibirlo, pero no debe reenviarlo al canal externo.

---

### Protocolo (`meta.ctx` vs `meta.context`)

- `meta.ctx`: contexto conversacional
- `meta.context`: metadata adicional (OPA + carrier temporal para campos no tipados)
- IO metadata de canal bajo `meta.context.io.*`
- `thread_id`: solo en `meta.thread_id` (no en `meta.context.thread_id`)

---

### Persistencia en el Nodo IO

El MVP usa memoria para estado tecnico local (dedup/sessions/outbox).
IO no es source of truth; la persistencia canonica permanece en core (Router/Storage/Identity).



