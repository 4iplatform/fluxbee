# JSON Router - 03 Shared Memory

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core

---

## 1. Modelo: Una Región por Router

La shared memory es el mecanismo de coordinación entre routers. Cada router tiene su propia región que solo él escribe, y todos los demás leen.

```
┌─────────────────────────────────────────────────────────────────────────┐
│                           Isla "produccion"                              │
│                                                                          │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────────┐             │
│  │  Router A    │     │  Router B    │     │  Router C    │             │
│  │              │     │              │     │              │             │
│  │ ┌──────────┐ │     │ ┌──────────┐ │     │ ┌──────────┐ │             │
│  │ │ SHM-A    │ │     │ │ SHM-A    │ │     │ │ SHM-A    │ │             │
│  │ │(WRITER)  │◄├────►├─┤(reader)  │◄├────►├─┤(reader)  │ │             │
│  │ └──────────┘ │     │ └──────────┘ │     │ └──────────┘ │             │
│  │ ┌──────────┐ │     │ ┌──────────┐ │     │ ┌──────────┐ │             │
│  │ │ SHM-B    │ │     │ │ SHM-B    │ │     │ │ SHM-B    │ │             │
│  │ │(reader)  │◄├────►├─┤(WRITER)  │◄├────►├─┤(reader)  │ │             │
│  │ └──────────┘ │     │ └──────────┘ │     │ └──────────┘ │             │
│  │ ┌──────────┐ │     │ ┌──────────┐ │     │ ┌──────────┐ │             │
│  │ │ SHM-C    │ │     │ │ SHM-C    │ │     │ │ SHM-C    │ │             │
│  │ │(reader)  │◄├────►├─┤(reader)  │◄├────►├─┤(WRITER)  │ │             │
│  │ └──────────┘ │     │ └──────────┘ │     │ └──────────┘ │             │
│  └──────────────┘     └──────────────┘     └──────────────┘             │
│                                                                          │
│  Cada router:                                                            │
│  - ESCRIBE solo en SU región (es el único writer)                       │
│  - LEE las regiones de TODOS los otros routers                          │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

**Principio clave:** Cada región tiene exactamente un writer (su dueño) y múltiples readers (todos los demás). Esto permite usar seqlock sin conflictos.

---

## 2. Naming de Regiones

El nombre de la región incluye el UUID del router dueño:

```
/jsr-<router_uuid>
```

Ejemplo:
```
/jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890
```

**En macOS:** El nombre se trunca a 31 caracteres (usar primeros 16 hex del UUID).

---

## 3. Contenido de Cada Región

Cada router escribe en SU región únicamente:

| Dato | Descripción |
|------|-------------|
| **Sus nodos** | Nodos conectados directamente a este router |
| **Sus rutas CONNECTED** | Rutas automáticas por nodos locales |
| **Su estado** | owner_pid, heartbeat, timestamps |

**NO escribe:**
- Rutas de otros routers (esas las lee de sus regiones)
- Nodos de otros routers (esas las lee de sus regiones)

---

## 4. Layout de la Región

```
┌─────────────────────────────────────────────────────────────┐
│ ShmHeader (192 bytes)                                       │
│ - magic, version, owner info, seqlock, timestamps           │
├─────────────────────────────────────────────────────────────┤
│ NodeEntry[MAX_NODES] (1024 entries × 312 bytes)             │
│ - Nodos conectados a ESTE router                            │
│ - Total: ~312 KB                                            │
├─────────────────────────────────────────────────────────────┤
│ RouteEntry[MAX_ROUTES] (256 entries × 304 bytes)            │
│ - Rutas CONNECTED de ESTE router                            │
│ - Total: ~76 KB                                             │
└─────────────────────────────────────────────────────────────┘
Total aproximado: ~390 KB por router
```

**Nota:** No hay `RouterEntry[]` en cada región. La lista de routers se construye dinámicamente a partir de los HELLOs recibidos.

---

## 5. Sincronización: Seqlock

Cada región tiene exactamente un writer (su dueño), lo que permite usar seqlock sin conflictos:

- Header contiene `seq: AtomicU64`
- Sin locks, sin deadlocks, sin contención
- Usa memory orderings para coherencia entre procesos

### 5.1 Protocolo del Writer (solo el router dueño)

```rust
// Comenzar escritura (seq queda impar = "escribiendo")
header.seq.fetch_add(1, Ordering::Relaxed);

// Escribir datos en la región
// ...

// Finalizar escritura (seq queda par = "snapshot consistente")
atomic::fence(Ordering::Release);
header.seq.fetch_add(1, Ordering::Relaxed);
```

### 5.2 Protocolo del Reader (todos los demás)

```rust
loop {
    // Leer seq
    let s1 = header.seq.load(Ordering::Acquire);
    
    // Si impar, el writer está escribiendo, reintentar
    if s1 & 1 != 0 {
        std::hint::spin_loop();
        continue;
    }
    
    // Barrera antes de leer datos
    atomic::fence(Ordering::Acquire);
    
    // Copiar datos que necesita
    let data = /* copiar snapshot */;
    
    // Barrera después de leer datos
    atomic::fence(Ordering::Acquire);
    
    // Verificar que seq no cambió durante la lectura
    let s2 = header.seq.load(Ordering::Acquire);
    if s1 == s2 {
        break; // Snapshot consistente
    }
    // Si cambió, descartar y reintentar
}
```

**Importante:** `seq` debe estar alineado a 8 bytes para garantizar atomicidad.

---

## 6. Constantes

```rust
// Identificación
pub const SHM_MAGIC: u32 = 0x4A535352;  // "JSSR" en ASCII
pub const SHM_VERSION: u32 = 1;
pub const SHM_NAME_PREFIX: &str = "/jsr-";

// Capacidades por router
pub const MAX_NODES: u32 = 1024;    // Nodos por router
pub const MAX_ROUTES: u32 = 256;    // Rutas por router
pub const MAX_PEERS: usize = 16;    // Máximo de peers mapeados

// Tamaños de campos
pub const NAME_MAX_LEN: usize = 256;
pub const ISLAND_ID_MAX_LEN: usize = 64;
pub const SHM_NAME_MAX_LEN: usize = 64;

// Flags de estado
pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;  // Marcado para reusar slot

// Match kinds (para rutas)
pub const MATCH_EXACT: u8 = 0;   // "AI.soporte.l1" matchea solo eso
pub const MATCH_PREFIX: u8 = 1;  // "AI.soporte" matchea "AI.soporte.*"
pub const MATCH_GLOB: u8 = 2;    // Patterns con wildcards

// Tipos de ruta (route_type)
pub const ROUTE_CONNECTED: u8 = 0;  // Nodo conectado directamente
pub const ROUTE_STATIC: u8 = 1;     // Configurada (viene de config shm)
pub const ROUTE_LSA: u8 = 2;        // Aprendida de otro router

// Admin distances (menor = preferido)
pub const AD_CONNECTED: u16 = 0;    // Siempre preferir nodos locales
pub const AD_STATIC: u16 = 1;       // Rutas manuales
pub const AD_LSA: u16 = 10;         // Rutas aprendidas

// Identificadores de link
pub const LINK_LOCAL: u32 = 0;      // Nodo conectado localmente
// LINK 1..N = uplinks a otros routers

// Timers
pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;
```

---

## 7. Estructuras de Datos

Todas las estructuras usan `#[repr(C)]` para layout determinístico en memoria.

### 7.1 ShmHeader

```rust
use std::sync::atomic::AtomicU64;

#[repr(C)]
pub struct ShmHeader {
    // === IDENTIFICACIÓN (8 bytes) ===
    pub magic: u32,                 // SHM_MAGIC para validar
    pub version: u32,               // SHM_VERSION para compatibilidad
    
    // === OWNER (40 bytes) ===
    pub router_uuid: [u8; 16],      // UUID del router dueño
    pub owner_pid: u32,             // PID del proceso dueño
    pub _pad0: u32,                 // Padding para alineación
    pub owner_start_time: u64,      // Epoch ms cuando arrancó (anti PID-reuse)
    pub generation: u64,            // Incrementa en cada recreación
    
    // === SEQLOCK (8 bytes, alineado) ===
    pub seq: AtomicU64,             // Impar=escribiendo, par=consistente
    
    // === CONTADORES (8 bytes) ===
    pub node_count: u32,            // Nodos activos
    pub route_count: u32,           // Rutas activas
    
    // === CAPACIDADES (8 bytes) ===
    pub node_max: u32,              // MAX_NODES
    pub route_max: u32,             // MAX_ROUTES
    
    // === TIMESTAMPS (24 bytes) ===
    pub created_at: u64,            // Epoch ms creación
    pub updated_at: u64,            // Epoch ms última modificación
    pub heartbeat: u64,             // Epoch ms último heartbeat
    
    // === ISLA (66 bytes) ===
    pub island_id: [u8; 64],        // "produccion", "staging", etc.
    pub island_id_len: u16,
    
    // === OPA POLICY (12 bytes) ===
    pub opa_policy_version: u64,    // Versión del policy cargado
    pub opa_load_status: u8,        // 0=OK, 1=ERROR, 2=LOADING
    pub _pad1: [u8; 3],
    
    // === RESERVADO (18 bytes para llegar a 192) ===
    pub _reserved: [u8; 18],
}
// Total: 192 bytes
```

### 7.2 NodeEntry

Registro de un nodo conectado a este router.

```rust
#[repr(C)]
pub struct NodeEntry {
    // === IDENTIFICACIÓN (16 bytes) ===
    pub uuid: [u8; 16],          // UUID del nodo (capa 1)
    
    // === NOMBRE CAPA 2 (258 bytes) ===
    pub name: [u8; 256],         // "AI.soporte.l1@produccion" (UTF-8)
    pub name_len: u16,           // Longitud en bytes
    
    // === VPN (v1.13) (4 bytes) ===
    pub vpn_id: u32,             // VPN asignado al nodo
    
    // === ESTADO (10 bytes) ===
    pub flags: u16,              // FLAG_ACTIVE, FLAG_DELETED
    pub connected_at: u64,       // Epoch ms cuando conectó
    
    // === RESERVADO (24 bytes) ===
    pub _reserved: [u8; 24],
}
// Total: 312 bytes
// Con 1024 entries: ~312 KB
```

**Nota v1.13:** Se agregó `vpn_id` para registrar el VPN asignado al nodo.

### 7.3 RouteEntry

Entrada en la tabla de ruteo. Solo rutas CONNECTED de este router.

```rust
#[repr(C)]
pub struct RouteEntry {
    // === MATCHING (260 bytes) ===
    pub prefix: [u8; 256],       // Pattern: "AI.soporte.*" (UTF-8)
    pub prefix_len: u16,         // Longitud en bytes
    pub match_kind: u8,          // MATCH_EXACT, MATCH_PREFIX, MATCH_GLOB
    pub route_type: u8,          // ROUTE_CONNECTED, ROUTE_LSA
    
    // === FORWARDING (24 bytes) ===
    pub next_hop_router: [u8; 16], // UUID del router next-hop
    pub out_link: u32,           // 0=local, 1..N=uplink id
    pub metric: u32,             // Costo de esta ruta
    
    // === SELECCIÓN (4 bytes) ===
    pub admin_distance: u16,     // AD_CONNECTED, AD_LSA
    pub flags: u16,              // FLAG_ACTIVE, FLAG_DELETED
    
    // === METADATA (16 bytes) ===
    pub installed_at: u64,       // Epoch ms cuando se instaló
    pub _reserved: [u8; 8],
}
// Total: 304 bytes
// Con 256 entries: ~76 KB
```

---

## 8. Inicialización y Recuperación

### 8.1 ¿Quién crea la región?

Cada router crea su propia región al arrancar.

### 8.2 Detección de región stale (crash anterior)

```
1. Intentar abrir región existente con mi UUID
2. Si existe:
   a. Leer owner_pid y owner_start_time
   b. Verificar si el proceso está vivo:
      - kill(owner_pid, 0) == OK
      - Y owner_start_time coincide con start_time real
   c. Si el proceso NO existe o start_time no coincide → región stale
   d. Si heartbeat > HEARTBEAT_STALE_MS → región stale
   e. Si región stale:
      - shm_unlink(shm_name)
      - Crear región nueva con generation++
   f. Si proceso vivo y heartbeat reciente → ERROR: otro proceso usa mi UUID
3. Si no existe:
   a. Crear región nueva
   b. Inicializar header con magic, version, mi pid, start_time, generation=1
```

### 8.3 Obtener start_time del proceso

```rust
fn get_process_start_time(pid: u32) -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // Leer /proc/<pid>/stat, campo 22 (starttime en ticks)
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
        let fields: Vec<&str> = stat.split_whitespace().collect();
        let start_ticks: u64 = fields.get(21)?.parse().ok()?;
        Some(start_ticks)
    }
    
    #[cfg(not(target_os = "linux"))]
    {
        None  // Fallback a heartbeat
    }
}

fn is_owner_alive(header: &ShmHeader) -> bool {
    if unsafe { libc::kill(header.owner_pid as i32, 0) } != 0 {
        return false;
    }
    
    match get_process_start_time(header.owner_pid) {
        Some(actual_start) => header.owner_start_time == actual_start,
        None => true,  // OS no soporta, confiar en heartbeat
    }
}
```

### 8.4 Heartbeat

El router dueño actualiza `heartbeat` cada 5 segundos:

```rust
loop {
    // ... procesar mensajes ...
    
    if tiempo_desde_ultimo_heartbeat > HEARTBEAT_INTERVAL_MS {
        seqlock_write_begin();
        header.heartbeat = now_epoch_ms();
        seqlock_write_end();
    }
}
```

### 8.5 Detección de peer muerto

```
1. Verificar heartbeat del peer
2. Si heartbeat > HEARTBEAT_STALE_MS (30s):
   a. Considerar peer como muerto
   b. Dejar de usar sus rutas en la FIB
   c. munmap() la región
   d. Intentar re-abrir periódicamente
3. Si generation cambió:
   a. munmap() y re-abrir para obtener nueva generación
```

---

## 9. Operaciones de Escritura

Solo el router dueño escribe en su región.

### 9.1 Registrar nodo (cuando conecta)

```
1. Buscar slot libre en NodeEntry[] (flags == 0 o FLAG_DELETED)
2. seqlock_begin_write()
3. Escribir uuid, name, name_len, vpn_id, connected_at
4. flags = FLAG_ACTIVE
5. node_count++
6. updated_at = now()
7. seqlock_end_write()
8. Crear RouteEntry CONNECTED para el nodo
```

### 9.2 Desregistrar nodo (cuando desconecta)

```
1. Buscar NodeEntry por uuid
2. seqlock_begin_write()
3. flags = FLAG_DELETED
4. node_count--
5. Marcar RouteEntry asociada como FLAG_DELETED
6. route_count--
7. updated_at = now()
8. seqlock_end_write()
```

---

## 10. Descubrimiento de Regiones via HELLO

Los routers descubren las regiones de sus peers mediante el mensaje HELLO:

```json
{
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "router_id": "uuid-router-a",
    "island_id": "produccion",
    "shm_name": "/jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890"
  }
}
```

**Flujo:**

```
1. Router A arranca, crea su región /jsr-<uuid-A>
2. Router A envía HELLO
3. Router B recibe HELLO de A
4. Router B extrae shm_name del HELLO
5. Router B mapea /jsr-<uuid-A> en modo read-only
6. Router B ahora puede leer los nodos de A
7. Router B responde con su propio HELLO
8. Router A mapea /jsr-<uuid-B>
```

---

## 11. Encoding de Strings

**UTF-8** para `name`, `prefix`, e `island_id`.

- Los nombres pueden contener Unicode: "AI.soporte.español@produccion"
- `name_len`, `prefix_len` son longitud en **bytes**, no caracteres
- Validación de caracteres permitidos aplica a code points:

```rust
fn validate_name(name: &str) -> bool {
    name.chars().all(|c| {
        c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '@'
    })
}
```

---

## 12. Qué vive en Shared Memory

| Dato | Quién escribe | Descripción |
|------|---------------|-------------|
| Header | Router dueño | Identificación, seqlock, heartbeat |
| Nodos | Router dueño | Nodos conectados a este router |
| Rutas CONNECTED | Router dueño | Una por cada nodo local |

## 13. Qué NO vive en Shared Memory

| Dato | Por qué no | Dónde vive |
|------|------------|------------|
| Lista de routers peers | Dinámica, via HELLOs | Memoria local |
| FIB compilada | Derivada de todas las regiones | Memoria local |
| Estado del socket | El socket lo indica | Local |
| Rutas de otros routers | Cada uno las tiene en su región | Se leen, no se copian |
| Rutas STATIC | En región de config separada | `/jsr-config-<island>` |

---

## 14. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura, islas | `01-arquitectura.md` |
| Routing, FIB | `04-routing.md` |
| Región de config (rutas estáticas, VPN) | `06-config-shm.md` |
