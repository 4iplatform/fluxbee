# JSON Router - Nodos SY (System)

**Estado:** Draft v0.11
**Fecha:** 2025-01-19
**Documento relacionado:** JSON Router Especificación Técnica v1.11

---

## 1. Introducción

Los nodos SY (System) son componentes de infraestructura que proveen servicios esenciales al sistema de mensajería. A diferencia de los nodos AI, WF e IO que procesan lógica de negocio, los nodos SY mantienen la operación del sistema mismo.

### 1.1 Características comunes

- **Lenguaje:** Rust (mismo stack que el router)
- **Distribución:** Parte del paquete `json-router`
- **Privilegios:** Pueden acceder a shared memory directamente
- **Ciclo de vida:** Gestionados por systemd junto con los routers
- **Nomenclatura:** `SY.<servicio>.{isla}`
- **Librería compartida:** Usan `node-lib` del router (ver Parte II-B de la spec principal)

### 1.3 Uso de la Librería de Nodos (node-lib)

**IMPORTANTE:** Todos los nodos SY deben usar la librería estándar `node-lib` para comunicación con el router. Ver **Parte II-B: Librería de Nodos** en la especificación técnica del router para detalles completos.

La librería maneja:
- **Generación y persistencia de UUID** (el nodo no genera UUIDs manualmente)
- **Conexión al router** (socket Unix)
- **Handshake HELLO** (automático al conectar)
- **Framing de mensajes** (length-prefix)
- **Reconexión automática** (con backoff)

**Uso en nodo SY:**

```rust
use json_router::node_client::{NodeClient, NodeConfig};
use json_router::protocol::{Message, Meta};

#[tokio::main]
async fn main() -> Result<()> {
    let config = NodeConfig {
        name: format!("SY.config.routes.{}", island_id),
        island_id: island_id.clone(),
        router_socket: "/var/run/json-router/router.sock".into(),
        uuid_persistence_dir: "/var/lib/json-router/nodes/".into(),
    };
    
    // Conectar (UUID y HELLO automáticos)
    let client = NodeClient::connect(config).await?;
    
    // Usar send/recv para comunicación
    client.send(message).await?;
    let msg = client.recv().await?;
    
    Ok(())
}
```

**Regla:** Si un nodo SY toca sockets, framing, o UUIDs directamente, debe refactorizarse para usar la librería.

### 1.4 Catálogo de nodos SY

| Nodo | Instancias | Estado | Descripción |
|------|------------|--------|-------------|
| `SY.admin` | Único global | Especificado | Gateway HTTP REST para toda la infraestructura |
| `SY.orchestrator` | Uno por isla | Especificado | Orquestación de routers y nodos |
| `SY.config.routes` | Uno por isla | Especificado | Configuración de rutas estáticas y VPNs |
| `SY.opa.rules` | Uno por isla | Especificado | Gestión de policies OPA |
| `SY.time` | Uno por isla | Por especificar | Sincronización de tiempo |
| `SY.log` | Uno por isla | Por especificar | Colector de logs |

---

## 2. SY.config.routes

Responsable de la configuración centralizada de rutas estáticas y VPNs.

### 2.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.config.routes` (uno por isla, sin instancia) |
| Región SHM | `/jsr-config-<island_id>` |
| Persistencia | `/etc/json-router/sy-config-routes.yaml` |
| Propagación | Broadcast (CONFIG_ANNOUNCE) |

**Restricciones:**
- Solo puede correr UNO por isla
- Si ya hay uno corriendo, el segundo debe salir con error
- Lee island_id del archivo de configuración (no por CLI)

### 2.2 Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                      SY.config.routes                       │
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ API Handler │  │ SHM Writer  │  │ Persistence │         │
│  │             │  │             │  │             │         │
│  │ - add_route │  │ - seqlock   │  │ - sy-config │         │
│  │ - del_route │  │ - heartbeat │  │   -routes.  │         │
│  │ - list_*    │  │ - version++ │  │   yaml      │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         └────────────────┼────────────────┘                 │
│                          │                                  │
│  ┌───────────────────────┴───────────────────────┐         │
│  │              Broadcast Handler                 │         │
│  │                                                │         │
│  │  - Envía CONFIG_ANNOUNCE (broadcast)          │         │
│  │  - Recibe CONFIG_ANNOUNCE de otras islas      │         │
│  │  - Actualiza zona global en shm               │         │
│  └───────────────────────────────────────────────┘         │
└──────────────────────────────────────────────────────────────┘
                           │
                           ▼
              /jsr-config-<island_id>
              ┌─────────────────────┐
              │ LocalRoutes[]       │ ← De archivo local
              │ GlobalRoutes[]      │ ← De broadcasts recibidos
              └─────────────────────┘
```

**Separación de responsabilidades:**
- **Router:** Solo mueve mensajes (data plane). Lee config de shm, no la propaga.
- **SY.config.routes:** Escribe config local, hace broadcast, recibe broadcasts de otras islas.

### 2.3 Línea de comandos

```bash
# Arranque simple - todo por default
sy-config-routes

# Con directorio custom
sy-config-routes --config-dir /otro/path
```

| Parámetro | CLI | Env | Default | Obligatorio |
|-----------|-----|-----|---------|-------------|
| Config directory | `--config-dir` | `JSR_CONFIG_DIR` | `/etc/json-router` | No |
| Log level | `--log-level` | `JSR_LOG_LEVEL` | `info` | No |

**No hay parámetros obligatorios.** Todo se lee del archivo de configuración.

### 2.4 Directorios por Default

| Directorio | Default | Descripción |
|------------|---------|-------------|
| Config | `/etc/json-router/` | Archivos de configuración |
| State | `/var/lib/json-router/` | Estado persistente |
| Sockets | `/var/run/json-router/` | Unix sockets |
| SHM | `/dev/shm/` | Shared memory (automático) |

### 2.5 Archivos

```
/etc/json-router/
├── island.yaml              # Config de la isla (lee island_id de acá)
└── sy-config-routes.yaml    # Rutas estáticas locales (este nodo lo maneja)

/dev/shm/
└── jsr-config-<island>      # Región SHM (este nodo la crea/escribe)
```

**Convención de nombres:** `sy-<servicio>.yaml` para todos los nodos SY.

### 2.6 Flujo de arranque

```
sy-config-routes
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Leer configuración                                       │
│    - Leer /etc/json-router/island.yaml → island_id         │
│    - Leer /etc/json-router/sy-config-routes.yaml → rutas   │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. Verificar instancia única                                │
│    - Intentar abrir /jsr-config-<island>                   │
│    - Si existe y owner_pid está vivo → EXIT "Ya corriendo" │
│    - Si existe y owner muerto → Reclamar región            │
│    - Si no existe → Crear región                           │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Inicializar shared memory                                │
│    - Escribir header (magic, version, owner_pid, etc.)     │
│    - Escribir rutas locales (de archivo)                   │
│    - Zona global vacía (se llena con broadcasts)           │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Conectar al router local                                 │
│    - Crear socket en /var/run/json-router/                 │
│    - Enviar HELLO, registrarse como SY.config.routes       │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Broadcast inicial                                        │
│    - Enviar CONFIG_ANNOUNCE con rutas locales              │
│    - Otras islas reciben y actualizan su zona global       │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Loop principal                                           │
│    - Actualizar heartbeat en SHM cada 5s                   │
│    - Escuchar mensajes (API, CONFIG_ANNOUNCE de otros)     │
│    - Broadcast periódico de refresh (cada 60s)             │
└─────────────────────────────────────────────────────────────┘
```
┌─────────────────────────────────────────────────────────────┐
│ 2. Verificar región SHM existente                           │
│    - Si existe y owner vivo → ERROR (ya hay primary)        │
│    - Si existe y owner muerto → Reclamar                    │
│    - Si no existe → Crear                                   │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Cargar configuración                                     │
│    - Leer /etc/json-router/routes.yaml                     │
│    - Si no existe → Crear vacío                            │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Escribir en región SHM                                   │
│    - Inicializar header (magic, version, owner, etc.)      │
│    - Escribir todas las rutas y VPNs                       │
│    - config_version = 1                                    │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Conectar al router local                                 │
│    - Crear socket en node_socket_dir                       │
│    - Enviar HELLO, recibir asignación de UUID              │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Loop principal                                           │
│    - Actualizar heartbeat en SHM cada 5s                   │
│    - Escuchar mensajes de API                              │
│    - Procesar requests, actualizar SHM, persistir          │
└─────────────────────────────────────────────────────────────┘
```

### 2.6 API de mensajes

El nodo recibe mensajes JSON via el router (como cualquier nodo).

#### 2.6.1 Listar rutas estáticas

**Request:**
```json
{
  "routing": {
    "src": "<uuid-cliente>",
    "dst": "<uuid-sy-config>",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "admin",
    "action": "list_routes"
  },
  "payload": {}
}
```

**Response:**
```json
{
  "routing": { "src": "<uuid-sy-config>", "dst": "<uuid-cliente>", ... },
  "meta": { "type": "admin", "action": "list_routes", "status": "ok" },
  "payload": {
    "config_version": 42,
    "routes": [
      {
        "prefix": "AI.soporte.*",
        "match_kind": "PREFIX",
        "action": "FORWARD",
        "next_hop_island": "produccion-us",
        "metric": 10,
        "priority": 100,
        "installed_at": 1737110400000
      }
    ]
  }
}
```

#### 2.6.2 Agregar ruta estática

**Request:**
```json
{
  "meta": { "type": "admin", "action": "add_route" },
  "payload": {
    "prefix": "AI.ventas.*",
    "match_kind": "PREFIX",
    "action": "FORWARD",
    "next_hop_island": "produccion-us",
    "metric": 10,
    "priority": 100
  }
}
```

**Response (éxito):**
```json
{
  "meta": { "type": "admin", "action": "add_route", "status": "ok" },
  "payload": {
    "config_version": 43,
    "route": { ... }
  }
}
```

**Response (error):**
```json
{
  "meta": { "type": "admin", "action": "add_route", "status": "error" },
  "payload": {
    "error": "DUPLICATE_PREFIX",
    "message": "Route with prefix 'AI.ventas.*' already exists"
  }
}
```

#### 2.6.3 Eliminar ruta estática

**Request:**
```json
{
  "meta": { "type": "admin", "action": "delete_route" },
  "payload": {
    "prefix": "AI.ventas.*"
  }
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "delete_route", "status": "ok" },
  "payload": {
    "config_version": 44
  }
}
```

#### 2.6.4 Listar VPNs

**Request:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns" },
  "payload": {}
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns", "status": "ok" },
  "payload": {
    "vpns": [
      {
        "vpn_id": 1,
        "vpn_name": "vpn-staging",
        "remote_island": "staging",
        "endpoints": ["10.0.1.100:9000", "10.0.1.101:9000"],
        "created_at": 1737110400000
      }
    ]
  }
}
```

#### 2.6.5 Agregar VPN

**Request:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn" },
  "payload": {
    "vpn_name": "vpn-staging",
    "remote_island": "staging",
    "endpoints": ["10.0.1.100:9000", "10.0.1.101:9000"]
  }
}
```

#### 2.6.6 Eliminar VPN

**Request:**
```json
{
  "meta": { "type": "admin", "action": "delete_vpn" },
  "payload": {
    "vpn_id": 1
  }
}
```

### 2.7 Códigos de error

| Error | Descripción |
|-------|-------------|
| `DUPLICATE_PREFIX` | Ya existe una ruta con ese prefix |
| `PREFIX_NOT_FOUND` | No existe ruta con ese prefix |
| `INVALID_PREFIX` | Formato de prefix inválido |
| `INVALID_ACTION` | Acción no reconocida |
| `INVALID_MATCH_KIND` | match_kind debe ser EXACT, PREFIX o GLOB |
| `VPN_NOT_FOUND` | No existe VPN con ese ID |
| `DUPLICATE_VPN_NAME` | Ya existe VPN con ese nombre |
| `MAX_ROUTES_EXCEEDED` | Se alcanzó el límite de 256 rutas |
| `MAX_VPNS_EXCEEDED` | Se alcanzó el límite de 64 VPNs |
| `PERSISTENCE_ERROR` | Error al guardar en disco |

### 2.8 Persistencia: sy-config-routes.yaml

```yaml
# /etc/json-router/sy-config-routes.yaml
# Solo rutas LOCALES de esta isla
# Rutas de otras islas NO se persisten (llegan por broadcast)

version: 1
updated_at: "2025-01-18T10:30:00Z"

routes:
  # Rutas capa 2 (se propagan por broadcast)
  - prefix: "AI.soporte.*"
    match_kind: PREFIX
    action: FORWARD
    metric: 10
    priority: 100
    
  - prefix: "AI.ventas.interno"
    match_kind: EXACT
    action: DROP
    
  # Rutas capa 1 (NO se propagan, solo locales)
  - prefix: "a1b2c3d4-e5f6-7890-abcd-ef1234567890"
    match_kind: EXACT
    action: FORWARD
    metric: 5

vpns:
  - vpn_id: 1
    vpn_name: vpn-staging
    remote_island: staging
    endpoints:
      - "10.0.1.100:9000"
      - "10.0.1.101:9000"
```

**Nota:** Este archivo solo contiene rutas de ESTA isla. Las rutas de otras islas se reciben por broadcast y se guardan solo en shm (no se persisten).

### 2.9 Systemd

```ini
# /etc/systemd/system/sy-config-routes.service

[Unit]
Description=JSON Router Config Service
After=network.target json-router.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-config-routes
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
# Habilitar
systemctl enable sy-config-routes

# Iniciar
systemctl start sy-config-routes

# Ver logs
journalctl -u sy-config-routes -f
```

**Nota:** No hay template `@` porque solo puede haber uno por isla/máquina.

### 2.10 Propagación entre Islas (Broadcast)

La propagación de configuración entre islas usa **broadcast**, no mensajes punto a punto. El router maneja el broadcast de forma transparente.

#### 2.10.1 Modelo Simplificado

```
┌─────────────────────────────────────────────────────────────────────────┐
│                              BROADCAST                                  │
│                                                                         │
│   SY.config.A ─────► CONFIG_ANNOUNCE ─────► todas las islas            │
│   SY.config.B ─────► CONFIG_ANNOUNCE ─────► todas las islas            │
│   SY.config.C ─────► CONFIG_ANNOUNCE ─────► todas las islas            │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘

Cada SY.config.routes:
  - Archivo local: solo rutas de SU isla (persiste)
  - SHM local: rutas propias + zona global (recibida por broadcast)
  - NO persiste rutas de otras islas (se reconstruyen al arrancar)
```

**Principios:**
- El archivo `sy-config-routes.yaml` solo tiene rutas locales
- La zona global en shm se llena con broadcasts recibidos
- Si reinicia, espera broadcasts de otras islas para reconstruir zona global

#### 2.10.2 Mensaje CONFIG_ANNOUNCE

```json
{
  "routing": {
    "src": "<uuid-sy-config>",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "CONFIG_ANNOUNCE",
    "target": "SY.config.*"
  },
  "payload": {
    "island": "produccion",
    "version": 42,
    "timestamp": "2025-01-18T10:00:00Z",
    "routes": [
      {
        "prefix": "AI.soporte.*",
        "match_kind": "PREFIX",
        "action": "FORWARD",
        "metric": 10,
        "priority": 100
      }
    ],
    "vpns": [ ... ]
  }
}
```

**Campos importantes:**
- `dst: "broadcast"` - El router lo envía a todos
- `meta.target: "SY.config.*"` - Filtro: solo nodos SY.config.* lo procesan
- `payload.island` - Quién origina esta configuración
- `payload.version` - Para detectar duplicados/orden

#### 2.10.3 Cuándo hacer Broadcast

| Evento | Acción |
|--------|--------|
| Arranque | Broadcast inmediato de rutas locales |
| Cambio de config local | Broadcast inmediato |
| Timer periódico (60s) | Broadcast de refresh |

El refresh periódico asegura que islas que arrancaron después reciban la config.

#### 2.10.4 Al Recibir CONFIG_ANNOUNCE

```
1. ¿Es de mi propia isla? → Ignorar
2. ¿version > la que tengo de esa isla? 
   → Sí: Actualizar zona global en shm
   → No: Ignorar (ya tengo igual o más nuevo)
```

**No se persiste.** Solo se guarda en shm. Si reinicia, espera nuevos broadcasts.

#### 2.10.5 Estructura de la Shared Memory

```
/jsr-config-<island>
├── Header
│   ├── magic, version
│   ├── owner_pid, heartbeat
│   └── local_version, global_versions[]
│
├── LocalRoutes[]           ← De sy-config-routes.yaml
│   └── Rutas de ESTA isla
│
└── GlobalRoutes[]          ← De CONFIG_ANNOUNCE recibidos
    ├── [isla-a]: version, timestamp, routes[]
    ├── [isla-b]: version, timestamp, routes[]
    └── ...
```

#### 2.10.6 Rutas que NO se propagan

Solo se propagan rutas **capa 2** (por nombre). Rutas capa 1 (por UUID) son locales.

```yaml
# sy-config-routes.yaml

routes:
  # Esta se propaga (capa 2, nombre)
  - prefix: "AI.ventas.*"
    match_kind: PREFIX
    action: FORWARD
    
  # Esta NO se propaga (capa 1, UUID)
  - prefix: "a1b2c3d4-e5f6-..."
    match_kind: EXACT
    action: DROP
```

**Detección automática:** Si prefix es un UUID válido → no propagar.

#### 2.10.7 Consistencia Eventual

- Sin coordinación distribuida (no hay Paxos/Raft)
- Puede haber ventanas donde islas tienen versiones distintas
- El refresh periódico converge eventualmente
- Para operaciones críticas: verificar version antes de actuar

---

## 3. SY.opa.rules

Gestión centralizada de policies OPA para routing dinámico.

### 3.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.opa.rules` (uno por isla) |
| Función | Compilar y distribuir policies Rego/WASM |
| Persistencia | `/var/lib/json-router/policy.wasm` |
| Propagación | Broadcast `OPA_RELOAD` |

### 3.2 Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                      SY.opa.rules                           │
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ API Handler │  │ Compiler    │  │ Distributor │         │
│  │             │  │             │  │             │         │
│  │ - update    │  │ - opa build │  │ - write     │         │
│  │ - get       │  │ - validate  │  │   policy    │         │
│  │ - status    │  │             │  │ - broadcast │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         └────────────────┼────────────────┘                 │
│                          │                                  │
│  ┌───────────────────────┴───────────────────────┐         │
│  │              Verification Handler              │         │
│  │                                                │         │
│  │  - Lee SHM de todos los routers               │         │
│  │  - Verifica opa_policy_version                │         │
│  │  - Rollback si falla                          │         │
│  └───────────────────────────────────────────────┘         │
└──────────────────────────────────────────────────────────────┘
```

### 3.3 Directorios

```
/var/lib/json-router/
├── policy.wasm          # WASM activo (runtime)
├── policy.wasm.bak      # Backup para rollback
└── policy.wasm.new      # Temporal durante actualización

/etc/json-router/
└── policy.rego          # Fuente Rego (opcional, referencia)
```

**Nota:** El directorio `/var/lib/json-router/` es compartido por todos los routers de la isla. SY.opa.rules escribe ahí; los routers leen.

### 3.4 Flujo de Arranque

```
sy-opa-rules
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Leer configuración                                       │
│    - /etc/json-router/island.yaml → island_id              │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. Verificar instancia única                                │
│    - Lock file o similar                                    │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Conectar al router local                                 │
│    - Registrarse como SY.opa.rules                         │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Loop principal                                           │
│    - Escuchar API (update_policy, get_policy, status)      │
│    - Monitorear estado de routers en SHM                   │
└─────────────────────────────────────────────────────────────┘
```

### 3.5 API

#### 3.5.1 Actualizar Policy

**Request:**
```json
{
  "meta": { "type": "admin", "action": "update_policy" },
  "payload": {
    "rego": "package router\n\ndefault target = null\n\ntarget = \"AI.soporte.l1\" {\n    input.meta.context.cliente_tier == \"standard\"\n}\n"
  }
}
```

**Response OK:**
```json
{
  "meta": { "type": "admin", "action": "update_policy" },
  "payload": {
    "status": "ok",
    "version": 42,
    "routers_updated": 3,
    "routers_total": 3
  }
}
```

**Response ERROR (compilación):**
```json
{
  "meta": { "type": "admin", "action": "update_policy" },
  "payload": {
    "status": "error",
    "error": "compilation_failed",
    "detail": "policy.rego:5: rego_parse_error: unexpected token"
  }
}
```

**Response ERROR (rollback):**
```json
{
  "meta": { "type": "admin", "action": "update_policy" },
  "payload": {
    "status": "error",
    "error": "rollback",
    "detail": "1 router failed to load new policy",
    "failed_routers": ["uuid-router-b"],
    "version": 41
  }
}
```

#### 3.5.2 Obtener Policy Actual

**Request:**
```json
{
  "meta": { "type": "admin", "action": "get_policy" },
  "payload": {}
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "get_policy" },
  "payload": {
    "version": 42,
    "rego": "package router\n...",
    "updated_at": "2025-01-18T10:00:00Z"
  }
}
```

#### 3.5.3 Estado de Routers

**Request:**
```json
{
  "meta": { "type": "admin", "action": "policy_status" },
  "payload": {}
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "policy_status" },
  "payload": {
    "current_version": 42,
    "routers": [
      {
        "uuid": "uuid-router-a",
        "opa_policy_version": 42,
        "opa_load_status": 0,
        "status": "ok"
      },
      {
        "uuid": "uuid-router-b",
        "opa_policy_version": 42,
        "opa_load_status": 0,
        "status": "ok"
      }
    ]
  }
}
```

### 3.6 Flujo de Actualización con Rollback

```
Recibe update_policy con nuevo Rego
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 1. Compilar Rego → WASM                                     │
│    - Ejecutar: opa build -t wasm -e router/target          │
│    - Si falla → responder error, FIN                       │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 2. Preparar archivos                                        │
│    - Guardar WASM nuevo como policy.wasm.new               │
│    - version_nueva = version_actual + 1                     │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 3. Swap atómico                                             │
│    - Renombrar policy.wasm → policy.wasm.bak               │
│    - Renombrar policy.wasm.new → policy.wasm               │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 4. Broadcast OPA_RELOAD                                     │
│    - dst: "broadcast", TTL: 16                             │
│    - payload: { version: 42 }                              │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 5. Esperar verificación (5s configurable)                   │
└─────────────────────────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────────────────────────┐
│ 6. Verificar routers (leer SHM)                            │
│    - Escanear /dev/shm/jsr-*                               │
│    - Leer opa_policy_version de cada header                │
└─────────────────────────────────────────────────────────────┘
    │
    ├─► Todos OK (version == version_nueva)
    │       │
    │       ▼
    │   ┌─────────────────────────────────────────────────────┐
    │   │ 7a. Éxito                                           │
    │   │    - Borrar policy.wasm.bak                        │
    │   │    - Guardar Rego fuente (opcional)                │
    │   │    - Responder OK                                  │
    │   └─────────────────────────────────────────────────────┘
    │
    └─► Alguno FALLÓ (version != version_nueva o status == ERROR)
            │
            ▼
        ┌─────────────────────────────────────────────────────┐
        │ 7b. Rollback                                        │
        │    - Renombrar policy.wasm.bak → policy.wasm       │
        │    - Broadcast OPA_RELOAD con version_anterior     │
        │    - Esperar verificación                          │
        │    - Responder ERROR con lista de routers fallidos │
        └─────────────────────────────────────────────────────┘
```

### 3.7 Mensaje OPA_RELOAD

```json
{
  "routing": {
    "src": "<uuid-sy-opa-rules>",
    "dst": "broadcast",
    "ttl": 16,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "OPA_RELOAD"
  },
  "payload": {
    "version": 42,
    "hash": "sha256:abc123..."
  }
}
```

El router usa `version` para actualizar `opa_policy_version` en su SHM después de cargar exitosamente.

### 3.8 Códigos de Error

| Error | Descripción |
|-------|-------------|
| `COMPILATION_FAILED` | Rego no compila |
| `INVALID_REGO` | Sintaxis inválida |
| `ROLLBACK` | Algunos routers no cargaron, se revirtió |
| `TIMEOUT` | Routers no respondieron a tiempo |
| `NO_ROUTERS` | No hay routers activos en la isla |

### 3.9 Systemd

```ini
# /etc/systemd/system/sy-opa-rules.service

[Unit]
Description=JSON Router OPA Rules Service
After=network.target json-router.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-opa-rules
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

---

## 4. SY.time

Sincronización de tiempo en la red.

### 3.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.time.<instancia>` |
| Función | Broadcast periódico de tiempo UTC |
| Dependencias | Ninguna (puede usar NTP del sistema) |

### 3.2 Funcionalidad

- Broadcast periódico (cada 1s o configurable) con timestamp UTC
- Los nodos pueden usar este tiempo para sincronizar operaciones
- No requiere región SHM propia (usa mensajes)

### 3.3 Mensaje TIME_SYNC

```json
{
  "routing": {
    "src": "<uuid-sy-time>",
    "dst": "broadcast",
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "TIME_SYNC"
  },
  "payload": {
    "timestamp_utc": "2025-01-17T10:30:00.123Z",
    "epoch_ms": 1737110400123,
    "seq": 12345
  }
}
```

**dst: "broadcast"** envía a todos los nodos. **TTL=1** limita a la isla local (no cruza WAN).

### 3.4 Por especificar

- Mecanismo de broadcast (OPA policy, lista de suscriptores, o multicast address)
- Precisión requerida
- Manejo de drift

---

## 5. SY.admin

Gateway HTTP REST único para toda la infraestructura. Punto de entrada para humanos y AI de infraestructura.

### 5.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre L2 | `SY.admin` (único en todo el sistema) |
| Función | Gateway HTTP → protocolo interno |
| Interfaz | HTTP REST |
| Seguridad | Ninguna (usar nginx/proxy externo si se requiere) |
| Socket | `/var/run/json-router/nodes/` (como cualquier nodo) |

### 5.2 Conexión al Router

SY.admin se conecta como un nodo normal:

```
1. Conecta a socket en /var/run/json-router/nodes/
2. Envía HELLO:
   {
     "meta": { "type": "system", "msg": "HELLO" },
     "payload": {
       "name": "SY.admin",
       "version": "1.0"
     }
   }
3. Router lo registra con nombre L2 "SY.admin"
4. SY.admin envía/recibe mensajes normalmente
```

**Requisito de deployment:** SY.admin debe correr en la primera isla del bus (la que tiene conectividad directa o transitiva a todas las demás islas).

### 5.3 Arquitectura

```
┌─────────────────────────────────────────────────────────────────────┐
│                           SY.admin                                  │
│                      (único en todo el sistema)                     │
│                                                                      │
│  ┌──────────────┐    ┌──────────────┐    ┌──────────────┐          │
│  │ HTTP Server  │    │  Translator  │    │  Router Conn │          │
│  │              │    │              │    │              │          │
│  │ REST API     │───►│ HTTP → Frame │───►│ Socket Unix  │          │
│  │ JSON in/out  │◄───│ Frame → HTTP │◄───│              │          │
│  └──────────────┘    └──────────────┘    └──────────────┘          │
└─────────────────────────────────────────────────────────────────────┘
                                │
                                ▼
                           Router local
                                │
            ┌───────────────────┼───────────────────┐
            │                   │                   │
            ▼                   ▼                   ▼
    ┌──────────────┐   ┌──────────────┐   ┌──────────────┐
    │ Isla A       │   │ Isla B       │   │ Isla C       │
    │              │   │              │   │              │
    │ SY.orch      │   │ SY.orch      │   │ SY.orch      │
    │ SY.config    │   │ SY.config    │   │ SY.config    │
    │ SY.opa       │   │ SY.opa       │   │ SY.opa       │
    └──────────────┘   └──────────────┘   └──────────────┘
```

**Principios:**
- Sin autenticación propia (delegar a proxy externo)
- Traduce HTTP REST a mensajes del protocolo
- Envía por socket, nunca lee SHM directo
- Conoce las islas por configuración

### 5.3 API REST

#### 5.3.1 Islas

```
GET /islands
```
Lista todas las islas conocidas.

```json
{
  "islands": [
    { "id": "produccion", "status": "online" },
    { "id": "staging", "status": "online" },
    { "id": "desarrollo", "status": "offline" }
  ]
}
```

```
GET /islands/{isla}/status
```
Estado detallado de una isla.

#### 5.3.2 Routers (→ SY.orchestrator)

```
GET /islands/{isla}/routers
```
Lista routers de la isla.

```json
{
  "routers": [
    {
      "uuid": "uuid-router-a",
      "name": "RT.produccion.primary",
      "is_primary": true,
      "wan_enabled": true,
      "nodes_count": 12,
      "status": "alive"
    }
  ]
}
```

```
POST /islands/{isla}/routers
```
Levantar router secundario.

```json
{
  "name": "RT.produccion.secondary",
  "config": { ... }
}
```

```
DELETE /islands/{isla}/routers/{uuid}
```
Matar router (no permite matar el primario).

#### 5.3.3 Nodos (→ SY.orchestrator)

```
GET /islands/{isla}/nodes
```
Lista todos los nodos conectados.

```json
{
  "nodes": [
    {
      "uuid": "uuid-node-1",
      "name": "AI.soporte.l1.español",
      "router": "uuid-router-a",
      "connected_at": "2025-01-19T10:00:00Z",
      "status": "active"
    }
  ]
}
```

```
POST /islands/{isla}/nodes
```
Correr nuevo nodo.

```json
{
  "type": "AI.soporte",
  "instance": "l1.español",
  "config": { ... }
}
```

```
DELETE /islands/{isla}/nodes/{uuid}
```
Matar nodo.

#### 5.3.4 Rutas Estáticas (→ SY.config.routes)

```
GET /islands/{isla}/routes
```
Lista rutas estáticas.

```json
{
  "version": 42,
  "routes": [
    {
      "prefix": "AI.soporte.*",
      "match_kind": "PREFIX",
      "action": "FORWARD",
      "metric": 10
    }
  ]
}
```

```
POST /islands/{isla}/routes
```
Agregar ruta.

```json
{
  "prefix": "AI.ventas.*",
  "match_kind": "PREFIX",
  "action": "FORWARD",
  "metric": 20
}
```

```
DELETE /islands/{isla}/routes/{prefix}
```
Eliminar ruta (prefix URL-encoded).

#### 5.3.5 VPNs (→ SY.config.routes)

```
GET /islands/{isla}/vpns
POST /islands/{isla}/vpns
DELETE /islands/{isla}/vpns/{id}
```

#### 5.3.6 OPA Policies (→ SY.opa.rules)

```
GET /islands/{isla}/opa/policy
```
Obtener policy actual.

```json
{
  "version": 42,
  "rego": "package router\n...",
  "updated_at": "2025-01-19T10:00:00Z"
}
```

```
POST /islands/{isla}/opa/policy
```
Actualizar policy.

```json
{
  "rego": "package router\n\ndefault target = null\n..."
}
```

```
GET /islands/{isla}/opa/status
```
Estado de OPA en routers.

```json
{
  "version": 42,
  "routers": [
    { "uuid": "uuid-a", "version": 42, "status": "ok" },
    { "uuid": "uuid-b", "version": 42, "status": "ok" }
  ]
}
```

### 5.4 Traducción HTTP → Mensajes

| HTTP | Destino | meta.action |
|------|---------|-------------|
| `GET /islands/{isla}/routes` | `SY.config.routes` de isla | `list_routes` |
| `POST /islands/{isla}/routes` | `SY.config.routes` de isla | `add_route` |
| `DELETE /islands/{isla}/routes/{p}` | `SY.config.routes` de isla | `delete_route` |
| `POST /islands/{isla}/opa/policy` | `SY.opa.rules` de isla | `update_policy` |
| `GET /islands/{isla}/nodes` | `SY.orchestrator` de isla | `list_nodes` |
| `POST /islands/{isla}/nodes` | `SY.orchestrator` de isla | `run_node` |
| `DELETE /islands/{isla}/nodes/{id}` | `SY.orchestrator` de isla | `kill_node` |

### 5.5 Configuración

```yaml
# /etc/json-router/sy-admin.yaml

islands:
  - id: produccion
  - id: staging
```

**Resolución de nombres L2:** SY.admin construye los destinos automáticamente:
- `SY.orchestrator.{isla}`
- `SY.config.routes.{isla}`
- `SY.opa.rules.{isla}`

Opcionalmente se puede sobrescribir por isla si se necesita:

```yaml
islands:
  - id: produccion
    orchestrator: "SY.orchestrator.produccion"
    config_routes: "SY.config.routes.produccion"
    opa_rules: "SY.opa.rules.produccion"
```

**Listen:** SY.admin expone HTTP en `0.0.0.0:8080` por defecto. Para cambiarlo, usar `JSR_ADMIN_LISTEN` (o `SY_ADMIN_LISTEN`).

**Nota sobre island_id:** El identificador de isla es un string simple (ej: `produccion`, `staging`), no sigue convención de capa 2. Es solo un identificador de ubicación.

### 5.6 Formato Común de Respuestas

Todas las APIs admin usan el mismo formato de respuesta:

**Éxito:**
```json
{
  "status": "ok",
  // ... datos específicos de la operación
}
```

**Error:**
```json
{
  "status": "error",
  "error": "ERROR_CODE",
  "detail": "Mensaje legible para humanos"
}
```

**Códigos de error estándar:**

| Código | Descripción |
|--------|-------------|
| `NOT_FOUND` | Recurso no existe |
| `INVALID_REQUEST` | Request malformado o parámetros inválidos |
| `TIMEOUT` | Timeout esperando respuesta de nodo destino |
| `UNREACHABLE` | No se puede alcanzar el nodo destino |
| `CANNOT_KILL_PRIMARY` | Intento de matar router primario |
| `COMPILATION_FAILED` | Rego no compila (para OPA) |
| `ROLLBACK` | Operación falló y se revirtió |
| `ALREADY_EXISTS` | Recurso ya existe (ej: ruta duplicada) |
| `INTERNAL_ERROR` | Error interno del sistema |

### 5.7 Nombres L2 Estándar de Nodos SY

| Nodo | Nombre L2 que anuncia en HELLO |
|------|-------------------------------|
| SY.admin | `SY.admin` |
| SY.orchestrator | `SY.orchestrator.{isla}` |
| SY.config.routes | `SY.config.routes.{isla}` |
| SY.opa.rules | `SY.opa.rules.{isla}` |
| SY.time | `SY.time.{isla}` |

Donde `{isla}` es el island_id (ej: `produccion`, `staging`).

**Ejemplo:** En isla "produccion", SY.orchestrator anuncia nombre `SY.orchestrator.produccion`.

### 5.8 Systemd

```ini
# /etc/systemd/system/sy-admin.service

[Unit]
Description=JSON Router Admin Gateway
After=network.target json-router.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-admin
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

---

## 6. SY.orchestrator

Orquestación de routers y nodos. Uno por isla.

### 6.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre L2 | `SY.orchestrator.{isla}` (uno por isla) |
| Función | Listar, correr, matar routers y nodos |
| Lectura | SHM de routers locales (scan `/dev/shm/jsr-*`) |
| Ejecución | systemd / docker / proceso directo |

### 6.2 Descubrimiento de Routers

SY.orchestrator descubre routers de su isla escaneando shared memory:

```rust
fn discover_routers(my_island: &str) -> Vec<RouterInfo> {
    let mut routers = Vec::new();
    
    // Scan /dev/shm/jsr-*
    for entry in glob("/dev/shm/jsr-*") {
        // Abrir región y leer header
        let shm = ShmRegion::open(&entry);
        let header = shm.read_header();
        
        // Filtrar por mi isla
        if header.island_id == my_island {
            routers.push(RouterInfo {
                uuid: header.router_uuid,
                pid: header.owner_pid,
                heartbeat: header.heartbeat,
                node_count: header.node_count,
                route_count: header.route_count,
                // ...
            });
        }
    }
    
    routers
}
```

**Información disponible en SHM header:**
- `router_uuid`: UUID del router
- `owner_pid`: PID del proceso
- `island_id`: Isla a la que pertenece
- `heartbeat`: Último heartbeat (para detectar si está vivo)
- `node_count`: Cantidad de nodos conectados
- `route_count`: Cantidad de rutas

### 6.3 Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                   SY.orchestrator.{isla}                    │
│                                                              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ API Handler │  │ SHM Reader  │  │ Executor    │         │
│  │             │  │             │  │             │         │
│  │ - list_*    │  │ - scan shm  │  │ - systemd   │         │
│  │ - run_*     │  │ - read hdrs │  │ - docker    │         │
│  │ - kill_*    │  │             │  │ - process   │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         └────────────────┼────────────────┘                 │
│                          │                                  │
└──────────────────────────┼──────────────────────────────────┘
                           │
           ┌───────────────┴───────────────┐
           │                               │
           ▼                               ▼
    /dev/shm/jsr-*                  systemd / docker
    (lectura)                       (ejecución)
```

### 6.4 Flujo de Arranque de una Isla

```
Systemd inicia servicios base:
    │
    ├── json-router (router principal, WAN activa)
    ├── sy-orchestrator
    ├── sy-config-routes
    └── sy-opa-rules
           │
           ▼
SY.orchestrator conecta al router
           │
           ▼
SY.orchestrator lee config de nodos a levantar
           │
           ▼
SY.orchestrator ejecuta nodos según config:
    ├── systemctl start ai-soporte@l1
    ├── systemctl start ai-ventas@l1
    └── ...
```

### 6.5 API (via mensajes)

#### 6.4.1 Listar Routers

**Request:**
```json
{
  "meta": { "type": "admin", "action": "list_routers" },
  "payload": {}
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "list_routers" },
  "payload": {
    "routers": [
      {
        "uuid": "uuid-router-a",
        "name": "RT.produccion.primary",
        "pid": 1234,
        "is_primary": true,
        "wan_enabled": true,
        "nodes_count": 12,
        "routes_count": 5,
        "heartbeat_age_ms": 2500,
        "status": "alive"
      }
    ]
  }
}
```

#### 6.4.2 Listar Nodos

**Request:**
```json
{
  "meta": { "type": "admin", "action": "list_nodes" },
  "payload": {}
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "list_nodes" },
  "payload": {
    "nodes": [
      {
        "uuid": "uuid-node-1",
        "name": "AI.soporte.l1.español",
        "router_uuid": "uuid-router-a",
        "connected_at": "2025-01-19T10:00:00Z",
        "status": "active"
      }
    ]
  }
}
```

#### 6.4.3 Correr Nodo

**Request:**
```json
{
  "meta": { "type": "admin", "action": "run_node" },
  "payload": {
    "type": "AI.soporte",
    "instance": "l1.español",
    "executor": "systemd",
    "config": { ... }
  }
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "run_node" },
  "payload": {
    "status": "ok",
    "pid": 5678,
    "name": "AI.soporte.l1.español"
  }
}
```

#### 6.4.4 Matar Nodo

**Request:**
```json
{
  "meta": { "type": "admin", "action": "kill_node" },
  "payload": {
    "uuid": "uuid-node-1"
  }
}
```

**Response:**
```json
{
  "meta": { "type": "admin", "action": "kill_node" },
  "payload": {
    "status": "ok",
    "name": "AI.soporte.l1.español"
  }
}
```

#### 6.4.5 Correr Router Secundario

**Request:**
```json
{
  "meta": { "type": "admin", "action": "run_router" },
  "payload": {
    "name": "RT.produccion.secondary"
  }
}
```

#### 6.4.6 Matar Router

**Request:**
```json
{
  "meta": { "type": "admin", "action": "kill_router" },
  "payload": {
    "uuid": "uuid-router-b"
  }
}
```

**Error si intenta matar el primario:**
```json
{
  "payload": {
    "status": "error",
    "error": "CANNOT_KILL_PRIMARY",
    "detail": "Cannot kill primary router with active WAN"
  }
}
```

### 6.6 Executors

SY.orchestrator soporta diferentes formas de ejecutar procesos:

| Executor | Comando | Uso |
|----------|---------|-----|
| `systemd` | `systemctl start {unit}` | Producción |
| `docker` | `docker run {image}` | Contenedores |
| `process` | `{binary} {args}` | Desarrollo |

Configurado por nodo o por defecto de la isla.

### 6.7 Protección del Router Primario

El router primario (con WAN activa) no puede ser matado via API:

```
kill_router(uuid):
    router = find_router(uuid)
    if router.is_primary and router.wan_enabled:
        return error("CANNOT_KILL_PRIMARY")
    else:
        executor.kill(router.pid)
```

Para matar el primario, hay que hacerlo manualmente o con shutdown de la isla.

### 6.8 Configuración

```yaml
# /etc/json-router/sy-orchestrator.yaml

executor: systemd  # default executor

node_templates:
  AI.soporte:
    executor: systemd
    unit: "ai-soporte@{instance}"
    
  AI.ventas:
    executor: docker
    image: "ai-ventas:latest"
    args: ["--instance", "{instance}"]

startup_nodes:
  - type: AI.soporte
    instance: l1.español
  - type: AI.soporte
    instance: l1.ingles
  - type: AI.ventas
    instance: l1
```

### 6.9 Systemd

```ini
# /etc/systemd/system/sy-orchestrator.service

[Unit]
Description=JSON Router Orchestrator
After=network.target json-router.service

[Service]
Type=simple
ExecStart=/usr/bin/sy-orchestrator
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

---

## 7. SY.time

Sincronización de tiempo en la red.

### 7.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.time.{isla}` (uno por isla) |
| Función | Broadcast periódico de tiempo UTC |
| Dependencias | Ninguna (puede usar NTP del sistema) |

### 7.2 Funcionalidad

- Broadcast periódico (cada 1s o configurable) con timestamp UTC
- Los nodos pueden usar este tiempo para sincronizar operaciones
- No requiere región SHM propia (usa mensajes)

### 7.3 Mensaje TIME_SYNC

```json
{
  "routing": {
    "src": "<uuid-sy-time>",
    "dst": "broadcast",
    "ttl": 1,
    "trace_id": "<uuid>"
  },
  "meta": {
    "type": "system",
    "msg": "TIME_SYNC"
  },
  "payload": {
    "timestamp_utc": "2025-01-19T10:30:00.123Z",
    "epoch_ms": 1737110400123,
    "seq": 12345
  }
}
```

**dst: "broadcast"** envía a todos los nodos. **TTL=1** limita a la isla local (no cruza WAN).

### 7.4 Por especificar

- Precisión requerida
- Manejo de drift
- Stratum/jerarquía de tiempo

---

## 8. SY.log

Colector centralizado de logs.

### 8.1 Resumen

| Aspecto | Valor |
|---------|-------|
| Nombre | `SY.log.{isla}` (uno por isla) |
| Función | Recibir y consolidar logs de todos los nodos |
| Salida | Archivo, stdout, o sistema externo |

### 8.2 Por especificar

- Formato de mensajes de log
- Rotación y retención
- Integración con sistemas externos (Loki, Elasticsearch)

---

## Apéndice A: Estructura del paquete

```
json-router/
├── Cargo.toml
├── src/
│   ├── bin/
│   │   ├── json-router.rs       # Router principal
│   │   ├── sy-admin.rs          # Gateway HTTP
│   │   ├── sy-orchestrator.rs   # Orquestador
│   │   ├── sy-config-routes.rs  # Rutas estáticas
│   │   ├── sy-opa-rules.rs      # Policies OPA
│   │   ├── sy-time.rs           # Tiempo
│   │   └── shm-watch.rs         # Herramienta de diagnóstico
│   ├── lib.rs                   # Biblioteca compartida
│   ├── protocol/                # Mensajes (pub)
│   ├── socket/                  # Framing (pub)
│   ├── shm/                     # Shared memory
│   └── ...
└── tests/
```

---

## Apéndice B: Prioridades de implementación

| Prioridad | Nodo | Razón |
|-----------|------|-------|
| 1 | `SY.config.routes` | Necesario para rutas estáticas y VPNs |
| 2 | `SY.opa.rules` | Necesario para routing dinámico |
| 3 | `SY.orchestrator` | Necesario para gestión de nodos |
| 4 | `SY.admin` | Gateway HTTP para administración |
| 5 | `SY.time` | Útil pero no crítico inicialmente |
| 6 | `SY.log` | Puede usar logging estándar por ahora |
