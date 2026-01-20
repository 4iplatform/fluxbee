# JSON Router - 02 Protocolo de Mensajes

**Estado:** v1.13  
**Fecha:** 2025-01-20  
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

## 2. Sección `routing` (Header de Red) — v1.13

Usado por el router para decisiones de capa 1. El router DEBE poder tomar decisiones leyendo SOLO esta sección.

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "uuid-destino | null | \"broadcast\"",
    "ttl": 16,
    "trace_id": "uuid-correlación",
    "vpn": 0
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `src` | UUID | Sí | Nodo origen del mensaje |
| `dst` | UUID, "broadcast", o null | Sí | Unicast / Broadcast / Resolve via OPA |
| `ttl` | int | Sí | Time-to-live, decrementa por hop WAN y broadcast forwarding |
| `trace_id` | UUID | Sí | ID de correlación para trazabilidad |
| `vpn` | uint32 | No (default 0) | Zona lógica (VRF) dentro de la isla. Aísla visibilidad y routing. |

### 2.1 Campo `vpn` (v1.13)

El campo `vpn` define el dominio VRF en el que se resuelve el destino:

- `vpn: 0` = global (default, todos los nodos se ven)
- `vpn: N` = zona N (solo nodos de esa zona se ven entre sí)

**Reglas:**
- Broadcast de negocio respeta VPN (broadcast dentro de la VPN).
- Broadcast de sistema (`meta.type="system"`) puede ignorar VPN (infra esencial).

### 2.2 Determinación del VPN Efectivo

El `vpn` efectivo de un mensaje se define así:

1. El nodo tiene un `vpn_id` asignado por el router al conectar (via membership rules).
2. Todo mensaje enviado por ese nodo se considera dentro de ese `vpn_id`.
3. Si el nodo envía `routing.vpn` distinto al asignado, el router lo **normaliza** al vpn asignado.

**El nodo no puede "escalar" de VPN.** El router impone el aislamiento.

---

## 3. Sección `meta` (Metadata para OPA y Sistema)

Usado por OPA para decisiones de capa 2, y por el router para broadcast filtrado.

```json
{
  "meta": {
    "type": "user",
    "target": "AI.soporte.l1@produccion",
    "priority": "high",
    "context": {
      "cliente_tier": "vip",
      "caso_id": "12345",
      "horario": "nocturno"
    }
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `type` | string | Sí | Tipo de mensaje: `"user"`, `"system"`, `"admin"` |
| `msg` | string | Sí si type=system | Tipo de mensaje de sistema (ej: `"HELLO"`, `"TIME_SYNC"`) |
| `target` | string (nombre capa 2) | Condicional | Para OPA o broadcast filter |
| `priority` | string | No | Hint de prioridad para OPA |
| `context` | object | No | Datos adicionales para reglas OPA |
| `action` | string | No | Para mensajes admin: acción a ejecutar |

### 3.1 Uso de `target` según contexto

| `dst` | `meta.target` | Comportamiento |
|-------|---------------|----------------|
| `null` | nombre capa 2 | OPA resuelve destino usando target |
| `"broadcast"` | patrón capa 2 | Router filtra: solo entrega a nodos que matchean |
| `"broadcast"` | ausente | Router entrega a todos los nodos |
| UUID | - | Ignorado (unicast directo) |

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

Las siguientes estructuras definen el formato de mensajes. Deben exponerse públicamente en la librería.

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

/// Header de routing (capa 1) — v1.13 con vpn
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Routing {
    pub src: String,                    // UUID del nodo origen
    #[serde(deserialize_with = "deserialize_dst")]
    pub dst: Destination,               // UUID, "broadcast", o null
    pub ttl: u8,
    pub trace_id: String,

    #[serde(default)]
    pub vpn: u32,                       // 0 = default/global (v1.13)
}

/// Destino del mensaje
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Destination {
    Unicast(String),                    // UUID específico
    Broadcast,                          // Literal "broadcast"
    Resolve,                            // null - resolver via OPA
}

/// Metadata (capa 2 y sistema)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    #[serde(rename = "type")]
    pub msg_type: String,               // "user", "system", "admin"
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,            // Para system: "HELLO", "TIME_SYNC", etc.
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,         // Para OPA o broadcast filter
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,         // Para admin: "add_route", etc.
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,       // Hint para OPA
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,         // Datos adicionales para OPA
}
```

**Exposición en librería:**

```rust
// lib.rs
pub mod protocol;  // Expone Message, Routing, Meta, Destination
pub mod socket;    // Expone funciones de framing (read_frame, write_frame)
```

---

## 6. Framing de Mensajes

**IMPORTANTE:** Todos los sockets del sistema (LAN y WAN) usan el mismo framing.

### 6.1 Formato del Frame

```
┌──────────────┬─────────────────────┐
│ length (4B)  │ JSON message        │
│ big-endian   │ (length bytes)      │
└──────────────┴─────────────────────┘
```

- `length`: uint32 en big-endian, indica el tamaño del JSON en bytes
- `length` NO incluye los 4 bytes del header
- Máximo tamaño de mensaje: configurable (default 64KB para inline)

### 6.2 Escritura de Frame

```
1. Serializar mensaje a JSON (UTF-8)
2. Calcular length = bytes del JSON
3. Escribir 4 bytes de length (big-endian)
4. Escribir bytes del JSON
```

### 6.3 Lectura de Frame

```
1. Leer exactamente 4 bytes
2. Interpretar como uint32 big-endian → length
3. Leer exactamente length bytes
4. Parsear JSON
```

### 6.4 Pseudocódigo Rust

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

### 6.5 Pseudocódigo Node.js

```javascript
function writeFrame(socket, msg) {
    const json = Buffer.from(JSON.stringify(msg), 'utf8');
    const header = Buffer.alloc(4);
    header.writeUInt32BE(json.length, 0);
    socket.write(Buffer.concat([header, json]));
}

// Leer (acumular en buffer)
let buffer = Buffer.alloc(0);

socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    
    while (buffer.length >= 4) {
        const len = buffer.readUInt32BE(0);
        if (buffer.length < 4 + len) break;
        
        const json = buffer.slice(4, 4 + len);
        buffer = buffer.slice(4 + len);
        
        const msg = JSON.parse(json.toString('utf8'));
        handleMessage(msg);
    }
});
```

---

## 7. Flujo de Resolución

```
Mensaje llega al router
        │
        ▼
  ┌─────────────────┐
  │ Leer frame      │
  │ (length+JSON)   │
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ Leer routing.dst│
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐     dst tiene valor
  │ ¿dst es null?   │ ───────────────────────► Forward directo a UUID
  └─────────────────┘
        │ dst es null
        ▼
  ┌─────────────────┐
  │ Pasar meta a OPA│
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ OPA retorna     │
  │ nombre capa 2   │
  └─────────────────┘
        │
        ▼
  ┌─────────────────────────┐
  │ Router busca en tabla:  │
  │ ¿qué UUIDs tienen ese   │
  │ nombre en este VPN?     │
  └─────────────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ Elegir uno      │
  │ (balanceo)      │
  └─────────────────┘
        │
        ▼
  ┌─────────────────┐
  │ Escribir frame  │
  │ al nodo destino │
  └─────────────────┘
```

---

## 8. Tipos de Mensaje por Tamaño

### 8.1 Mensaje Inline (< 64KB)

JSON completo con payload incluido. Texto, comandos, metadata, respuestas LLM normales.

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "text",
    "content": "Mensaje de texto normal"
  }
}
```

### 8.2 Mensaje por Referencia (> 64KB)

El payload es un archivo en disco. El JSON lleva el path.

```json
{
  "routing": { ... },
  "meta": { ... },
  "payload": {
    "type": "blob_ref",
    "blob_id": "sha256:a1b2c3d4...",
    "size": 5242880,
    "sha256": "a1b2c3d4...",
    "mime": "image/png",
    "spool_day": "2025-01-15"
  }
}
```

**Flujo:**
1. Nodo origen guarda archivo en `/var/spool/mesh/blobs/`
2. Nodo origen manda JSON con `blob_ref` al router
3. Router rutea el JSON (chiquito) al nodo destino
4. Nodo destino lee el archivo del path

---

## 9. Mensajes de Sistema

Nomenclatura basada en estándares de red existentes.

### 9.1 Descubrimiento y Registro

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `HELLO` | Nodo | Router | Nodo anuncia existencia (UUID + nombre capa 2) |
| `ANNOUNCE` | Router | Nodo | Router confirma registro del nodo |
| `WITHDRAW` | Nodo | Router | Nodo anuncia shutdown limpio |

### 9.2 Health y Diagnóstico (ICMP-like)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ECHO` | Cualquiera | Cualquiera | Ping |
| `ECHO_REPLY` | Cualquiera | Cualquiera | Pong |
| `UNREACHABLE` | Router | Nodo origen | Destino no existe |
| `UNREACHABLE_IN_VPN` | Router | Nodo origen | Destino no visible en esta VPN (v1.13) |
| `TTL_EXCEEDED` | Router | Nodo origen | TTL llegó a 0 |
| `SOURCE_QUENCH` | Router/Nodo | Nodo origen | Backpressure |

### 9.3 Routing (entre routers)

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `HELLO` | Router | Routers | Anunciar existencia |
| `SYNC_REQUEST` | Router | Router | Pedir tabla completa |
| `SYNC_REPLY` | Router | Router | Respuesta con tabla |

### 9.4 Administrativos

| Mensaje | Origen | Destino | Propósito |
|---------|--------|---------|-----------|
| `ADM_SHUTDOWN` | Admin | Nodo/Router | Orden de apagar |
| `ADM_RELOAD` | Admin | Nodo/Router | Recargar configuración |
| `ADM_STATUS` | Admin | Nodo/Router | Solicitar estado |
| `TIME_SYNC` | SY.time | Todos | Broadcast de tiempo UTC |
| `OPA_RELOAD` | SY.opa.rules | Routers | Recargar policy WASM |

### 9.5 Tiempo del Sistema

**Todo tiempo en el sistema es UTC.**

```json
{
  "routing": {
    "src": "uuid-router",
    "dst": "broadcast",
    "ttl": 1,
    "trace_id": "uuid",
    "vpn": 0
  },
  "meta": {
    "type": "system",
    "msg": "TIME_SYNC"
  },
  "payload": {
    "utc": "2025-01-15T10:00:00.000Z",
    "epoch_ms": 1736935200000,
    "source": "SY.time@produccion",
    "stratum": 1
  }
}
```

---

## 10. Handshake: HELLO

### 10.1 Protocolo

**HELLO es el único handshake para nodos.**

| Mensaje | Dirección | Propósito |
|---------|-----------|-----------|
| `HELLO` | Nodo → Router | Registrar nodo |
| `ANNOUNCE` | Router → Nodo | Confirmar registro (incluye vpn_id asignado) |

### 10.2 Mensaje HELLO (enviado por nodo)

```json
{
  "routing": {
    "src": "<uuid-nodo>",
    "dst": null,
    "ttl": 1,
    "trace_id": "<uuid>",
    "vpn": 0
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

### 10.3 Mensaje ANNOUNCE (respuesta del router)

```json
{
  "meta": {
    "type": "system",
    "msg": "ANNOUNCE"
  },
  "payload": {
    "uuid": "<uuid-nodo>",
    "name": "AI.soporte.l1@produccion",
    "status": "registered",
    "vpn_id": 10,
    "router_id": "<uuid-router>"
  }
}
```

**Campos importantes (v1.13):**
- `vpn_id`: El router asigna el VPN del nodo según membership rules.
- El nodo **no elige** su VPN; el router la impone.

---

## 11. Conexión de Nodos

### 11.1 Socket Unix

Los nodos crean un socket Unix y esperan que el router conecte:

```
/var/run/mesh/nodes/<uuid>.sock
```

Los routers monitorean este directorio con `inotify`.

### 11.2 Modelo de Conexión

El nodo es pasivo (servidor), el router es activo (cliente):

**Nodo:**
```
socket(AF_UNIX, SOCK_STREAM) → bind() → listen(1) → accept()
```

**Router:**
```
inotify detecta socket nuevo → random backoff → connect()
```

El `listen(1)` con backlog 1 garantiza que solo un router puede conectar.

### 11.3 Detección de Link Down

| Evento | Qué pasa | Acción del router |
|--------|----------|-------------------|
| Nodo termina limpio | `close()` del socket | Router recibe EOF |
| Nodo crashea | Kernel cierra socket | Router recibe `ECONNRESET` |
| Nodo se cuelga | Timeout en operación | Router detecta inactividad |

No hay heartbeat ni polling. El kernel notifica.

---

## 12. Librería de Nodos (node_client)

### 12.1 Principio

**La librería es el único lugar donde se implementa la comunicación con el router.**

| Responsabilidad | Dónde se implementa |
|-----------------|---------------------|
| Generar UUID | Librería |
| Persistir UUID | Librería |
| Conexión socket | Librería |
| Framing (read/write) | Librería |
| Handshake HELLO | Librería |
| Reconexión automática | Librería |

### 12.2 Ejemplo de Uso (Rust)

```rust
use json_router::node_client::{NodeClient, NodeConfig, NodeError};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), NodeError> {
    let island_id = read_island_id("/etc/json-router/island.yaml")?;

    let config = NodeConfig {
        name: "AI.soporte.l1".to_string(),    // sin @isla
        island_id: island_id.clone(),          // se añade @isla automáticamente
        router_socket: PathBuf::from("/var/run/json-router/router.sock"),
        uuid_persistence_dir: PathBuf::from("/var/lib/json-router/nodes/"),
    };

    let client = NodeClient::connect(config).await?;
    println!("Conectado como {} (UUID: {})", client.full_name(), client.uuid());
    println!("VPN asignado: {}", client.vpn_id());

    loop {
        let msg = client.recv().await?;
        // Procesar...
    }
}
```

### 12.3 Persistencia de UUID

El UUID se persiste en:

```
/var/lib/json-router/nodes/<nombre>.uuid
```

Primera ejecución: genera y guarda. Siguientes: lee del archivo.

---

## 13. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura general, islas, naming | `01-arquitectura.md` |
| Shared memory, estructuras | `03-shm.md` |
| Routing, VPN zones | `04-routing.md` |
| Conectividad IRP/WAN | `05-conectividad.md` |
