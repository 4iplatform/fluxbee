# JSON Router - 04 Routing

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core

---

## 1. Conceptos de Redes Aplicados

| Concepto | En redes IP | En este sistema |
|----------|-------------|-----------------|
| **RIB** | Todas las rutas candidatas | Datos en todas las regiones shm |
| **FIB** | Rutas ganadoras para forwarding | Cache en memoria local del router |
| **Next-hop** | Router vecino al que enviar | `next_hop_router` en RouteEntry |
| **Admin distance** | Preferencia por origen | `admin_distance` (CONNECTED < STATIC < LSA) |
| **LPM** | Matcheo más específico gana | `prefix_len` + `match_kind` |
| **VRF** | Tabla de routing aislada | `vpn_id` (v1.13) |

---

## 2. VPN = Zonas / VRF dentro de una Isla (v1.13)

### 2.1 Definición

Una **VPN** es una zona lógica (VRF-like) dentro de una isla:

- El router mantiene una vista de routing separada (RIB/FIB por `vpn_id`)
- Los nodos en una VPN solo ven nodos/rutas de su misma VPN
- El aislamiento es **arquitectónico**, no "policy best-effort"

**Dominio efectivo:**

```
domain = (island_id, vpn_id)
```

**Convención:**
- `vpn_id = 0` es "global" (default, todos se ven)
- `vpn_id != 0` son zonas aisladas

### 2.2 Pertenencia de Nodo a VPN

Cada nodo pertenece a **exactamente una VPN** (`vpn_id`).

- Default: `vpn_id = 0` (global)
- La pertenencia la decide el **router por configuración**, no el nodo
- Se asigna al nodo al conectar (via membership rules)

### 2.3 Routing por VPN

Cuando el router resuelve `dst` dentro de isla:

```
1. Obtener vpn_id del nodo origen (o del mensaje)
2. Lookup se hace en la tabla del dominio vpn_id
3. Si no existe destino en esa VPN → UNREACHABLE_IN_VPN
```

### 2.4 Forwarding

Un mensaje con `routing.vpn = X` solo puede rutear a destinos dentro de la misma VPN X, **salvo reglas explícitas de leak**.

### 2.5 Broadcast

- **Broadcast de negocio** respeta VPN: se entrega solo a nodos de la misma VPN
- **Broadcast de sistema** (`meta.type="system"`) ignora VPN (infra esencial)

### 2.6 Leak entre VPNs

Se permiten reglas explícitas de **leak**:

```
from_vpn → to_vpn
pattern de destinos permitidos
```

Sin leak, el aislamiento es total.

---

## 3. Tabla de Ruteo (RIB)

### 3.1 Tipos de Ruta

| Tipo | route_type | admin_distance | Origen |
|------|------------|----------------|--------|
| CONNECTED | 0 | 0 | Nodo conectado directamente al router |
| STATIC | 1 | 1 | Configurada en región de config |
| LSA | 2 | 10 | Aprendida de otro router |

### 3.2 Selección de Mejor Ruta

Cuando hay múltiples rutas al mismo destino:

1. **Longest Prefix Match (LPM)**: La ruta más específica gana
2. **Admin Distance**: Menor valor gana (CONNECTED < STATIC < LSA)
3. **Metric**: Menor costo gana
4. **Installed At**: La más vieja gana (estabilidad)

### 3.3 Rutas CONNECTED (automáticas)

Cuando un nodo conecta, el router crea automáticamente una ruta CONNECTED:

```
prefix: <nombre capa 2 del nodo>
prefix_len: longitud exacta
match_kind: EXACT
next_hop_router: <uuid del router local>
out_link: LINK_LOCAL (0)
admin_distance: AD_CONNECTED (0)
route_type: ROUTE_CONNECTED
vpn_id: <vpn asignado al nodo>
```

### 3.4 Rutas STATIC (configuradas)

Las rutas estáticas se configuran centralizadamente mediante `SY.config.routes`. Ver documento `06-config-shm.md`.

```
SY.config.routes → /jsr-config-<island> → Routers leen → FIB actualizada
```

### 3.5 Rutas LSA (aprendidas)

Recibidas de otros routers de la misma isla via SHM. El router instala estas rutas con `admin_distance = AD_LSA`.

---

## 4. Construcción de la FIB

El router construye su FIB en memoria local leyendo todas las regiones.

### 4.1 Tabla peer_links

El router mantiene una tabla local que mapea cada peer a su link:

```rust
peer_links: HashMap<Uuid, u32>  // peer_uuid → link_id
```

**Cómo se llena:**
- Al recibir un HELLO por un uplink, se registra el link
- `peer_links[peer_uuid] = link_id`

### 4.2 Algoritmo de construcción

```
FIB = []  // Separada por vpn_id

// 1. Agregar mis rutas CONNECTED (nodos locales)
for node in mi_region.nodes:
    if node.flags & FLAG_ACTIVE:
        FIB[node.vpn_id].add(RouteEntry {
            prefix: node.name,
            match_kind: MATCH_EXACT,
            out_link: LINK_LOCAL (0),
            admin_distance: AD_CONNECTED,
            route_type: ROUTE_CONNECTED,
        })

// 2. Agregar nodos de peers como rutas LSA
for peer_region in peers_mapeados:
    if peer_region.heartbeat es reciente:
        let link_id = peer_links.get(peer_region.router_uuid)
        if link_id.is_none():
            continue
        
        for node in peer_region.nodes:
            if node.flags & FLAG_ACTIVE:
                FIB[node.vpn_id].add(RouteEntry {
                    prefix: node.name,
                    match_kind: MATCH_EXACT,
                    next_hop_router: peer_region.router_uuid,
                    out_link: link_id,
                    admin_distance: AD_LSA,
                    route_type: ROUTE_LSA,
                })

// 3. Agregar rutas STATIC de config (por vpn_scope)
for route in config_region.static_routes:
    if route.flags & FLAG_ACTIVE:
        if route.vpn_scope == 0:
            // Instalar en todas las VPNs
            for vpn_id in all_vpns:
                FIB[vpn_id].add(route)
        else:
            // Instalar solo en esa VPN
            FIB[route.vpn_scope].add(route)

// 4. Ordenar cada FIB[vpn] para lookup rápido
for vpn_id in all_vpns:
    FIB[vpn_id].sort_by(|a, b| {
        // LPM: más específico primero (prefix_len desc)
        // Luego admin_distance menor
        // Luego metric menor
    })
```

### 4.3 Semántica de LINK_LOCAL

**LINK_LOCAL (0) significa:** El nodo está conectado directamente a MIS sockets.

**Para nodos de otros routers:** Siempre se usa un `link_id` de uplink (1..N), incluso si el peer está en el mismo host.

---

## 5. Algoritmo de Selección de Ruta

Cuando el router necesita encontrar destino para un mensaje:

```
1. Obtener vpn_id del mensaje (del nodo origen o routing.vpn)

2. Parsear @isla del destino (nombre L2)
   - Si isla_destino != isla_local → forwarding inter-isla (ver sección 6)

3. Buscar en FIB[vpn_id] todas las rutas que matchean el destino

4. Filtrar solo FLAG_ACTIVE

5. Ordenar por:
   a. prefix_len descendente (LPM: más específico primero)
   b. admin_distance ascendente (menor = preferido)
   c. metric ascendente (menor costo)

6. Tomar la primera (ganadora)

7. Según action de la ruta:
   - ACTION_FORWARD (0): continuar a paso 8
   - ACTION_DROP (1): descartar mensaje, fin

8. Si out_link == LINK_LOCAL:
   → Nodo conectado a MI socket, entregar directamente

9. Si out_link > 0:
   → Enviar por ese uplink al next_hop_router (IRP)
```

### 5.1 Verificación de VPN Leaks

Si el lookup en `FIB[vpn_id]` no encuentra destino, verificar leak rules:

```
for leak_rule in vpn_leak_rules:
    if leak_rule.from_vpn == vpn_id:
        if destination matches leak_rule.pattern:
            // Buscar en FIB[leak_rule.to_vpn]
            route = lookup(FIB[leak_rule.to_vpn], destination)
            if route:
                return route
```

---

## 6. Orden de Evaluación Recomendado (v1.13)

```
(1) Validar/normalizar destino (parsear @isla)

(2) Resolver vpn efectivo:
    - Por nodo origen (vpn_id asignado)
    - O por routing.vpn del mensaje (normalizado)

(3) Aplicar rutas estáticas (FORWARD/DROP) dentro del vpn_scope

(4) Resolver destino:
    - Si @isla == local: lookup en FIB[vpn]
    - Si @isla != local: enviar al gateway inter-isla

(5) Si no se encuentra en FIB[vpn]:
    - Verificar leak rules
    - Si no hay leak: UNREACHABLE_IN_VPN

(6) OPA solo si corresponde (dst=null, policies de negocio)
```

---

## 7. Forwarding Inter-Isla (v1.13)

### 7.1 Principio

El routing inter-isla se decide parseando `@isla` del destino. **No hay LSA de nodos entre islas.**

### 7.2 Algoritmo

```
1. Parsear dst_island desde el nombre L2 destino (@isla)

2. Si dst_island == local_island:
   → Routing intra-isla normal (ver sección 5)

3. Si dst_island != local_island:
   a. Si router actual NO es gateway:
      → Forward al gateway por IRP (fabric intra-isla)
   b. Si router actual ES gateway:
      → Buscar conexión a dst_island
      → Si existe: enviar por TCP
      → Si no existe: NO_ROUTE_TO_ISLAND

4. TTL: decrementa al cruzar inter-isla
```

### 7.3 Gateway Único por Isla (v1.13)

Cada isla tiene **un único Router Gateway** que mantiene uplinks TCP hacia otras islas.

- Los demás routers de la isla NO levantan WAN
- Los demás routers reenvían tráfico inter-isla al gateway por IRP

Ver documento `05-conectividad.md` para detalles.

---

## 8. Forwarding Completo

```
Mensaje entrante con dst = "AI.soporte.l1@staging"
    │
    ▼
┌─────────────────────────────────────────┐
│ 1. Parsear @isla del destino            │
│    → isla_destino = "staging"           │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 2. ¿isla_destino == isla_local?         │
│    → No (staging != produccion)         │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 3. ¿Soy el gateway?                     │
│    → No                                 │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 4. Forward al gateway por IRP           │
│    (fabric intra-isla)                  │
└─────────────────────────────────────────┘
    │
    ▼
[Gateway recibe]
    │
    ▼
┌─────────────────────────────────────────┐
│ 5. Buscar conexión a isla "staging"     │
│    → Encontrada en uplinks[]            │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 6. Decrementar TTL                      │
│    → TTL > 0, continuar                 │
└─────────────────────────────────────────┘
    │
    ▼
┌─────────────────────────────────────────┐
│ 7. Enviar por TCP a gateway de staging  │
└─────────────────────────────────────────┘
```

---

## 9. Policies OPA

Las policies OPA permiten decisiones de routing dinámicas basadas en metadata del mensaje.

### 9.1 Cuándo se usa OPA

OPA se consulta cuando `routing.dst = null`. El router necesita resolver el destino.

### 9.2 Input para OPA

```json
{
  "meta": { ... },
  "routing": {
    "src": "uuid-origen",
    "vpn": 10
  }
}
```

### 9.3 Output de OPA

```json
{
  "target": "AI.soporte.l1@produccion"
}
```

### 9.4 Ejemplo de Policy (Rego)

```rego
package router

default target = null

# Clientes VIP van a soporte L2
target = "AI.soporte.l2@produccion" {
    input.meta.context.cliente_tier == "vip"
}

# Clientes standard van a soporte L1
target = "AI.soporte.l1@produccion" {
    input.meta.context.cliente_tier == "standard"
}
```

### 9.5 Relación OPA vs VPN

- **VPN** es aislamiento arquitectónico (el router lo impone)
- **OPA** es decisión de negocio (resuelve destino)
- VPN existe sin OPA
- OPA opera dentro del contexto del VPN del nodo origen

---

## 10. Broadcast y Multicast

### 10.1 Tipos de Destino

| `dst` | `meta.target` | Comportamiento |
|-------|---------------|----------------|
| UUID | - | Unicast: entregar a ese nodo |
| `null` | nombre capa 2 | Resolver via OPA, luego unicast |
| `"broadcast"` | - | Broadcast: entregar a todos (respetando VPN) |
| `"broadcast"` | patrón capa 2 | Multicast: entregar a nodos que matchean |

### 10.2 Broadcast respeta VPN

**Broadcast de negocio** (`meta.type != "system"`):

```
Para cada nodo en mi isla:
    if nodo.vpn_id == mensaje.vpn:
        if nodo.name matchea meta.target (si existe):
            entregar
```

**Broadcast de sistema** (`meta.type == "system"`):

```
Para cada nodo en mi isla:
    if nodo.name matchea meta.target (si existe):
        entregar
    // Ignora VPN - infra esencial
```

### 10.3 TTL en Broadcast

| TTL | Alcance |
|-----|---------|
| 1 | Solo nodos conectados a ESTE router |
| 2 | Toda la isla (este router + peers via IRP) |
| 3+ | Cruza WAN a otras islas |

### 10.4 Prevención de Loops

Cada router mantiene cache de `trace_id`:

```rust
struct BroadcastCache {
    seen: HashMap<Uuid, Instant>,
    ttl: Duration,  // 60 segundos
}

impl BroadcastCache {
    fn check_and_add(&mut self, trace_id: Uuid) -> bool {
        self.cleanup_expired();
        if self.seen.contains_key(&trace_id) {
            false  // Ya visto, DROP
        } else {
            self.seen.insert(trace_id, Instant::now());
            true   // Nuevo, procesar
        }
    }
}
```

---

## 11. Balanceo entre Nodos del Mismo Rol

Cuando hay múltiples nodos con la capacidad requerida (mismo nombre capa 2, mismo VPN):

- **Round-robin** por defecto
- **Least-connections** opcional
- **Sticky** por `trace_id` opcional

*Detalles de implementación por definir.*

---

## 12. Referencias

| Tema | Documento |
|------|-----------|
| Shared memory, estructuras | `03-shm.md` |
| Conectividad IRP/WAN | `05-conectividad.md` |
| Región config, VPN zones | `06-config-shm.md` |
