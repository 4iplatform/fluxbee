# Fluxbee - 11 Context Layer (ICH + CTX)

**Estado:** v1.16  
**Fecha:** 2026-02-04  
**Audiencia:** Desarrolladores de nodos IO/AI, router core, infraestructura

---

## 1. Resumen

El sistema de contexto permite mantener conversaciones con historia entre interlocutores a través de diferentes canales de comunicación.

| Entidad | ID | Formato | Descripción |
|---------|-----|---------|-------------|
| ILK | Interlocutor Key | `ilk:<uuid>` | Quién habla (persona, agente, sistema) |
| ICH | Interlocutor Channel | `ich:<uuid>` | Por dónde habla (WhatsApp, Slack, email) |
| CTX | Context | `ctx:<hash>` | La conversación (ILK + ICH = CTX) |

**Principio clave:** CTX es determinístico. `ctx = hash(ilk + ich)`. Mismo interlocutor + mismo canal = mismo contexto siempre.

---

## 2. ICH — Interlocutor Channel

### 2.1 Definición

Un **ICH** representa un canal de comunicación específico asociado a un interlocutor:

```
ICH = medio por el cual un ILK puede enviar/recibir mensajes
```

### 2.2 Formato

```
ich:<uuid>

Ejemplos:
  ich:7c9e6679-7425-40de-944b-e07fc1f90ae7
  ich:550e8400-e29b-41d4-a716-446655440000
```

### 2.3 Estructura del ICH

```json
{
  "ich": "ich:7c9e6679-...",
  "type": "whatsapp",
  "external_id": "+5491155551234",
  "ilk": "ilk:john-doe-uuid",
  "created_at": "2026-02-04T10:00:00Z",
  "metadata": {
    "display_name": "John's WhatsApp",
    "verified": true
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `ich` | string | Sí | Identificador único del channel |
| `type` | string | Sí | Tipo de canal: `whatsapp`, `slack`, `email`, `telegram`, etc. |
| `external_id` | string | Sí | ID en el sistema externo (+54911..., U12345, user@mail.com) |
| `ilk` | string | Sí | ILK dueño de este canal |
| `created_at` | timestamp | Sí | Fecha de creación |
| `metadata` | object | No | Datos adicionales del canal |

### 2.4 Relación ILK ↔ ICH

Un ILK puede tener múltiples ICH:

```
ILK: ilk:john-doe
├── ich:wapp-john     (WhatsApp +5491155551234)
├── ich:slack-john    (Slack U12345)
├── ich:email-john    (Email john@acme.com)
└── ich:telegram-john (Telegram @johndoe)

ILK: ilk:agent-soporte
├── ich:internal-agent (Canal interno del sistema)
└── ich:slack-agent    (Slack bot)
```

### 2.5 Creación de ICH

El ICH se crea cuando GOD (o el sistema de administración) configura un nuevo canal:

```
Usuario: "Creame un canal de WhatsApp para John Doe"
GOD:
  1. Verifica que ILK existe (o lo crea): ilk:john-doe
  2. Crea ICH: ich:<nuevo-uuid>
     - type: whatsapp
     - external_id: +5491155551234
     - ilk: ilk:john-doe
  3. Registra en SY.identity
  4. Responde: "Canal ich:xxx creado para ilk:john-doe"
```

---

## 3. CTX — Context (Conversación)

### 3.1 Definición

Un **CTX** representa una conversación entre un interlocutor usando un canal específico:

```
CTX = ILK + ICH → conversación única con su historia
```

### 3.2 Formato

```
ctx:<hash>

Donde hash = SHA256(ilk + ":" + ich)[0:32] (primeros 32 chars hex)

Ejemplo:
  ILK: ilk:john-doe-uuid
  ICH: ich:wapp-john-uuid
  CTX: ctx:a1b2c3d4e5f6789012345678901234ab
```

### 3.3 Determinismo

**El CTX es calculable, no almacenado.** Cualquier componente puede derivarlo:

```rust
fn compute_ctx(ilk: &str, ich: &str) -> String {
    use sha2::{Sha256, Digest};
    let input = format!("{}:{}", ilk, ich);
    let hash = Sha256::digest(input.as_bytes());
    format!("ctx:{}", hex::encode(&hash[..16]))  // 32 hex chars
}
```

### 3.4 Un ILK, múltiples CTX

El mismo interlocutor puede tener conversaciones separadas por canal:

```
ilk:john-doe + ich:wapp-john   = ctx:abc... (habla de facturación)
ilk:john-doe + ich:email-john  = ctx:def... (habla de soporte técnico)
ilk:john-doe + ich:slack-john  = ctx:ghi... (habla de ventas)
```

**Cada CTX tiene su propia historia independiente.**

---

## 4. Turns — Historia de la Conversación

### 4.1 Definición

Un **Turn** es un mensaje individual dentro de un contexto:

```json
{
  "ctx": "ctx:abc123...",
  "seq": 1,
  "ts": "2026-02-04T15:00:00Z",
  "from_ilk": "ilk:john-doe",
  "to_ilk": "ilk:agent-soporte",
  "ich": "ich:wapp-john",
  "msg_type": "text",
  "content": {
    "text": "Hola, necesito ayuda"
  }
}
```

### 4.2 Campos del Turn

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `ctx` | string | Sí | Context ID |
| `seq` | bigint | Sí | Número de secuencia (monotónico por ctx) |
| `ts` | timestamp | Sí | Timestamp del mensaje |
| `from_ilk` | string | Sí | ILK que envía |
| `to_ilk` | string | No | ILK destino (si aplica) |
| `ich` | string | Sí | Canal usado |
| `msg_type` | string | Sí | Tipo: `text`, `image`, `file`, `audio`, etc. |
| `content` | jsonb | Sí | Contenido del mensaje |

---

## 5. Mensaje con Contexto

### 5.1 Estructura del Mensaje

```json
{
  "routing": {
    "src": "uuid-io-whatsapp",
    "dst": null,
    "ttl": 16,
    "trace_id": "uuid-trace"
  },
  "meta": {
    "type": "user",
    "src_ilk": "ilk:john-doe",
    "dst_ilk": "ilk:agent-soporte",
    "ich": "ich:wapp-john",
    "ctx": "ctx:abc123def456...",
    "ctx_seq": 47
  },
  "payload": {
    "type": "text",
    "content": "Tengo un problema con mi factura"
  }
}
```

### 5.2 Campos de Contexto en meta

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `src_ilk` | string | Sí | ILK que envía el mensaje |
| `dst_ilk` | string | No | ILK destino (si se conoce) |
| `ich` | string | Sí | Canal por el cual llegó/sale el mensaje |
| `ctx` | string | Sí | Context ID (calculado de src_ilk + ich) |
| `ctx_seq` | integer | Sí | Último seq conocido de este contexto |

### 5.3 Mensaje Liviano

**El mensaje NO lleva la historia completa.** Solo lleva:
- El turno actual (en payload)
- Metadata del contexto (ctx, ctx_seq)

El nodo destino reconstruye la historia si la necesita.

---

## 6. Persistencia — PostgreSQL

### 6.1 Tabla de Turns

```sql
CREATE TABLE turns (
    ctx         TEXT NOT NULL,
    seq         BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now(),
    from_ilk    TEXT NOT NULL,
    to_ilk      TEXT,
    ich         TEXT NOT NULL,
    msg_type    TEXT NOT NULL,
    content     JSONB NOT NULL,
    PRIMARY KEY (ctx, seq)
);

-- Índice para queries por contexto
CREATE INDEX idx_turns_ctx ON turns (ctx);

-- Índice para cleanup por tiempo
CREATE INDEX idx_turns_ts ON turns (ts);
```

### 6.2 Tabla de Contextos (metadata)

```sql
CREATE TABLE contexts (
    ctx             TEXT PRIMARY KEY,
    ilk             TEXT NOT NULL,
    ich             TEXT NOT NULL,
    tenant_ilk      TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_activity   TIMESTAMPTZ NOT NULL DEFAULT now(),
    turn_count      BIGINT NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'active',  -- active, closed, archived
    metadata        JSONB
);

CREATE INDEX idx_contexts_ilk ON contexts (ilk);
CREATE INDEX idx_contexts_tenant ON contexts (tenant_ilk);
CREATE INDEX idx_contexts_status ON contexts (status);
```

### 6.3 Connection String

```yaml
# /etc/fluxbee/island.yaml
database:
  url: "postgresql://fluxbee:password@localhost:5432/fluxbee"
  pool_size: 10
  connect_timeout_ms: 5000
```

---

## 7. Router — Persistencia de Turns

### 7.1 Rol del Router

El router es el único componente que ve **todos** los mensajes. Cuando pasa un mensaje con contexto:

1. Extrae ctx, seq, contenido
2. Inserta turn en PostgreSQL (async, no bloquea routing)
3. Reenvía mensaje sin modificar

### 7.2 Flujo

```
Mensaje llega al router
        │
        ▼
¿Tiene meta.ctx?
        │
    ┌───┴───┐
    │       │
   NO      SÍ
    │       │
    │       ▼
    │   INSERT INTO turns (async)
    │       │
    └───┬───┘
        │
        ▼
    Forward normal
```

### 7.3 Implementación (Rust)

```rust
impl Router {
    async fn handle_message(&mut self, msg: Message) -> Result<()> {
        // Persistir turn si tiene contexto
        if let Some(ctx) = &msg.meta.ctx {
            self.persist_turn(&msg).await;  // async, no espera
        }
        
        // Routing normal
        self.route_message(msg).await
    }
    
    async fn persist_turn(&self, msg: &Message) {
        let ctx = msg.meta.ctx.as_ref().unwrap();
        let seq = msg.meta.ctx_seq.unwrap_or(0) + 1;
        
        // Fire-and-forget, no bloquea
        let pool = self.db_pool.clone();
        let turn = Turn::from_message(msg, seq);
        
        tokio::spawn(async move {
            if let Err(e) = sqlx::query!(
                r#"INSERT INTO turns (ctx, seq, ts, from_ilk, to_ilk, ich, msg_type, content)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                   ON CONFLICT (ctx, seq) DO NOTHING"#,
                turn.ctx, turn.seq, turn.ts, turn.from_ilk, turn.to_ilk,
                turn.ich, turn.msg_type, turn.content
            )
            .execute(&pool)
            .await {
                log::warn!("Failed to persist turn: {}", e);
            }
        });
    }
}
```

---

## 8. ctx_client — Librería de Contexto

### 8.1 Propósito

Librería común para todos los nodos que necesitan acceder al contexto. Provee:
- Cache local en memoria
- Fallback a PostgreSQL
- Gestión de memoria (eviction)

### 8.2 Interfaz

```rust
pub struct CtxClient {
    db_pool: PgPool,
    cache: HashMap<String, CtxState>,
    config: CtxClientConfig,
}

pub struct CtxState {
    ctx: String,
    seq: u64,
    turns: Vec<Turn>,
    last_access: Instant,
}

pub struct CtxClientConfig {
    pub cache_max_contexts: usize,      // Default: 1000
    pub cache_max_age: Duration,        // Default: 1 hour
    pub db_query_timeout: Duration,     // Default: 5 seconds
}

impl CtxClient {
    /// Crea cliente conectado a PostgreSQL
    pub async fn new(db_url: &str, config: CtxClientConfig) -> Result<Self>;
    
    /// Obtiene historia del contexto
    /// Usa cache si está actualizado, sino va a DB
    pub async fn get_turns(&mut self, ctx: &str, expected_seq: u64) -> Result<Vec<Turn>>;
    
    /// Registra un turno nuevo en cache local
    /// (No escribe a DB, eso lo hace el router)
    pub fn record_turn(&mut self, ctx: &str, turn: Turn);
    
    /// Limpia contextos viejos del cache
    pub fn evict_stale(&mut self);
    
    /// Calcula CTX a partir de ILK + ICH
    pub fn compute_ctx(ilk: &str, ich: &str) -> String;
}
```

### 8.3 Uso en Nodo AI

```rust
impl AiNode {
    async fn handle_message(&mut self, msg: Message) -> Result<()> {
        let ctx = msg.meta.ctx.as_ref().ok_or("Missing ctx")?;
        let expected_seq = msg.meta.ctx_seq.unwrap_or(0);
        
        // Obtener historia (cache o DB)
        let turns = self.ctx_client.get_turns(ctx, expected_seq).await?;
        
        // Procesar con contexto completo
        let response = self.process_with_context(&msg, &turns).await?;
        
        // Registrar respuesta en cache local
        self.ctx_client.record_turn(ctx, Turn::from_response(&response));
        
        // Enviar respuesta
        self.send_response(response).await
    }
}
```

### 8.4 Lógica de Cache

```rust
impl CtxClient {
    pub async fn get_turns(&mut self, ctx: &str, expected_seq: u64) -> Result<Vec<Turn>> {
        // 1. Verificar cache
        if let Some(state) = self.cache.get_mut(ctx) {
            state.last_access = Instant::now();
            
            if state.seq >= expected_seq {
                // Cache hit - tenemos todo
                return Ok(state.turns.clone());
            }
            // Cache tiene datos pero incompletos, ir a DB
        }
        
        // 2. Query a PostgreSQL
        let turns = sqlx::query_as!(
            Turn,
            "SELECT ctx, seq, ts, from_ilk, to_ilk, ich, msg_type, content 
             FROM turns WHERE ctx = $1 ORDER BY seq",
            ctx
        )
        .fetch_all(&self.db_pool)
        .await?;
        
        // 3. Actualizar cache
        let seq = turns.last().map(|t| t.seq).unwrap_or(0);
        self.cache.insert(ctx.to_string(), CtxState {
            ctx: ctx.to_string(),
            seq: seq as u64,
            turns: turns.clone(),
            last_access: Instant::now(),
        });
        
        // 4. Evict si hay muchos
        if self.cache.len() > self.config.cache_max_contexts {
            self.evict_stale();
        }
        
        Ok(turns)
    }
}
```

---

## 9. Flujo Completo

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ 1. John escribe "Hola" en WhatsApp                                          │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 2. IO.whatsapp recibe webhook                                               │
│    - Busca en SHM: (+5491155551234, whatsapp) → ich:wapp-john              │
│    - Busca ILK del ICH: ich:wapp-john → ilk:john-doe                       │
│    - Calcula CTX: hash(ilk:john-doe + ich:wapp-john) → ctx:abc123          │
│    - Arma mensaje:                                                          │
│      {                                                                      │
│        meta: { src_ilk, ich, ctx, ctx_seq: 0 },                            │
│        payload: { text: "Hola" }                                           │
│      }                                                                      │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 3. Router recibe mensaje                                                    │
│    - INSERT async en PostgreSQL: (ctx:abc123, seq:1, "Hola", ...)          │
│    - OPA resuelve destino → AI.soporte.l1@prod                             │
│    - Forward a AI.soporte.l1                                               │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 4. AI.soporte.l1 recibe mensaje                                             │
│    - ctx_client.get_turns("ctx:abc123", 0)                                 │
│      - Cache miss → SELECT FROM turns → [turn1]                            │
│    - Procesa con historia                                                   │
│    - Genera respuesta: "¿En qué puedo ayudarte?"                           │
│    - ctx_client.record_turn(turn2)  // actualiza cache local               │
│    - Envía respuesta con ctx, ctx_seq: 1                                   │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 5. Router recibe respuesta                                                  │
│    - INSERT async: (ctx:abc123, seq:2, "¿En qué puedo...", ...)            │
│    - Forward a IO.whatsapp                                                  │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 6. IO.whatsapp recibe respuesta                                             │
│    - Entrega a WhatsApp API                                                 │
│    - Actualiza su cache local de ctx si lo tiene                           │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ 7. John escribe "Tengo un problema con mi factura"                          │
│    ... el ciclo continúa con ctx_seq incrementando ...                      │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 10. Nodo IO — Resolución de ICH

### 10.1 Mapeo External ID → ICH

El nodo IO necesita resolver el external_id del canal externo a ICH:

```rust
impl IoNode {
    fn resolve_ich(&self, channel_type: &str, external_id: &str) -> Option<String> {
        // Buscar en SHM jsr-identity
        let identity = self.identity_shm.read();
        
        for mapping in identity.ich_mappings.iter() {
            if mapping.channel_type == channel_type 
               && mapping.external_id == external_id {
                return Some(mapping.ich.clone());
            }
        }
        None
    }
    
    fn get_ilk_for_ich(&self, ich: &str) -> Option<String> {
        let identity = self.identity_shm.read();
        identity.get_ich(ich).map(|entry| entry.ilk.clone())
    }
}
```

### 10.2 Flujo en IO

```rust
impl IoWhatsApp {
    async fn handle_webhook(&mut self, webhook: WhatsAppWebhook) -> Result<()> {
        let external_id = &webhook.from;  // +5491155551234
        
        // 1. Resolver ICH
        let ich = self.resolve_ich("whatsapp", external_id)
            .ok_or("Unknown channel")?;
        
        // 2. Obtener ILK del ICH
        let ilk = self.get_ilk_for_ich(&ich)
            .ok_or("ICH has no ILK")?;
        
        // 3. Calcular CTX
        let ctx = CtxClient::compute_ctx(&ilk, &ich);
        
        // 4. Obtener último seq conocido (de cache local o 0)
        let ctx_seq = self.local_ctx_seq.get(&ctx).copied().unwrap_or(0);
        
        // 5. Armar mensaje
        let msg = Message {
            routing: Routing { dst: None, .. },
            meta: Meta {
                src_ilk: Some(ilk),
                ich: Some(ich),
                ctx: Some(ctx),
                ctx_seq: Some(ctx_seq),
                ..
            },
            payload: webhook.to_payload(),
        };
        
        // 6. Enviar
        self.send(msg).await
    }
}
```

---

## 11. ICH en SHM (jsr-identity)

### 11.1 Estructura IchEntry

```rust
#[repr(C)]
pub struct IchEntry {
    pub ich: [u8; 48],              // "ich:<uuid>"
    pub channel_type: [u8; 16],     // "whatsapp", "slack", etc.
    pub external_id: [u8; 128],     // "+5491155551234", etc.
    pub ilk: [u8; 48],              // ILK dueño
    pub flags: u16,
    pub _pad: [u8; 14],
}
// Total: 256 bytes

#[repr(C)]
pub struct IchMappingEntry {
    pub channel_type: [u8; 16],
    pub external_id: [u8; 128],
    pub ich: [u8; 48],
    pub flags: u16,
    pub _pad: [u8; 62],
}
// Total: 256 bytes (para lookup rápido)
```

### 11.2 Layout actualizado de jsr-identity

```
┌────────────────────────────────────────────────────────────────┐
│ IdentityHeader (128 bytes)                                      │
├────────────────────────────────────────────────────────────────┤
│ IlkEntry[MAX_ILKS] (8192 × 256 bytes = ~2 MB)                  │
├────────────────────────────────────────────────────────────────┤
│ IchEntry[MAX_ICHS] (4096 × 256 bytes = ~1 MB)                  │
├────────────────────────────────────────────────────────────────┤
│ IchMappingEntry[MAX_ICH_MAPPINGS] (8192 × 256 bytes = ~2 MB)   │
├────────────────────────────────────────────────────────────────┤
│ ModuleEntry[MAX_MODULES] (1024 × 512 bytes = ~512 KB)          │
├────────────────────────────────────────────────────────────────┤
│ DegreeEntry[MAX_DEGREES] (512 × 1024 bytes = ~512 KB)          │
└────────────────────────────────────────────────────────────────┘
Total aproximado: ~6 MB por isla
```

---

## 12. Constantes

```rust
// Context
pub const CTX_HASH_LEN: usize = 32;  // 32 hex chars = 16 bytes

// ICH
pub const MAX_ICHS: u32 = 4096;
pub const MAX_ICH_MAPPINGS: u32 = 8192;

// Channel types
pub const CHANNEL_TYPE_WHATSAPP: &str = "whatsapp";
pub const CHANNEL_TYPE_SLACK: &str = "slack";
pub const CHANNEL_TYPE_EMAIL: &str = "email";
pub const CHANNEL_TYPE_TELEGRAM: &str = "telegram";
pub const CHANNEL_TYPE_INSTAGRAM: &str = "instagram";
pub const CHANNEL_TYPE_WEBCHAT: &str = "webchat";
pub const CHANNEL_TYPE_INTERNAL: &str = "internal";

// ctx_client defaults
pub const CTX_CACHE_MAX_CONTEXTS: usize = 1000;
pub const CTX_CACHE_MAX_AGE_SECS: u64 = 3600;  // 1 hour
pub const CTX_DB_QUERY_TIMEOUT_MS: u64 = 5000;
```

---

## 13. Cleanup y Mantenimiento

### 13.1 Contextos Inactivos

```sql
-- Marcar contextos inactivos (sin actividad en 7 días)
UPDATE contexts 
SET status = 'archived' 
WHERE last_activity < now() - interval '7 days'
  AND status = 'active';

-- Mover turns viejos a tabla de archivo (opcional)
INSERT INTO turns_archive 
SELECT * FROM turns t
JOIN contexts c ON t.ctx = c.ctx
WHERE c.status = 'archived';

DELETE FROM turns t
USING contexts c
WHERE t.ctx = c.ctx AND c.status = 'archived';
```

### 13.2 Job de Mantenimiento

```yaml
# Cron o job de SY.admin
context_maintenance:
  archive_after_days: 7
  delete_archived_after_days: 90
  run_at: "03:00"  # 3 AM
```

---

## 14. Dependencia de PostgreSQL

### 14.1 ¿Es crítico?

| Escenario | Sin PostgreSQL |
|-----------|----------------|
| Router recibe mensaje | Routing funciona, pero no persiste turns |
| AI necesita contexto | Cache miss → falla (no puede reconstruir historia) |
| Nueva conversación | Funciona (ctx_seq empieza en 0) |
| Conversación existente | Falla si no está en cache del nodo |

**PostgreSQL es crítico para conversaciones con historia.**

### 14.2 Mitigaciones

1. **Cache agresivo** — Nodos mantienen contextos activos en memoria
2. **Retry en escritura** — Router reintenta INSERT si falla
3. **PostgreSQL HA** — Replicación master-standby
4. **Graceful degradation** — Si DB muere, nuevas conversaciones funcionan

### 14.3 Futuro: Redis como cache

Si la latencia se vuelve problema:

```
Router → Redis (fast) → PostgreSQL (async)
Nodo → Cache local → Redis → PostgreSQL
```

Pero empezamos con PostgreSQL solo.

---

## 15. Referencias

| Tema | Documento |
|------|-----------|
| ILK e Identity | `10-identity-layer3.md` |
| Estructura de mensaje | `02-protocolo.md` |
| Router | `04-routing.md` |
| Operaciones | `07-operaciones.md` |
