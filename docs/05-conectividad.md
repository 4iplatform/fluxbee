# JSON Router - 05 Conectividad

**Estado:** v1.13  
**Fecha:** 2025-01-20  
**Audiencia:** Desarrolladores de router core, Ops/SRE

---

## 1. Resumen de Conectividad

| Escenario | Nombre | Transporte | Configuración |
|-----------|--------|------------|---------------|
| Routers misma isla | IRP | Unix socket | Automática (SHM) |
| Routers islas distintas | IIL (WAN) | TCP | Explícita (config) |
| Nodo → Router local | - | Unix socket | Automática |

---

## 2. IRP — Inter-Router Peering Intra-Isla (v1.13)

### 2.1 Motivación

Si hay múltiples routers en la misma isla:

- Todos ven SHM y toman decisiones localmente (no consultan)
- Pero cada nodo está conectado físicamente a **un router dueño**
- Para entregar frames al router dueño se usa **IRP**

### 2.2 Principio Clave

```
Decisión: lookup por SHM local
Transporte: IRP al router dueño si corresponde
```

**Diagrama:**

```
[Node X] → [Router A] --IRP--> [Router B] → [Node Y]
            (misma isla, mismo SHM)
```

### 2.3 Forwarding Intra-Isla: El Router como Switch

**IMPORTANTE:** Dentro de una isla, los routers actúan como un **switch de capa 2**. El forwarding entre routers es transparente.

```
Isla A (un solo host)
┌─────────────────────────────────────────────────────────────┐
│                                                              │
│   [Nodo X]                              [Nodo Y]            │
│      │                                      │                │
│      │ socket                               │ socket         │
│      ▼                                      ▼                │
│  ┌──────────┐    IRP (socket)        ┌──────────┐          │
│  │ Router A │◄──────────────────────►│ Router B │          │
│  └──────────┘                        └──────────┘          │
│                                                              │
│                    /dev/shm                                 │
│              (todos leen todo)                              │
└─────────────────────────────────────────────────────────────┘

Mensaje de Nodo X a Nodo Y:
  1. Nodo X envía a Router A (su router local)
  2. Router A lee SHM, ve que Nodo Y está en Router B
  3. Router A reenvía a Router B por IRP (Unix socket)
  4. Router B entrega a Nodo Y

El mensaje NO cambia. No se modifica routing.src ni routing.dst.
Router A y Router B actúan como UN SOLO switch lógico.
```

### 2.4 Reglas del Forwarding Intra-Isla

1. **Sin modificación de headers**: El mensaje pasa intacto
2. **Sin TTL decrement**: No es salto de capa 1, es forwarding de capa 2
3. **Sin tablas de rutas para IRP**: Se descubre peers por SHM
4. **Transparente para los nodos**: Un nodo no sabe si el destino está en su router

### 2.5 Aclaración Importante (v1.13)

> Dentro de una isla, **ningún router consulta a otro router** para tomar decisiones.
> Cada router lee SHM de todos (snapshot) y construye su FIB local.
>
> El forwarding a un nodo que vive en otro router es:
> 1. Lookup en FIB local
> 2. Enviar por IRP (fabric) al router dueño
>
> El IRP es **solo transporte**; la decisión sale de SHM.

---

## 3. IIL — Inter-Isla Links (WAN) (v1.13)

### 3.1 Regla Operativa: Gateway Único por Isla

**v1.13 simplifica:**

- Cada isla tiene **1 router gateway** inter-isla
- El gateway expone un puerto fijo (ej: 9000)
- El gateway puede tener uplinks directos a varias islas remotas
- Se mantiene **1 conexión activa por isla remota** (por gateway)

**Los demás routers de la isla:**

- NO corren WAN (no levantan TCP listener)
- Reenvían tráfico inter-isla al gateway por IRP

### 3.2 Ventajas del Gateway Único

- No hay ambigüedad de "¿a cuál uplink salgo?"
- Evitás routing transitivo intra-isla por múltiples gateways
- WAN queda como borde claro del dominio (como en redes reales)

### 3.3 Configuración del Gateway

```yaml
# /etc/json-router/routers/RT.gateway@produccion/config.yaml
router:
  name: RT.gateway
  island_id: produccion

inter_isla:
  listen: "0.0.0.0:9000"
  uplinks:
    - "10.0.1.100:9000"    # staging
    - "10.0.2.100:9000"    # desarrollo
```

### 3.4 Impacto en Forwarding

Si un router **no-gateway** recibe un mensaje cuyo destino está en otra isla:

```
1. Lookup local: detecta "remote_island != local_island"
2. Reenvía al gateway por IRP
3. El gateway lo manda por TCP/WAN
```

Esto mantiene el principio: **la decisión se toma local** (por SHM/config) y **el transporte se ejecuta en el router correcto**.

---

## 4. Comparación IRP vs WAN

| Aspecto | IRP (Intra-Isla) | WAN (Inter-Isla) |
|---------|------------------|------------------|
| Transporte | Unix socket | TCP |
| Configuración | Automática (SHM) | Explícita (config.yaml) |
| TTL | No decrementa | Decrementa |
| Visibilidad | Transparente | Salto explícito |
| En tabla de rutas | No (implícito) | Sí (`out_link = 1..N`) |
| Quién lo usa | Todos los routers de isla | Solo el gateway |

---

## 5. Handshake WAN

### 5.1 Flujo de Establecimiento

```
Router A (iniciador)              Router B (receptor)
    │                                     │
    │◄────────TCP connect─────────────────┤
    │                                     │
    ├─────────HELLO──────────────────────►│
    │◄────────HELLO───────────────────────┤
    │                                     │
    │         (validación de HELLO)       │
    │                                     │
    ├─────────UPLINK_ACCEPT──────────────►│  (o UPLINK_REJECT)
    │◄────────UPLINK_ACCEPT───────────────┤  (o UPLINK_REJECT)
    │                                     │
    ├─────────SYNC_REQUEST───────────────►│
    │◄────────SYNC_REPLY──────────────────┤
    │                                     │
    │         (operación normal)          │
    │                                     │
    ├─────────HELLO periódico────────────►│
    │◄────────HELLO periódico─────────────┤
```

### 5.2 Payload del HELLO WAN

```json
{
  "routing": {
    "src": "uuid-router-a",
    "dst": "uuid-router-b",
    "ttl": 16,
    "trace_id": "uuid",
    "vpn": 0
  },
  "meta": {
    "type": "system",
    "msg": "HELLO"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:00Z",
    "seq": 1,
    "protocol": "json-router/1",
    "router_id": "uuid-router-a",
    "island_id": "produccion",
    "shm_name": "/jsr-a1b2c3d4-e5f6-7890-abcd-ef1234567890",
    "capabilities": {
      "sync": true,
      "forwarding": true
    },
    "timers": {
      "hello_interval_ms": 10000,
      "dead_interval_ms": 40000
    }
  }
}
```

| Campo | Propósito |
|-------|-----------|
| `protocol` | Versión del protocolo |
| `router_id` | UUID del router |
| `island_id` | Identificador de la isla |
| `shm_name` | Nombre de región SHM (para descubrimiento local) |
| `capabilities` | Qué soporta este router |
| `timers` | Intervalos de hello/dead |

### 5.3 Payload de UPLINK_ACCEPT

```json
{
  "meta": {
    "type": "system",
    "msg": "UPLINK_ACCEPT"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:01Z",
    "peer_router_id": "uuid-router-b",
    "negotiated": {
      "protocol": "json-router/1",
      "hello_interval_ms": 10000,
      "dead_interval_ms": 40000
    }
  }
}
```

### 5.4 Payload de UPLINK_REJECT

```json
{
  "meta": {
    "type": "system",
    "msg": "UPLINK_REJECT"
  },
  "payload": {
    "timestamp": "2025-01-15T10:00:01Z",
    "reason": "ISLAND_NOT_AUTHORIZED",
    "message": "Island 'unknown' not in authorized list"
  }
}
```

| Reason | Descripción |
|--------|-------------|
| `PROTOCOL_MISMATCH` | Versión de protocolo no soportada |
| `ISLAND_NOT_AUTHORIZED` | Island ID no está en lista de permitidos |
| `CAPABILITY_MISSING` | Falta capability requerida |
| `OVERLOADED` | Router no puede aceptar más uplinks |

---

## 6. Identidad del Enlace Inter-Isla (v1.13)

El enlace se identifica por el par de islas:

```
link_id = min(islaA, islaB) + "<->" + max(islaA, islaB)
```

Ejemplo:
```
produccion<->staging
```

Si se crean dos sockets por carrera (ambos gateways intentan conectar), se conserva 1 usando tie-break determinista por `router_uuid` (menor gana).

---

## 7. Forwarding Inter-Isla (v1.13)

### 7.1 Algoritmo

```
1. Parsear dst_island desde el nombre L2 destino (@isla)

2. Si dst_island == local_island:
   → Routing intra-isla normal

3. Si dst_island != local_island:
   a. Si router actual NO es gateway:
      → Forward al gateway por IRP
   b. Si router actual ES gateway:
      → Buscar conexión a dst_island
      → Si existe: enviar
      → Si no existe: NO_ROUTE_TO_ISLAND

4. TTL: decrementa al cruzar inter-isla
```

### 7.2 No hay LSA para Nodos (v1.13)

**No se anuncian "rutas de nodos" entre islas.**

Con el naming `@isla`, solo hace falta conectividad al gateway remoto. El gateway remoto resuelve el nodo localmente.

---

## 8. Operación Normal WAN

Una vez establecido el uplink:

- **HELLO periódico** cada `hello_interval_ms`
- **Dead detection**: sin HELLO por `dead_interval_ms` = peer caído
- **TTL**: se decrementa por hop, previene loops entre islas

---

## 9. Topología WAN: Modelo Bus/Estrella

### 9.1 Topología Bus (Lineal)

```
Isla A ──► Isla B ──► Isla C ──► Isla D
```

**Configuración mínima:**

```yaml
# Isla A (primera)
inter_isla:
  listen: "0.0.0.0:9000"
  uplinks:
    - "10.0.1.2:9000"    # → B

# Isla B
inter_isla:
  listen: "0.0.0.0:9000"
  uplinks:
    - "10.0.2.2:9000"    # → C

# Isla C
inter_isla:
  listen: "0.0.0.0:9000"
  uplinks:
    - "10.0.3.2:9000"    # → D

# Isla D (última)
inter_isla:
  listen: "0.0.0.0:9000"
  # Sin uplink, es el final
```

### 9.2 Topología Estrella

```
         Isla A (hub)
        /    |    \
   Isla B  Isla C  Isla D
```

**Isla A configura uplinks a todas:**

```yaml
# Isla A (hub)
inter_isla:
  listen: "0.0.0.0:9000"
  uplinks:
    - "10.0.1.100:9000"  # → B
    - "10.0.2.100:9000"  # → C
    - "10.0.3.100:9000"  # → D
```

### 9.3 Conexiones Bidireccionales Automáticas

Cuando A conecta a B por uplink:
- A conoce a B (su uplink configurado)
- B conoce a A (conexión entrante aceptada)

**No es necesario configurar ambos lados.**

```
A config: uplink → B
B config: (nada hacia A)

Resultado:
  A ◄────────► B
      (TCP)
  
A sabe de B: por su config
B sabe de A: por conexión entrante
```

---

## 10. Routing Transitivo Inter-Isla

Si A quiere enviar a isla C pero solo tiene uplink a B:

```
A ──► B ──► C

1. A recibe mensaje para nodo en Isla C
2. A no tiene uplink directo a C
3. A envía a B (único uplink)
4. B recibe, ve que destino es Isla C
5. B tiene uplink a C, reenvía
6. C recibe, entrega al nodo local
```

**TTL previene loops:** El mensaje decrementa TTL en cada hop inter-isla.

---

## 11. Seguridad WAN

La autenticación se delega al edge proxy (NGINX, Envoy, etc.):

| Capa | Responsable | Mecanismo |
|------|-------------|-----------|
| Transporte | Edge proxy | TLS/mTLS |
| Autorización | Edge proxy | Allowlist de IPs/certs |
| Rate limiting | Edge proxy | Por conexión/bytes |
| Protocolo | Router | HELLO + UPLINK_ACCEPT/REJECT |

El proxy no interpreta el protocolo. Es transparente L4.

La validación de `island_id` contra lista de islas autorizadas puede hacerse en el router al recibir HELLO.

---

## 12. Requisitos de Deployment (v1.13)

### 12.1 Gateway por Isla

- Cada isla tiene UN router con WAN activa
- Este router es el punto de entrada/salida inter-isla
- No se puede matar via API (protegido)

### 12.2 SY.admin

- Debe correr en una isla con conectividad a todas las demás
- Requiere rutas (directas o transitivas) a todas las islas que administra

---

## 13. Referencias

| Tema | Documento |
|------|-----------|
| Arquitectura, islas | `01-arquitectura.md` |
| Routing, FIB | `04-routing.md` |
| Operaciones, systemd | `07-operaciones.md` |
