# JSON Router - 02 Protocolo de Mensajes

**Estado:** v1.16  
**Fecha:** 2026-02-04  
**Audiencia:** Desarrolladores de librería de nodo, desarrolladores de nodos

---

## 1. Estructura del Mensaje

Todo mensaje en la red tiene tres secciones:

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": { ... }
}
```

---

## 2. Sección `routing` (Header de Red)

Usado por el router para decisiones de capa 1. El router DEBE poder tomar decisiones leyendo SOLO esta sección.

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "uuid-destino | nombre-l2@hive | null | \"broadcast\"",
    "ttl": 16,
    "trace_id": "uuid-correlación"
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `src` | UUID | Sí | Nodo origen del mensaje |
| `dst` | UUID, nombre L2, "broadcast", o null | Sí | Unicast (UUID o nombre) / Broadcast / Resolve via OPA |
| `ttl` | int | Sí | Time-to-live, decrementa por hop WAN y broadcast |
| `trace_id` | UUID | Sí | ID de correlación para trazabilidad |

### 2.1 Valores de `dst`

| Valor | Comportamiento |
|-------|----------------|
| UUID | Unicast directo a ese nodo |
| Nombre L2 (`SY.admin@motherbee`) | Resolve directo en FIB por nombre (sin OPA) |
| `"broadcast"` | Entregar a todos (filtrado por `meta.target` si existe) |
| `null` | Resolver destino via OPA usando `meta.target` |

---

## 3. Sección `meta` (Metadata para OPA y Sistema)

Usado por OPA para decisiones de capa 2/3, por el router para broadcast filtrado, persistencia de contexto y enriquecimiento cognitivo.

```json
{
  "meta": {
    "type": "user",
    "scope": "vpn",
    "target": "AI.soporte.l1@produccion",
    "src_ilk": "ilk:550e8400-e29b-41d4-a716-446655440000",
    "dst_ilk": "ilk:7c9e6679-7425-40de-944b-e07fc1f90ae7",
    "ich": "ich:a1b2c3d4-5678-90ab-cdef-1234567890ab",
    "ctx": "ctx:abc123def456789012345678901234ab",
    "ctx_seq": 47,
    "ctx_window": [
      { "seq": 28, "ts": "2026-02-04T14:30:00Z", "from": "ilk:550e8400-...", "type": "text", "text": "Hola" },
      { "seq": 29, "ts": "2026-02-04T14:30:05Z", "from": "ilk:7c9e6679-...", "type": "text", "text": "¿En qué puedo ayudarte?" },
      { "seq": 47, "ts": "2026-02-04T15:00:00Z", "from": "ilk:550e8400-...", "type": "text", "text": "Tengo un problema con mi factura" }
    ],
    "memory_package": {
      "events": [...],
      "evidence_expand": {...}
    },
    "priority": "high",
    "context": {
      "intent": "billing_question"
    }
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `type` | string | Sí | Tipo de mensaje: `"user"`, `"system"`, `"admin"` |
| `msg` | string | Sí si type=system | Tipo de mensaje de sistema (ej: `"HELLO"`, `"LSA"`) |
| `scope` | string | No | Alcance VPN para broadcast/multicast: `"vpn"` (default) o `"global"` (solo system) |
| `target` | string | Condicional | Para OPA o broadcast filter (nombre L2 con @isla) |
| `src_ilk` | string | Sí (L3) | ILK del interlocutor que envía. OPA deriva tenant via `data.identity` |
| `dst_ilk` | string | No | ILK del interlocutor destino (si se conoce) |
| `ich` | string | Sí (L3) | ICH (Interlocutor Channel) por el cual se comunica |
| `ctx` | string | Sí (L3) | Context ID. Calculado: `hash(src_ilk + ich)` |
| `ctx_seq` | integer | Sí (L3) | Último número de secuencia conocido del contexto |
| `ctx_window` | array | Sí (L3) | Últimos 20 turns del contexto. Agregado por router |
| `memory_package` | object | No | Antecedentes episódicos relevantes. Agregado por router |
| `priority` | string | No | Hint de prioridad para OPA |
| `context` | object | No | Datos adicionales para reglas OPA |
| `action` | string | No | Para mensajes admin: acción a ejecutar |

**Regla:** si `type="system"` y `scope="global"`, el router ignora el filtro de VPN para broadcast/multicast.

### 3.1 Uso de `target` según contexto

| `dst` | `meta.target` | Comportamiento |
|-------|---------------|----------------|
| `null` | nombre L2 | OPA resuelve destino usando target |
| `"broadcast"` | patrón L2 | Router filtra: solo entrega a nodos que matchean |
| `"broadcast"` | ausente | Router entrega a todos los nodos |
| UUID o nombre L2 | - | Ignorado (unicast directo) |

### 3.2 Campos de Contexto (ich, ctx, ctx_seq, ctx_window)

Estos campos permiten mantener conversaciones con historia:

- **`ich`**: Canal por el cual el interlocutor se comunica (WhatsApp, Slack, email, etc.)
- **`ctx`**: Identificador único de la conversación, calculado como `hash(src_ilk + ich)`
- **`ctx_seq`**: Número de secuencia del último mensaje conocido en este contexto
- **`ctx_window`**: Array con los últimos 20 turns del contexto (o menos si hay menos)

El router usa `ctx` y `ctx_seq` para persistir la historia de la conversación en PostgreSQL.
El router incluye `ctx_window` al hacer forward, permitiendo que el nodo destino responda inmediatamente sin consultar la DB en el caso común.

### 3.3 Antecedentes Cognitivos (memory_package)

El router consulta `jsr-memory` y `LanceDB` para incluir episodios relevantes:

- **`memory_package.events`**: Episodios pasados que matchean los tags del mensaje actual
- **`memory_package.evidence_expand`**: Punteros para expandir a evidencia completa si se necesita

Ver `12-cognition.md` para detalles del sistema cognitivo.

**Estructura de cada turn en ctx_window:**
```json
{
  "seq": 47,
  "ts": "2026-02-04T15:00:00Z",
  "from": "ilk:...",
  "type": "text",
  "text": "contenido del mensaje"
}
```

Ver `11-context.md` para detalles completos.

---

## 4. Sección `payload` (Datos de Aplicación)

El contenido del mensaje. Ni el router ni OPA lo leen. Solo el nodo destino lo procesa.

```json
{
  "payload": {
    "type": "text",
    "content": "Hola, necesito ayuda con mi factura",
    "attachments": []
  }
}
```

La estructura interna es libre y la definen los nodos.

---

## 5. Estructuras Rust del Protocolo

```rust
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Mensaje completo del protocolo
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub routing: Routing,
    pub meta: Meta,
    #[serde(default)]
    pub payload: Value,
}

/// Header de routing (capa 1)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub src: String,
    #[serde(deserialize_with = "deserialize_dst")]
    pub dst: Destination,
    pub ttl: u8,
    pub trace_id: String,
}

/// Destino del mensaje
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Destination {
    Unicast(String),      // UUID específico o nombre L2 (ej: SY.orchestrator@motherbee)
    Broadcast,            // Literal "broadcast"
    Resolve,              // null - resolver via OPA
}

/// Metadata (capa 2 y sistema)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "type")]
    pub msg_type: String,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    
    /// ILK del interlocutor origen. OPA deriva tenant via data.identity lookup
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src_ilk: Option<String>,
    
    /// ILK del interlocutor destino
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dst_ilk: Option<String>,
    
    /// ICH (Interlocutor Channel) - canal de comunicación
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ich: Option<String>,
    
    /// Context ID - hash(src_ilk + ich), identifica la conversación
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<String>,
    
    /// Último número de secuencia conocido del contexto
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_seq: Option<u64>,
    
    /// Últimos N turns del contexto (max 20). El router los incluye al forward.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx_window: Option<Vec<CtxTurn>>,
    
    /// Antecedentes episódicos relevantes. El router los incluye si disponibles.
    /// Max 32KB. Si excede, se truncan highlights.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_package: Option<MemoryPackage>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

/// Paquete de antecedentes cognitivos
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryPackage {
    pub package_version: String,
    pub query: MemoryQuery,
    pub events: Vec<MemoryEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_expand: Option<EvidenceExpand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryQuery {
    pub ctx: String,
    pub cues_turn: Vec<String>,
    pub limits: MemoryLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryLimits {
    pub max_events: usize,
    pub max_items: usize,
    pub max_highlights_per_event: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEvent {
    pub event_id: i64,
    pub header: MemoryEventHeader,
    pub highlights: Vec<Highlight>,
    pub items: Vec<MemoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEventHeader {
    pub ctx: String,
    pub start_seq: i64,
    pub end_seq: i64,
    pub boundary_reason: String,
    pub cues_agg: Vec<String>,
    pub outcome: Outcome,
    pub priority_state: PriorityState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub status: String,
    pub duration_ms: i64,
    pub escalations: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriorityState {
    pub activation_strength: f32,
    pub context_inhibition: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Highlight {
    pub seq: i64,
    pub role: String,
    pub text: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryItem {
    pub memory_id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub confidence: f32,
    pub content: Value,
    pub cues_signature: Vec<String>,
    pub evidence_refs: Vec<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceExpand {
    pub available: bool,
    pub how: String,
    pub ranges: Vec<EvidenceRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRange {
    pub event_id: i64,
    pub ctx: String,
    pub start_seq: i64,
    pub end_seq: i64,
}

/// Turn dentro de ctx_window
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtxTurn {
    pub seq: u64,
    pub ts: String,
    pub from: String,
    #[serde(rename = "type")]
    pub turn_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}
```

---

## 6. Framing de Mensajes

**Todos los sockets del sistema (LAN y WAN) usan el mismo framing.**

### 6.1 Formato del Frame

```
┌──────────────┬─────────────────────┐
│ length (4B)  │ JSON message        │
│ big-endian   │ (length bytes)      │
└──────────────┴─────────────────────┘
```

- `length`: uint32 en big-endian, indica el tamaño del JSON en bytes
- `length` NO incluye los 4 bytes del header
- Máximo tamaño de mensaje: 64KB para inline, blob_ref para mayores

### 6.2 Pseudocódigo Rust

```rust
async fn write_frame(socket: &mut UnixStream, msg: &[u8]) -> io::Result<()> {
    let len = msg.len() as u32;
    socket.write_all(&len.to_be_bytes()).await?;
    socket.write_all(msg).await?;
    Ok(())
}

async fn read_frame(socket: &mut UnixStream) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    socket.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    
    let mut buf = vec![0u8; len];
    socket.read_exact(&mut buf).await?;
    Ok(buf)
}
```

### 6.3 Pseudocódigo Node.js

```javascript
function writeFrame(socket, msg) {
    const json = Buffer.from(JSON.stringify(msg), 'utf8');
    const header = Buffer.alloc(4);
    header.writeUInt32BE(json.length, 0);
    socket.write(Buffer.concat([header, json]));
}

let buffer = Buffer.alloc(0);
socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4) {
        const len = buffer.readUInt32BE(0);
        if (buffer.length < 4 + len) break;
        const json = buffer.slice(4, 4 + len);
        buffer = buffer.slice(4 + len);
        handleMessage(JSON.parse(json.toString('utf8')));
    }
});
```

---

## 7. Mensajes de Sistema

### 7.1 Descubrimiento y Registro

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `HELLO` | Nodo | Router | Registrar nodo |
| `ANNOUNCE` | Router | Nodo | Confirmar registro |
| `WITHDRAW` | Nodo | Router | Cierre limpio (enviado por `close()`) |

### 7.2 Health y Diagnóstico

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ECHO` | Cualquiera | Cualquiera | Ping |
| `ECHO_REPLY` | Cualquiera | Cualquiera | Pong |
| `UNREACHABLE` | Router | Nodo origen | Destino no existe |
| `TTL_EXCEEDED` | Router | Nodo origen | TTL llegó a 0 |

### 7.3 Inter-Isla (Gateway)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `LSA` | Gateway | Gateway | Intercambio de topología entre islas |

### 7.4 Configuración

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `CONFIG_CHANGED` | SY.admin | Broadcast (todos) | Notificar cambio de configuración |

**CONFIG_CHANGED** es el mensaje unificado para todos los cambios de configuración del sistema. SY.admin (único, en mother hive) es el único que lo emite.

#### 7.4.1 Formato

```json
{
  "routing": {
    "src": "<uuid-sy-admin>",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_CHANGED"
  },
  "payload": {
    "subsystem": "routes",
    "version": 42,
    "config": { ... }
  }
}
```

#### 7.4.2 Subsystems

| Subsystem | Quién actúa | Contenido de `config` |
|-----------|-------------|----------------------|
| `routes` | SY.config.routes, RT.* | Rutas estáticas |
| `vpn` | SY.config.routes, RT.* | Tabla VPN |
| `opa` | SY.opa.rules, RT.* | Policies OPA |
| `storage` | SY.orchestrator | Path del storage |
| `hives` | SY.orchestrator | Lista de islas |

#### 7.4.3 Ejemplos

**Cambio de rutas:**
```json
{
  "payload": {
    "subsystem": "routes",
    "version": 43,
    "config": {
      "routes": [
        { "prefix": "AI.soporte.*", "action": "FORWARD" }
      ]
    }
  }
}
```

**Cambio de storage:**
```json
{
  "payload": {
    "subsystem": "storage",
    "version": 1,
    "config": {
      "path": "/mnt/jsr-shared"
    }
  }
}
```

#### 7.4.4 Reglas de procesamiento

Cada nodo al recibir CONFIG_CHANGED:

1. Verificar si `subsystem` le incumbe (sino ignorar)
2. Verificar `version > last_version` (sino ignorar)
3. Aplicar cambios en memoria
4. Persistir en archivo local (para restart)
5. Actualizar `last_version`

#### 7.4.5 Subsystem OPA: Acciones especiales

El subsystem `opa` tiene un campo adicional `action` que define la operación:

| Action | Con rego | auto_apply | Comportamiento |
|--------|----------|------------|----------------|
| `compile` | SÍ | false (default) | Compilar nuevo rego, guardar como staged |
| `compile` | NO | false | Recompilar rego actual (refresh) |
| `compile` | SÍ | true | Compilar Y aplicar en un paso |
| `apply` | - | - | Activar versión staged |
| `rollback` | - | - | Volver a versión backup |

**Versionado:** SY.admin es el único que asigna números de versión usando un contador monotónico persistido en `/var/lib/json-router/opa-version.txt`. El contador siempre incrementa, incluso si la operación falla.

**Ejemplo: Compilar nueva policy (queda staged):**
```json
{
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "auto_apply": false,
    "config": {
      "rego": "package router\n\ndefault target = null\n...",
      "entrypoint": "router/target"
    }
  }
}
```

**Ejemplo: Compilar Y aplicar en un paso:**
```json
{
  "payload": {
    "subsystem": "opa",
    "action": "compile",
    "version": 43,
    "auto_apply": true,
    "config": {
      "rego": "package router\n...",
      "entrypoint": "router/target"
    }
  }
}
```

**Ejemplo: Aplicar policy staged:**
```json
{
  "payload": {
    "subsystem": "opa",
    "action": "apply",
    "version": 43
  }
}
```

**Ejemplo: Rollback:**
```json
{
  "payload": {
    "subsystem": "opa",
    "action": "rollback"
  }
}
```

### 7.5 OPA_RELOAD (local)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `OPA_RELOAD` | SY.opa.rules | Broadcast local (TTL 2) | Notificar routers que recarguen WASM |

Este mensaje lo emite SY.opa.rules después de aplicar una policy. Es **local** a la isla (TTL 2).

```json
{
  "routing": {
    "src": "<uuid-sy-opa-rules>",
    "dst": "broadcast",
    "ttl": 2
  },
  "meta": {
    "type": "system",
    "msg": "OPA_RELOAD"
  },
  "payload": {
    "version": 43,
    "hash": "sha256:abc123..."
  }
}
```

Los routers pueden:
1. Escuchar OPA_RELOAD y recargar el WASM de SHM
2. O detectar cambio en SHM por `policy_version` diferente

### 7.6 Tiempo

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `TIME_SYNC` | SY.time / Router | Broadcast | Sincronización UTC (scope=`"global"`) |

### 7.7 Cognición y Memoria

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `LSA_MEMORY` | Gateway | Gateway | Propagación de activaciones entre islas |
| `MEMORY_FETCH` | SY.cognition | Gateway destino | Request: obtener EventPackage de otra isla |
| `MEMORY_RESPONSE` | Gateway | SY.cognition | Response: EventPackage solicitado |

#### 7.7.1 LSA_MEMORY

Propaga punteros de eventos con alta activación para que otras islas sepan que existen:

```json
{
  "routing": {
    "src": "<uuid-gateway>",
    "dst": "gateway@otra-isla",
    "ttl": 8
  },
  "meta": {
    "type": "system",
    "msg": "LSA_MEMORY"
  },
  "payload": {
    "hive": "produccion",
    "epoch": 12345,
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

#### 7.7.2 MEMORY_FETCH / MEMORY_RESPONSE

Request/response para obtener EventPackage completo de otra isla:

**Request:**
```json
{
  "routing": {
    "src": "<uuid-sy-cognition>",
    "dst": "gateway@isla-origen",
    "ttl": 8
  },
  "meta": {
    "type": "system",
    "msg": "MEMORY_FETCH"
  },
  "payload": {
    "request_id": "<uuid>",
    "event_ids": [98, 122],
    "include_highlights": true,
    "include_items": true
  }
}
```

**Response:**
```json
{
  "routing": {
    "src": "<uuid-gateway>",
    "dst": "<uuid-sy-cognition>",
    "ttl": 8
  },
  "meta": {
    "type": "system",
    "msg": "MEMORY_RESPONSE"
  },
  "payload": {
    "request_id": "<uuid>",
    "status": "ok",
    "events": [
      {
        "event_id": 98,
        "header": { ... },
        "highlights": [ ... ],
        "items": [ ... ]
      }
    ]
  }
}
```

---

### 7.8 Orchestrator (Runtimes y Nodos)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `RUNTIME_UPDATE` | (externo) | SY.orchestrator | Notificar nueva versión de runtimes |
| `SPAWN_NODE` | SY.admin | SY.orchestrator | Solicitar ejecución de nodo |
| `KILL_NODE` | SY.admin | SY.orchestrator | Solicitar terminación de nodo |

#### 7.8.1 RUNTIME_UPDATE

Notifica al orchestrator que hay nuevas versiones de runtimes:

```json
{
  "routing": {
    "src": "<quien-notifica>",
    "dst": "SY.orchestrator@motherbee"
  },
  "meta": {
    "type": "system",
    "msg": "RUNTIME_UPDATE"
  },
  "payload": {
    "version": 43,
    "updated_at": "2026-02-08T10:00:00Z",
    "runtimes": {
      "AI.soporte": {
        "current": "1.3.0",
        "available": ["1.2.0", "1.3.0"]
      },
      "IO.whatsapp": {
        "current": "2.1.0",
        "available": ["2.0.0", "2.1.0"]
      }
    },
    "hash": "sha256:abc123..."
  }
}
```

#### 7.8.2 SPAWN_NODE

Solicita al orchestrator que ejecute un nodo:

```json
{
  "routing": {
    "src": "SY.admin@motherbee",
    "dst": "SY.orchestrator@motherbee"
  },
  "meta": {
    "type": "system",
    "msg": "SPAWN_NODE"
  },
  "payload": {
    "runtime": "AI.soporte",
    "version": "1.3.0",
    "target": "worker-3",
    "config": {
      "degree": "degree:soporte-l1",
      "instance_id": "ai-soporte-001"
    }
  }
}
```

#### 7.8.3 KILL_NODE

Solicita al orchestrator que termine un nodo:

```json
{
  "routing": {
    "src": "SY.admin@motherbee",
    "dst": "SY.orchestrator@motherbee"
  },
  "meta": {
    "type": "system",
    "msg": "KILL_NODE"
  },
  "payload": {
    "node_name": "AI.soporte.001@worker-3",
    "signal": "SIGTERM",
    "timeout_ms": 10000
  }
}
```

---

## 8. Handshake: HELLO

### 8.1 Flujo

```
Nodo                                    Router
  │                                        │
  ├────────────HELLO──────────────────────►│
  │                                        │ (registra en SHM)
  │◄───────────ANNOUNCE────────────────────┤
  │                                        │
  │         (operación normal)             │
```

### 8.2 Mensaje HELLO

```json
{
  "routing": {
    "src": "<uuid-nodo>",
    "dst": null,
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "uuid": "<uuid-nodo>",
    "name": "AI.soporte.l1@produccion",
    "version": "1.0"
  }
}
```

**Nota:** El `name` incluye `@isla`. La librería lo agrega automáticamente.

### 8.3 Mensaje ANNOUNCE

```json
{
  "routing": {
    "src": "<uuid-router>",
    "dst": "<uuid-nodo>",
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "ANNOUNCE"
  },
  "payload": {
    "uuid": "<uuid-nodo>",
    "name": "AI.soporte.l1@produccion",
    "status": "registered",
    "vpn_id": 10,
    "router_name": "RT.primary@produccion"
  }
}
```

**Campos importantes:**
- `vpn_id`: VPN asignado al nodo por el router (según tabla VPN en config)
- `router_name`: Nombre L2 del router que atiende al nodo

---

## 9. Mensaje LSA (entre Gateways)

El gateway consolida la topología de su isla y la envía a gateways remotos.

### 9.1 Formato

```json
{
  "routing": {
    "src": "<uuid-gateway-prod>",
    "dst": "<uuid-gateway-staging>",
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "LSA"
  },
  "payload": {
    "hive": "produccion",
    "seq": 42,
    "timestamp": "2025-01-20T10:00:00Z",
    "nodes": [
      {
        "uuid": "...",
        "name": "AI.soporte.l1@produccion",
        "vpn_id": 10
      },
      {
        "uuid": "...",
        "name": "RT.primary@produccion",
        "vpn_id": 0
      }
    ],
    "routes": [
      {
        "prefix": "AI.backup.*",
        "action": "FORWARD",
        "next_hop_hive": "disaster-recovery"
      }
    ],
    "vpns": [
      {
        "pattern": "AI.soporte.*",
        "vpn_id": 10
      },
      {
        "pattern": "AI.ventas.*",
        "vpn_id": 20
      }
    ]
  }
}
```

### 9.2 ¿Cuándo se envía?

- **Al conectar:** El gateway envía su LSA completo al peer
- **Cuando algo cambia:** Nodo conecta/desconecta, ruta cambia, VPN cambia
- **Periódicamente:** Como heartbeat (cada `hello_interval_ms`)

### 9.3 ¿Qué incluye?

| Dato | Descripción |
|------|-------------|
| `nodes` | **Todos** los nodos de la isla (AI, WF, IO, SY, RT) |
| `routes` | Rutas estáticas configuradas |
| `vpns` | Tabla de asignación VPN |

---

## 10. Librería de Nodos (node_client)

### 10.1 Principio de Diseño

La librería sigue el **modelo POSIX socket** con colas desacopladas y **split sender/receiver** (como Tokio). No inventamos mecanismos nuevos; replicamos patrones probados.

```
┌─────────────────────────────────────────────────────────────────┐
│                     Connection Manager                           │
│                        (interno, Arc)                            │
│                                                                  │
│  ┌─────────────┐     ┌─────────────┐     ┌─────────────┐       │
│  │   Socket    │────►│  RX Queue   │────►│ NodeReceiver│       │
│  │  (recv)     │     │ (mpsc 256)  │     │   recv()    │       │
│  └─────────────┘     └─────────────┘     └─────────────┘       │
│                                                                  │
│  ┌─────────────┐     ┌─────────────┐     ┌─────────────┐       │
│  │   Socket    │◄────│  TX Queue   │◄────│ NodeSender  │       │
│  │  (send)     │     │ (mpsc 256)  │     │   send()    │       │
│  └─────────────┘     └─────────────┘     └─────────────┘       │
│                                                                  │
│  Tasks internos:                                                 │
│  • RX Task: socket.read() → rx_queue.send()                     │
│  • TX Task: tx_queue.recv() → socket.write()                    │
│  • Reconnect: backoff exponencial, re-HELLO                     │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**Características clave:**
- **Full duplex real:** Sender y Receiver son independientes, sin locks
- **Colas desacopladas:** Capacidad fija de 256 mensajes, backpressure natural
- **Reconnect automático:** La librería reconecta con backoff exponencial

### 10.2 API Pública (Split Model)

La conexión retorna dos handles separados, como `TcpStream::into_split()` de Tokio:

```rust
/// Conectar al router. Retorna handles separados para send/recv.
pub async fn connect(config: NodeConfig) -> Result<(NodeSender, NodeReceiver), NodeError>;

pub struct NodeSender {
    // interno: mpsc::Sender + Arc<ConnectionManager>
}

pub struct NodeReceiver {
    // interno: mpsc::Receiver + Arc<ConnectionManager>
}
```

#### NodeSender

```rust
impl NodeSender {
    /// Enviar mensaje. Await si la cola está llena (backpressure).
    pub async fn send(&self, msg: Message) -> Result<(), NodeError>;
    
    /// Cierre limpio. Envía WITHDRAW al router.
    pub async fn close(self) -> Result<(), NodeError>;
    
    /// Info del nodo
    pub fn uuid(&self) -> &str;
    pub fn full_name(&self) -> &str;
}
```

#### NodeReceiver

```rust
impl NodeReceiver {
    /// Recibir mensaje. Bloquea hasta que hay mensaje.
    pub async fn recv(&mut self) -> Result<Message, NodeError>;
    
    /// Recibir con timeout.
    pub async fn recv_timeout(&mut self, timeout: Duration) -> Result<Message, NodeError>;
    
    /// Recibir sin bloquear. None si la cola está vacía.
    pub fn try_recv(&mut self) -> Option<Message>;
    
    /// ¿Estamos conectados?
    pub fn is_connected(&self) -> bool;
    
    /// Esperar hasta estar conectado.
    pub async fn wait_connected(&self);
    
    /// Info del nodo
    pub fn uuid(&self) -> &str;
    pub fn full_name(&self) -> &str;
    pub fn vpn_id(&self) -> u32;
}
```

### 10.3 Equivalencia con POSIX y Tokio

| Operación | POSIX Socket | Tokio mpsc | NodeClient |
|-----------|--------------|------------|------------|
| Enviar (espera si lleno) | `send()` | `tx.send().await` | `sender.send().await` |
| Recibir (bloquea) | `recv()` | `rx.recv().await` | `receiver.recv().await` |
| Recibir (con timeout) | `setsockopt(SO_RCVTIMEO)` | `timeout(d, rx.recv())` | `receiver.recv_timeout(d).await` |
| Recibir (no bloquea) | `recv()` + `O_NONBLOCK` | `rx.try_recv()` | `receiver.try_recv()` |
| Cerrar | `close()` | drop | `sender.close().await` |
| Split | `dup()` + disciplina | `into_split()` | `connect()` retorna tupla |

### 10.4 Errores

```rust
pub enum NodeError {
    /// Socket desconectado, reconexión en progreso o fallida
    Disconnected,
    
    /// Timeout esperando mensaje
    Timeout,
    
    /// Error en HELLO/ANNOUNCE
    HandshakeFailed(String),
    
    /// Error de I/O
    Io(std::io::Error),
    
    /// Error de serialización
    Json(serde_json::Error),
}
```

### 10.5 Restricción Fundamental

> **Un nodo = Un UUID = Una conexión**

El router asocia cada UUID a exactamente una conexión. Si un proceso abre dos conexiones con el mismo UUID, el router desconecta la anterior.

### 10.6 Reconexión Automática

La librería maneja la reconexión de forma transparente:

| Evento | Acción |
|--------|--------|
| Socket cae | Estado = Reconnecting, **vaciar TX queue** |
| Intento fallido | Backoff exponencial (100ms → 200ms → 400ms → ... → 30s max) |
| Conexión OK | Enviar HELLO, esperar ANNOUNCE |
| ANNOUNCE recibido | Estado = Connected |

**Importante:** Al reconectar se vacía la cola TX. Los mensajes pendientes se pierden. Es responsabilidad del nodo re-enviar si es necesario (el nodo puede detectar desconexión via `is_connected()`).

```
        CONNECTED
            │
            ▼
    ┌───────────────┐
    │  Socket cae   │
    └───────┬───────┘
            │
            ▼
      RECONNECTING ──── vaciar TX queue
            │
            ▼
    ┌───────────────┐
    │ Backoff 100ms │──► connect() ──► ¿OK?
    └───────────────┘                    │
            ▲                       ┌────┴────┐
            │                      NO        SÍ
            │                       │         │
            └─────── Backoff x2 ────┘         ▼
                     (max 30s)             HELLO
                                              │
                                              ▼
                                          ANNOUNCE
                                              │
                                              ▼
                                         CONNECTED
```

### 10.7 Mensaje WITHDRAW (cierre limpio)

Cuando el nodo llama `close()`, la librería envía WITHDRAW antes de cerrar:

```json
{
  "routing": {
    "src": "<uuid-nodo>",
    "dst": null,
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "WITHDRAW"
  },
  "payload": {
    "reason": "shutdown"
  }
}
```

El router al recibir WITHDRAW remueve el nodo de su tabla inmediatamente.

**Nota:** Si el nodo hace `drop()` sin llamar `close()`, no se envía WITHDRAW. El router detectará la desconexión por el socket cerrado.

### 10.8 Patrón Request/Response (responsabilidad del nodo)

La librería NO implementa request/response. El nodo que necesite este patrón debe implementarlo usando `trace_id` para correlación:

```rust
struct PendingRequests {
    pending: HashMap<String, oneshot::Sender<Message>>,
}

impl MyNode {
    async fn request(&mut self, msg: Message, timeout: Duration) -> Result<Message> {
        let trace_id = msg.routing.trace_id.clone();
        
        let (tx, rx) = oneshot::channel();
        self.pending.insert(trace_id.clone(), tx);
        
        self.sender.send(msg).await?;
        
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(Error::ChannelClosed),
            Err(_) => {
                self.pending.remove(&trace_id);
                Err(Error::Timeout)
            }
        }
    }
    
    async fn run(&mut self) {
        loop {
            let msg = self.receiver.recv().await.unwrap();
            
            if let Some(sender) = self.pending.remove(&msg.routing.trace_id) {
                sender.send(msg).ok();
            } else {
                self.handle_message(msg).await;
            }
        }
    }
}
```

### 10.9 Responsabilidades

| Responsabilidad | Quién |
|-----------------|-------|
| Conexión socket | Librería |
| Framing (length-prefix) | Librería |
| Colas TX/RX desacopladas (256 cap) | Librería |
| Handshake HELLO/ANNOUNCE | Librería |
| Reconexión automática con backoff | Librería |
| Envío de WITHDRAW en close() | Librería |
| Generar y persistir UUID | Librería |
| Agregar @isla al nombre | Librería |
| Request/response (correlación) | **Nodo** |
| Qué hacer post-reconexión | **Nodo** |
| Timeouts de aplicación | **Nodo** |

### 10.10 Flujo de Conexión

```
1. Leer /etc/json-router/hive.yaml → obtener hive_id
   - Si no existe → ERROR, no arrancar
   
2. Cargar o generar UUID desde archivo de persistencia

3. Construir nombre L2 completo:
   - Config del nodo: name = "AI.soporte.l1"
   - Librería agrega: name = "AI.soporte.l1@produccion"

4. Conectar al socket del router

5. Crear colas internas (mpsc, capacidad 256)

6. Spawn tasks internos (RX task, TX task)

7. Enviar HELLO con nombre completo

8. Esperar ANNOUNCE → obtener vpn_id asignado

9. Retornar (NodeSender, NodeReceiver)
```

### 10.11 Persistencia de UUID

```
/var/lib/json-router/state/nodes/<nombre-sin-isla>.uuid
```

Ejemplo: `/var/lib/json-router/state/nodes/AI.soporte.l1.uuid`

### 10.12 Ejemplo de Uso

```rust
use json_router::node_client::{connect, NodeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NodeConfig {
        name: "AI.soporte.l1".to_string(),  // SIN @isla
        router_socket: "/var/run/json-router/routers".into(),
        uuid_persistence_dir: "/var/lib/json-router/state/nodes/".into(),
        config_dir: "/etc/json-router".into(),
        version: "1.0".to_string(),
    };

    let (sender, mut receiver) = connect(config).await?;
    
    println!("Conectado como: {}", receiver.full_name());  
    // → "AI.soporte.l1@produccion"

    // Sender en otro task si hace falta
    let sender = Arc::new(sender);
    
    // Loop principal
    loop {
        // Detectar reconexión
        if !receiver.is_connected() {
            receiver.wait_connected().await;
            println!("Reconectado, re-sincronizando...");
        }
        
        let msg = receiver.recv().await?;
        let response = process_message(&msg);
        sender.send(response).await?;
    }
}
```
```

---

## 11. Tipos de Mensaje por Tamaño

### 11.1 Mensaje Inline (< 64KB)

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "text",
    "content": "Mensaje normal"
  }
}
```

### 11.2 Mensaje por Referencia (> 64KB)

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "blob_ref",
    "blob_id": "sha256:a1b2c3d4...",
    "size": 5242880,
    "mime": "image/png",
    "spool_day": "2025-01-15"
  }
}
```

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura, islas | `01-arquitectura.md` |
| Shared memory | `03-shm.md` |
| Routing | `04-routing.md` |
| LSA detallado | `05-conectividad.md` |
