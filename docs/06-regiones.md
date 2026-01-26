# JSON Router - 06 Regiones Config y LSA

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core, SY.config.routes

---

## 1. Visión General

Este documento detalla las dos regiones de memoria compartida que NO son de routers:

| Región | Writer | Propósito |
|--------|--------|-----------|
| `jsr-config-<island>` | SY.config.routes | Rutas estáticas, tabla VPN |
| `jsr-lsa-<island>` | Gateway | Topología de islas remotas |

---

## 2. Región Config: jsr-config-<island>

### 2.1 Propósito

Almacena la configuración centralizada de la isla:

- **Rutas estáticas**: Override explícito de routing
- **Tabla VPN**: Asignación de nodos a zonas de aislamiento

### 2.2 Writer Único

Solo `SY.config.routes@<island>` escribe en esta región. Los routers solo leen.

### 2.3 API de SY.config.routes

#### Rutas Estáticas

**Listar:**
```json
{
  "meta": { "type": "admin", "action": "list_routes" },
  "payload": {}
}
```

**Agregar:**
```json
{
  "meta": { "type": "admin", "action": "add_route" },
  "payload": {
    "prefix": "AI.ventas.*",
    "match_kind": "PREFIX",
    "action": "FORWARD",
    "next_hop_island": "",
    "metric": 10,
    "priority": 100
  }
}
```

**Eliminar:**
```json
{
  "meta": { "type": "admin", "action": "delete_route" },
  "payload": {
    "prefix": "AI.ventas.*"
  }
}
```

#### Tabla VPN

**Listar:**
```json
{
  "meta": { "type": "admin", "action": "list_vpns" },
  "payload": {}
}
```

**Agregar:**
```json
{
  "meta": { "type": "admin", "action": "add_vpn" },
  "payload": {
    "pattern": "AI.soporte.*",
    "match_kind": "PREFIX",
    "vpn_id": 10,
    "priority": 100
  }
}
```

**Eliminar:**
```json
{
  "meta": { "type": "admin", "action": "delete_vpn" },
  "payload": {
    "pattern": "AI.soporte.*"
  }
}
```

### 2.4 Persistencia

SY.config.routes persiste la configuración en YAML:

```yaml
# /etc/json-router/sy-config-routes.yaml
version: 1
updated_at: "2025-01-20T10:00:00Z"

routes:
  - prefix: "AI.backup.*"
    match_kind: PREFIX
    action: FORWARD
    next_hop_island: "disaster-recovery"
    metric: 10
    priority: 100
    
  - prefix: "AI.interno.*"
    match_kind: PREFIX
    action: DROP
    priority: 50

vpns:
  - pattern: "AI.soporte.*"
    match_kind: PREFIX
    vpn_id: 10
    priority: 100
    
  - pattern: "AI.ventas.*"
    match_kind: PREFIX
    vpn_id: 20
    priority: 100
```

### 2.5 Flujo de Actualización

```
1. SY.admin recibe request HTTP (ej: POST /routes)
2. SY.admin envía trama JSON a SY.config.routes
3. SY.config.routes valida
4. SY.config.routes escribe en jsr-config-<island>
5. SY.config.routes incrementa config_version
6. SY.config.routes persiste en YAML
7. SY.config.routes responde OK a SY.admin
8. SY.admin envía broadcast CONFIG_CHANGED (subsystem: routes)
9. Todos los nodos (incluido routers de otras islas) re-leen config
```

**IMPORTANTE:** CONFIG_CHANGED lo emite SY.admin, no SY.config.routes. SY.admin es el único emisor de CONFIG_CHANGED en todo el sistema.

### 2.6 Notificación y Aplicación de Cambios

Cuando SY.admin completa una operación de config:

```
1. SY.config.routes escribe en jsr-config-<island>
2. SY.config.routes incrementa config_version
3. SY.config.routes responde OK a SY.admin
4. SY.admin envía broadcast CONFIG_CHANGED (subsystem: routes)
```

**Mensaje CONFIG_CHANGED (emitido por SY.admin):**

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
    "config": {
      "routes": [ ... ]
    }
  }
}
```

**Cada nodo al recibir CONFIG_CHANGED:**

1. Verificar si `subsystem` le incumbe (routers: routes, vpn, opa)
2. Verificar `version > last_version`
3. Aplicar cambios

**Ejemplo para routers:**

```rust
fn handle_config_changed(&mut self, payload: &ConfigChangedPayload) {
    // 1. Verificar subsystem
    if payload.subsystem != "routes" && payload.subsystem != "vpn" {
        return;  // No me incumbe
    }
    
    // 2. Verificar versión
    if payload.version <= self.last_config_version {
        return;  // Ya procesado
    }
    
    // 3. Re-leer config de SHM
    let config = self.read_config_region();
    
    // 4. Actualizar rutas estáticas
    self.static_routes = config.routes.clone();
    
    // 5. Actualizar tabla VPN
    self.vpn_table = config.vpns.clone();
    
    // 6. Re-evaluar VPN de TODOS los nodos conectados
    for node in self.my_nodes.iter_mut() {
        let new_vpn = self.evaluate_vpn(&node.name);
        if node.vpn_id != new_vpn {
            log::info!("Node {} VPN changed: {} -> {}", 
                       node.name, node.vpn_id, new_vpn);
            node.vpn_id = new_vpn;
        }
    }
    
    // 7. Escribir cambios en SHM
    self.write_nodes_to_shm();
    
    // 8. Recordar versión
    self.last_config_version = payload.version;
}
```

**IMPORTANTE:** Los cambios de VPN se aplican **en tiempo real** a nodos ya conectados. No es necesario reconectar.

---

## 3. Región LSA: jsr-lsa-<island>

### 3.1 Propósito

Almacena la topología de **otras islas** recibida via LSA del gateway.

### 3.2 Writer Único

Solo el **Gateway** de la isla escribe en esta región. Los routers solo leen.

### 3.3 Contenido por Isla Remota

Para cada isla remota, se almacena:

| Dato | Descripción |
|------|-------------|
| `island_id` | Nombre de la isla remota |
| `last_lsa_seq` | Último número de secuencia LSA recibido |
| `last_updated` | Timestamp de última actualización |
| `nodes[]` | Nodos de esa isla (uuid, name, vpn_id) |
| `routes[]` | Rutas estáticas de esa isla |
| `vpns[]` | Tabla VPN de esa isla |

### 3.4 Flujo de Actualización

```
1. Gateway recibe LSA de isla remota
2. Gateway valida seq > last_seq (evitar duplicados)
3. Gateway escribe en jsr-lsa-<local-island>
4. Routers leen jsr-lsa y actualizan FIB
```

### 3.5 Detección de Isla Muerta

Si no se recibe LSA por `dead_interval_ms`:

```rust
fn check_remote_islands(&mut self) {
    let now = now_epoch_ms();
    
    for island in self.lsa_region.islands.iter_mut() {
        if now - island.last_updated > self.dead_interval_ms {
            island.flags |= FLAG_STALE;
            // Los routers deberían ignorar nodos de islas stale
        }
    }
}
```

---

## 4. Cómo el Router Lee Ambas Regiones

```rust
impl Router {
    fn update_from_config_and_lsa(&mut self) {
        // 1. Leer config
        if let Some(config) = self.map_region(&self.config_shm_name) {
            if config.header.config_version != self.last_config_version {
                // Actualizar rutas estáticas
                self.static_routes = config.routes.clone();
                
                // Actualizar tabla VPN
                self.vpn_table = config.vpns.clone();
                
                self.last_config_version = config.header.config_version;
            }
        }
        
        // 2. Leer LSA (si no soy gateway)
        if !self.is_gateway {
            if let Some(lsa) = self.map_region(&self.lsa_shm_name) {
                for island in lsa.islands.iter() {
                    if island.flags & FLAG_STALE == 0 {
                        for node in island.nodes.iter() {
                            self.add_remote_route(node, &island.island_id);
                        }
                    }
                }
            }
        }
    }
}
```

---

## 5. Tabla VPN: Detalle

### 5.1 Semántica

La tabla VPN define a qué zona pertenece cada nodo basado en pattern matching de su nombre L2.

### 5.2 Evaluación

```rust
fn assign_vpn(&self, node_name: &str) -> u32 {
    // Ordenar por priority (menor primero)
    let sorted = self.vpn_table.iter()
        .filter(|v| v.flags & FLAG_ACTIVE != 0)
        .sorted_by_key(|v| v.priority);
    
    for vpn in sorted {
        if pattern_match(&vpn.pattern, vpn.match_kind, node_name) {
            return vpn.vpn_id;
        }
    }
    
    0  // Default: VPN global
}
```

### 5.3 Ejemplos

| Pattern | Match Kind | VPN ID | Resultado |
|---------|------------|--------|-----------|
| `AI.soporte.*` | PREFIX | 10 | `AI.soporte.l1` → VPN 10 |
| `AI.ventas.*` | PREFIX | 20 | `AI.ventas.closer` → VPN 20 |
| `WF.crm` | EXACT | 20 | `WF.crm` → VPN 20 |
| (ninguno) | - | 0 | `IO.wapp.123` → VPN 0 (global) |

### 5.4 Nodos SY.* y RT.*

Los nodos de sistema (`SY.*`) y routers (`RT.*`) **ignoran VPN**:

- Siempre se les asigna `vpn_id = 0`
- Pueden comunicarse con cualquier nodo de cualquier VPN
- Esto es hardcoded, no configurable

```rust
fn assign_vpn(&self, node_name: &str) -> u32 {
    // SY.* y RT.* siempre en VPN global
    if node_name.starts_with("SY.") || node_name.starts_with("RT.") {
        return 0;
    }
    
    // ... evaluación normal de tabla VPN
}
```

---

## 6. Rutas Estáticas: Detalle

### 6.1 Acciones

| Acción | Efecto |
|--------|--------|
| `FORWARD` | Rutear normalmente, o a `next_hop_island` si está definido |
| `DROP` | Descartar mensaje (blackhole) |

### 6.2 Prioridad

Las rutas estáticas se evalúan **antes** del lookup normal y tienen prioridad absoluta.

### 6.3 Ejemplos de Uso

**Blackhole para nodos internos:**
```yaml
- prefix: "AI.interno.*"
  action: DROP
```

**Forzar tráfico a otra isla:**
```yaml
- prefix: "AI.backup.*"
  action: FORWARD
  next_hop_island: "disaster-recovery"
```

**Documentar ruta existente (sin efecto real):**
```yaml
- prefix: "AI.soporte.*"
  action: FORWARD
  next_hop_island: ""  # Local
```

---

## 7. Códigos de Error

### 7.1 Config

| Código | Descripción |
|--------|-------------|
| `DUPLICATE_PREFIX` | Ya existe ruta con ese prefix |
| `PREFIX_NOT_FOUND` | Ruta no existe |
| `INVALID_PREFIX` | Formato de prefix inválido |
| `INVALID_ACTION` | Acción desconocida |
| `MAX_ROUTES_EXCEEDED` | Límite de 256 rutas alcanzado |

### 7.2 VPN

| Código | Descripción |
|--------|-------------|
| `DUPLICATE_PATTERN` | Ya existe VPN con ese pattern |
| `PATTERN_NOT_FOUND` | VPN no existe |
| `INVALID_PATTERN` | Formato de pattern inválido |
| `MAX_VPNS_EXCEEDED` | Límite de 256 VPNs alcanzado |

---

## 8. Alta Disponibilidad

### 8.1 SY.config.routes HA

```
SY.config.routes.primary@prod   (activo, escribe)
SY.config.routes.backup@prod    (standby, monitorea)
```

El backup monitorea el heartbeat del primary. Si detecta stale:

1. Verifica que el primary realmente murió
2. Reclama la región
3. Se convierte en primary
4. Lee YAML y reescribe la región

### 8.2 Gateway HA

El gateway es único por diseño. Si muere, la isla queda aislada inter-isla hasta que se reinicie.

Para v2 se puede considerar gateway secundario con failover.

---

## 9. Referencias

| Tema | Documento |
|------|-----------|
| Estructuras SHM | `03-shm.md` |
| Routing | `04-routing.md` |
| SY.config.routes completo | `SY_nodes_spec.md` |
