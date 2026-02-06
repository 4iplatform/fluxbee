# Fluxbee - 12 Cognición (SY.cognition)

**Estado:** v1.16  
**Fecha:** 2026-02-05  
**Audiencia:** Desarrolladores de router core, infraestructura, AI

---

## 1. Resumen

El sistema cognitivo permite que Fluxbee **aprenda de la experiencia** sin acumular historial infinito ni requerir configuración manual. Opera en paralelo al flujo de mensajes, consolidando evidencia en memorias accionables.

```
┌─────────────────────────────────────────────────────────────────┐
│                         CAPAS DE MEMORIA                        │
├─────────────────────────────────────────────────────────────────┤
│  ctx_window (20 turns)                                          │
│  "Lo que está pasando AHORA"                                    │
│  Fuente: Router (inline)                                        │
├─────────────────────────────────────────────────────────────────┤
│  memory_package (episodios + items)                             │
│  "Lo que viví antes con patrones similares"                     │
│  Fuente: jsr-memory → LanceDB                                   │
├─────────────────────────────────────────────────────────────────┤
│  jsr-memory (índice de activación)                              │
│  "Qué antecedentes son relevantes para estos tags"              │
│  Fuente: SY.cognition (single-writer)                           │
├─────────────────────────────────────────────────────────────────┤
│  PostgreSQL (evidencia inmutable)                               │
│  "Todo lo que pasó, para auditoría y reconstrucción"            │
│  Fuente: Router (async)                                         │
└─────────────────────────────────────────────────────────────────┘
```

**Principios clave:**

1. **Evidencia inmutable** — Los turns son la fuente de verdad, nunca se editan
2. **Consolidación por episodios** — La experiencia se agrupa en eventos con boundaries claros
3. **Recuperación por tags** — Los antecedentes se buscan por cues normalizados, no texto libre
4. **Prioridad dinámica** — Lo que cambia es la activación, no la evidencia
5. **Cache reconstruible** — LanceDB es derivada de PostgreSQL, puede regenerarse

---

## 2. Arquitectura de Recursos

### 2.1 Separación de Responsabilidades

```
┌─────────────────────────────────────────────────────────────────┐
│                     ISLA MADRE (PostgreSQL)                     │
│                                                                 │
│  ┌───────────────────────────────────────────────────────────┐ │
│  │ PostgreSQL                                                 │ │
│  │ ├── turns (evidencia inmutable)                           │ │
│  │ └── contexts (metadata de conversaciones)                 │ │
│  │                                                           │ │
│  │ Source of truth. Auditoría. Backup.                       │ │
│  └───────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────┘
           │
           │ Todas las islas escriben aquí (async)
           │ SY.cognition lee de aquí
           │
┌──────────┴──────────────────────────────────────────────────────┐
│                     ISLA HIJA (o Madre también)                 │
│                                                                 │
│  /dev/shm/                                                      │
│  └── jsr-memory-<island>     ← Índice de activación (SHM)      │
│      Writer: SY.cognition                                       │
│      Readers: Router, nodos                                     │
│                                                                 │
│  /var/lib/fluxbee/                                             │
│  └── memory.lance            ← Cache cognitiva (LanceDB)       │
│      Writer: SY.cognition                                       │
│      Readers: Router, nodos                                     │
│                                                                 │
│  Ambos son RECONSTRUIBLES desde PostgreSQL                     │
└─────────────────────────────────────────────────────────────────┘
```

### 2.2 PostgreSQL: Source of Truth

PostgreSQL centralizado en la isla madre. Todas las islas escriben aquí.

```sql
-- Evidencia inmutable
CREATE TABLE turns (
    ctx         TEXT NOT NULL,
    seq         BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now(),
    from_ilk    TEXT NOT NULL,
    to_ilk      TEXT,
    ich         TEXT NOT NULL,
    msg_type    TEXT NOT NULL,
    content     JSONB NOT NULL,
    tags        TEXT[],           -- Tags extraídos (puede ser NULL inicialmente)
    PRIMARY KEY (ctx, seq)
);

CREATE INDEX idx_turns_ctx ON turns (ctx);
CREATE INDEX idx_turns_ts ON turns (ts);
CREATE INDEX idx_turns_ilk ON turns (from_ilk);

-- Metadata de contextos
CREATE TABLE contexts (
    ctx             TEXT PRIMARY KEY,
    ilk             TEXT NOT NULL,
    ich             TEXT NOT NULL,
    tenant_ilk      TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_activity   TIMESTAMPTZ NOT NULL DEFAULT now(),
    turn_count      BIGINT NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'active',
    metadata        JSONB
);
```

**¿Por qué centralizado?**

- Un solo lugar para auditoría y compliance
- Backup simple
- Las islas hijas son workers, no necesitan PostgreSQL propio
- La escritura es async, no afecta el hot path

### 2.3 LanceDB: Cache Cognitiva Local

LanceDB embebida en cada isla. **Es reconstruible desde PostgreSQL.**

```
/var/lib/fluxbee/memory.lance
├── events/          # Episodios consolidados
├── highlights/      # Turns verbatim seleccionados
├── items/           # Facts, procedures, constraints derivados
└── links/           # Asociaciones entre eventos/items
```

**Características:**

- Serverless (no requiere proceso separado)
- Vectores nativos (para fallback semántico)
- Puede borrarse y reconstruirse
- Una por isla

**¿Qué pasa si LanceDB se corrompe?**

```
1. SY.cognition detecta corrupción o isla nueva
2. Lee turns de PostgreSQL (filtrado por tenant/isla)
3. Re-ejecuta consolidación: boundaries → eventos → items
4. Re-escribe LanceDB
5. Re-genera jsr-memory
6. Sistema operativo en minutos
```

### 2.4 jsr-memory: Índice de Activación (SHM)

Región de memoria compartida para consultas de micro-latencia.

```
/dev/shm/jsr-memory-<island>
```

**Contenido:**

- Inverted index: `tag → [event_ids con activation_strength]`
- EventHeaders mínimos para gating rápido
- Epoch para consistencia lock-free

**No contiene texto** — solo IDs y scores para decidir qué traer de LanceDB.

---

## 3. SY.cognition: El Proceso Cognitivo

### 3.1 Responsabilidades

SY.cognition es el único proceso que escribe en LanceDB y jsr-memory:

```
SY.cognition
│
├── INPUTS (lee):
│   ├── turns de PostgreSQL (poll o notify)
│   ├── reactivation events (de nodos AI)
│   └── LSA de otras islas (via Gateway)
│
├── PROCESA:
│   ├── Segmenta turns en episodios (boundaries)
│   ├── Extrae tags (via nodos AI)
│   ├── Selecciona highlights verbatim
│   ├── Deriva items (facts/procedures/constraints)
│   ├── Calcula activation_strength
│   └── Ajusta prioridades por reactivación
│
├── OUTPUTS (escribe):
│   ├── LanceDB: EventPackages
│   ├── jsr-memory: índice de tags
│   └── LSA de activación (para otras islas)
│
└── WORKERS (orquesta):
    ├── AI.tagger: extrae tags de turns
    └── AI.consolidator: deriva items de episodios
```

### 3.2 SY.cognition No Es AI

SY.cognition es un **orquestador**, no un modelo de lenguaje. Coordina nodos AI para el trabajo pesado:

```rust
impl Cognition {
    async fn process_pending_turns(&mut self) -> Result<()> {
        // 1. Leer turns sin procesar
        let turns = self.fetch_unprocessed_turns().await?;
        
        // 2. Detectar boundaries (reglas, no AI)
        let episodes = self.detect_boundaries(&turns);
        
        // 3. Para cada episodio nuevo
        for episode in episodes {
            // Extraer tags via AI.tagger
            let tags = self.call_ai_tagger(&episode).await?;
            
            // Derivar items via AI.consolidator
            let items = self.call_ai_consolidator(&episode, &tags).await?;
            
            // Escribir en LanceDB
            self.write_event_package(&episode, &tags, &items).await?;
            
            // Actualizar índice SHM
            self.update_memory_index(&episode, &tags).await?;
        }
        
        Ok(())
    }
}
```

### 3.3 Boundaries: Cuándo Termina un Episodio

Un episodio se cierra cuando:

| Trigger | Descripción |
|---------|-------------|
| `resolved` | Outcome explícito de resolución |
| `escalated` | Transferencia a otro agente |
| `timeout` | Sin actividad por N minutos |
| `topic_change` | Cambio claro de tema (detectado por AI) |
| `session_end` | Usuario cierra sesión/canal |

```rust
fn detect_boundaries(&self, turns: &[Turn]) -> Vec<Episode> {
    let mut episodes = vec![];
    let mut current_start = 0;
    
    for (i, turn) in turns.iter().enumerate() {
        if self.is_boundary(turn, &turns[current_start..i]) {
            episodes.push(Episode {
                ctx: turn.ctx.clone(),
                start_seq: turns[current_start].seq,
                end_seq: turn.seq,
                boundary_reason: self.classify_boundary(turn),
            });
            current_start = i + 1;
        }
    }
    
    episodes
}
```

---

## 4. Modelo de Datos

### 4.1 Turn (Evidencia) — PostgreSQL

```rust
pub struct Turn {
    pub ctx: String,
    pub seq: i64,
    pub ts: DateTime<Utc>,
    pub from_ilk: String,
    pub to_ilk: Option<String>,
    pub ich: String,
    pub msg_type: String,
    pub content: serde_json::Value,
    pub tags: Option<Vec<String>>,
}
```

### 4.2 Event (Episodio) — LanceDB

```rust
pub struct Event {
    pub event_id: i64,
    pub ctx: String,
    pub start_seq: i64,
    pub end_seq: i64,
    pub boundary_reason: String,  // resolved, escalated, timeout, etc.
    pub cues_agg: Vec<String>,    // Tags agregados del episodio
    pub outcome: Outcome,
    pub priority_state: PriorityState,
    pub created_at: DateTime<Utc>,
}

pub struct Outcome {
    pub status: String,           // resolved, failed, escalated, timeout
    pub duration_ms: i64,
    pub escalations: i32,
}

pub struct PriorityState {
    pub activation_strength: f32, // 0.0 - 1.0
    pub context_inhibition: f32,  // 0.0 - 1.0
    pub last_used_at: Option<DateTime<Utc>>,
    pub use_count: i32,
    pub success_count: i32,
}
```

### 4.3 Highlight (Verbatim) — LanceDB

```rust
pub struct Highlight {
    pub event_id: i64,
    pub seq: i64,
    pub role: String,             // user, assistant
    pub text: String,             // Contenido verbatim
    pub tags: Vec<String>,
}
```

### 4.4 MemoryItem (Derivado) — LanceDB

```rust
pub struct MemoryItem {
    pub memory_id: String,        // "mem:<event_id>:<type>:<n>"
    pub event_id: i64,
    pub item_type: ItemType,
    pub content: serde_json::Value,
    pub confidence: f32,
    pub cues_signature: Vec<String>,
    pub evidence_refs: Vec<i64>,  // seqs dentro del evento
    pub priority_state: PriorityState,
}

pub enum ItemType {
    Fact,           // Dato verificable
    Procedure,      // Secuencia de pasos
    Constraint,     // Regla/limitación
    Preference,     // Preferencia del usuario
    Mapping,        // Equivalencia/alias
}
```

### 4.5 Link (Asociación) — LanceDB

```rust
pub struct Link {
    pub from_kind: String,        // "event" o "memory"
    pub from_id: String,
    pub to_kind: String,
    pub to_id: String,
    pub link_type: String,        // cooccurrence, reactivation, causal, contradiction
    pub weight: f32,
    pub last_activated_at: DateTime<Utc>,
}
```

---

## 5. jsr-memory: Diseño del Índice SHM

### 5.1 Objetivo

Dado `cues_turn[]` (tags), devolver en micro-latencia:

- `candidate_event_ids[]` (Top-K)
- Con `activation_strength` y `context_inhibition`
- EventHeaders mínimos para gating (tenant/scope)

### 5.2 Layout

```
/dev/shm/jsr-memory-<island>
│
├── Header (64 bytes)
│   ├── magic: u32              // 0x4A534D45 "JSME"
│   ├── version: u32
│   ├── epoch: u64
│   ├── tenant_count: u32
│   ├── total_events: u32
│   ├── total_tags: u32
│   └── _reserved
│
├── TenantPartition[] (variable)
│   ├── tenant_id: [u8; 48]
│   ├── tag_table_offset: u64
│   ├── posting_pool_offset: u64
│   └── event_header_offset: u64
│
├── TagTable (por tenant)
│   ├── tag_count: u32
│   └── TagEntry[]
│       ├── tag_hash: u64
│       ├── tag_string: [u8; 64]
│       ├── posting_offset: u32
│       └── posting_len: u32
│
├── PostingPool (por tenant)
│   └── Posting[]
│       ├── event_id: u32
│       ├── activation_strength: f32
│       ├── context_inhibition: f32
│       ├── scope_bits: u16
│       └── last_used_bucket: u16
│
└── EventHeaderTable (por tenant)
    └── EventHeader[]
        ├── event_id: u32
        ├── ctx_hash: u64
        ├── start_seq: u32
        ├── end_seq: u32
        ├── intent_primary: [u8; 32]
        ├── outcome_status: u8
        └── origin_island_id: u8
```

### 5.3 Constantes

```rust
pub const MEMORY_MAGIC: u32 = 0x4A534D45;  // "JSME"
pub const MEMORY_VERSION: u32 = 1;

pub const MAX_TENANTS: usize = 256;
pub const MAX_TAGS_PER_TENANT: usize = 16384;
pub const MAX_EVENTS_PER_TENANT: usize = 65536;
pub const MAX_POSTINGS_PER_TAG: usize = 1024;

pub const TAG_MAX_LEN: usize = 64;
pub const TOP_K_DEFAULT: usize = 10;
```

### 5.4 Algoritmo de Consulta

```rust
impl MemoryIndex {
    pub fn query(&self, tenant_id: &str, cues: &[String], top_k: usize) -> Vec<EventCandidate> {
        // 1. Encontrar partición del tenant
        let partition = self.find_tenant(tenant_id)?;
        
        // 2. Mapear tags a posting lists
        let mut posting_lists: Vec<&[Posting]> = vec![];
        for cue in cues {
            if let Some(postings) = partition.get_postings(cue) {
                posting_lists.push(postings);
            }
        }
        
        // 3. Intersecar (empezar por lista más corta)
        posting_lists.sort_by_key(|p| p.len());
        let candidates = self.intersect_postings(&posting_lists);
        
        // 4. Calcular score y ordenar
        let mut scored: Vec<_> = candidates.iter()
            .map(|p| {
                let score = p.activation_strength * (1.0 - p.context_inhibition);
                (p.event_id, score)
            })
            .collect();
        
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        
        // 5. Devolver Top-K con headers
        scored.truncate(top_k);
        scored.iter()
            .map(|(id, score)| EventCandidate {
                event_id: *id,
                score: *score,
                header: partition.get_header(*id),
            })
            .collect()
    }
}
```

### 5.5 Publicación (Single Writer)

```rust
impl CognitionWriter {
    pub fn publish_index(&mut self, events: &[Event], tags: &HashMap<String, Vec<i64>>) {
        // 1. Construir nuevas estructuras en shadow region
        let shadow = self.build_shadow_index(events, tags);
        
        // 2. Incrementar epoch
        let new_epoch = self.header.epoch + 1;
        shadow.header.epoch = new_epoch;
        
        // 3. Swap atómico
        std::ptr::swap(&mut self.active_region, &mut shadow);
        
        // 4. Publicar epoch (los readers lo ven)
        self.header.epoch.store(new_epoch, Ordering::Release);
        
        // 5. Esperar a que readers terminen con región vieja (RCU)
        std::thread::sleep(Duration::from_millis(10));
    }
}
```

---

## 6. Vocabulario de Tags (Cues)

### 6.1 Namespaces

| Namespace | Descripción | Ejemplo |
|-----------|-------------|---------|
| `intent:` | Clasificación de tarea | `intent:billing.issue` |
| `ent:` | Entidades (IDs, productos) | `ent:invoice_id:INV-123` |
| `kw:` | Keywords normalizadas | `kw:doble_cobro` |
| `tenant:` | Tenant operativo | `tenant:acme` |
| `chan:` | Canal de comunicación | `chan:whatsapp` |
| `topic:` | Cluster semántico | `topic:facturacion` |

### 6.2 Normalización

```rust
fn normalize_tag(raw: &str) -> String {
    raw.to_lowercase()
        .replace(['á', 'à', 'ä'], "a")
        .replace(['é', 'è', 'ë'], "e")
        .replace(['í', 'ì', 'ï'], "i")
        .replace(['ó', 'ò', 'ö'], "o")
        .replace(['ú', 'ù', 'ü'], "u")
        .replace(' ', "_")
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ':' || *c == '.')
        .collect()
}
```

### 6.3 Extracción de Tags

**Tags exactos (metadata, sin AI):**

```rust
fn extract_exact_tags(msg: &Message) -> Vec<String> {
    let mut tags = vec![];
    
    // Del routing/meta
    if let Some(ich) = &msg.meta.ich {
        if ich.contains("whatsapp") {
            tags.push("chan:whatsapp".into());
        }
        // ...
    }
    
    // Del tenant (derivado de src_ilk)
    if let Some(tenant) = derive_tenant(&msg.meta.src_ilk) {
        tags.push(format!("tenant:{}", tenant));
    }
    
    tags
}
```

**Tags semánticos (requiere AI):**

```rust
// SY.cognition llama a AI.tagger (async, no en hot path)
async fn extract_semantic_tags(&self, episode: &Episode) -> Vec<String> {
    let request = TaggerRequest {
        turns: episode.get_turns(),
        extract: vec!["intent", "entities", "keywords"],
    };
    
    let response = self.call_node("AI.tagger", request).await?;
    response.tags
}
```

### 6.4 Flujo de Enriquecimiento (Opción C)

```
Primera interacción:
├── Router extrae tags exactos (chan, tenant)
├── jsr-memory: pocos candidatos o ninguno
├── Forward con ctx_window, sin memory_package rico
├── AI.soporte responde (funciona, sin antecedentes)
└── SY.cognition (async):
    ├── Lee turns de PostgreSQL
    ├── Llama AI.tagger → tags semánticos
    ├── Consolida episodio
    └── Actualiza jsr-memory

Segunda interacción (similar):
├── Router extrae tags exactos
├── jsr-memory: encuentra episodio anterior
├── Fetch LanceDB → memory_package
├── Forward con ctx_window + memory_package
└── AI.soporte responde con antecedentes
```

---

## 7. MemoryPackage: Contrato de Entrega

### 7.1 Estructura

```json
{
  "package_version": "1.0",
  "query": {
    "ctx": "ctx:9f2c...",
    "ilk": "ilk:john-doe",
    "ich": "ich:whatsapp:+549...",
    "turn_seq": 104,
    "cues_turn": ["intent:billing.issue", "kw:doble_cobro", "ent:invoice_id:INV-123"],
    "limits": {
      "max_events": 3,
      "max_items": 12,
      "max_highlights_per_event": 8
    }
  },
  "events": [
    {
      "event_id": 98,
      "header": {
        "ctx": "ctx:9f2c...",
        "start_seq": 61,
        "end_seq": 78,
        "boundary_reason": "resolved",
        "cues_agg": ["intent:billing.issue", "kw:doble_cobro"],
        "outcome": {
          "status": "resolved",
          "duration_ms": 420000,
          "escalations": 1
        },
        "priority_state": {
          "activation_strength": 0.82,
          "context_inhibition": 0.05,
          "last_used_at": "2026-02-05T17:00:00Z"
        }
      },
      "highlights": [
        {
          "seq": 62,
          "role": "user",
          "text": "Me cobraron dos veces la factura INV-123.",
          "tags": ["kw:doble_cobro", "ent:invoice_id:INV-123"]
        },
        {
          "seq": 66,
          "role": "assistant",
          "text": "Identifiqué el duplicado: se generó un reintento.",
          "tags": ["kw:reverso", "kw:reintento"]
        }
      ],
      "items": [
        {
          "memory_id": "mem:98:fact:1",
          "type": "fact",
          "confidence": 0.95,
          "content": {
            "invoice_id": "INV-123",
            "issue_type": "doble_cobro"
          },
          "cues_signature": ["intent:billing.issue", "ent:invoice_id:INV-123"],
          "evidence_refs": [62]
        },
        {
          "memory_id": "mem:98:procedure:1",
          "type": "procedure",
          "confidence": 0.75,
          "content": {
            "name": "doble_cobro_triage",
            "steps": [
              "confirmar si duplicado es en medio de pago o en factura",
              "verificar reintentos y correlacionar timestamps",
              "si reintento confirmado: iniciar reverso"
            ]
          },
          "cues_signature": ["intent:billing.issue", "kw:doble_cobro"],
          "evidence_refs": [66, 74]
        }
      ]
    }
  ],
  "evidence_expand": {
    "available": true,
    "how": "fetch_turn_range",
    "ranges": [{"event_id": 98, "ctx": "ctx:9f2c...", "start_seq": 61, "end_seq": 78}]
  }
}
```

### 7.2 Campos en meta (mensaje enriquecido)

```json
{
  "meta": {
    "src_ilk": "ilk:john-doe",
    "ich": "ich:wapp-john",
    "ctx": "ctx:abc123...",
    "ctx_seq": 47,
    "ctx_window": [...],
    "memory_package": { ... }
  }
}
```

---

## 8. Flujo Completo

### 8.1 Online (Síncrono)

```
IO.whatsapp recibe mensaje
        │
        ▼
Router
├── 1. Persiste turn en PostgreSQL Madre (async)
├── 2. Extrae tags exactos (metadata, sin AI)
├── 3. Consulta jsr-memory con tags (SHM, µs)
│       └── Top-K event_ids
├── 4. Fetch MemoryPackage de LanceDB (ms, timeout 50ms)
├── 5. Arma ctx_window (últimos 20 turns)
├── 6. Forward con ctx_window + memory_package
        │
        ▼
AI.soporte
├── Usa ctx_window (foco actual)
├── Usa memory_package.events[].highlights (vivencias)
├── Usa memory_package.events[].items (procedures, constraints)
├── Responde
└── Emite reactivation event
```

### 8.2 Offline (Paralelo)

```
SY.cognition (loop continuo)
        │
        ├── Lee turns nuevos de PostgreSQL
        │
        ├── Detecta boundaries → episodios
        │       └── Reglas simples, no AI
        │
        ├── Para cada episodio:
        │   ├── Llama AI.tagger → tags semánticos
        │   ├── Selecciona highlights (top N turns)
        │   ├── Llama AI.consolidator → items derivados
        │   ├── Calcula activation_strength inicial
        │   └── Escribe EventPackage en LanceDB
        │
        ├── Actualiza jsr-memory (epoch bump)
        │
        ├── Lee reactivation events
        │   ├── Ajusta activation_strength por uso/éxito
        │   └── Ajusta context_inhibition si aplica
        │
        └── Si activation > threshold:
            └── Publica en LSA para otras islas
```

### 8.3 Cold Start

Cuando una isla arranca sin memoria:

```
1. jsr-memory vacío → consultas devuelven []
2. Router forward sin memory_package (solo ctx_window)
3. AI.soporte funciona (degradado, sin antecedentes)
4. SY.cognition lee turns de PostgreSQL Madre
5. Re-genera episodios → LanceDB → jsr-memory
6. Próximas consultas ya tienen memoria
```

### 8.4 Reconstrucción desde PostgreSQL

Si LanceDB se corrompe o es isla nueva:

```rust
impl Cognition {
    async fn rebuild_from_postgres(&mut self) -> Result<()> {
        log::info!("Rebuilding cognitive cache from PostgreSQL...");
        
        // 1. Limpiar LanceDB
        self.lancedb.clear()?;
        
        // 2. Leer todos los turns relevantes
        let turns = sqlx::query_as!(Turn,
            "SELECT * FROM turns WHERE ctx IN (
                SELECT ctx FROM contexts WHERE tenant_ilk = $1
            ) ORDER BY ctx, seq",
            self.tenant_ilk
        )
        .fetch_all(&self.pg_pool)
        .await?;
        
        // 3. Re-procesar (igual que process_pending_turns)
        let episodes = self.detect_boundaries(&turns);
        for episode in episodes {
            let tags = self.call_ai_tagger(&episode).await?;
            let items = self.call_ai_consolidator(&episode, &tags).await?;
            self.write_event_package(&episode, &tags, &items).await?;
        }
        
        // 4. Regenerar índice SHM
        self.rebuild_memory_index().await?;
        
        log::info!("Rebuild complete: {} episodes", episodes.len());
        Ok(())
    }
}
```

---

## 9. Reactivación y Aprendizaje

### 9.1 Evento de Reactivación

Cuando un nodo AI usa memoria, emite:

```json
{
  "type": "memory.reactivation",
  "ctx": "ctx:9f2c...",
  "turn_seq": 104,
  "reactivated": {
    "events": [
      {"event_id": 98, "used": true},
      {"event_id": 122, "used": false}
    ],
    "items": [
      {"memory_id": "mem:98:procedure:1", "used": true, "success": true}
    ]
  },
  "outcome": {
    "status": "resolved",
    "duration_ms": 180000
  }
}
```

### 9.2 Ajuste de Prioridad

```rust
impl Cognition {
    fn process_reactivation(&mut self, event: ReactivationEvent) {
        for ev in &event.reactivated.events {
            let entry = self.get_event_mut(ev.event_id);
            
            if ev.used {
                // Fue recuperado y usado → reforzar
                entry.priority_state.activation_strength *= 1.1;
                entry.priority_state.use_count += 1;
                
                if event.outcome.status == "resolved" {
                    entry.priority_state.success_count += 1;
                }
            } else {
                // Fue recuperado pero no usado → debilitar levemente
                entry.priority_state.activation_strength *= 0.95;
            }
            
            entry.priority_state.last_used_at = Some(Utc::now());
            
            // Clamp
            entry.priority_state.activation_strength = 
                entry.priority_state.activation_strength.clamp(0.1, 1.0);
        }
        
        // Republicar índice
        self.publish_index();
    }
}
```

### 9.3 Reemplazo Funcional (Sin Borrado)

Si un procedimiento deja de funcionar:

```
1. Reactivaciones con outcome=failed incrementan
2. activation_strength baja
3. context_inhibition sube para ese contexto
4. Nuevas experiencias generan items competidores
5. Índice empieza a devolver primero los nuevos
6. El viejo queda pero inhibido (no se borra)
```

**Esto implementa "cambió la importancia" sin reescribir historia.**

---

## 10. Sincronización Inter-Isla (LSA)

### 10.1 LSA de Activación

El Gateway no replica todo LanceDB. Propaga punteros:

```json
{
  "meta": {
    "type": "system",
    "msg": "LSA_MEMORY"
  },
  "payload": {
    "island": "produccion",
    "events": [
      {
        "event_id": 98,
        "cues_signature_digest": "a1b2c3d4",
        "intent_primary": "billing.issue",
        "tenant_id": "acme",
        "activation_strength": 0.82,
        "package_digest": "xyz789"
      }
    ]
  }
}
```

### 10.2 Fetch On-Demand

Si Isla B necesita un evento de Isla A:

```
1. Isla B consulta jsr-memory local
2. Encuentra event_id con origin_island=A
3. Envía SY.memory.fetch a Gateway
4. Gateway A responde con EventPackage
5. Isla B persiste en LanceDB local (cache)
6. Próximas consultas usan cache local
```

---

## 11. Constantes Consolidadas

```rust
// jsr-memory
pub const MEMORY_MAGIC: u32 = 0x4A534D45;  // "JSME"
pub const MEMORY_VERSION: u32 = 1;
pub const MAX_TENANTS: usize = 256;
pub const MAX_TAGS_PER_TENANT: usize = 16384;
pub const MAX_EVENTS_PER_TENANT: usize = 65536;

// Consultas
pub const TOP_K_DEFAULT: usize = 10;
pub const MEMORY_FETCH_TIMEOUT_MS: u64 = 50;

// Prioridad
pub const ACTIVATION_INITIAL: f32 = 0.5;
pub const ACTIVATION_MIN: f32 = 0.1;
pub const ACTIVATION_MAX: f32 = 1.0;
pub const INHIBITION_THRESHOLD: f32 = 0.8;

// Consolidación
pub const EPISODE_TIMEOUT_MINUTES: u64 = 30;
pub const MAX_HIGHLIGHTS_PER_EVENT: usize = 8;
pub const MAX_ITEMS_PER_EVENT: usize = 10;

// LanceDB
pub const LANCEDB_PATH: &str = "/var/lib/fluxbee/memory.lance";
```

---

## 12. Comparación: Antes vs Después

| Aspecto | Sin Cognición | Con Cognición |
|---------|---------------|---------------|
| Primera interacción | Sin contexto | Sin contexto (igual) |
| Segunda interacción | 20 turns (ctx_window) | 20 turns + episodios similares |
| Usuario recurrente | Solo esta conversación | Patrones de conversaciones anteriores |
| Procedimiento aprendido | Manual (degree/OPA) | Automático (procedures derivados) |
| Preferencias | No recordadas | Extraídas y recuperadas |
| Escalabilidad | Lineal con historial | Constante (consolidado) |

---

## 13. Referencias

| Tema | Documento |
|------|-----------|
| Contexto (ICH, CTX, turns) | `11-context.md` |
| Routing | `04-routing.md` |
| Shared Memory | `03-shm.md` |
| Protocolo de mensajes | `02-protocolo.md` |
| Operaciones | `07-operaciones.md` |
| Nodos SY | `SY_nodes_spec.md` |
