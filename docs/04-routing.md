# JSON Router - 04 Routing

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core

---

## 1. Conceptos de Redes Aplicados

| Concepto | En redes IP | En este sistema |
|----------|-------------|-----------------|
| **RIB** | Todas las rutas candidatas | Datos en todas las regiones SHM |
| **FIB** | Rutas ganadoras | Cache en memoria local del router |
| **Next-hop** | Router vecino | UUID del router o isla destino |
| **Admin distance** | Preferencia por origen | LOCAL < STATIC < LSA |
| **LPM** | Matcheo más específico | `prefix_len` + `match_kind` |

---

## 2. Fuentes de Rutas

El router construye su FIB a partir de tres fuentes:

| Fuente | Región SHM | Admin Distance | Descripción |
|--------|------------|----------------|-------------|
| LOCAL | `jsr-<uuid>` (propios + peers) | 0 | Nodos conectados a routers de esta isla |
| STATIC | `jsr-config-<island>` | 1 | Rutas configuradas |
| REMOTE | `jsr-lsa-<island>` | 2 | Nodos de otras islas (via LSA) |

---

## 3. VPN = Zonas de Aislamiento

### 3.1 Concepto

Una VPN es una subred aislada dentro de la isla. Nodos en distintas VPNs no se ven entre sí.

### 3.2 Tabla VPN

La tabla VPN vive en `jsr-config-<island>`:

```
pattern           → vpn_id
─────────────────────────────
AI.soporte.*      → 10
AI.ventas.*       → 20
WF.crm.*          → 20
(default)         → 0
```

### 3.3 Asignación de VPN

Cuando un nodo conecta y envía HELLO:

```rust
fn assign_vpn(&self, node_name: &str) -> u32 {
    // Evaluar reglas en orden de priority (menor primero)
    for rule in self.vpn_table.iter()
        .filter(|r| r.flags & FLAG_ACTIVE != 0)
        .sorted_by_key(|r| r.priority)
    {
        if pattern_match(&rule.pattern, rule.match_kind, node_name) {
            return rule.vpn_id;
        }
    }
    0  // Default: VPN global
}
```

### 3.4 Filtro VPN en Routing

Al buscar destino:

```rust
fn find_route(&self, dst_name: &str, src_vpn: u32, src_is_sy: bool) -> Option<Route> {
    // Nodos SY.* ignoran filtro VPN
    if src_is_sy {
        return self.fib.lookup(dst_name);
    }
    
    // Otros nodos solo ven su VPN
    self.fib.lookup_in_vpn(dst_name, src_vpn)
}
```

### 3.5 Excepción: Nodos SY.*

Los nodos de sistema (`SY.*`) son **inmunes a VPN**:

- Pueden enviar a cualquier nodo de cualquier VPN
- Pueden recibir mensajes de cualquier VPN
- Esto permite que `SY.config.routes`, `SY.admin`, etc. operen sin restricciones

```rust
fn is_system_node(name: &str) -> bool {
    name.starts_with("SY.") || name.starts_with("RT.")
}
```

---

## 4. Construcción de la FIB

### 4.1 Algoritmo

```rust
fn build_fib(&mut self) {
    self.fib.clear();
    
    // 1. Nodos locales (mi región)
    for node in self.my_nodes.iter() {
        self.fib.add(FibEntry {
            prefix: node.name.clone(),
            match_kind: MATCH_EXACT,
            vpn_id: node.vpn_id,
            next_hop: NextHop::Local(node.uuid),
            admin_distance: 0,  // LOCAL
            source: RouteSource::Local,
        });
    }
    
    // 2. Nodos de peers (misma isla)
    for peer in self.peer_regions.iter() {
        if is_region_stale(&peer.header) {
            continue;
        }
        for node in peer.nodes.iter() {
            self.fib.add(FibEntry {
                prefix: node.name.clone(),
                match_kind: MATCH_EXACT,
                vpn_id: node.vpn_id,
                next_hop: NextHop::Router(peer.router_uuid),
                admin_distance: 0,  // LOCAL (mismo admin_distance que local)
                source: RouteSource::Peer,
            });
        }
    }
    
    // 3. Rutas estáticas (config)
    if let Some(config) = &self.config_region {
        for route in config.static_routes.iter() {
            self.fib.add(FibEntry {
                prefix: route.prefix.clone(),
                match_kind: route.match_kind,
                vpn_id: 0,  // Las estáticas aplican a todas las VPNs
                next_hop: if route.next_hop_island.is_empty() {
                    NextHop::Resolve  // Resolver localmente
                } else {
                    NextHop::Island(route.next_hop_island.clone())
                },
                admin_distance: 1,  // STATIC
                source: RouteSource::Static,
                action: route.action,
            });
        }
    }
    
    // 4. Nodos remotos (LSA)
    if let Some(lsa) = &self.lsa_region {
        for island in lsa.islands.iter() {
            for node in island.nodes.iter() {
                self.fib.add(FibEntry {
                    prefix: node.name.clone(),
                    match_kind: MATCH_EXACT,
                    vpn_id: node.vpn_id,
                    next_hop: NextHop::Island(island.island_id.clone()),
                    admin_distance: 2,  // REMOTE
                    source: RouteSource::Lsa,
                });
            }
        }
    }
    
    // 5. Ordenar para lookup eficiente
    self.fib.sort();
}
```

### 4.2 Orden de Prioridad

Cuando hay múltiples rutas al mismo destino:

1. **Longest Prefix Match (LPM)**: Más específico gana
2. **Admin Distance**: Menor gana (LOCAL < STATIC < REMOTE)
3. **Metric**: Menor gana
4. **Installed At**: Más viejo gana (estabilidad)

---

## 5. Algoritmo de Routing

### 5.1 Flujo Completo

```
Mensaje llega
    │
    ▼
┌─────────────────────────────────────────┐
│ 1. Parsear destino                      │
│    - Si dst = UUID → buscar por UUID    │
│    - Si dst = null → OPA resuelve       │
│    - Si dst = broadcast → multicast     │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 2. Extraer @isla del nombre destino     │
│    "AI.soporte.l1@staging" → staging    │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 3. ¿Isla destino == mi isla?            │
└─────────────────────────────────────────┘
    │ Sí                    │ No
    ▼                       ▼
  Routing                 Routing
  Intra-Isla             Inter-Isla
```

### 5.2 Routing Intra-Isla

```rust
fn route_intra_isla(&self, msg: &Message, dst_name: &str) -> Result<Action> {
    let src_node = self.find_node_by_uuid(&msg.routing.src)?;
    let src_vpn = src_node.vpn_id;
    let src_is_sy = is_system_node(&src_node.name);
    
    // 1. Verificar rutas estáticas (tienen prioridad)
    if let Some(static_route) = self.find_static_route(dst_name) {
        match static_route.action {
            ACTION_DROP => return Ok(Action::Drop),
            ACTION_FORWARD => {
                if !static_route.next_hop_island.is_empty() {
                    return Ok(Action::SendToIsland(static_route.next_hop_island));
                }
                // Continuar con lookup normal
            }
        }
    }
    
    // 2. Buscar en FIB (con filtro VPN)
    let route = self.find_route(dst_name, src_vpn, src_is_sy)?;
    
    match route.next_hop {
        NextHop::Local(uuid) => {
            // Nodo está en mis sockets
            Ok(Action::DeliverLocal(uuid))
        }
        NextHop::Router(router_uuid) => {
            // Nodo está en otro router de la isla
            Ok(Action::ForwardToRouter(router_uuid))
        }
        _ => Err(RouteError::Unreachable),
    }
}
```

### 5.3 Routing Inter-Isla

```rust
fn route_inter_isla(&self, msg: &Message, dst_name: &str, dst_island: &str) -> Result<Action> {
    // 1. Verificar que conozco esa isla (vía LSA)
    if !self.lsa_has_island(dst_island) {
        return Err(RouteError::UnknownIsland(dst_island.to_string()));
    }
    
    // 2. Verificar que el nodo existe en esa isla
    if !self.lsa_has_node(dst_island, dst_name) {
        return Err(RouteError::UnreachableRemote(dst_name.to_string()));
    }
    
    // 3. ¿Soy el gateway?
    if self.is_gateway {
        // Enviar directo por TCP
        Ok(Action::SendWan(dst_island.to_string()))
    } else {
        // Forward al gateway de mi isla por IRP
        Ok(Action::ForwardToGateway)
    }
}
```

---

## 6. Rutas Estáticas

### 6.1 Prioridad

Las rutas estáticas son **override explícito** y se evalúan **antes** del lookup normal.

### 6.2 Acciones

| Acción | Efecto |
|--------|--------|
| `ACTION_FORWARD` | Rutear normalmente (o a `next_hop_island` si está definido) |
| `ACTION_DROP` | Descartar mensaje (blackhole) |

### 6.3 Ejemplos

```yaml
# Bloquear acceso a nodos internos
- prefix: "AI.interno.*"
  action: DROP

# Forzar backup a otra isla
- prefix: "AI.backup.*"
  action: FORWARD
  next_hop_island: "disaster-recovery"

# Ruta normal (solo para documentar)
- prefix: "AI.soporte.*"
  action: FORWARD
```

---

## 7. Broadcast y Multicast

### 7.1 Tipos

| `dst` | `meta.target` | Comportamiento |
|-------|---------------|----------------|
| `"broadcast"` | ausente | A todos los nodos (respetando VPN) |
| `"broadcast"` | patrón | A nodos que matchean el patrón (respetando VPN) |

### 7.2 Broadcast respeta VPN

```rust
fn broadcast(&self, msg: &Message) -> Vec<Action> {
    let src_node = self.find_node_by_uuid(&msg.routing.src)?;
    let src_vpn = src_node.vpn_id;
    let src_is_sy = is_system_node(&src_node.name);
    let target_pattern = msg.meta.target.as_deref();
    
    let mut actions = Vec::new();
    
    for node in self.all_nodes() {
        // Filtro VPN (excepto para SY.*)
        if !src_is_sy && node.vpn_id != src_vpn {
            continue;
        }
        
        // Filtro por pattern (si existe)
        if let Some(pattern) = target_pattern {
            if !pattern_match(pattern, &node.name) {
                continue;
            }
        }
        
        // No enviar al origen
        if node.uuid == msg.routing.src {
            continue;
        }
        
        actions.push(Action::DeliverTo(node.uuid));
    }
    
    actions
}
```

### 7.3 TTL en Broadcast

| TTL | Alcance |
|-----|---------|
| 1 | Solo mis nodos locales |
| 2 | Toda la isla (mis nodos + peers via IRP) |
| 3+ | Cruza WAN a otras islas |

### 7.4 Prevención de Loops

```rust
struct BroadcastCache {
    seen: HashMap<Uuid, Instant>,
}

impl BroadcastCache {
    fn check_and_add(&mut self, trace_id: &Uuid) -> bool {
        self.cleanup_expired();
        if self.seen.contains_key(trace_id) {
            false  // Ya procesado, DROP
        } else {
            self.seen.insert(*trace_id, Instant::now());
            true   // Nuevo, procesar
        }
    }
}
```

---

## 8. OPA

### 8.1 Cuándo se usa

Cuando `routing.dst = null`, el router consulta OPA para resolver el destino.

### 8.2 Input

```json
{
  "meta": { ... },
  "routing": {
    "src": "uuid-origen"
  }
}
```

### 8.3 Output

```json
{
  "target": "AI.soporte.l1@produccion"
}
```

### 8.4 Ejemplo de Policy (Rego)

```rego
package router

default target = null

target = "AI.soporte.l2@produccion" {
    input.meta.context.cliente_tier == "vip"
}

target = "AI.soporte.l1@produccion" {
    input.meta.context.cliente_tier == "standard"
}
```

---

## 9. Pattern Matching

### 9.1 Tipos de Match

| Tipo | Ejemplo | Matchea |
|------|---------|---------|
| EXACT | `AI.soporte.l1` | Solo `AI.soporte.l1` |
| PREFIX | `AI.soporte.*` | `AI.soporte.l1`, `AI.soporte.l2`, etc. |
| GLOB | `AI.*.l1` | `AI.soporte.l1`, `AI.ventas.l1`, etc. |

### 9.2 Implementación

```rust
fn pattern_match(pattern: &str, name: &str, match_kind: u8) -> bool {
    match match_kind {
        MATCH_EXACT => pattern == name,
        MATCH_PREFIX => {
            let prefix = pattern.trim_end_matches(".*");
            name.starts_with(prefix)
        }
        MATCH_GLOB => {
            // Convertir * a regex .*
            let regex = pattern.replace("*", ".*");
            Regex::new(&format!("^{}$", regex))
                .map(|r| r.is_match(name))
                .unwrap_or(false)
        }
        _ => false,
    }
}
```

---

## 10. Casos Especiales

### 10.1 Destino con @* (wildcard de isla)

Si el destino es `AI.soporte.l1@*`:

```rust
fn route_wildcard_island(&self, dst_name_base: &str) -> Result<Action> {
    // Buscar en isla local primero
    let local_name = format!("{}@{}", dst_name_base, self.island_id);
    if let Ok(action) = self.route_intra_isla(&local_name) {
        return Ok(action);
    }
    
    // Buscar en islas remotas (LSA)
    for island in self.lsa_islands() {
        let remote_name = format!("{}@{}", dst_name_base, island.island_id);
        if self.lsa_has_node(&island.island_id, &remote_name) {
            return Ok(Action::SendToIsland(island.island_id.clone()));
        }
    }
    
    Err(RouteError::Unreachable)
}
```

### 10.2 Mensaje UNREACHABLE

Cuando no se encuentra destino:

```json
{
  "routing": {
    "src": "<uuid-router>",
    "dst": "<uuid-nodo-origen>",
    "ttl": 16,
    "trace_id": "<trace-id-original>"
  },
  "meta": {
    "type": "system",
    "msg": "UNREACHABLE"
  },
  "payload": {
    "original_dst": "AI.soporte.l1@staging",
    "reason": "NODE_NOT_FOUND"
  }
}
```

---

## 11. Referencias

| Tema | Documento |
|------|-----------|
| Shared memory | `03-shm.md` |
| Conectividad, LSA | `05-conectividad.md` |
| Config de rutas | `06-regiones.md` |
