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

## 2. Sección `routing` (Header de Red)

Usado por el router para decisiones de capa 1. El router DEBE poder tomar decisiones leyendo SOLO esta sección.

```json
{
  "routing": {
    "src": "uuid-origen",
    "dst": "uuid-destino | null | \"broadcast\"",
    "ttl": 16,
    "trace_id": "uuid-correlación"
  }
}
```

| Campo | Tipo | Obligatorio | Descripción |
|-------|------|-------------|-------------|
| `src` | UUID | Sí | Nodo origen del mensaje |
| `dst` | UUID, "broadcast", o null | Sí | Unicast / Broadcast / Resolve via OPA |
| `ttl` | int | Sí | Time-to-live, decrementa por hop WAN y broadcast |
| `trace_id` | UUID | Sí | ID de correlación para trazabilidad |

### 2.1 Valores de `dst`

| Valor | Comportamiento |
|-------|----------------|
| UUID | Unicast directo a ese nodo |
| `"broadcast"` | Entregar a todos (filtrado por `meta.target` si existe) |
| `null` | Resolver destino via OPA usando `meta.target` |

---

## 3. Sección `meta` (Metadata para OPA y Sistema)

Usado por OPA para decisiones de capa 2, y por el router para broadcast filtrado.

```json
{
  "meta": {
    "type": "user",
    "scope": "vpn",
    "target": "AI.soporte.l1@produccion",
    "priority": "high",
    "context": {
      "cliente_tier": "vip",
      "caso_id": "12345"
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
    Unicast(String),      // UUID específico
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
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
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
| `WITHDRAW` | Nodo | Router | Shutdown limpio |

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

**CONFIG_CHANGED** es el mensaje unificado para todos los cambios de configuración del sistema. SY.admin (único, en mother island) es el único que lo emite.

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
| `islands` | SY.orchestrator | Lista de islas |

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
    "island": "produccion",
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
        "next_hop_island": "disaster-recovery"
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

La librería sigue el **modelo POSIX socket** con colas desacopladas. No inventamos mecanismos nuevos; replicamos patrones probados de BSD sockets y Tokio.

```
┌─────────────────────────────────────────────────────────────────┐
│                        NodeClient                                │
│                                                                  │
│  ┌─────────────┐     ┌─────────────┐     ┌─────────────┐       │
│  │   Socket    │────►│  RX Queue   │────►│    App      │       │
│  │  (recv)     │     │  (mpsc)     │     │   recv()    │       │
│  └─────────────┘     └─────────────┘     └─────────────┘       │
│                                                                  │
│  ┌─────────────┐     ┌─────────────┐     ┌─────────────┐       │
│  │   Socket    │◄────│  TX Queue   │◄────│    App      │       │
│  │  (send)     │     │  (mpsc)     │     │   send()    │       │
│  └─────────────┘     └─────────────┘     └─────────────┘       │
│                                                                  │
│  Tasks internos:                                                 │
│  • RX Task: socket.read() → rx_queue.send()                     │
│  • TX Task: tx_queue.recv() → socket.write()                    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

**El socket queda desacoplado de la aplicación.** La app nunca toca el socket directo.

### 10.2 API Pública

La API replica el modelo POSIX/Tokio. La librería **no sabe de request/response, correlación, ni trace_id**. Solo mueve mensajes.

```rust
impl NodeClient {
    /// Conectar al router. Hace HELLO automático.
    pub async fn connect(config: NodeConfig) -> Result<Self, NodeError>;
    
    /// Enviar mensaje. No bloqueante (encola en TX queue).
    pub async fn send(&self, msg: Message) -> Result<(), NodeError>;
    
    /// Recibir mensaje. Bloqueante hasta que hay mensaje.
    pub async fn recv(&self) -> Result<Message, NodeError>;
    
    /// Recibir con timeout.
    pub async fn recv_timeout(&self, timeout: Duration) -> Result<Message, NodeError>;
    
    /// Recibir sin bloquear. Retorna None si la cola está vacía.
    pub fn try_recv(&self) -> Option<Message>;
    
    /// Cerrar conexión.
    pub async fn close(self);
    
    /// Info del nodo
    pub fn uuid(&self) -> &str;
    pub fn full_name(&self) -> &str;  // Incluye @isla
    pub fn vpn_id(&self) -> u32;
}
```

### 10.3 Equivalencia con POSIX y Tokio

| Operación | POSIX Socket | Tokio mpsc | NodeClient |
|-----------|--------------|------------|------------|
| Enviar (espera si lleno) | `send()` | `tx.send().await` | `send().await` |
| Enviar (no bloquea) | `send()` + `O_NONBLOCK` | `tx.try_send()` | — (no expuesto) |
| Recibir (bloquea) | `recv()` | `rx.recv().await` | `recv().await` |
| Recibir (con timeout) | `setsockopt(SO_RCVTIMEO)` | `timeout(d, rx.recv())` | `recv_timeout(d).await` |
| Recibir (no bloquea) | `recv()` + `O_NONBLOCK` | `rx.try_recv()` | `try_recv()` |
| Cerrar | `close()` | drop tx/rx | `close().await` |

### 10.4 Restricción Fundamental

> **Un nodo = Un UUID = Una conexión**

El router asocia cada UUID a exactamente una conexión. Si un proceso abre dos conexiones con el mismo UUID, el router desconecta la anterior.

**Consecuencia:** Si un nodo necesita enviar un mensaje y esperar respuesta específica, debe implementar la correlación en su código usando `trace_id`, no abriendo conexiones adicionales.

### 10.5 Patrón Request/Response (responsabilidad del nodo)

La librería NO implementa request/response. El nodo que necesite este patrón debe implementarlo así:

```rust
// Ejemplo: SY.admin esperando CONFIG_RESPONSE

struct PendingRequests {
    pending: HashMap<String, oneshot::Sender<Message>>,
}

impl MyNode {
    async fn request(&self, msg: Message, timeout: Duration) -> Result<Message> {
        let trace_id = msg.routing.trace_id.clone();
        
        // Crear canal para la respuesta
        let (tx, rx) = oneshot::channel();
        self.pending.insert(trace_id.clone(), tx);
        
        // Enviar
        self.client.send(msg).await?;
        
        // Esperar respuesta (con timeout)
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => Err(Error::ChannelClosed),
            Err(_) => {
                self.pending.remove(&trace_id);
                Err(Error::Timeout)
            }
        }
    }
    
    async fn run(&self) {
        loop {
            let msg = self.client.recv().await.unwrap();
            
            // ¿Es respuesta a un request pendiente?
            if let Some(sender) = self.pending.remove(&msg.routing.trace_id) {
                sender.send(msg).ok();
            } else {
                // Mensaje normal, procesar
                self.handle_message(msg).await;
            }
        }
    }
}
```

**La correlación por `trace_id` es responsabilidad del nodo, no de la librería.**

### 10.6 Responsabilidades

| Responsabilidad | Quién |
|-----------------|-------|
| Conexión socket | Librería |
| Framing (length-prefix) | Librería |
| Colas TX/RX desacopladas | Librería |
| Handshake HELLO/ANNOUNCE | Librería |
| Generar y persistir UUID | Librería |
| Agregar @isla al nombre | Librería |
| Reconexión automática | Librería |
| Request/response (correlación) | **Nodo** |
| Timeouts de aplicación | **Nodo** |
| Manejo de múltiples respuestas | **Nodo** |

### 10.7 Implementación Interna (referencia)

```rust
pub struct NodeClient {
    // Canales internos (Tokio mpsc)
    tx: mpsc::Sender<Message>,
    rx: mpsc::Receiver<Message>,
    
    // Info del nodo
    uuid: String,
    full_name: String,
    vpn_id: u32,
    
    // Handle al task de background
    _rx_task: JoinHandle<()>,
    _tx_task: JoinHandle<()>,
}

impl NodeClient {
    pub async fn connect(config: NodeConfig) -> Result<Self, NodeError> {
        // 1. Leer island.yaml
        let island_id = read_island_yaml(&config.config_dir)?;
        
        // 2. Cargar o generar UUID
        let uuid = load_or_create_uuid(&config.uuid_persistence_dir, &config.name)?;
        
        // 3. Construir nombre completo
        let full_name = format!("{}@{}", config.name, island_id);
        
        // 4. Conectar al socket
        let socket = UnixStream::connect(&config.router_socket).await?;
        let (read_half, write_half) = socket.into_split();
        
        // 5. Crear canales internos
        let (app_tx, internal_rx) = mpsc::channel(256);  // App → TX task
        let (internal_tx, app_rx) = mpsc::channel(256);  // RX task → App
        
        // 6. Spawn TX task
        let tx_task = tokio::spawn(async move {
            tx_loop(write_half, internal_rx).await
        });
        
        // 7. Spawn RX task  
        let rx_task = tokio::spawn(async move {
            rx_loop(read_half, internal_tx).await
        });
        
        // 8. Enviar HELLO
        let hello = build_hello(&uuid, &full_name, &config.version);
        app_tx.send(hello).await?;
        
        // 9. Esperar ANNOUNCE
        let announce = app_rx.recv().await.ok_or(NodeError::Disconnected)?;
        let vpn_id = parse_announce(&announce)?;
        
        Ok(Self {
            tx: app_tx,
            rx: app_rx,
            uuid,
            full_name,
            vpn_id,
            _rx_task: rx_task,
            _tx_task: tx_task,
        })
    }
    
    pub async fn send(&self, msg: Message) -> Result<(), NodeError> {
        self.tx.send(msg).await.map_err(|_| NodeError::Disconnected)
    }
    
    pub async fn recv(&self) -> Result<Message, NodeError> {
        self.rx.recv().await.ok_or(NodeError::Disconnected)
    }
    
    pub async fn recv_timeout(&self, d: Duration) -> Result<Message, NodeError> {
        tokio::time::timeout(d, self.rx.recv())
            .await
            .map_err(|_| NodeError::Timeout)?
            .ok_or(NodeError::Disconnected)
    }
    
    pub fn try_recv(&self) -> Option<Message> {
        self.rx.try_recv().ok()
    }
}

async fn rx_loop(mut socket: OwnedReadHalf, tx: mpsc::Sender<Message>) {
    loop {
        match read_frame(&mut socket).await {
            Ok(bytes) => {
                if let Ok(msg) = serde_json::from_slice(&bytes) {
                    if tx.send(msg).await.is_err() {
                        break;  // App cerró
                    }
                }
            }
            Err(_) => break,  // Socket cerrado
        }
    }
}

async fn tx_loop(mut socket: OwnedWriteHalf, mut rx: mpsc::Receiver<Message>) {
    while let Some(msg) = rx.recv().await {
        if let Ok(bytes) = serde_json::to_vec(&msg) {
            if write_frame(&mut socket, &bytes).await.is_err() {
                break;  // Socket cerrado
            }
        }
    }
}
```

### 10.8 Flujo de Conexión

```
1. Leer /etc/json-router/island.yaml → obtener island_id
   - Si no existe → ERROR, no arrancar
   
2. Cargar o generar UUID desde archivo de persistencia

3. Construir nombre L2 completo:
   - Config del nodo: name = "AI.soporte.l1"
   - Librería agrega: name = "AI.soporte.l1@produccion"

4. Conectar al socket del router

5. Spawn tasks internos (RX y TX)

6. Enviar HELLO con nombre completo

7. Esperar ANNOUNCE → obtener vpn_id asignado

8. Listo para enviar/recibir mensajes
```

### 10.9 Persistencia de UUID

```
/var/lib/json-router/state/nodes/<nombre-sin-isla>.uuid
```

Ejemplo: `/var/lib/json-router/state/nodes/AI.soporte.l1.uuid`

### 10.10 Ejemplo de Uso Simple

```rust
use json_router::node_client::{NodeClient, NodeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = NodeConfig {
        name: "AI.soporte.l1".to_string(),  // SIN @isla
        router_socket: "/var/run/json-router/routers".into(),
        uuid_persistence_dir: "/var/lib/json-router/state/nodes/".into(),
        config_dir: "/etc/json-router".into(),
        version: "1.0".to_string(),
    };

    let mut client = NodeClient::connect(config).await?;
    
    println!("Conectado como: {}", client.full_name());  
    // → "AI.soporte.l1@produccion"

    // Loop simple: recibir y responder
    loop {
        let msg = client.recv().await?;
        let response = process_message(&msg);
        client.send(response).await?;
    }
}
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
