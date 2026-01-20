# JSON Router - 06 Región de Configuración (Config SHM)

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core, SY.config.routes

---

## 1. Propósito

La región de configuración (`/jsr-config-<island>`) es una área de shared memory **separada** de las regiones de routers. Contiene:

- Rutas estáticas (FORWARD/DROP) con alcance por VPN
- Definición de VPN zones (VRF)
- Membership rules (asignación de nodos a VPN)
- Leak rules (permisos entre VPNs)

**Un único proceso escribe:** `SY.config.routes@<isla>`

**Todos los routers leen** esta región y actualizan su FIB.

---

## 2. Arquitectura

```
┌─────────────────────────────────────────────────────────────┐
│                      SY.config.routes                       │
│                                                             │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │ API Handler │  │ SHM Writer  │  │ Persistence │         │
│  │             │  │             │  │             │         │
│  │ - add_*     │  │ - seqlock   │  │ - sy-config │         │
│  │ - del_*     │  │ - heartbeat │  │   -routes   │         │
│  │ - list_*    │  │ - version++ │  │   .yaml     │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         │                │                │                 │
│         └────────────────┼────────────────┘                 │
│                          │                                  │
└──────────────────────────┼──────────────────────────────────┘
                           │
                           ▼
                 /dev/shm/jsr-config-<island_id>
                 ┌─────────────────────────────┐
                 │ ConfigHeader                │
                 │ StaticRouteEntry[]          │
                 │ VpnZoneEntry[]              │
                 │ VpnMemberRuleEntry[]        │
                 │ VpnLeakRuleEntry[]          │
                 └─────────────────────────────┘
```

**Separación de responsabilidades:**
- **Router:** data plane + routing core. Lee config desde SHM, no la propaga.
- **SY.config.routes:** escribe config LOCAL en SHM y persiste YAML.

---

## 3. Layout de la Región (v1.13)

```
┌─────────────────────────────────────────────────────────────┐
│ ConfigHeader (224 bytes)                                    │
│ - magic, version, owner info, seqlock, timestamps           │
│ - contadores actualizados                                   │
├─────────────────────────────────────────────────────────────┤
│ StaticRouteEntry[MAX_STATIC_ROUTES] (256 entries)          │
│ - 256 × 368 bytes = ~92 KB                                  │
├─────────────────────────────────────────────────────────────┤
│ VpnZoneEntry[MAX_VPN_ZONES] (64 entries)                   │
│ - 64 × 128 bytes = ~8 KB                                    │
├─────────────────────────────────────────────────────────────┤
│ VpnMemberRuleEntry[MAX_VPN_MEMBER_RULES] (256 entries)     │
│ - 256 × 304 bytes = ~76 KB                                  │
├─────────────────────────────────────────────────────────────┤
│ VpnLeakRuleEntry[MAX_VPN_LEAK_RULES] (128 entries)         │
│ - 128 × 304 bytes = ~38 KB                                  │
└─────────────────────────────────────────────────────────────┘
Total aproximado: ~215 KB
```

---

## 4. Constantes (v1.13)

```rust
// Región de configuración
pub const CONFIG_MAGIC: u32 = 0x4A534343;  // "JSCC"
pub const CONFIG_VERSION: u32 = 2;          // v1.13
pub const CONFIG_SHM_PREFIX: &str = "/jsr-config-";

// Capacidades
pub const MAX_STATIC_ROUTES: u32 = 256;
pub const MAX_VPN_ZONES: u32 = 64;
pub const MAX_VPN_MEMBER_RULES: u32 = 256;
pub const MAX_VPN_LEAK_RULES: u32 = 128;

// Acciones de ruta (v1.13: ACTION_VPN eliminado)
pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;

// Timers
pub const CONFIG_HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const CONFIG_HEARTBEAT_STALE_MS: u64 = 30_000;
```

---

## 5. Estructuras de Datos (v1.13)

### 5.1 ConfigHeader

```rust
#[repr(C)]
pub struct ConfigHeader {
    // === IDENTIFICACIÓN (80 bytes) ===
    pub magic: u32,                   // CONFIG_MAGIC = 0x4A534343
    pub version: u32,                 // CONFIG_VERSION = 2
    pub island_id: [u8; 64],
    pub island_id_len: u16,
    pub _pad1: [u8; 6],
    
    // === OWNERSHIP (40 bytes) ===
    pub owner_pid: u64,
    pub owner_start_time: u64,
    pub owner_uuid: [u8; 16],         // UUID de SY.config.routes
    pub heartbeat: u64,
    
    // === SEQLOCK (8 bytes) ===
    pub seq: u64,                     // AtomicU64
    
    // === CONTADORES (v1.13: actualizados) (24 bytes) ===
    pub static_route_count: u32,
    pub vpn_zone_count: u32,          // Nuevo
    pub vpn_member_rule_count: u32,   // Nuevo
    pub vpn_leak_rule_count: u32,     // Nuevo
    pub config_version: u64,          // Incrementa con cada cambio
    
    // === TIMESTAMPS (16 bytes) ===
    pub created_at: u64,
    pub updated_at: u64,
    
    // === RESERVED (56 bytes para llegar a 224) ===
    pub _reserved: [u8; 56],
}
// Total: 224 bytes
```

### 5.2 StaticRouteEntry (v1.13)

```rust
#[repr(C)]
pub struct StaticRouteEntry {
    // === MATCHING (260 bytes) ===
    pub prefix: [u8; 256],            // Pattern: "AI.soporte.*"
    pub prefix_len: u16,
    pub match_kind: u8,               // MATCH_EXACT, MATCH_PREFIX, MATCH_GLOB
    pub _pad1: u8,

    // === FORWARDING (76 bytes) ===
    pub next_hop_island: [u8; 64],    // "" = local; si no vacío = inter-isla via gateway
    pub next_hop_island_len: u16,
    pub action: u8,                   // ACTION_FORWARD o ACTION_DROP
    pub _pad2: u8,
    pub metric: u32,

    // === VPN SCOPE (v1.13) (8 bytes) ===
    pub vpn_scope: u32,               // 0 = instala en todas las VPNs; !=0 = solo en esa VPN
    pub _pad_vpn: u32,

    // === ESTADO (8 bytes) ===
    pub flags: u16,
    pub priority: u16,
    pub _pad3: [u8; 4],

    // === METADATA (16 bytes) ===
    pub installed_at: u64,
    pub _reserved: [u8; 8],
}
// Total: 368 bytes
```

**Acciones válidas (v1.13):**

| action | Valor | Descripción |
|--------|-------|-------------|
| ACTION_FORWARD | 0 | Rutear normalmente |
| ACTION_DROP | 1 | Descartar (blackhole) |

**Nota v1.13:** `ACTION_VPN` se elimina. El forwarding inter-isla se resuelve por `next_hop_island` + gateway único.

### 5.3 VpnZoneEntry (v1.13 - Nueva)

```rust
#[repr(C)]
pub struct VpnZoneEntry {
    pub vpn_id: u32,                  // ID único de la zona
    pub flags: u16,
    pub _pad0: u16,

    pub vpn_name: [u8; 64],           // Nombre descriptivo
    pub vpn_name_len: u16,
    pub _pad1: [u8; 6],

    pub parent_vpn_id: u32,           // 0 = sin padre (top-level)
    pub _pad2: u32,

    pub created_at: u64,
    pub _reserved: [u8; 40],
}
// Total: 128 bytes
```

**Semántica:**
- Define una zona VRF dentro de la isla
- `vpn_id = 0` es implícito (global), no necesita entrada
- `parent_vpn_id` permite jerarquía opcional

### 5.4 VpnMemberRuleEntry (v1.13 - Nueva)

```rust
#[repr(C)]
pub struct VpnMemberRuleEntry {
    pub pattern: [u8; 256],           // "AI.soporte.*"
    pub pattern_len: u16,
    pub match_kind: u8,
    pub _pad0: u8,

    pub vpn_id: u32,                  // A qué VPN asigna
    pub flags: u16,
    pub _pad1: u16,

    pub priority: u16,                // menor = evalúa primero
    pub _pad2: [u8; 6],

    pub installed_at: u64,
    pub _reserved: [u8; 16],
}
// Total: 304 bytes
```

**Semántica:**
- El router asigna `vpn_id` al nodo al conectar, según estas reglas
- Se evalúan en orden de `priority` (menor primero)
- Si ninguna regla matchea, `vpn_id = 0` (global)

### 5.5 VpnLeakRuleEntry (v1.13 - Nueva)

```rust
#[repr(C)]
pub struct VpnLeakRuleEntry {
    pub from_vpn: u32,
    pub to_vpn: u32,

    pub pattern: [u8; 256],           // Targets permitidos a ver/rutear
    pub pattern_len: u16,
    pub match_kind: u8,
    pub _pad0: u8,

    pub flags: u16,
    pub _pad1: [u8; 2],

    pub priority: u16,
    pub _pad2: [u8; 6],

    pub installed_at: u64,
    pub _reserved: [u8; 16],
}
// Total: 304 bytes
```

**Semántica:**
- Permite route leaking desde `from_vpn` hacia `to_vpn`
- Solo para destinos que matchean `pattern`
- Sin leak rules, el aislamiento entre VPNs es total

---

## 6. Flujo del Router al Leer Config

```rust
fn update_fib_from_config(&mut self) {
    // 1. Abrir región de config si no está mapeada
    let config_shm_name = format!("{}{}", CONFIG_SHM_PREFIX, self.island_id);
    if self.config_region.is_none() {
        self.config_region = map_config_region(&config_shm_name);
    }
    
    // 2. Leer con seqlock
    let config = match &self.config_region {
        Some(region) => seqlock_read(region),
        None => return,
    };
    
    // 3. Verificar heartbeat
    if is_config_stale(&config.header) {
        log::warn!("Config region stale, SY.config.routes may be down");
    }
    
    // 4. Si config_version cambió, actualizar
    if config.header.config_version != self.last_config_version {
        // Cargar VPN zones
        self.load_vpn_zones(&config.vpn_zones);
        
        // Cargar membership rules
        self.load_membership_rules(&config.vpn_member_rules);
        
        // Cargar leak rules
        self.load_leak_rules(&config.vpn_leak_rules);
        
        // Instalar rutas estáticas (por vpn_scope)
        self.install_static_routes(&config.static_routes);
        
        self.last_config_version = config.header.config_version;
    }
}
```

**El router NO propaga configuración.** Solo lee y aplica.

---

## 7. Asignación de VPN a Nodos

Cuando un nodo conecta y envía HELLO:

```rust
fn assign_vpn_to_node(&self, node_name: &str) -> u32 {
    // Evaluar membership rules en orden de priority
    for rule in self.vpn_member_rules.iter()
        .filter(|r| r.flags & FLAG_ACTIVE != 0)
        .sorted_by_key(|r| r.priority)
    {
        if pattern_match(&rule.pattern, rule.match_kind, node_name) {
            return rule.vpn_id;
        }
    }
    
    // Default: VPN global
    0
}
```

El `vpn_id` asignado se:
1. Guarda en `NodeEntry.vpn_id`
2. Envía al nodo en el ANNOUNCE
3. Se usa para normalizar `routing.vpn` de mensajes del nodo

---

## 8. API de SY.config.routes (v1.13)

### 8.1 Rutas Estáticas

**Listar rutas:**
```json
{
  "meta": { "type": "admin", "action": "list_routes" },
  "payload": {}
}
```

**Agregar ruta:**
```json
{
  "meta": { "type": "admin", "action": "add_route" },
  "payload": {
    "prefix": "AI.ventas.*",
    "match_kind": "PREFIX",
    "action": "FORWARD",
    "next_hop_island": "",
    "metric": 10,
    "priority": 100,
    "vpn_scope": 0
  }
}
```

**Eliminar ruta:**
```json
{
  "meta": { "type": "admin", "action": "delete_route" },
  "payload": {
    "prefix": "AI.ventas.*"
  }
}
```

### 8.2 VPN Zones

**Listar VPNs:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns" },
  "payload": {}
}
```

**Respuesta:**
```json
{
  "payload": {
    "vpns": [
      { "vpn_id": 0, "vpn_name": "global" },
      { "vpn_id": 10, "vpn_name": "soporte" },
      { "vpn_id": 20, "vpn_name": "ventas" }
    ]
  }
}
```

**Agregar VPN:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn" },
  "payload": {
    "vpn_id": 10,
    "vpn_name": "soporte",
    "parent_vpn_id": 0
  }
}
```

### 8.3 Membership Rules

**Listar membership rules:**
```json
{
  "meta": { "type": "admin", "action": "list_vpn_member_rules" },
  "payload": {}
}
```

**Agregar membership rule:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn_member_rule" },
  "payload": {
    "pattern": "AI.soporte.*",
    "match_kind": "PREFIX",
    "vpn_id": 10,
    "priority": 100
  }
}
```

### 8.4 Leak Rules

**Listar leak rules:**
```json
{
  "meta": { "type": "admin", "action": "list_vpn_leak_rules" },
  "payload": {}
}
```

**Agregar leak rule:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn_leak_rule" },
  "payload": {
    "from_vpn": 10,
    "to_vpn": 0,
    "pattern": "WF.route.handoff",
    "match_kind": "EXACT",
    "priority": 100
  }
}
```

---

## 9. Persistencia: sy-config-routes.yaml (v2)

```yaml
version: 2
updated_at: "2025-01-20T00:00:00Z"

routes:
  - prefix: "AI.soporte.*"
    match_kind: PREFIX
    action: FORWARD
    next_hop_island: ""
    metric: 10
    priority: 100
    vpn_scope: 10

  - prefix: "AI.ventas.interno"
    match_kind: EXACT
    action: DROP
    priority: 50
    vpn_scope: 0

vpns:
  - vpn_id: 0
    vpn_name: global
    parent_vpn_id: 0

  - vpn_id: 10
    vpn_name: soporte
    parent_vpn_id: 0

vpn_member_rules:
  - pattern: "AI.soporte.*"
    match_kind: PREFIX
    vpn_id: 10
    priority: 100

vpn_leak_rules:
  - from_vpn: 10
    to_vpn: 0
    pattern: "WF.route.handoff"
    match_kind: EXACT
    priority: 100
```

**Notas v1.13:**
- No hay rutas "globales de otras islas"
- No hay endpoints de VPN (modelo viejo eliminado)
- VPN es zona intra-isla, no túnel inter-isla

---

## 10. Códigos de Error

| Código | Descripción |
|--------|-------------|
| `DUPLICATE_PREFIX` | Ya existe ruta con ese prefix |
| `PREFIX_NOT_FOUND` | Ruta no existe |
| `INVALID_PREFIX` | Formato de prefix inválido |
| `INVALID_ACTION` | Acción desconocida |
| `INVALID_MATCH_KIND` | Match kind desconocido |
| `VPN_NOT_FOUND` | VPN referenciado no existe |
| `DUPLICATE_VPN_NAME` | Ya existe VPN con ese nombre |
| `MAX_ROUTES_EXCEEDED` | Límite de rutas alcanzado |
| `MAX_VPNS_EXCEEDED` | Límite de VPNs alcanzado |
| `PERSISTENCE_ERROR` | Error guardando en disco |
| `DUPLICATE_RULE` | Regla duplicada |
| `RULE_NOT_FOUND` | Regla no existe |
| `INVALID_VPN_SCOPE` | VPN scope inválido |

---

## 11. Nota v1.13: VPN ya no es túnel WAN

En v1.12, "VPN" se usaba para forzar caminos WAN con `ACTION_VPN` y endpoints.

**En v1.13:**
- VPN significa **zona/VRF dentro de la isla**
- `ACTION_VPN` se elimina
- El forwarding inter-isla se resuelve por:
  - `next_hop_island` en rutas estáticas
  - Gateway único por isla

La segmentación lógica se resuelve por `routing.vpn` + tablas separadas por VPN.

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Shared memory routers | `03-shm.md` |
| Routing, VPN semantics | `04-routing.md` |
| SY.config.routes completo | `SY_nodes_spec.md` |
