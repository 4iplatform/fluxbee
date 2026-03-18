# io-common â€“ EspecificaciÃ³n tÃ©cnica
> Adaptation note for `json-router`:
> In this repo, identity flow follows Router-driven onboarding (v1.16+):
> IO performs best-effort lookup only; on miss/unavailable it forwards with `meta.src_ilk = null`.
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
> ⚠️ **Estado actual (core/SDK):** el SDK de mensaje aún no expone todos esos campos como propiedades top-level; se dispone de `meta.context` (JSON) como “carrier”.

✅ **NORMATIVO (HOY, implementación compatible):**
- IO Nodes **MUST** usar `meta.context` como carrier temporal para hints de identidad/contexto cuando el core/SDK no provea campos dedicados.
- La metadata específica del canal (Slack/WhatsApp/Email) debe vivir bajo `meta.context.io.*`.
- Mientras dure la transición, es aceptable transportar `src_ilk/dst_ilk` como `meta.context.src_ilk` / `meta.context.dst_ilk` (carrier legacy).

⚠️ **ADVERTENCIA:**
- Este carrier es temporal. Cuando el core/SDK exponga campos dedicados, IO Nodes deben migrar a los campos canónicos y dejar de “polucionar” `meta.context` raíz.

🧩 **A ESPECIFICAR (cuando el core se alinee):**
- Momento exacto de corte (release) donde `ctx_seq/ctx_window` pasan a ser obligatorios en el happy path.



## 0. Objetivo

`io-common` es una librerÃ­a (o conjunto de librerÃ­as por lenguaje) que encapsula la
lÃ³gica **crÃ­tica y repetible** de los Nodos IO.

Su objetivo es:
- evitar duplicaciÃ³n de lÃ³gica entre IOs,
- reducir errores en deduplicaciÃ³n, retries e idempotencia,
- estandarizar operaciÃ³n, observabilidad y lifecycle,
- permitir evolucionar de un MVP single-instance a una arquitectura escalable.

`io-common` **no** es consciente del canal (Slack, WhatsApp, email, etc.).
Los detalles especÃ­ficos quedan en cada Nodo IO.

---

## 1. Principios de diseÃ±o

- **AgnÃ³stico de canal**: no conoce APIs externas.
- **Sin persistencia local**: `io-common` **NO** asume DB/Redis dentro de los IO. Todo estado tÃ©cnico es **en memoria**.
- **Best-effort por proceso**: deduplicaciÃ³n, sessionizaciÃ³n y reintentos funcionan mientras el proceso estÃ¡ vivo; tras restart pueden perderse.
- **Compatible con el protocolo**: no asume campos fuera de `routing/meta/payload`.
- **Acotado y observable**: todo buffer/cachÃ© debe tener TTL, lÃ­mites y mÃ©tricas para evitar crecimiento sin control.
- **Evolutivo**: el diseÃ±o deja ganchos para incorporar persistencia (DB/Redis) fuera del IO si el sistema lo habilita en el futuro.

---

## 2. Responsabilidades de io-common

### 2.1 Inbound reliability
- DeduplicaciÃ³n inbound **en memoria** con TTL y lÃ­mites (cap por entries/bytes).
- Helpers para patrÃ³n â€œACK rÃ¡pidoâ€:
  - el ACK al proveedor **no** espera respuesta del router,
  - el procesamiento downstream ocurre asincrÃ³nicamente.
- Cola/buffer **acotado** de mensajes â€œpendientesâ€ (p.ej. esperando identidad) con timeouts.

### 2.2 Outbound reliability
- Outbox **en memoria** (best-effort) con:
  - cola acotada,
  - reintentos con backoff,
  - timeouts y estados mÃ­nimos (`PENDING/SENDING/SENT/FAILED/DEAD`) solo para operaciÃ³n interna.
- Idempotencia outbound **por proceso** mediante claves internas en memoria (si se requiere).
- Worker/dispatcher en memoria para procesar el outbox.
- **Nota**: al no haber persistencia, tras restart puede perderse el outbox y, por ende, mensajes outbound que no hayan sido enviados todavÃ­a.

### 2.2.1 Identity Resolution Client (SY.identity)

`io-common` **DEBE** proveer un cliente para integrar con `SY.identity` como nodo (no librerÃ­a).

Funciones mÃ­nimas:
- **Lookup local** (rÃ¡pido): lectura de SHM de identidad si estÃ¡ disponible en la isla.
- **Miss handling**: emitir un mensaje **unicast** a `SY.identity@mother` (o al target definido por configuraciÃ³n) para crear/linkear la identidad cuando no exista.
- **No bloquear ACK**: el IO puede ACKear al proveedor y mantener el evento en un buffer acotado hasta resolver `src_ilk`.
- **CorrelaciÃ³n**: usar `routing.trace_id` (o un correlation_id en `meta.context`) para correlacionar request/response.

**Helper normativo (io-common): `resolve_or_create()

> ⚠️ **DEPRECATED**: IO Nodes no deben bloquear ni esperar a `SY.identity`. El modo vigente es lookup-only best-effort por SHM. Si hay miss, forward con `src_ilk=null` y permitir onboarding por router/OPA.`**
- Entrada: `{ channel, external_id, tenant_hint?, attributes? }`
- Salida: `{ src_ilk, tenant_ilk?, created: bool }` o error clasificado.
- SemÃ¡ntica:
  1) intenta `lookup(channel, external_id)` (SHM/local),
  2) si MISS â†’ envÃ­a comando a `SY.identity` para crear/linkear,
  3) reintenta `lookup` hasta timeout,
  4) devuelve `src_ilk`.
- Reglas:
  - **NO** bloquea el ACK al proveedor.
  - Usa un buffer en memoria acotado para eventos pendientes de identidad.
  - Diferencia explÃ­citamente `MISS` vs `ERROR` para decidir si crear o reintentar.

> **Definido (io-common):** `io-common` expone un helper `resolve_or_create()` que implementa el flujo *lookup â†’ (si miss) create/link â†’ re-lookup* sin bloquear el ACK al proveedor. El wire-contract exacto con `SY.identity` (mensajes y respuestas) sigue pendiente (ver secciÃ³n 7.0).

### 2.3 SessionizaciÃ³n (opcional)
- Agrupamiento de fragmentos en turnos.
- Ventanas configurables.
- ImplementaciÃ³n local (MVP) o durable (post-MVP).

### 2.4 Retry y errores
- ClasificaciÃ³n de errores (retryable / non-retryable / rate_limited).
- Circuit breaker opcional.
- PolÃ­ticas configurables por IO.

### 2.5 Lifecycle
- Estados: STARTING / READY / DRAINING / STOPPED.
- Manejo de SIGTERM.
- Flush de outbox en shutdown.

### 2.6 Observabilidad
- Logs estructurados comunes.
- MÃ©tricas estÃ¡ndar.
- PropagaciÃ³n de `trace_id`.

---

## 3. Capa 3: Identidad (IdentityResolver)

Para garantizar la coherencia del sistema y cumplir con la arquitectura de capas (ver Doc 10), el `io-common` integra un mecanismo obligatorio de resoluciÃ³n de identidades. El objetivo es que los nodos internos (AI, WF) nunca operen con IDs propietarios de canales (ej: Slack IDs), sino Ãºnicamente con **ILKs** (`ilk:<uuid>`).

### 3.1 Interfaz del Resolver (Contrato)
El `io-common` define este contrato que el Nodo IO debe implementar al inicializarse. Esta interfaz actÃºa como un puente hacia la librerÃ­a de sistema `SY.identity`.

| MÃ©todo | Entrada | Salida | DescripciÃ³n |
| :--- | :--- | :--- | :--- |
| `resolve_to_ilk` | `provider`, `external_id` | `Result<ILK>` | **Inbound:** Mapea ID de canal a ILK universal. |
| `resolve_to_external` | `ilk`, `provider` | `Result<String>` | **Outbound:** Mapea ILK a ID de canal para entrega. |

### 3.2 Flujo Inbound (Canal â†’ Router)
Cada vez que el Nodo IO recibe un mensaje o evento desde el exterior:
1. **ExtracciÃ³n:** El Nodo IO extrae el ID del proveedor (ej: `U12345`).
2. **ResoluciÃ³n:** Se invoca `resolve_to_ilk("slack", "U12345")`.
3. **ValidaciÃ³n:** Si la resoluciÃ³n falla, el mensaje **no debe entrar al Router**. Se queda en la capa de persistencia del IO para reintento o descarte.
4. **Enriquecimiento:** Si tiene Ã©xito, el `io-common` construye el mensaje inyectando el resultado en `meta.src_ilk`.

### 3.3 Flujo Outbound (Router â†’ Canal)
Cuando el Router entrega un mensaje para ser enviado al exterior:
1. **Captura:** El worker de **Outbox** toma el mensaje que contiene un `meta.dst_ilk`.
2. **TraducciÃ³n:** Se invoca `resolve_to_external(dst_ilk, "slack")`.
3. **PreparaciÃ³n:** El estado del mensaje en el outbox pasa a `READY_TO_SEND` solo si se obtiene un ID externo vÃ¡lido.
4. **Despacho:** El Nodo IO usa ese ID para llamar a la API del canal (ej: `chat.postMessage`).

### 3.4 Manejo de Errores y Estados en Outbox
Para el MVP, se aÃ±ade el estado `RESOLVING_ID` en el ciclo de vida del Outbox para manejar fallos en la comunicaciÃ³n con el servicio de identidad sin bloquear el flujo de red del IO.

| Estado | DescripciÃ³n |
| :--- | :--- |
| `PENDING` | Mensaje recibido del router, guardado en DB local. |
| `RESOLVING_ID` | Consultando al `IdentityResolver` el mapeo de ILK a External ID. |
| `READY_TO_SEND` | Identidad resuelta con Ã©xito, listo para el envÃ­o al canal. |
| `SENT` | ConfirmaciÃ³n de entrega (ACK) recibida por el proveedor externo. |
| `FAILED` | Error irrecuperable en resoluciÃ³n o envÃ­o. |

---

## Fuera de alcance de io-common

- ValidaciÃ³n de firmas de proveedores.
- AutenticaciÃ³n OAuth.
- ExtracciÃ³n de IDs especÃ­ficos de canal.
- Decisiones de routing u OPA.
- GestiÃ³n de secretos.

---

## 4. Arquitectura interna

### 4.1 Interfaces principales

**InboundAdapter (por canal)**
- parse_event(raw) â†’ InboundEvent
- dedup_key(event) â†’ DedupKey
- session_key(event) â†’ SessionKey?
- to_internal_message(event|turn) â†’ Message

**OutboundAdapter (por canal)**
- build_outbound(message) â†’ OutboxItem
- send(outbox_item) â†’ SendResult

**ErrorClassifier**
- classify(error) â†’ retryable | non_retryable | rate_limited | auth_error

**Storage**
- dedup_store
- outbox_store
- session_store (opcional)

---

---

## 4.X Contrato de metadata IO (meta.context.io (TENTATIVO))

`io-common` **DEBE** proveer utilidades para construir y validar el bloque estandarizado
`meta.context.io (TENTATIVO)` (ver tambiÃ©n la spec de Nodos IO). El objetivo es que todos los IO produzcan/consuman
una metadata homogÃ©nea, minimizando excepciones por canal.

Elementos clave:
- `entrypoint`: identidad/cuenta del negocio (p.ej. nÃºmero A vs B).
- `reply_target`: parÃ¡metros mÃ­nimos para responder (contrato clave).
- `message.id`: ID externo para dedup best-effort.
- `conversation`: identificador externo (y thread si aplica).

`io-common` **PUEDE** proveer helpers tipo:
- `build_io_context_inbound(...)`
- `extract_reply_target(outbound_message)`
- `propagate_io_context(inbound â†’ outbound)`

> Nota: el contenido exacto por canal se modela vÃ­a `reply_target.kind` y `reply_target.params`.


## 5. Estado y almacenamiento (dentro del IO)

Bajo las directivas actuales, los Nodos IO **NO** manejan base de datos ni Redis.
Por lo tanto, `io-common` implementa almacenamiento **en memoria** para:

- `dedup_cache` (TTL + cap)
- `session_buffer` (ventanas + caps)
- `outbox_queue` (cola + caps)

### 5.1 Reglas obligatorias de acotamiento

`io-common` **DEBE** exponer configuraciÃ³n para limitar memoria y evitar crecimiento sin control:

- `dedup.ttl_ms`, `dedup.max_entries`, `dedup.max_bytes`
- `session.window_ms`, `session.max_sessions`, `session.max_fragments`, `session.max_bytes`
- `outbox.max_pending`, `outbox.max_inflight`, `outbox.max_age_ms`
- polÃ­ticas de expulsiÃ³n (eviction) y flush forzado

`io-common` **DEBE** exponer mÃ©tricas por:
- evictions,
- drops (por lÃ­mites),
- size actual de caches/colas,
- timeouts.

---

## 6. Opciones futuras: persistencia y aceleraciÃ³n fuera del IO

Si en el futuro el sistema permite persistencia para IO, hay dos caminos tÃ­picos:

- **Postgres** (fuente de verdad) para dedup/outbox/mapeos (idempotencia) con auditorÃ­a.
- **Redis** como acelerador (cache TTL, buffers temporales, rate limiting).

Estas opciones **NO** estÃ¡n habilitadas dentro del IO bajo las directivas actuales, pero el diseÃ±o
de `io-common` debe mantener puntos de extensiÃ³n para incorporarlas (por ejemplo, una interfaz `StorageBackend`).
 (post-MVP)

### 6.1 Usos recomendados
- Cache de dedup (SETNX + TTL).
- Buffer temporal de sessionizaciÃ³n.
- Rate limiting distribuido.

### 6.2 No recomendado como fuente de verdad
- Outbox durable.
- AuditorÃ­a final.

### 6.3 Modelo hÃ­brido
- Redis fast-path + Postgres truth.
- Fallback a Postgres ante misses.

---

## 7. Decisiones abiertas

### 7.1 DB por IO vs schema por IO
- DB por IO: mÃ¡s aislamiento, mÃ¡s ops.
- Schema por IO: menos ops, menos aislamiento.

### 7.2 SessionizaciÃ³n obligatoria u opcional
- Obligatoria en chat: menos ruido downstream.
- Opcional: MVP mÃ¡s simple.

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
- deduplicaciÃ³n durable,
- outbox y retries,
- backoff,
- lifecycle y draining,
- mÃ©tricas y logging,
- sessionizaciÃ³n (si aplica).

Riesgos:
- inconsistencia entre canales,
- bugs de duplicaciÃ³n,
- alta deuda tÃ©cnica.

MitigaciÃ³n mÃ­nima:
- checklist de conformidad + tests de contrato.

---

## 9. Entregables de io-common

- Migraciones SQL versionadas.
- API pÃºblica estable.
- MÃ©tricas estÃ¡ndar.
- DocumentaciÃ³n de integraciÃ³n.


---

## Anexo A â€“ Esquema sugerido si se habilita Postgres (post-MVP)

> Este anexo es **informativo** y **no aplica** mientras los IO no puedan usar DB/Redis.

### A.1 Tabla `io_dedup` (inbound)

- `dedup_key TEXT PRIMARY KEY`
- `first_seen_at TIMESTAMPTZ NOT NULL DEFAULT now()`
- `expires_at TIMESTAMPTZ NOT NULL`
- `meta JSONB NULL`

OperaciÃ³n: `INSERT ... ON CONFLICT DO NOTHING` (conflict â‡’ DUPLICATE).

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

Ãndices: `(state, next_attempt_at)`, `(lock_until)`.

### A.3 Tabla `io_session_buffer` (sessionizaciÃ³n)

- `session_key TEXT PRIMARY KEY`
- `buffer JSONB NOT NULL`
- `window_deadline TIMESTAMPTZ NOT NULL`
- `updated_at TIMESTAMPTZ NOT NULL`


> **Outbox (aclaraciÃ³n):** El outbox de los nodos IO **no es durable**. Su estado vive Ãºnicamente en memoria y puede perderse ante reinicios. Sin embargo, **se mantiene el mecanismo ya establecido de TTL, retries y ventana de vida mÃ¡xima**, tras la cual los mensajes pendientes se descartan.

---

## Aclaraciones v1.16 â€“ AlineaciÃ³n con Fluxbee

### Identidad L3

El Nodo IO **NO debe esperar ni consumir mensajes sÃ­ncronos de `SY.identity`**.

La resoluciÃ³n de identidad se realiza **exclusivamente mediante lectura de Shared Memory**
(`jsr-identity-<island>`, vÃ­a `jsr-identity` / `jsr-identity-client`).

Cuando no se puede resolver `src_ilk`, el Nodo IO **DEBE** enviar el mensaje al Router con
`meta.src_ilk = null` y permitir que el Router (vÃ­a OPA) desvÃ­e el flujo a onboarding.

Esta definiciÃ³n reemplaza cualquier mecanismo previo de espera, buffer o relay
de identidad en el Nodo IO.

---

### Contexto (`ctx_window`)

`ctx_window` es inyectado **Ãºnicamente por el Router** y contiene los Ãºltimos 20 turns
del contexto conversacional.

El Nodo IO:
- DEBE aceptar mensajes que incluyan `ctx_window`
- NO DEBE reenviar `ctx_window` al canal externo
- NO DEBE persistir ni exponer este campo al usuario final

Cualquier referencia previa a manejo de historial conversacional en el Nodo IO
debe considerarse obsoleta.

---

### Protocolo (`meta.ctx` vs `meta.context`)

SegÃºn la especificaciÃ³n del Router v1.16:

- `meta.ctx` representa el **Contexto Conversacional (CTX)**
- `meta.context` se reserva exclusivamente para **datos adicionales evaluados por OPA**

`meta.context` **NO** debe utilizarse para almacenar historia, turns ni contexto
conversacional.

---

### Persistencia en el Nodo IO

Cuando esta especificaciÃ³n menciona persistencia "en memoria", se refiere a la
**implementaciÃ³n MVP**.

Normativamente, el Nodo IO **DEBE definir una interfaz de persistencia**, cuya
implementaciÃ³n puede variar:

- MVP: InMemory
- EvoluciÃ³n: SQLite
- Escala: PostgreSQL

El Nodo IO **NO es source of truth**. La persistencia canÃ³nica corresponde al Router
y a los nodos de Storage.

