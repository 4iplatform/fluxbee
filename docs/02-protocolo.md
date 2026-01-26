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

### 7.5 Tiempo

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

### 10.1 Responsabilidades

| Responsabilidad | Dónde se implementa |
|-----------------|---------------------|
| Generar UUID | Librería |
| Persistir UUID | Librería |
| **Agregar @isla al nombre** | **Librería** (desde island.yaml) |
| Conexión socket | Librería |
| Framing | Librería |
| Handshake HELLO | Librería |
| Reconexión automática | Librería |

### 10.2 Flujo de Conexión

```
1. Leer /etc/json-router/island.yaml → obtener island_id
   - Si no existe → ERROR, no arrancar
   
2. Cargar o generar UUID desde archivo de persistencia

3. Construir nombre L2 completo:
   - Config del nodo: name = "AI.soporte.l1"
   - Librería agrega: name = "AI.soporte.l1@produccion"

4. Conectar al socket del router

5. Enviar HELLO con nombre completo

6. Esperar ANNOUNCE → obtener vpn_id asignado

7. Listo para enviar/recibir mensajes
```

### 10.3 Ejemplo de Uso (Rust)

```rust
use json_router::node_client::{NodeClient, NodeConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // La librería lee island.yaml automáticamente
    let config = NodeConfig {
        name: "AI.soporte.l1".to_string(),  // SIN @isla
        router_socket: "/var/run/json-router/routers".into(),
        uuid_persistence_dir: "/var/lib/json-router/state/nodes/".into(),
        config_dir: "/etc/json-router".into(),
        version: "1.0".to_string(),
    };
    // Si se quiere un router específico:
    // router_socket: "/var/run/json-router/routers/<router-uuid>.sock"

    let client = NodeClient::connect(config).await?;
    
    // La librería agregó @isla automáticamente
    println!("Conectado como: {}", client.full_name());  
    // → "AI.soporte.l1@produccion"
    
    println!("VPN asignado: {}", client.vpn_id());
    // → 10

    loop {
        let msg = client.recv().await?;
        // Procesar...
    }
}
```

### 10.4 Persistencia de UUID

```
/var/lib/json-router/state/nodes/<nombre-sin-isla>.uuid
```

Ejemplo: `/var/lib/json-router/state/nodes/AI.soporte.l1.uuid`

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
