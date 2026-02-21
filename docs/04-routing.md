# JSON Router - 04 Routing

**Estado:** v1.16  
**Fecha:** 2026-02-04  
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
| STATIC | `jsr-config-<hive>` | 1 | Rutas configuradas |
| REMOTE | `jsr-lsa-<hive>` | 2 | Nodos de otras islas (via LSA) |

---

## 3. VPN = Zonas de Aislamiento

### 3.1 Concepto

Una VPN es una subred aislada dentro de la isla. Nodos en distintas VPNs no se ven entre sí.

### 3.2 Tabla VPN

La tabla VPN vive en `jsr-config-<hive>`:

```
pattern           → vpn_id
─────────────────────────────
AI.soporte.*      → 10
AI.ventas.*       → 20
WF.crm.*          → 20
(default)         → 0
```

### 3.3 Asignación de VPN

El router asigna VPN cuando un nodo conecta **y re-evalúa cuando cambia la config**:

```rust
fn assign_vpn(&self, node_name: &str) -> u32 {
    // SY.* y RT.* siempre en VPN global
    if node_name.starts_with("SY.") || node_name.starts_with("RT.") {
        return 0;
    }
    
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

**Cuándo se evalúa:**
- Al conectar (HELLO → ANNOUNCE)
- Cuando llega CONFIG_CHANGED (re-evalúa todos los nodos)

**Los cambios de VPN se aplican en tiempo real.** No es necesario reconectar nodos. El router actualiza el `vpn_id` de los nodos en su región `jsr-<uuid>`.

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
                next_hop: if route.next_hop_hive.is_empty() {
                    NextHop::Resolve  // Resolver localmente
                } else {
                    NextHop::Hive(route.next_hop_hive.clone())
                },
                admin_distance: 1,  // STATIC
                source: RouteSource::Static,
                action: route.action,
            });
        }
    }
    
    // 4. Nodos remotos (LSA)
    if let Some(lsa) = &self.lsa_region {
        for hive in lsa.hives.iter() {
            for node in hive.nodes.iter() {
                self.fib.add(FibEntry {
                    prefix: node.name.clone(),
                    match_kind: MATCH_EXACT,
                    vpn_id: node.vpn_id,
                    next_hop: NextHop::Hive(hive.hive_id.clone()),
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
                if !static_route.next_hop_hive.is_empty() {
                    return Ok(Action::SendToHive(static_route.next_hop_hive));
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
fn route_inter_isla(&self, msg: &Message, dst_name: &str, dst_hive: &str) -> Result<Action> {
    // 1. Verificar que conozco esa isla (vía LSA)
    if !self.lsa_has_hive(dst_hive) {
        return Err(RouteError::UnknownHive(dst_hive.to_string()));
    }
    
    // 2. Verificar que el nodo existe en esa isla
    if !self.lsa_has_node(dst_hive, dst_name) {
        return Err(RouteError::UnreachableRemote(dst_name.to_string()));
    }
    
    // 3. ¿Soy el gateway?
    if self.is_gateway {
        // Enviar directo por TCP
        Ok(Action::SendWan(dst_hive.to_string()))
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
| `ACTION_FORWARD` | Rutear normalmente (o a `next_hop_hive` si está definido) |
| `ACTION_DROP` | Descartar mensaje (blackhole) |

### 6.3 Ejemplos

```yaml
# Bloquear acceso a nodos internos
- prefix: "AI.interno.*"
  action: DROP

# Forzar backup a otra isla
- prefix: "AI.backup.*"
  action: FORWARD
  next_hop_hive: "disaster-recovery"

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
| `"broadcast"` | ausente/patrón + `meta.scope="global"` | A todos los nodos, ignorando VPN (solo system) |

### 7.2 Broadcast respeta VPN

```rust
fn broadcast(&self, msg: &Message) -> Vec<Action> {
    let src_node = self.find_node_by_uuid(&msg.routing.src)?;
    let src_vpn = src_node.vpn_id;
    let src_is_sy = is_system_node(&src_node.name);
    let scope_global = msg.meta.type == "system"
        && msg.meta.scope.as_deref() == Some("global");
    let target_pattern = msg.meta.target.as_deref();
    
    let mut actions = Vec::new();
    
    for node in self.all_nodes() {
        // Filtro VPN (excepto para SY.* o scope=global)
        if !src_is_sy && !scope_global && node.vpn_id != src_vpn {
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

Si `routing.dst` trae un nombre L2 (ej: `SY.orchestrator@motherbee`), el router resuelve
directo por FIB (sin OPA), igual que cuando recibe un UUID directo.

### 8.2 Input

OPA recibe el mensaje completo (excepto payload). El `src_ilk` permite derivar tenant via `data.identity`:

```json
{
  "routing": {
    "src": "uuid-origen"
  },
  "meta": {
    "type": "user",
    "target": "AI.soporte.*",
    "src_ilk": "ilk:cliente-juan",
    "context": {
      "channel": "whatsapp"
    }
  }
}
```

### 8.3 Output

```json
{
  "target": "AI.soporte.l1@produccion"
}
```

### 8.3.1 Resolver OPA (WASM): prioridad de dumps

Para serializar el resultado del policy engine en el resolver OPA del router:

- Se DEBE priorizar `opa_json_dump` cuando esté exportado.
- `opa_value_dump` se usa solo como fallback de compatibilidad si `opa_json_dump` no existe.

Motivo operativo: `opa_value_dump` puede devolver formatos no JSON estrictos en algunos builds;
si se intenta parsear ese output como JSON se generan errores de resolución (`OPA_ERROR`).

### 8.4 Derivar tenant desde src_ilk

OPA accede a `data.identity` (cargado desde SHM) para obtener el tenant del ilk:

```rego
package router

# Helper: obtener tenant de un ilk
get_tenant(ilk) = tenant {
    tenant := data.identity[ilk].tenant_ilk
}
```

### 8.5 Reglas por Tenant

Las reglas filtran por tenant derivado del `src_ilk`:

```rego
# Regla para tenant ACME: VIP va a L2
target = "AI.soporte.l2@produccion" {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-acme"
    input.meta.context.cliente_tier == "vip"
}

# Regla para tenant ACME: standard va a L1
target = "AI.soporte.l1@produccion" {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-acme"
    input.meta.context.cliente_tier == "standard"
}

# Regla para tenant BETA: todo va a L2 (política diferente)
target = "AI.soporte.l2@produccion" {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-beta"
}
```

### 8.6 Reglas por ILK específico

También se pueden crear reglas para un ilk específico (cliente VIP, caso especial, etc.):

```rego
# Regla para un cliente específico (VIP máximo)
target = "AI.soporte.ceo@produccion" {
    input.meta.src_ilk == "ilk:cliente-importante-uuid"
}

# Regla para un agente específico
target = "AI.supervisor@produccion" {
    input.meta.src_ilk == "ilk:agente-nuevo-uuid"
    input.meta.context.needs_review == true
}
```

### 8.7 Orden de evaluación

OPA evalúa reglas en orden. Reglas más específicas (por ilk) deben ir primero:

```rego
package router

default target = null

# 1. Reglas por ILK específico (más específicas)
target = "AI.soporte.ceo@produccion" {
    input.meta.src_ilk == "ilk:cliente-vip-especial"
}

# 2. Reglas por tenant (generales)
target = "AI.soporte.l1@produccion" {
    tenant := get_tenant(input.meta.src_ilk)
    tenant == "ilk:tenant-acme"
}

# 3. Fallback (si no matchea nada)
target = "AI.default@produccion" {
    input.meta.src_ilk  # Solo si tiene ilk
}
```

### 8.8 Acceso a data.identity

`data.identity` contiene la tabla de ILKs cargada desde SHM:

```rego
# Estructura disponible en data.identity[ilk]:
# {
#   "type": "human",
#   "human_subtype": "external", 
#   "tenant_ilk": "ilk:tenant-acme",
#   "handler_node": null,
#   "capabilities": []
# }

# Ejemplo: verificar tipo de ilk
allow {
    data.identity[input.meta.src_ilk].type == "human"
    data.identity[input.meta.src_ilk].human_subtype == "internal"
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
fn route_wildcard_hive(&self, dst_name_base: &str) -> Result<Action> {
    // Buscar en isla local primero
    let local_name = format!("{}@{}", dst_name_base, self.hive_id);
    if let Ok(action) = self.route_intra_isla(&local_name) {
        return Ok(action);
    }
    
    // Buscar en islas remotas (LSA)
    for hive in self.lsa_hives() {
        let remote_name = format!("{}@{}", dst_name_base, hive.hive_id);
        if self.lsa_has_node(&hive.hive_id, &remote_name) {
            return Ok(Action::SendToHive(hive.hive_id.clone()));
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

## 11. Persistencia de Contexto y ctx_window

### 11.1 Rol del Router

El router es el único componente que ve **todos** los mensajes. Cuando un mensaje tiene `meta.ctx`:

1. Persiste el turn en PostgreSQL (async)
2. Agrega `ctx_window` con los últimos 20 turns antes de hacer forward

### 11.2 Flujo

```
Mensaje llega
      │
      ▼
¿Tiene meta.ctx?
      │
  ┌───┴───┐
  │       │
 NO      SÍ
  │       │
  │       ▼
  │   INSERT INTO turns (async)
  │       │
  │       ▼
  │   Fetch últimos 20 turns
  │       │
  │       ▼
  │   Agregar ctx_window al mensaje
  │       │
  └───┬───┘
      │
      ▼
  Forward normal
```

### 11.3 Implementación

```rust
const CTX_WINDOW_SIZE: usize = 20;

impl Router {
    async fn handle_message(&mut self, msg: Message) -> Result<()> {
        // 1. Si tiene contexto, persistir y enriquecer
        if let Some(ctx) = &msg.meta.ctx {
            // Persistir turn actual (async, no bloquea)
            self.persist_turn(&msg);
            
            // Agregar ctx_window si no viene ya poblado
            if msg.meta.ctx_window.is_none() {
                let window = self.fetch_ctx_window(ctx).await;
                msg.meta.ctx_window = Some(window);
            }
        }
        
        // 2. Routing normal
        self.route_message(msg).await
    }
    
    fn persist_turn(&self, msg: &Message) {
        let pool = self.db_pool.clone();
        let ctx = msg.meta.ctx.clone().unwrap();
        let seq = msg.meta.ctx_seq.unwrap_or(0) + 1;
        let from_ilk = msg.meta.src_ilk.clone().unwrap_or_default();
        let to_ilk = msg.meta.dst_ilk.clone();
        let ich = msg.meta.ich.clone().unwrap_or_default();
        let content = msg.payload.clone();
        
        // Fire-and-forget: no bloquea el routing
        tokio::spawn(async move {
            let _ = sqlx::query!(
                r#"INSERT INTO turns (ctx, seq, ts, from_ilk, to_ilk, ich, msg_type, content)
                   VALUES ($1, $2, now(), $3, $4, $5, 'message', $6)
                   ON CONFLICT (ctx, seq) DO NOTHING"#,
                ctx, seq as i64, from_ilk, to_ilk, ich, content
            )
            .execute(&pool)
            .await;
        });
    }
    
    async fn fetch_ctx_window(&self, ctx: &str) -> Vec<CtxTurn> {
        // Intentar fetch de DB con timeout corto
        let result = tokio::time::timeout(
            Duration::from_millis(50),
            self.query_recent_turns(ctx, CTX_WINDOW_SIZE)
        ).await;
        
        match result {
            Ok(Ok(turns)) => turns,
            _ => vec![],  // Si falla o timeout, continuar sin window
        }
    }
    
    async fn query_recent_turns(&self, ctx: &str, limit: usize) -> Result<Vec<CtxTurn>> {
        sqlx::query_as!(
            CtxTurn,
            r#"SELECT seq, ts, from_ilk as "from", msg_type as turn_type, 
                      content->>'text' as text
               FROM turns 
               WHERE ctx = $1 
               ORDER BY seq DESC 
               LIMIT $2"#,
            ctx, limit as i64
        )
        .fetch_all(&self.db_pool)
        .await
        .map(|mut v| { v.reverse(); v })  // Ordenar cronológicamente
    }
}
```

### 11.4 Beneficios de ctx_window

| Beneficio | Descripción |
|-----------|-------------|
| Latencia cero | Nodo AI responde inmediato si 20 turns son suficientes |
| Resiliencia | Si DB está lenta/caída, conversaciones activas funcionan |
| OPA | Puede usar ctx_window para reglas basadas en historial |
| Simplicidad | Nodo no necesita ctx_client si ctx_window es suficiente |

### 11.5 Cuándo el nodo necesita más historia

Si el nodo necesita más de 20 turns (raro):

```rust
impl AiNode {
    async fn handle_message(&mut self, msg: Message) -> Result<()> {
        let window = msg.meta.ctx_window.as_ref().unwrap_or(&vec![]);
        
        if self.needs_full_history(&msg) && window.len() >= CTX_WINDOW_SIZE {
            // Caso raro: necesita más historia
            let ctx = msg.meta.ctx.as_ref().unwrap();
            let full_history = self.ctx_client.get_turns(ctx, 0).await?;
            return self.process_with_full_history(&msg, &full_history).await;
        }
        
        // Caso común: ctx_window es suficiente
        self.process_with_window(&msg, window).await
    }
}
```

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Shared memory | `03-shm.md` |
| Conectividad, LSA | `05-conectividad.md` |
| Config de rutas | `06-regiones.md` |
| Contexto (ICH, CTX) | `11-context.md` |
