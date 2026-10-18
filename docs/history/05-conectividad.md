# JSON Router - 05 Conectividad

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core, Ops/SRE

---

## 1. Resumen de Conectividad

| Escenario | Nombre | Transporte | Quién |
|-----------|--------|------------|-------|
| Nodo ↔ Router | - | Unix socket | Todos |
| Router ↔ Router (misma isla) | IRP | Unix socket | Todos los routers |
| Gateway ↔ Gateway (distintas islas) | WAN | TCP | Solo gateways |

---

## 2. IRP — Inter-Router Peering (Intra-Isla)

### 2.1 Concepto

Los routers de una misma isla se comunican via IRP (Unix sockets) para reenviar mensajes a nodos que están en otros routers.

```
Isla "produccion"
┌─────────────────────────────────────────────────────────────┐
│                                                             │
│   [Nodo X]                              [Nodo Y]           │
│      │                                      │               │
│      ▼                                      ▼               │
│  ┌──────────┐    IRP (Unix socket)   ┌──────────┐         │
│  │ Router A │◄──────────────────────►│ Router B │         │
│  └──────────┘                        └──────────┘         │
│                                                             │
│                    /dev/shm                                │
│              (todos leen todo)                             │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 Principio

```
Decisión: lookup en SHM local (FIB)
Transporte: IRP al router dueño del nodo destino
```

**El router NO consulta a otros routers.** Cada router lee SHM y toma decisiones localmente.

### 2.3 Reglas del IRP

1. **Sin modificación de headers**: El mensaje pasa intacto
2. **Sin TTL decrement**: No es salto inter-isla
3. **Descubrimiento via SHM**: No hay config manual de peers
4. **Transparente para nodos**: El nodo no sabe si el destino está en otro router

### 2.4 Flujo

```
1. Nodo X envía mensaje para Nodo Y a Router A
2. Router A busca en FIB: Nodo Y está en Router B
3. Router A envía por IRP a Router B
4. Router B entrega a Nodo Y
```

---

## 3. WAN — Conectividad Inter-Isla

### 3.1 Gateway Único por Isla

Cada isla tiene **un único router gateway**:

- Es el único que mantiene conexiones TCP a otras islas
- Es el único que escribe en `jsr-lsa-<hive>`
- Los demás routers reenvían tráfico inter-isla al gateway via IRP

### 3.2 Identificación del Gateway

El gateway se identifica por un flag en su región SHM:

```rust
// En ShmHeader
pub is_gateway: u8,  // 1 si es gateway
```

Los demás routers leen las regiones de peers y detectan cuál es el gateway:

```rust
fn find_gateway(&self) -> Option<RouterInfo> {
    for peer in self.peer_regions.iter() {
        if peer.header.is_gateway == 1 {
            return Some(RouterInfo {
                uuid: peer.header.router_uuid,
                name: peer.header.router_name,
            });
        }
    }
    None
}
```

### 3.3 Configuración del Gateway

No existe `config.yaml` por router. El gateway se identifica via `is_gateway` en SHM (set por el orchestrator).  
La configuración WAN se declara **solo** en `hive.yaml` y aplica al gateway.

```yaml
# /etc/fluxbee/hive.yaml
hive_id: produccion

wan:
  listen: "0.0.0.0:9000"
  uplinks:
    - address: "10.0.1.100:9000"    # staging
    - address: "10.0.2.100:9000"    # desarrollo
  authorized_hives:
    - staging
    - desarrollo
```

### 3.4 Topología WAN

```
         ┌─────────────────┐
         │   produccion    │
         │   (Gateway)     │
         └────────┬────────┘
                  │ TCP
        ┌─────────┴─────────┐
        │                   │
        ▼                   ▼
┌───────────────┐   ┌───────────────┐
│    staging    │   │  desarrollo   │
│   (Gateway)   │   │   (Gateway)   │
└───────────────┘   └───────────────┘
```

---

## 4. LSA — Link State Advertisement

### 4.1 Concepto

El gateway intercambia información de topología con otros gateways via mensajes **LSA**.

### 4.2 ¿Qué contiene el LSA?

El gateway consolida **toda** la información de su isla:

| Dato | Fuente | Descripción |
|------|--------|-------------|
| Nodos | `jsr-<router-uuid>` de todos los routers | Todos los nodos conectados |
| Rutas | `jsr-config-<hive>` | Rutas estáticas |
| VPNs | `jsr-config-<hive>` | Tabla de asignación VPN |

### 4.3 Formato del Mensaje LSA

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
    "hive": "produccion",
    "seq": 42,
    "timestamp": "2025-01-20T10:00:00Z",
    "nodes": [
      {
        "uuid": "a1b2c3d4-...",
        "name": "AI.soporte.l1@produccion",
        "vpn_id": 10
      },
      {
        "uuid": "b2c3d4e5-...",
        "name": "RT.primary@produccion",
        "vpn_id": 0
      },
      {
        "uuid": "c3d4e5f6-...",
        "name": "SY.config.routes@produccion",
        "vpn_id": 0
      }
    ],
    "routes": [
      {
        "prefix": "AI.backup.*",
        "match_kind": "PREFIX",
        "action": "FORWARD",
        "next_hop_hive": "disaster-recovery",
        "metric": 10
      }
    ],
    "vpns": [
      {
        "pattern": "AI.soporte.*",
        "match_kind": "PREFIX",
        "vpn_id": 10
      },
      {
        "pattern": "AI.ventas.*",
        "match_kind": "PREFIX",
        "vpn_id": 20
      }
    ]
  }
}
```

### 4.4 ¿Cuándo se envía LSA?

| Evento | Acción |
|--------|--------|
| Conexión establecida | Enviar LSA completo |
| Nodo conecta/desconecta | Enviar LSA actualizado |
| Ruta estática cambia | Enviar LSA actualizado |
| VPN cambia | Enviar LSA actualizado |
| Periódicamente | Enviar como heartbeat |

### 4.5 Flujo de LSA

```
Gateway Prod                              Gateway Staging
     │                                          │
     │◄────────── TCP connect ─────────────────►│
     │                                          │
     ├─────────── LSA (prod) ──────────────────►│
     │                                          │ (escribe en jsr-lsa-staging)
     │◄────────── LSA (staging) ────────────────┤
     │ (escribe en jsr-lsa-prod)                │
     │                                          │
     │         (operación normal)               │
     │                                          │
     │ [nodo conecta en prod]                   │
     ├─────────── LSA (prod) ──────────────────►│
     │                                          │
     │                    [nodo conecta en staging]
     │◄────────── LSA (staging) ────────────────┤
     │                                          │
```

### 4.6 Gateway Escribe en jsr-lsa

Cuando el gateway recibe LSA de otra isla:

```rust
fn handle_lsa(&mut self, lsa: LsaMessage) {
    // 1. Validar LSA
    if lsa.seq <= self.last_lsa_seq.get(&lsa.hive).unwrap_or(&0) {
        return;  // LSA viejo, ignorar
    }
    
    // 2. Escribir en jsr-lsa-<mi-isla>
    self.lsa_region.seqlock_begin_write();
    
    // Actualizar o crear entrada para esta isla
    let hive_entry = self.lsa_region.find_or_create_hive(&lsa.hive);
    hive_entry.last_lsa_seq = lsa.seq;
    hive_entry.last_updated = now_epoch_ms();
    
    // Reemplazar nodos, rutas, VPNs de esa isla
    self.lsa_region.replace_nodes(&lsa.hive, &lsa.nodes);
    self.lsa_region.replace_routes(&lsa.hive, &lsa.routes);
    self.lsa_region.replace_vpns(&lsa.hive, &lsa.vpns);
    
    self.lsa_region.seqlock_end_write();
    
    // 3. Recordar seq para evitar duplicados
    self.last_lsa_seq.insert(lsa.hive.clone(), lsa.seq);
}
```

### 4.7 Routers Leen jsr-lsa

Todos los routers de la isla leen `jsr-lsa-<hive>` para saber:

- Qué islas remotas existen
- Qué nodos hay en cada isla remota
- Qué rutas estáticas tienen
- Cómo están asignados sus VPNs

```rust
fn lookup_remote(&self, dst_name: &str, dst_hive: &str) -> Option<RemoteNodeEntry> {
    let lsa = self.lsa_region.as_ref()?;
    
    for hive in lsa.hives.iter() {
        if hive.hive_id == dst_hive {
            for node in hive.nodes.iter() {
                if node.name == dst_name {
                    return Some(node.clone());
                }
            }
        }
    }
    None
}
```

---

## 5. Handshake WAN

### 5.1 Flujo

```
Gateway A (iniciador)              Gateway B (receptor)
    │                                     │
    │◄────────TCP connect─────────────────┤
    │                                     │
    ├─────────HELLO──────────────────────►│
    │◄────────HELLO───────────────────────┤
    │                                     │
    │         (validación)                │
    │                                     │
    ├─────────WAN_ACCEPT─────────────────►│
    │◄────────WAN_ACCEPT──────────────────┤
    │                                     │
    ├─────────LSA────────────────────────►│
    │◄────────LSA─────────────────────────┤
    │                                     │
    │         (operación normal)          │
    │                                     │
```

### 5.2 Mensaje HELLO (WAN)

```json
{
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "protocol": "json-router/1.13",
    "router_id": "<uuid>",
    "router_name": "RT.gateway@produccion",
    "hive_id": "produccion",
    "capabilities": ["lsa", "forwarding"],
    "timers": {
      "hello_interval_ms": 10000,
      "dead_interval_ms": 40000
    }
  }
}
```

### 5.3 Mensaje WAN_ACCEPT

```json
{
  "meta": {
    "type": "system",
    "msg": "WAN_ACCEPT"
  },
  "payload": {
    "peer_router_id": "<uuid>",
    "negotiated": {
      "protocol": "json-router/1.13",
      "hello_interval_ms": 10000,
      "dead_interval_ms": 40000
    }
  }
}
```

### 5.4 Mensaje WAN_REJECT

```json
{
  "meta": {
    "type": "system",
    "msg": "WAN_REJECT"
  },
  "payload": {
    "reason": "HIVE_NOT_AUTHORIZED",
    "message": "Hive 'unknown' not in authorized list"
  }
}
```

---

## 6. Heartbeat entre Gateways

El LSA también funciona como heartbeat:

- Se envía periódicamente (cada `hello_interval_ms`)
- Si no se recibe LSA por `dead_interval_ms`, se considera el peer muerto
- Al detectar peer muerto, se marcan sus nodos como inaccesibles

```rust
fn check_peer_health(&mut self) {
    let now = now_epoch_ms();
    
    for (hive, last_seen) in self.peer_last_seen.iter() {
        if now - last_seen > self.dead_interval_ms {
            log::warn!("Peer hive {} appears dead", hive);
            self.mark_hive_unreachable(hive);
        }
    }
}
```

---

## 7. Forwarding Inter-Isla

### 7.1 Desde Router Normal

```rust
fn route_to_remote_hive(&self, msg: &Message, dst_hive: &str) -> Action {
    // Soy router normal, reenviar al gateway
    let gateway = self.find_gateway()
        .expect("No gateway found in this hive");
    
    Action::ForwardToRouter(gateway.uuid)
}
```

### 7.2 Desde Gateway

```rust
fn route_to_remote_hive(&self, msg: &Message, dst_hive: &str) -> Action {
    // Soy gateway, buscar conexión a esa isla
    if let Some(conn) = self.wan_connections.get(dst_hive) {
        // Decrementar TTL
        let mut msg = msg.clone();
        msg.routing.ttl -= 1;
        if msg.routing.ttl == 0 {
            return Action::TtlExceeded;
        }
        
        Action::SendWan(conn.clone(), msg)
    } else {
        Action::NoRouteToHive(dst_hive.to_string())
    }
}
```

---

## 8. TTL

### 8.1 Reglas de Decremento

| Escenario | TTL decrementa |
|-----------|----------------|
| IRP (intra-isla) | No |
| WAN (inter-isla) | Sí |
| Broadcast forwarding | Sí |

### 8.2 Alcance por TTL

| TTL | Alcance |
|-----|---------|
| 1 | Solo nodos locales del router |
| 2 | Toda la isla (incluye peers via IRP) |
| 3+ | Cruza a otras islas |

### 8.3 TTL Exceeded

```json
{
  "meta": {
    "type": "system",
    "msg": "TTL_EXCEEDED"
  },
  "payload": {
    "original_dst": "AI.soporte.l1@staging",
    "last_hop": "RT.gateway@produccion"
  }
}
```

---

## 9. Seguridad WAN

### 9.1 Capas de Seguridad

| Capa | Responsable | Mecanismo |
|------|-------------|-----------|
| Transporte | Edge proxy | TLS/mTLS |
| Autorización | Edge proxy | Allowlist IPs/certs |
| Protocolo | Gateway | HELLO + WAN_ACCEPT/REJECT |

### 9.2 Validación de Hive ID

El gateway valida que el `hive_id` del peer esté en su lista de autorizados:

```yaml
# /etc/fluxbee/hive.yaml
wan:
  authorized_hives:
    - staging
    - desarrollo
```

---

## 10. Comparación IRP vs WAN

| Aspecto | IRP (Intra-Isla) | WAN (Inter-Isla) |
|---------|------------------|------------------|
| Transporte | Unix socket | TCP |
| Configuración | Automática (SHM) | Explícita (`hive.yaml`) |
| TTL | No decrementa | Decrementa |
| Quién lo usa | Todos los routers | Solo gateways |
| Descubrimiento | Via SHM | Via config + HELLO |
| LSA | No necesario | Obligatorio |

---

## 11. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura | `01-arquitectura.md` |
| Shared memory, jsr-lsa | `03-shm.md` |
| Routing | `04-routing.md` |
