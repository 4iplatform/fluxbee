# JSON Router - 03 Shared Memory

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core

---

## 1. Modelo de Tres Regiones

El sistema usa tres tipos de regiones de memoria compartida:

```
/dev/shm/
├── jsr-<router-uuid>        # Una por router
├── jsr-config-<island>      # Una por isla
└── jsr-lsa-<island>         # Una por isla
```

| Región | Writer | Contenido |
|--------|--------|-----------|
| `jsr-<uuid>` | Router dueño | Nodos CONNECTED a ese router |
| `jsr-config-<island>` | SY.config.routes | Rutas estáticas, tabla VPN |
| `jsr-lsa-<island>` | Gateway | Topología de islas remotas |

**Principio clave:** Cada región tiene **un único writer** y múltiples readers. Esto permite usar seqlock sin conflictos.

---

## 2. Región del Router: jsr-<uuid>

Cada router escribe únicamente en su propia región.

### 2.1 Naming

```
/jsr-<router_uuid>

Ejemplo: /jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

**En macOS:** Truncar a 31 caracteres (primeros 16 hex del UUID).

### 2.2 Contenido

| Dato | Descripción |
|------|-------------|
| Header | Identificación, seqlock, heartbeat |
| Nodos | Nodos conectados directamente a este router |

### 2.3 Layout

```
┌─────────────────────────────────────────────────────────────┐
│ ShmHeader (192 bytes)                                       │
├─────────────────────────────────────────────────────────────┤
│ NodeEntry[MAX_NODES] (1024 entries × 320 bytes)             │
│ Total: ~320 KB                                              │
└─────────────────────────────────────────────────────────────┘
Total aproximado: ~320 KB por router
```

### 2.4 Estructuras

```rust
pub const SHM_MAGIC: u32 = 0x4A535352;  // "JSSR"
pub const SHM_VERSION: u32 = 2;
pub const MAX_NODES: u32 = 1024;

#[repr(C)]
pub struct ShmHeader {
    // Identificación (8 bytes)
    pub magic: u32,
    pub version: u32,
    
    // Owner (40 bytes)
    pub router_uuid: [u8; 16],
    pub owner_pid: u32,
    pub _pad0: u32,
    pub owner_start_time: u64,
    pub generation: u64,
    
    // Seqlock (8 bytes)
    pub seq: u64,  // AtomicU64
    
    // Contadores (8 bytes)
    pub node_count: u32,
    pub node_max: u32,
    
    // Timestamps (24 bytes)
    pub created_at: u64,
    pub updated_at: u64,
    pub heartbeat: u64,
    
    // Isla (66 bytes)
    pub island_id: [u8; 64],
    pub island_id_len: u16,
    
    // Router info (66 bytes)
    pub router_name: [u8; 64],      // "RT.primary@produccion"
    pub router_name_len: u16,
    
    // Flags (4 bytes)
    pub is_gateway: u8,              // 1 si este router es el gateway
    pub _flags_reserved: [u8; 3],

    // OPA policy (16 bytes)
    pub opa_policy_version: u64,    // Versión del policy cargado (0 = no cargado)
    pub opa_load_status: u8,        // 0=OK, 1=ERROR, 2=LOADING
    pub _opa_pad: [u8; 7],
    
    // Reserved (para llegar a 224 bytes)
    pub _reserved: [u8; 6],
}
// Total: 224 bytes

#[repr(C)]
pub struct NodeEntry {
    // Identificación (16 bytes)
    pub uuid: [u8; 16],
    
    // Nombre L2 (258 bytes)
    pub name: [u8; 256],             // "AI.soporte.l1@produccion"
    pub name_len: u16,
    
    // VPN (4 bytes)
    pub vpn_id: u32,
    
    // Estado (10 bytes)
    pub flags: u16,
    pub connected_at: u64,
    
    // Reserved (32 bytes)
    pub _reserved: [u8; 32],
}
// Total: 320 bytes
```

---

## 3. Región de Config: jsr-config-<island>

Una sola región por isla, escrita únicamente por `SY.config.routes`.

### 3.1 Naming

```
/jsr-config-<island_id>

Ejemplo: /jsr-config-produccion
```

### 3.2 Contenido

| Dato | Descripción |
|------|-------------|
| Header | Identificación, seqlock, contadores |
| Rutas estáticas | Configuración de routing |
| Tabla VPN | Asignación de nodos a VPN |

### 3.3 Layout

```
┌─────────────────────────────────────────────────────────────┐
│ ConfigHeader (128 bytes)                                    │
├─────────────────────────────────────────────────────────────┤
│ StaticRouteEntry[MAX_STATIC_ROUTES] (256 × 320 bytes)      │
│ Total: ~80 KB                                               │
├─────────────────────────────────────────────────────────────┤
│ VpnAssignment[MAX_VPN_ASSIGNMENTS] (256 × 288 bytes)       │
│ Total: ~72 KB                                               │
└─────────────────────────────────────────────────────────────┘
Total aproximado: ~153 KB
```

### 3.4 Estructuras

```rust
pub const CONFIG_MAGIC: u32 = 0x4A534343;  // "JSCC"
pub const CONFIG_VERSION: u32 = 1;
pub const MAX_STATIC_ROUTES: u32 = 256;
pub const MAX_VPN_ASSIGNMENTS: u32 = 256;

#[repr(C)]
pub struct ConfigHeader {
    // Identificación (8 bytes)
    pub magic: u32,
    pub version: u32,
    
    // Owner (40 bytes)
    pub owner_uuid: [u8; 16],        // UUID de SY.config.routes
    pub owner_pid: u32,
    pub _pad0: u32,
    pub owner_start_time: u64,
    pub heartbeat: u64,
    
    // Seqlock (8 bytes)
    pub seq: u64,
    
    // Contadores (16 bytes)
    pub static_route_count: u32,
    pub vpn_assignment_count: u32,
    pub config_version: u64,         // Incrementa con cada cambio
    
    // Isla (66 bytes)
    pub island_id: [u8; 64],
    pub island_id_len: u16,
    
    // Timestamps (16 bytes)
    pub created_at: u64,
    pub updated_at: u64,
    
    // Reserved
    pub _reserved: [u8; 38],
}
// Total: 128 bytes

// Acciones de ruta
pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;

#[repr(C)]
pub struct StaticRouteEntry {
    // Pattern (260 bytes)
    pub prefix: [u8; 256],           // "AI.soporte.*"
    pub prefix_len: u16,
    pub match_kind: u8,              // EXACT, PREFIX, GLOB
    pub action: u8,                  // FORWARD, DROP
    
    // Forwarding (36 bytes)
    pub next_hop_island: [u8; 32],   // "" = local, "staging" = inter-isla
    pub next_hop_island_len: u8,
    pub _pad: [u8; 3],
    
    // Metadata (24 bytes)
    pub metric: u32,
    pub priority: u16,               // Menor = evalúa primero
    pub flags: u16,
    pub installed_at: u64,
    pub _reserved: [u8; 8],
}
// Total: 320 bytes

#[repr(C)]
pub struct VpnAssignment {
    // Pattern (260 bytes)
    pub pattern: [u8; 256],          // "AI.soporte.*"
    pub pattern_len: u16,
    pub match_kind: u8,
    pub _pad0: u8,
    
    // Asignación (8 bytes)
    pub vpn_id: u32,
    pub priority: u16,               // Menor = evalúa primero
    pub flags: u16,
    
    // Reserved (20 bytes)
    pub _reserved: [u8; 20],
}
// Total: 288 bytes
```

---

## 4. Región LSA: jsr-lsa-<island>

Una sola región por isla, escrita únicamente por el **Gateway**.

### 4.1 Naming

```
/jsr-lsa-<island_id>

Ejemplo: /jsr-lsa-produccion
```

### 4.2 Contenido

Topología de **otras islas** (no la local):

| Dato | Descripción |
|------|-------------|
| Header | Identificación, lista de islas remotas |
| Por cada isla remota | Nodos, rutas, VPNs de esa isla |

### 4.3 Layout

```
┌─────────────────────────────────────────────────────────────┐
│ LsaHeader (128 bytes)                                       │
├─────────────────────────────────────────────────────────────┤
│ RemoteIslandEntry[MAX_REMOTE_ISLANDS] (16 × 96 bytes)      │
│ Total: ~1.5 KB                                              │
├─────────────────────────────────────────────────────────────┤
│ RemoteNodeEntry[MAX_REMOTE_NODES] (1024 × 288 bytes)       │
│ Total: ~288 KB                                              │
├─────────────────────────────────────────────────────────────┤
│ RemoteRouteEntry[MAX_REMOTE_ROUTES] (256 × 320 bytes)      │
│ Total: ~80 KB                                               │
├─────────────────────────────────────────────────────────────┤
│ RemoteVpnEntry[MAX_REMOTE_VPNS] (256 × 288 bytes)          │
│ Total: ~72 KB                                               │
└─────────────────────────────────────────────────────────────┘
Total aproximado: ~442 KB
```

### 4.4 Estructuras

```rust
pub const LSA_MAGIC: u32 = 0x4A534C41;  // "JSLA"
pub const LSA_VERSION: u32 = 1;
pub const MAX_REMOTE_ISLANDS: u32 = 16;
pub const MAX_REMOTE_NODES: u32 = 1024;      // Total entre todas las islas
pub const MAX_REMOTE_ROUTES: u32 = 256;
pub const MAX_REMOTE_VPNS: u32 = 256;

#[repr(C)]
pub struct LsaHeader {
    // Identificación (8 bytes)
    pub magic: u32,
    pub version: u32,
    
    // Owner (40 bytes)
    pub gateway_uuid: [u8; 16],
    pub owner_pid: u32,
    pub _pad0: u32,
    pub owner_start_time: u64,
    pub heartbeat: u64,
    
    // Seqlock (8 bytes)
    pub seq: u64,
    
    // Contadores (16 bytes)
    pub island_count: u32,
    pub total_node_count: u32,
    pub total_route_count: u32,
    pub total_vpn_count: u32,
    
    // Timestamps (16 bytes)
    pub created_at: u64,
    pub updated_at: u64,
    
    // Isla local (66 bytes)
    pub local_island_id: [u8; 64],
    pub local_island_id_len: u16,
    
    // Reserved
    pub _reserved: [u8; 38],
}
// Total: 128 bytes

#[repr(C)]
pub struct RemoteIslandEntry {
    // Isla (66 bytes)
    pub island_id: [u8; 64],         // "staging"
    pub island_id_len: u16,
    
    // Estado (18 bytes)
    pub last_lsa_seq: u64,
    pub last_updated: u64,
    pub flags: u16,
    
    // Contadores (12 bytes)
    pub node_count: u32,
    pub route_count: u32,
    pub vpn_count: u32,
}
// Total: 96 bytes

#[repr(C)]
pub struct RemoteNodeEntry {
    // Identificación (16 bytes)
    pub uuid: [u8; 16],
    
    // Nombre L2 (258 bytes)
    pub name: [u8; 256],             // "AI.soporte.l1@staging"
    pub name_len: u16,
    
    // VPN y isla (6 bytes)
    pub vpn_id: u32,
    pub island_index: u16,           // Índice en RemoteIslandEntry[]
    
    // Flags (8 bytes)
    pub flags: u16,
    pub _reserved: [u8; 6],
}
// Total: 288 bytes

#[repr(C)]
pub struct RemoteRouteEntry {
    // Pattern (260 bytes)
    pub prefix: [u8; 256],
    pub prefix_len: u16,
    pub match_kind: u8,
    pub action: u8,
    
    // Forwarding (36 bytes)
    pub next_hop_island: [u8; 32],
    pub next_hop_island_len: u8,
    pub _pad: [u8; 3],
    
    // Metadata (24 bytes)
    pub metric: u32,
    pub priority: u16,
    pub flags: u16,
    pub island_index: u16,
    pub _reserved: [u8; 14],
}
// Total: 320 bytes

#[repr(C)]
pub struct RemoteVpnEntry {
    // Pattern (260 bytes)
    pub pattern: [u8; 256],
    pub pattern_len: u16,
    pub match_kind: u8,
    pub _pad0: u8,
    
    // Asignación (8 bytes)
    pub vpn_id: u32,
    pub priority: u16,
    pub flags: u16,
    
    // Isla (4 bytes)
    pub island_index: u16,
    pub _reserved: [u8; 18],
}
// Total: 288 bytes
```

---

## 5. Sincronización: Seqlock

Todas las regiones usan seqlock para sincronización.

### 5.1 Protocolo del Writer

```rust
// Comenzar escritura (seq queda impar)
header.seq.fetch_add(1, Ordering::Relaxed);

// Escribir datos
// ...

// Finalizar escritura (seq queda par)
atomic::fence(Ordering::Release);
header.seq.fetch_add(1, Ordering::Relaxed);
```

### 5.2 Protocolo del Reader

```rust
loop {
    let s1 = header.seq.load(Ordering::Acquire);
    if s1 & 1 != 0 {
        std::hint::spin_loop();
        continue;
    }
    
    atomic::fence(Ordering::Acquire);
    let data = /* copiar snapshot */;
    atomic::fence(Ordering::Acquire);
    
    let s2 = header.seq.load(Ordering::Acquire);
    if s1 == s2 {
        break;  // Snapshot consistente
    }
}
```

---

## 6. Heartbeat y Detección de Stale

Cada writer actualiza `heartbeat` cada 5 segundos.

```rust
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;
```

**Detección de región stale:**

```rust
fn is_region_stale(header: &impl HasHeartbeat) -> bool {
    let now = current_epoch_ms();
    now - header.heartbeat() > HEARTBEAT_STALE_MS
}
```

---

## 7. Flujo de Lectura del Router

El router lee las tres categorías de regiones:

```rust
fn build_routing_table(&mut self) {
    // 1. Leer mis nodos (jsr-<mi-uuid>)
    //    → Ya los tengo, soy el writer
    
    // 2. Leer nodos de peers (jsr-<peer-uuid>)
    for peer_shm in self.discover_peer_regions() {
        if !is_region_stale(&peer_shm.header) {
            for node in peer_shm.nodes {
                self.add_route_to_peer(node, peer_shm.router_uuid);
            }
        }
    }
    
    // 3. Leer config (jsr-config-<island>)
    if let Some(config) = self.map_config_region() {
        // Rutas estáticas
        for route in config.static_routes {
            self.add_static_route(route);
        }
        // Tabla VPN (para asignar VPN a nodos nuevos)
        self.vpn_table = config.vpn_assignments.clone();
    }
    
    // 4. Leer LSA (jsr-lsa-<island>)
    if let Some(lsa) = self.map_lsa_region() {
        for remote_island in lsa.islands {
            for node in remote_island.nodes {
                self.add_remote_route(node, remote_island.island_id);
            }
        }
    }
}
```

---

## 8. Descubrimiento de Regiones

### 8.1 Regiones de Routers

Via mensaje HELLO entre routers:

```json
{
  "meta": { "type": "system", "msg": "HELLO" },
  "payload": {
    "router_id": "uuid",
    "router_name": "RT.primary@produccion",
    "shm_name": "/jsr-a1b2c3d4..."
  }
}
```

### 8.2 Región de Config

Nombre fijo basado en island_id:

```rust
let config_shm = format!("/jsr-config-{}", island_id);
```

### 8.3 Región de LSA

Nombre fijo basado en island_id:

```rust
let lsa_shm = format!("/jsr-lsa-{}", island_id);
```

---

## 9. Constantes Consolidadas

```rust
// Región de routers
pub const SHM_MAGIC: u32 = 0x4A535352;
pub const SHM_VERSION: u32 = 1;
pub const MAX_NODES: u32 = 1024;

// Región de config
pub const CONFIG_MAGIC: u32 = 0x4A534343;
pub const CONFIG_VERSION: u32 = 1;
pub const MAX_STATIC_ROUTES: u32 = 256;
pub const MAX_VPN_ASSIGNMENTS: u32 = 256;

// Región LSA
pub const LSA_MAGIC: u32 = 0x4A534C41;
pub const LSA_VERSION: u32 = 1;
pub const MAX_REMOTE_ISLANDS: u32 = 16;
pub const MAX_REMOTE_NODES: u32 = 1024;
pub const MAX_REMOTE_ROUTES: u32 = 256;
pub const MAX_REMOTE_VPNS: u32 = 256;

// Match kinds
pub const MATCH_EXACT: u8 = 0;
pub const MATCH_PREFIX: u8 = 1;
pub const MATCH_GLOB: u8 = 2;

// Flags
pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;

// Timers
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;
```

---

## 10. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura | `01-arquitectura.md` |
| Routing, FIB | `04-routing.md` |
| LSA entre gateways | `05-conectividad.md` |
| SY.config.routes | `06-regiones.md` |
