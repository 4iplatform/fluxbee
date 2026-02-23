# Fluxbee - 13 Persistencia (NATS + SY.storage)

**Estado:** v1.16  
**Fecha:** 2026-02-06  
**Audiencia:** Desarrolladores de router core, infraestructura

---

## 1. Resumen

La capa de persistencia desacopla los procesos de la base de datos mediante un buffer embebido (NATS JetStream). Esto elimina bloqueos, maneja fallos de WAN, y garantiza entrega sin pérdida de datos.

```
┌─────────────────────────────────────────────────────────────────┐
│                      PRINCIPIO CLAVE                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  Ningún proceso espera a PostgreSQL.                           │
│  NATS es el buffer. SY.storage es el único que toca la DB.     │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

**Beneficios:**

| Problema | Solución |
|----------|----------|
| Router bloqueado esperando DB | NATS fire-and-forget (~1ms) |
| WAN cae | NATS local bufferea hasta reconexión |
| PostgreSQL lento/caído | NATS bufferea, SY.storage retryea |
| Duplicados por retry | DB: `ON CONFLICT DO NOTHING` |
| Orden de llegada | No importa, (ctx, seq) es la clave |

---

## 2. Arquitectura

### 2.1 Vista General

```
┌─────────────────────────────────────────────────────────────────┐
│                         ISLA HIJA                               │
│                                                                 │
│  ┌────────────────────────────────────────────────────────────┐│
│  │                     Router (cualquiera)                    ││
│  │  ┌─────────────────┐                                       ││
│  │  │ NATS embebido   │  ← JetStream, persistencia local     ││
│  │  │ (si config)     │                                       ││
│  │  └────────┬────────┘                                       ││
│  │           │ publish "turns.local"                          ││
│  │           │                                                 ││
│  │  ┌────────▼────────┐                                       ││
│  │  │ Routing normal  │                                       ││
│  │  └─────────────────┘                                       ││
│  └────────────────────────────────────────────────────────────┘│
│                            │                                    │
│              ┌─────────────┼─────────────┐                     │
│              │             │             │                      │
│              ▼             ▼             ▼                      │
│       SY.cognition    Router+WAN    (otros)                    │
│       (consume)       (consume+     (consumen                  │
│           │            forward)      local)                    │
│           ▼               │                                    │
│       LanceDB             │                                    │
│       jsr-memory          │                                    │
│                           │                                    │
└───────────────────────────┼────────────────────────────────────┘
                            │
                            │ WAN (TCP)
                            │ turns.batch
                            │
┌───────────────────────────┼────────────────────────────────────┐
│                      ISLA MADRE                                │
│                           │                                    │
│                      ┌────▼─────┐                              │
│                      │Router+WAN│                              │
│                      │(recibe)  │                              │
│                      └────┬─────┘                              │
│                           │ publish "storage.turns"            │
│                           ▼                                    │
│                   ┌────────────────┐                           │
│                   │  NATS local    │                           │
│                   └───────┬────────┘                           │
│                           │                                    │
│            ┌──────────────┼──────────────┐                    │
│            │              │              │                     │
│            ▼              ▼              ▼                     │
│     SY.cognition    ┌───────────┐   SY.admin                  │
│     (madre)         │SY.storage │   (otros)                   │
│         │           │           │                              │
│         ▼           │ consume   │                              │
│     LanceDB         │ NATS      │                              │
│     jsr-memory      │    +      │                              │
│                     │ Socket    │                              │
│                     │    │      │                              │
│                     │    ▼      │                              │
│                     │PostgreSQL │                              │
│                     └───────────┘                              │
│                                                                │
└────────────────────────────────────────────────────────────────┘
```

### 2.2 Componentes

| Componente | Dónde | Responsabilidad |
|------------|-------|-----------------|
| Router | todas las islas | Produce a NATS local, routing normal |
| Router + NATS embebido | config decide | Buffer local JetStream |
| Router + WAN | config decide | Bridge NATS ↔ WAN |
| SY.storage | solo isla madre | Consume NATS → PostgreSQL |
| SY.cognition | todas las islas | Consume NATS → LanceDB |

---

## 3. NATS Embebido en Router

### 3.1 Configuración

Todos los routers son el **mismo binario**. La configuración decide qué capacidades activa:

```yaml
# Router simple (cliente NATS)
router:
  nats:
    mode: "client"
    url: "nats://router-gateway:4222"

# Router con NATS embebido
router:
  nats:
    mode: "embedded"
    port: 4222
    storage_dir: "/var/lib/fluxbee/nats"
    
# Router con NATS embebido + WAN
router:
  nats:
    mode: "embedded"
    port: 4222
    storage_dir: "/var/lib/fluxbee/nats"
  wan:
    peers:
      - "madre.example.com:9000"
    role: "hija"  # o "madre"
```

### 3.2 Estructura del Router Extendido

```rust
pub struct Router {
    // Existente
    uuid: Uuid,
    nodes: HashMap<Uuid, NodeConnection>,
    shm_config: ShmConfigReader,
    shm_lsa: ShmLsaReader,
    shm_identity: ShmIdentityReader,
    shm_memory: ShmMemoryReader,
    
    // NATS local (implementación actual)
    nats_mode: String,            // "embedded" | "client"
    nats_url: String,             // p.ej. nats://127.0.0.1:4222
    nats_publisher: Option<Arc<NatsPublisher>>,
    
    // Nuevo: WAN (opcional)
    wan: Option<WanBridge>,
}

// Nota: la implementación real usa `json_router::nats` (broker embebido + publish local),
// no tipos `async_nats::*` directos en el router.
```

### 3.3 Inicialización

```rust
impl Router {
    pub async fn run(&self) -> Result<()> {
        if self.nats_mode == "embedded" {
            start_embedded_broker_with_storage(
                &self.nats_url,
                Some(Path::new("/var/lib/fluxbee/nats")),
            )
            .await?;
        }

        wait_for_nats_ready(&self.nats_url, Duration::from_secs(20)).await?;

        // Publisher local para persistencia de turns
        let publisher = NatsPublisher::new(
            self.nats_url.clone(),
            "storage.turns".to_string(),
        );
        publisher.publish(turn_payload).await?;

        Ok(())
    }
}
```

### 3.4 Flujo de Mensajes

```rust
impl Router {
    pub async fn handle_message(&mut self, msg: Message) -> Result<()> {
        // 1. Routing normal (crítico, síncrono)
        self.route_message(&msg).await?;
        
        // 2. Publicar a NATS si tiene contexto (fire & forget)
        if let Some(ctx) = &msg.meta.ctx {
            let turn = Turn::from_message(&msg);
            
            // NO await en el publish, fire & forget
            if let Some(publisher) = &self.nats_publisher {
                let _ = publisher.publish(&turn.to_bytes()).await;
            }
        }
        
        Ok(())
    }
}
```

---

## 4. WAN Bridge

### 4.1 Responsabilidad

El router con WAN consume de NATS local y forward por TCP a la isla madre.

```rust
pub struct WanBridge {
    peers: Vec<WanPeer>,
    role: WanRole,
    nats_endpoint: String,
    turns_subject: String, // "turns.local"
}

pub enum WanRole {
    Hija,   // Consume local, envía a madre
    Madre,  // Recibe de hijas, publica a NATS local
}
```

### 4.2 Flujo Isla Hija

```rust
impl WanBridge {
    pub async fn run_hija(&mut self) -> Result<()> {
        // Suscribirse a turns locales (durable queue del broker embebido)
        let mut sub = subscribe_local(
            &config_dir,
            self.turns_subject.clone(),
            "durable.wan-bridge.turns",
        )?;
        
        // Buffer para batching
        let mut batch: Vec<Turn> = Vec::with_capacity(100);
        let mut last_flush = Instant::now();
        
        loop {
            tokio::select! {
                // Nuevo turn
                Some(msg) = sub.next() => {
                    batch.push(Turn::from_bytes(&msg.payload)?);
                    
                    // Flush si batch lleno
                    if batch.len() >= 100 {
                        self.flush_batch(&mut batch).await;
                    }
                }
                
                // Flush por tiempo (cada 100ms)
                _ = tokio::time::sleep_until(last_flush + Duration::from_millis(100)) => {
                    if !batch.is_empty() {
                        self.flush_batch(&mut batch).await;
                        last_flush = Instant::now();
                    }
                }
            }
        }
    }
    
    async fn flush_batch(&mut self, batch: &mut Vec<Turn>) {
        let wan_msg = WanMessage::TurnBatch(std::mem::take(batch));
        
        // Intentar enviar a madre
        for peer in &mut self.peers {
            match peer.send(&wan_msg).await {
                Ok(_) => {
                    // Éxito, ACK a NATS (implícito con WorkQueue)
                    return;
                }
                Err(e) => {
                    log::warn!("WAN send failed to {}: {}", peer.addr, e);
                    // Intentar siguiente peer o retry
                }
            }
        }
        
        // Si todos fallan, batch se mantiene en NATS (no se perdió)
        log::error!("All WAN peers failed, NATS will retry");
    }
}
```

### 4.3 Flujo Isla Madre

```rust
impl WanBridge {
    pub async fn run_madre(&mut self) -> Result<()> {
        loop {
            // Recibir de hijas
            let wan_msg = self.receive_from_hijas().await?;
            
            match wan_msg {
                WanMessage::TurnBatch(turns) => {
                    // Publicar a NATS local para SY.storage
                    for turn in turns {
                        publish_local(
                            &self.nats_endpoint,
                            "storage.turns",
                            &turn.to_bytes(),
                        )
                        .await?;
                    }
                }
                // ... otros tipos
            }
        }
    }
}
```

### 4.4 Protocolo WAN

```rust
#[derive(Serialize, Deserialize)]
pub enum WanMessage {
    // Existentes
    Hello(HelloPayload),
    Lsa(LsaPayload),
    Forward(Message),
    
    // Nuevos para persistencia
    TurnBatch(Vec<Turn>),
    TurnAck { 
        batch_id: u64,
        count: usize,
    },
}
```

---

## 5. SY.storage

### 5.1 Ubicación

SY.storage **solo corre en isla madre**, en el mismo host que PostgreSQL.

### 5.2 Interfaces

| Canal | Dirección | Uso |
|-------|-----------|-----|
| NATS `storage.turns` | IN | Writes de turns |
| NATS `storage.events` | IN | Writes de events |
| NATS `storage.items` | IN | Writes de items |
| NATS `storage.reactivation` | IN | Updates de activation |
| Unix Socket | IN/OUT | Queries síncronos |

### 5.3 Implementación

```rust
pub struct Storage {
    config_dir: PathBuf,
    nats_endpoint: String,
    socket_listener: UnixListener,
    pg_pool: PgPool,
}

impl Storage {
    pub async fn run(&mut self) -> Result<()> {
        // Suscripciones durables locales (ack en éxito, retry en error)
        let turns_sub = subscribe_local(
            &self.config_dir,
            "storage.turns".to_string(),
            "durable.sy-storage.turns",
        )?;
        let events_sub = subscribe_local(
            &self.config_dir,
            "storage.events".to_string(),
            "durable.sy-storage.events",
        )?;
        let items_sub = subscribe_local(
            &self.config_dir,
            "storage.items".to_string(),
            "durable.sy-storage.items",
        )?;
        let react_sub = subscribe_local(
            &self.config_dir,
            "storage.reactivation".to_string(),
            "durable.sy-storage.reactivation",
        )?;
        
        tokio::spawn(self.run_storage_subject(turns_sub, Subject::Turns));
        tokio::spawn(self.run_storage_subject(events_sub, Subject::Events));
        tokio::spawn(self.run_storage_subject(items_sub, Subject::Items));
        tokio::spawn(self.run_storage_subject(react_sub, Subject::Reactivation));

        // Queries síncronos via Socket
        loop {
            let (stream, _) = self.socket_listener.accept().await?;
            tokio::spawn(self.handle_socket_query(stream));
        }
    }
}
```

### 5.4 Handlers de Escritura

```rust
impl Storage {
    async fn handle_turn(&self, msg: NatsMessage) {
        let turn: Turn = Turn::from_bytes(&msg.payload)?;
        
        // Idempotente: ON CONFLICT DO NOTHING
        let result = sqlx::query!(
            r#"
            INSERT INTO turns (ctx, seq, ts, from_ilk, to_ilk, ich, msg_type, content, tags)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (ctx, seq) DO NOTHING
            "#,
            turn.ctx,
            turn.seq,
            turn.ts,
            turn.from_ilk,
            turn.to_ilk,
            turn.ich,
            turn.msg_type,
            turn.content,
            &turn.tags
        )
        .execute(&self.pg_pool)
        .await;
        
        match result {
            Ok(_) => {
                // Handler OK => ACK del subscriber durable
                msg.ack().ok();
            }
            Err(e) => {
                log::error!("PG write failed: {}", e);
                // Handler error => sin ACK, redelivery/retry
            }
        }
    }
    
    async fn handle_reactivation(&self, msg: NatsMessage) {
        let event: ReactivationEvent = serde_json::from_slice(&msg.payload)?;
        
        for ev in &event.reactivated.events {
            if ev.used {
                sqlx::query!(
                    r#"
                    UPDATE events SET
                        activation_strength = LEAST(activation_strength * 1.1, 1.0),
                        use_count = use_count + 1,
                        success_count = success_count + CASE WHEN $2 THEN 1 ELSE 0 END,
                        last_used_at = now()
                    WHERE event_id = $1
                    "#,
                    ev.event_id,
                    event.outcome.status == "resolved"
                )
                .execute(&self.pg_pool)
                .await?;
            } else {
                // Recuperado pero no usado → debilitar
                sqlx::query!(
                    r#"
                    UPDATE events SET
                        activation_strength = GREATEST(activation_strength * 0.95, 0.1)
                    WHERE event_id = $1
                    "#,
                    ev.event_id
                )
                .execute(&self.pg_pool)
                .await?;
            }
        }
        
        msg.ack().ok();
    }
}
```

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

## 6. SY.cognition Actualizado

SY.cognition ahora consume de NATS en lugar de poll a PostgreSQL:

```rust
impl Cognition {
    pub async fn run(&mut self) -> Result<()> {
        // Consumir de NATS local (no de PostgreSQL)
        let mut turns_sub = self.nats_client.subscribe("turns.local").await?;
        let mut react_sub = self.nats_client.subscribe("reactivations").await?;
        
        // Buffer de turns pendientes por contexto
        let mut pending: HashMap<String, Vec<Turn>> = HashMap::new();
        
        loop {
            tokio::select! {
                // Nuevo turn
                Some(msg) = turns_sub.next() => {
                    let turn: Turn = Turn::from_bytes(&msg.payload)?;
                    
                    // Agregar a buffer del contexto
                    pending.entry(turn.ctx.clone())
                        .or_default()
                        .push(turn);
                    
                    // Procesar si detectamos boundary
                    self.maybe_consolidate(&mut pending).await?;
                    
                    msg.ack().await.ok();
                }
                
                // Reactivación
                Some(msg) = react_sub.next() => {
                    let event: ReactivationEvent = serde_json::from_slice(&msg.payload)?;
                    self.process_reactivation(event).await?;
                    msg.ack().await.ok();
                }
                
                // Timeout: consolidar episodios abiertos
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    self.consolidate_stale_episodes(&mut pending).await?;
                }
            }
        }
    }
    
    async fn maybe_consolidate(&mut self, pending: &mut HashMap<String, Vec<Turn>>) -> Result<()> {
        let mut to_remove = vec![];
        
        for (ctx, turns) in pending.iter_mut() {
            if let Some(boundary) = self.detect_boundary(turns) {
                // Episodio completo → consolidar
                let episode = Episode::from_turns(turns, boundary);
                
                // Llamar AI.tagger y AI.consolidator
                let tags = self.call_ai_tagger(&episode).await?;
                let items = self.call_ai_consolidator(&episode, &tags).await?;
                
                // Escribir en LanceDB
                self.write_event_package(&episode, &tags, &items).await?;
                
                // Actualizar jsr-memory
                self.update_memory_index(&episode, &tags).await?;
                
                // Publicar a NATS para que SY.storage persista
                self.nats_client
                    .publish("storage.events", episode.to_bytes())
                    .await?;
                
                for item in &items {
                    self.nats_client
                        .publish("storage.items", item.to_bytes())
                        .await?;
                }
                
                to_remove.push(ctx.clone());
            }
        }
        
        for ctx in to_remove {
            pending.remove(&ctx);
        }
        
        Ok(())
    }
}
```

---

## 7. Tiempos y Garantías

### 7.1 Latencias

| Paso | Latencia | Bloquea Router |
|------|----------|----------------|
| Router → NATS local | ~1ms | No (fire & forget) |
| NATS local → SY.cognition | ~1ms | No |
| NATS local → WAN bridge | ~1ms | No |
| WAN → NATS madre | ~10-50ms (red) | No |
| NATS madre → SY.storage | ~1ms | No |
| SY.storage → PostgreSQL | ~5-20ms | No (otro proceso) |

**Total turn → PostgreSQL:** ~20-100ms

**Router nunca espera más de ~1ms** (solo el ACK de NATS local).

### 7.2 Degradación Graceful

| Fallo | Comportamiento |
|-------|----------------|
| WAN cae | NATS local bufferea (hasta 7 días default) |
| PostgreSQL cae | NATS madre bufferea, SY.storage retryea |
| NATS local cae | Router no puede publicar, routing sigue funcionando |
| SY.cognition cae | NATS bufferea, se procesa al reiniciar |
| LanceDB > 50ms | Forward solo con ctx_window (sin memory_package) |

### 7.3 Idempotencia

```sql
-- Turns: (ctx, seq) es unique
INSERT INTO turns ... ON CONFLICT (ctx, seq) DO NOTHING;

-- Events: event_id es serial, pero content es determinístico
INSERT INTO events ... ON CONFLICT DO UPDATE SET ...;

-- Items: memory_id es determinístico
INSERT INTO memory_items ... ON CONFLICT (memory_id) DO NOTHING;
```

---

## 8. Múltiples Routers con NATS

### 8.1 Escenarios Soportados

| Escenario | Funciona | Notas |
|-----------|----------|-------|
| 1 router con NATS + WAN | ✓ | Caso simple |
| N routers, 1 con NATS + WAN | ✓ | Demás son clientes |
| N routers, todos con NATS + WAN | ✓ | Cada uno forward, DB deduplica |
| N routers con NATS, 1 con WAN | ✓ | Solo uno bridge |

### 8.2 Múltiples NATS Embebidos (Independientes)

```
Router1 [NATS:4222] ──► WAN
Router2 [NATS:4223] ──► WAN
Router3 [NATS:4224] ──► WAN

Cada uno:
  - Bufferea localmente
  - Forward por WAN
  - Madre recibe de todos
  - PostgreSQL deduplica por (ctx, seq)
```

**No hay conflicto porque:**
- Cada turn tiene (ctx, seq) único
- `ON CONFLICT DO NOTHING` es idempotente
- El orden de llegada no importa

---

## 9. Streams NATS

### 9.1 Configuración

```
TURNS_LOCAL
├── Subject: turns.local
├── Retention: WorkQueue
├── Storage: File
├── Max Age: 7 días
└── Consumers: SY.cognition, WAN bridge

STORAGE (solo en madre)
├── Subjects: storage.turns, storage.events, storage.items, storage.reactivation
├── Retention: WorkQueue
├── Storage: File
├── Max Age: 7 días
└── Consumer: SY.storage

REACTIVATIONS
├── Subject: reactivations
├── Retention: WorkQueue
├── Storage: File
├── Max Age: 1 día
└── Consumers: SY.cognition, SY.storage
```

### 9.2 Consumer Groups

```rust
// SY.cognition y WAN bridge usan sus propias colas durables.
// El broker embebido mantiene cursor por durable_name.

let cognition_sub = subscribe_local(
    &config_dir,
    "turns.local".to_string(),
    "durable.sy-cognition.turns",
)?;

let wan_bridge_sub = subscribe_local(
    &config_dir,
    "turns.local".to_string(),
    "durable.wan-bridge.turns",
)?;
```

---

## 10. Constantes

```rust
// NATS
pub const NATS_DEFAULT_PORT: u16 = 4222;
pub const NATS_MAX_PAYLOAD: usize = 1024 * 1024;  // 1MB
pub const NATS_STREAM_MAX_AGE_DAYS: u64 = 7;

// WAN batching
pub const WAN_BATCH_SIZE: usize = 100;
pub const WAN_BATCH_TIMEOUT_MS: u64 = 100;
pub const WAN_RETRY_DELAY_MS: u64 = 5000;

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
// Límites en NATS
pub const MAX_PENDING_TURNS: usize = 100_000;
pub const MAX_PENDING_BYTES: usize = 100 * 1024 * 1024;  // 100MB

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
