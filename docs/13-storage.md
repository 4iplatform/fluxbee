# Fluxbee - 13 Persistencia (stream del router + SY.storage)

**Estado:** v1.16  
**Fecha:** 2026-02-06  
**Audiencia:** Desarrolladores de router core, infraestructura

> **Transporte (A-70).** Lo que versiones anteriores de este doc decían de NATS (reenvío entre
> hives, buffers de días) nunca existió así: el broker era propio, local a cada hive, y solo el
> tráfico del motherbee llegaba a la base. Lo reemplazó el stream de cada router,
> `docs/router-stream-v1.md`. Desde 0.1.76 los turns llegan a SY.storage por el stream de cada
> router, de todos los hives, y `turns` es el log de mensajes (§7.3). Desde 0.1.77 lo que deriva
> cognition también llega por el stream (`storage.cognition.batch`), por comunicación (hilo y
> agente), y cognition reconstruye su estado pidiéndoselo a SY.storage (`COGNITION_STATE_GET`).
> En 0.1.78 NATS se borró (etapa 2b): no hay puerto 4222, ni sección `nats:` en `hive.yaml`, ni
> `/var/lib/fluxbee/nats`; el orchestrator borra los restos una vez al arrancar, logueando cuántos
> mensajes tenía pendientes el broker.

---

## 1. Resumen

La capa de persistencia desacopla los procesos de la base de datos mediante el stream de cada router (`RT.<router>.stream@<hive>`): un log corto en disco que entrega sus registros en lotes con ack. El router nunca espera al log ni a la base. Es de mejor esfuerzo: suaviza tiempos, no es un almacén; lo que un sink no toma dentro de la ventana (120 s / 32 MB por router) se pierde y se cuenta.

```
┌─────────────────────────────────────────────────────────────────┐
│                      PRINCIPIO CLAVE                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Ningún proceso espera a PostgreSQL.                            │
│  El stream de cada router es el buffer. SY.storage es el único  │
│  que toca la DB.                                                │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**Beneficios:**

| Problema | Solución |
|----------|----------|
| Router bloqueado esperando DB | La copia va a una cola acotada del stream; el router nunca espera |
| WAN cae | El stream guarda y reintenta dentro de la ventana; lo que la excede se pierde y se cuenta |
| PostgreSQL lento/caído | Sin base, SY.storage no acka y el stream reenvía; si se cae la conexión, SY.storage sale y systemd lo reinicia |
| Duplicados por retry | DB: `ON CONFLICT DO NOTHING` |
| Orden de llegada | No importa: cada turno tiene su clave, el hash de su tema y su contenido |

---

## 2. Arquitectura

### 2.1 Vista General

```
 Cualquier hive (motherbee, worker, ingress, egress)
 ───────────────────────────────────────────────────
  nodo ──► router ──► routing normal (no espera al log)
             │
             │ copia cada mensaje (storage.turns) y los turnos
             │ de sus agentes AI.* (cognition.turns)
             ▼
  RT.<router>.stream@<hive>      log en disco, ventana 120 s / 32 MB
             │
             │ lotes STREAM_BATCH a cada sink, que contesta STREAM_ACK
             │ (socket de fluxbee: local, IRP o WAN)
             │
     ┌───────┴─────────────────────────┐
     ▼                                 ▼
  SY.cognition@<mismo hive>         SY.storage@motherbee
  (cognition.turns; solo en         (storage.turns, storage.cognition.*)
   motherbee y workers)                │
     │ jsr-memory (SHM local)          ▼
     │                              storage_inbox ──► turns (log de mensajes)
     │ STREAM_APPEND                              ──► cognition_*
     │ storage.cognition.batch                        (PostgreSQL)
     └──► stream de su router ──► SY.storage@motherbee
```

### 2.2 Componentes

| Componente | Dónde | Responsabilidad |
|------------|-------|-----------------|
| Router | todas las islas | Copia los turns a su stream (sin esperar), routing normal |
| Stream del router (`RT.<router>.stream@<hive>`) | uno por router | Log en disco con ventana; entrega lotes con ack a sus sinks |
| SY.storage | solo isla madre | Toma los lotes en `storage_inbox` → PostgreSQL |
| SY.cognition | motherbee y workers | Toma `cognition.turns` de los streams de su hive → jsr-memory; manda sus salidas por el stream |

---

## 3. El Stream del Router

### 3.1 Configuración

No se configura: cada router corre su stream, que también es un nodo de ese router (`RT.gateway.stream@worker1`; el prefijo `RT.` está reservado al proceso del router). No hay sección en `hive.yaml`: la sección `nats:` se borró en 0.1.78, y el orchestrator quita una vieja (y `/var/lib/fluxbee/nats`) una vez al arrancar. Los límites son constantes en `src/router/stream.rs` (§10).

El log vive en `/var/lib/fluxbee/state/<router L2>/stream/` (p.ej. `state/RT.gateway@worker1/stream/`): segmentos append-only y `state.json` con el próximo seq y el cursor de cada sink.

### 3.2 Cómo Entran los Registros

1. **Copia del turn:** el router donde entra un mensaje (salvo los tipos `system`, `admin` y `query_response`) lo copia como `storage.turns`, con el nombre real del emisor. Cada mensaje se copia una sola vez.
2. **Copia para cognition:** el router donde está conectado un agente (`AI.*`) copia como `cognition.turns` cada turno con `thread_id` que el agente envía (al entrar) o recibe (al entregarlo); ni copias de tap ni broadcasts.
3. **`STREAM_APPEND {subject, payload}`:** un nodo se lo manda a su propio router (`fluxbee_sdk::stream::append`, fire and forget). El router lo consume, no lo rutea a ningún nodo, y guarda como productor el nombre de la conexión. Solo acepta los subjects permitidos a ese productor: `storage.cognition.*` solo de `SY.cognition@<este hive>`; `storage.turns` y `cognition.turns` solo los copia el router. Un append rechazado se cuenta.

El router nunca espera: deja el registro en una cola acotada (`try_send`) y el trabajo de disco corre en un hilo propio del stream. Cola llena, o más de 16 MB esperando al disco: el registro se descarta y se cuenta.

### 3.3 Entrega

- **Un sender por sink:** cada sink tiene su cursor y un solo lote en vuelo (`STREAM_BATCH`, como mucho 256 registros y 96 KB), así recibe en orden.
- **Ack:** el sink contesta `STREAM_ACK {up_to_seq}` y el stream avanza su cursor. Solo vale el ack del sink de ese lote.
- **Reintentos:** sin ack en 5 s (o con `UNREACHABLE`) se reenvía el mismo lote, con backoff de 0,5 a 8 s. Al menos una vez: todo sink absorbe repeticiones.
- **Vencidos:** un registro más viejo que la ventana se saltea para ese sink y se cuenta; uno que no entra en un frame, también.

### 3.4 Pérdidas y Estado

Lo que se pierde (tope, cola llena, vencido, demasiado grande, rechazado) se cuenta una vez y se reporta en una línea de alarma cada 30 s; `lab/ops.py health` lo muestra como `streamlost`. El stream contesta `STREAM_STATUS` (agregados, entregados y vencidos por sink, descartados por motivo, backlog), y `GET /hives/{hive}/streams` de SY.admin lista los streams de un hive.

---

## 4. Entre Hives (sin bridge)

No hay bridge WAN: un lote es un mensaje más. El stream es un nodo de su router y el gateway lo anuncia en el LSA, así que sus `STREAM_BATCH` salen por el routing normal (entrega local, IRP a otro router del hive, WAN) y los `STREAM_ACK` vuelven por nombre desde cualquier hive. Si la WAN cae, el stream reintenta dentro de la ventana; lo que la excede se pierde y se cuenta.

Todos los hives mandan sus turns a `SY.storage@motherbee`. El ingress y el egress no corren SY.cognition, así que `cognition.turns` no tiene sink en ellos.

---

## 5. SY.storage

### 5.1 Ubicación

SY.storage **solo corre en isla madre**, en el mismo host que PostgreSQL.

### 5.2 Interfaces

| Canal | Dirección | Uso |
|-------|-----------|-----|
| `STREAM_BATCH` de los streams (`RT.*.stream@*`) | IN | `storage.turns` y `storage.cognition.batch` |
| `STREAM_ACK` | OUT | Ack al stream, después de escribir el lote en `storage_inbox` |
| `COGNITION_STATE_GET` | IN/OUT | Las filas de una comunicación, paginadas; solo para `SY.cognition@*` |
| `STORAGE_METRICS_GET` | IN/OUT | Métricas del inbox (`pending`, `pending_with_error`, `oldest_pending_age_s`, `processed_total`, `max_attempts`) para SY.admin y SY.orchestrator |
| Unix Socket | IN/OUT | Queries síncronos |

### 5.3 Implementación

- **Ingesta:** acepta un `STREAM_BATCH` solo de un stream (`RT.<router>.stream@<hive>`, el nombre que estampa el router). En una transacción hace upsert de cada registro en `storage_inbox` (clave `dedupe_key`: hash de subject y payload) y después acka hasta el último seq. Una tarea aparte procesa las filas nuevas, así el ack nunca espera al procesamiento. Sin base, o con la cola de ingesta llena, no acka y el stream reenvía.
- **Chequeos del productor:** cada registro debe venir de un nodo del hive del stream; un turn debe nombrar a su productor como emisor (`routing.src_l2_name`); `storage.cognition.*` solo de `SY.cognition@<hive del stream>`. Lo que no pasa se acka y se descarta.
- **`storage.cognition.batch`:** los sobres de un turno, empaquetados; se expande en una fila de inbox por sobre, bajo su subject `storage.cognition.<entidad>`.
- **Mantenimiento cada 60 s:** reintenta las filas fallidas de más de un minuto (hasta 10 intentos) y purga las procesadas de más de 24 h.
- **Base:** si se cae la conexión a PostgreSQL, SY.storage sale y systemd lo reinicia.
- **Por la malla:** contesta `COGNITION_STATE_GET` (solo a `SY.cognition@*`) y `STORAGE_METRICS_GET`. `GET /config/storage/metrics` de SY.admin y la espera de SY.orchestrator por la base al arrancar en el motherbee usan este último.

### 5.4 Handlers de Escritura

Cada fila del inbox se procesa por su subject:

| Subject | Tabla | Regla |
|---------|-------|-------|
| `storage.turns` | `turns` (el log de mensajes que lee la página Messages de Archi) | `ON CONFLICT (dedupe_key) DO NOTHING`; `ts` es cuando el router lo copió |
| `storage.cognition.<entidad>` | `cognition_threads`, `cognition_contexts`, `cognition_reasons`, `cognition_cooccurrences`, `cognition_scopes`, `cognition_scope_instances`, `cognition_memories`, `cognition_episodes` | Por comunicación (`thread_id` + ILK del agente); una fila solo se reemplaza con un sobre de turno igual o mayor |

`storage.events`, `storage.items` y `storage.reactivation` nunca tuvieron productor: su ingesta se borró en 0.1.78. Sus tablas (`events`, `memory_items`, `storage_reactivation_applied`) quedan.

### 5.5 Socket Protocol (Queries)

```rust
#[derive(Serialize, Deserialize)]
pub enum StorageRequest {
    // Turns
    GetTurns { ctx: String, from_seq: i64, limit: i64 },
    GetTurnsByIlk { ilk: String, since: DateTime<Utc>, limit: i64 },
    
    // Events
    GetEvents { event_ids: Vec<i64> },
    GetEventsByTags { tags: Vec<String>, tenant: String, limit: i64 },
    
    // Items
    GetItems { event_id: i64 },
    GetItemsByTags { tags: Vec<String>, limit: i64 },
    
    // Contextos
    GetContext { ctx: String },
}

#[derive(Serialize, Deserialize)]
pub enum StorageResponse {
    Turns(Vec<Turn>),
    Events(Vec<Event>),
    Items(Vec<MemoryItem>),
    Context(Context),
    Error { code: i32, message: String },
}

impl Storage {
    async fn handle_socket_query(&self, mut stream: UnixStream) {
        // Leer request
        let mut buf = vec![0u8; 65536];
        let n = stream.read(&mut buf).await?;
        let request: StorageRequest = serde_json::from_slice(&buf[..n])?;
        
        // Ejecutar query
        let response = match request {
            StorageRequest::GetTurns { ctx, from_seq, limit } => {
                let turns = sqlx::query_as!(Turn,
                    "SELECT * FROM turns WHERE ctx = $1 AND seq >= $2 ORDER BY seq LIMIT $3",
                    ctx, from_seq, limit
                )
                .fetch_all(&self.pg_pool)
                .await?;
                
                StorageResponse::Turns(turns)
            }
            StorageRequest::GetEventsByTags { tags, tenant, limit } => {
                let events = sqlx::query_as!(Event,
                    r#"
                    SELECT * FROM events 
                    WHERE cues_agg && $1 
                    AND ctx IN (SELECT ctx FROM contexts WHERE tenant_ilk = $2)
                    ORDER BY activation_strength DESC
                    LIMIT $3
                    "#,
                    &tags, tenant, limit
                )
                .fetch_all(&self.pg_pool)
                .await?;
                
                StorageResponse::Events(events)
            }
            // ... otros casos
        };
        
        // Enviar response
        let bytes = serde_json::to_vec(&response)?;
        stream.write_all(&bytes).await?;
    }
}
```

---

## 6. SY.cognition y el Stream

SY.cognition (motherbee y workers) es sink de los streams de su propio hive (`cognition.turns`) y no lee PostgreSQL:

- **Entrada:** toma `STREAM_BATCH` solo de `RT.*.stream@<su hive>`, acka al recibir en una cola acotada y saltea lo que ya aceptó de ese stream y epoch. Cola llena: descarta y cuenta.
- **Por comunicación:** `(thread_id, ILK del agente)`. El productor del registro es el nodo del agente, que cognition mapea a su ILK por la identity SHM. El orden de una comunicación es el número de turno de cognition; el mensaje no lleva secuencia.
- **Salidas:** `STREAM_APPEND` bajo `storage.cognition.batch`: los sobres de un turno, empaquetados en registros de hasta 64 KB; cada sobre lleva la comunicación y su número de turno.
- **Reconstrucción:** al arrancar, antes del primer turno, recarga de SY.storage por la malla (`COGNITION_STATE_GET`, paginado) las comunicaciones que tiene en `jsr-memory`; una que no tiene la carga en su primer turno.

Detalle: `12-cognition-v2.md` y `router-stream-v1.md` §6.

---

## 7. Tiempos y Garantías

### 7.1 Tiempos

| Paso | Bloquea Router |
|------|----------------|
| Router → cola del stream (`try_send`) | No |
| Cola → disco (hilo propio del stream; fsync cada 250 ms) | No |
| Stream → sink (`STREAM_BATCH`, un lote en vuelo por sink; reenvío a los 5 s sin ack) | No |
| SY.storage → `storage_inbox` (una transacción) → `STREAM_ACK` | No (otro proceso) |
| `storage_inbox` → `turns` / `cognition_*` (tarea aparte, después del ack) | No (otro proceso) |

**El router nunca espera al log:** solo deja la copia en una cola acotada. Un registro espera a su sink como mucho la ventana (120 s).

### 7.2 Degradación Graceful

| Fallo | Comportamiento |
|-------|----------------|
| WAN cae / sink caído | El stream guarda y reintenta (backoff 0,5–8 s) dentro de la ventana; lo que la excede se pierde y se cuenta |
| PostgreSQL cae | SY.storage sale y systemd lo reinicia; sin base no acka y el stream reenvía |
| Disco del stream inaccesible, o tope de 32 MB | Los registros nuevos se descartan y se cuentan; el routing sigue |
| Cola del stream llena (16 MB esperando al disco) | Se descarta y se cuenta; el router no espera |
| SY.cognition cae | Pierde su cola (lo ackeado y no procesado); al reiniciar recarga sus comunicaciones de SY.storage |
| `jsr-memory` no disponible / lenta | Forward sin `memory_package` (enrichment fail-open) |

### 7.3 Idempotencia

```sql
-- Turns: la clave es dedupe_key, la misma de storage_inbox (hash del tema y el contenido).
-- (ctx, seq) era la clave hasta 0.1.74: sale del trace_id, y una respuesta reusa el de su
-- pedido, así que la respuesta se descartaba (A-70). Hoy es solo un índice.
-- Desde 0.1.76 `turns` es el log de mensajes (Archi lo lee): guarda el mensaje entero
-- (message), su thread_id y su trace_id; ts es cuando el router lo copió y stored_at cuando
-- SY.storage lo escribió. storage_inbox borra lo procesado a las 24 h.
INSERT INTO turns ... ON CONFLICT (dedupe_key) DO NOTHING;

-- Cognición: cada fila lleva su comunicación (thread_id, agent_ilk) y el turno en que se
-- escribió (source_turn); solo se reemplaza con un turno igual o mayor, así un reintento
-- tardío o una cognition que arrancó vacía no la hace retroceder.
INSERT INTO cognition_contexts ... ON CONFLICT (context_id) DO UPDATE SET ...
    WHERE cognition_contexts.source_turn <= EXCLUDED.source_turn;

-- storage_inbox: la clave es dedupe_key (hash de subject y payload); un lote reenviado cae
-- en las mismas filas.
INSERT INTO storage_inbox ... ON CONFLICT (dedupe_key) DO UPDATE SET attempts = ...;
```

---

## 8. Varios Routers en un Hive

### 8.1 Escenarios Soportados

| Escenario | Funciona | Notas |
|-----------|----------|-------|
| 1 router por hive (PROD) | ✓ | Un stream por hive |
| N routers en un hive (gateway + switches) | ✓ | Un stream por router; cada mensaje se copia una vez, donde entra |

### 8.2 Un Stream por Router

```
RT.gateway.stream@worker1 ───┐
RT.switch1.stream@worker1 ───┼──► SY.storage@motherbee   (un cursor por stream)
RT.gateway.stream@motherbee ─┘

Cada stream:
  - Guarda localmente, dentro de la ventana
  - Manda sus lotes por el routing normal (los de un switch, por IRP al gateway)
  - El gateway anuncia todos los streams del hive en su LSA
  - SY.storage recibe de todos y deduplica por dedupe_key
```

**No hay conflicto porque:**
- Cada turn tiene su dedupe_key, que no se repite
- `ON CONFLICT DO NOTHING` es idempotente
- El orden de llegada no importa

---

## 9. Subjects y Sinks

### 9.1 Tabla

| Subject | Lo produce | Sink |
|---------|------------|------|
| `storage.turns` | El router donde entra el mensaje (todos los roles) | `SY.storage@motherbee` |
| `cognition.turns` | El router donde está el agente `AI.*` (motherbee, worker) | `SY.cognition@<mismo hive>` |
| `storage.cognition.batch` | `SY.cognition@<hive>`, con `STREAM_APPEND` | `SY.storage@motherbee` (se expande en `storage.cognition.<entidad>`) |

### 9.2 Cursores

Cada sink tiene su cursor (último seq ackeado) en `state.json`; un sink lento o ausente se atrasa solo, sin frenar a los demás. Un lote reenviado puede llegar dos veces: SY.storage lo absorbe por `dedupe_key`; SY.cognition por stream, epoch y seq (el epoch cambia cuando el seq del stream puede reiniciar).

---

## 10. Constantes

```rust
// Stream del router (src/router/stream.rs)
pub const STREAM_MAX_AGE_SECS: u64 = 120;                  // ventana: más viejo, se saltea y se cuenta
pub const STREAM_MAX_BYTES: u64 = 32 * 1024 * 1024;        // tope en disco por router
pub const STREAM_NODE_APPENDS_SHARE_PERCENT: u64 = 25;     // parte del tope para los STREAM_APPEND
pub const STREAM_QUEUE_MAX_BYTES: u64 = 16 * 1024 * 1024;  // en memoria, esperando al disco
pub const STREAM_BATCH_MAX_RECORDS: usize = 256;
pub const STREAM_BATCH_MAX_BYTES: usize = 96 * 1024;
pub const STREAM_ACK_TIMEOUT_SECS: u64 = 5;
pub const STREAM_RETRY_MIN_MS: u64 = 500;
pub const STREAM_RETRY_MAX_MS: u64 = 8_000;
pub const STREAM_FSYNC_INTERVAL_MS: u64 = 250;
pub const STREAM_ALARM_INTERVAL_SECS: u64 = 30;            // una línea de alarma con lo perdido

// SY.storage (src/bin/sy_storage.rs)
const INBOX_MAINTENANCE_INTERVAL_SECS: u64 = 60;           // reintento de fallidas + purga
const INBOX_MAX_ATTEMPTS: i32 = 10;
const STORAGE_INBOX_RETENTION_HOURS: i32 = 24;             // filas procesadas

// Timeouts
pub const MEMORY_FETCH_TIMEOUT_MS: u64 = 50;
pub const SOCKET_QUERY_TIMEOUT_MS: u64 = 1000;
pub const PG_WRITE_TIMEOUT_MS: u64 = 5000;
```

---

## 11. Robustez y Edge Cases

### 11.1 Timeouts de Fetch

```rust
// En Router, al armar memory_package
let memory_result = tokio::time::timeout(
    Duration::from_millis(MEMORY_FETCH_TIMEOUT_MS),
    self.fetch_memory_package(&tags)
).await;

let memory_package = match memory_result {
    Ok(Ok(pkg)) => Some(pkg),
    Ok(Err(e)) => {
        log::warn!("Memory fetch error: {}", e);
        None  // Continuar sin memory_package
    }
    Err(_) => {
        log::warn!("Memory fetch timeout (>50ms)");
        None  // Continuar sin memory_package
    }
};

// Forward siempre funciona, con o sin memory_package
```

### 11.2 Señales de Aprendizaje Negativo

```rust
impl Cognition {
    fn interpret_reactivation(&self, event: &ReactivationEvent) -> LearningSignal {
        match &event.outcome.status {
            "resolved" => LearningSignal::Positive {
                boost: 1.1,
                success: true,
            },
            "escalated" => LearningSignal::Negative {
                decay: 0.9,
                inhibit: 0.1,  // Procedure no fue suficiente
            },
            "timeout" => LearningSignal::Negative {
                decay: 0.85,
                inhibit: 0.15,  // Posible confusión
            },
            "failed" => LearningSignal::Negative {
                decay: 0.8,
                inhibit: 0.2,  // Claramente no funcionó
            },
            _ => LearningSignal::Neutral,
        }
    }
}
```

### 11.3 Protecciones de Recursos

```rust
// Límites en el stream del router (además del tope y la cola de §10)
pub const STREAM_TURN_QUEUE: usize = 4_096;         // copias de turns esperando al disco
pub const STREAM_NODE_APPEND_QUEUE: usize = 1_024;  // STREAM_APPEND de nodos: cola y tope aparte

// Límites en jsr-memory
pub const MAX_EVENTS_PER_TENANT: usize = 65536;
pub const MAX_TAGS_PER_TENANT: usize = 16384;

// Eviction si se llena
impl MemoryIndex {
    fn evict_if_needed(&mut self, tenant: &str) {
        let partition = self.get_partition_mut(tenant);
        
        if partition.event_count >= MAX_EVENTS_PER_TENANT {
            // Remover eventos con activation < threshold
            partition.evict_below_threshold(0.2);
        }
    }
}
```

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Cognición | `12-cognition.md` |
| Contexto | `11-context.md` |
| Routing | `04-routing.md` |
| Shared Memory | `03-shm.md` |
| Conectividad WAN | `05-conectividad.md` |
| Stream del router (diseño, A-70) | `router-stream-v1.md` |
