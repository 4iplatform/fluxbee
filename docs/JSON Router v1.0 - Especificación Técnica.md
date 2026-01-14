# JSON Router v1.0 — Especificación Técnica (revisada)

**Estado:** v1.0 (cerrada para implementación)  
**Fecha:** 2026-01-14  
**Lenguaje de referencia:** Node.js / TypeScript  
**Transporte (data-plane):** Kafka  
**Formato de trama:** JSON (obligatorio)

---

## 1. Objetivo

Construir un **router de mensajes JSON** con el **mismo esquema mental que un router de hardware**:

- recibe “tramas” (frames) por **puertos**,
- consulta su **tabla de forwarding (FIB)**,
- aplica **políticas (approve/deny)**,
- reenvía por un **puerto de salida**.

En vez de Ethernet/TCP, los **cables/puertos** del router se implementan como **colas/topics en Kafka**.

---

## 2. Principios de diseño (decisiones v1.0)

1. **Trama = JSON** (no negociable).
2. **Orden garantizado por stream**:
   - existe `stream_id`,
   - el **nodo originador** asigna `seq` monótono por `stream_id`,
   - Kafka usa `key = stream.stream_id` para ordering por partición.
3. **Kafka es el medio** (cola/backplane): se acepta su overhead porque compra:
   - ordering por clave,
   - backpressure/cola,
   - durabilidad/replay,
   - escalabilidad por particiones.
4. **Router “hardware-like”**:
   - **puertos = topics/colas**,
   - **FIB local** con longest-prefix (o match jerárquico equivalente),
   - forwarding hop-by-hop por puerto.
5. **Segmentación tipo VLAN/VRF**: `rd` (routing domain) separa dominios.
6. **Policies**:
   - un solo lugar de autoría,
   - distribución global por Kafka (bundle/version),
   - evaluación **local en el router** (Rego→WASM),
   - policy **NO rutea** y **NO modifica destino**: solo approve/deny + constraints.
7. **SDK común**: nodos y router usan la misma librería/capas.
8. **Management plane**: HTTP solo para configuración/observabilidad (no data-plane).
9. **No NAT, no DHCP** en v1.0.

---

## 3. Terminología

- **Nodo:** agente AI, I/O, workflow, etc. (participante final).
- **Router:** proceso que forwardea frames según FIB + policy.
- **Puerto (interface):** “cable lógico” del router implementado como topic/cola Kafka.
- **Trama / Frame:** mensaje JSON con header estándar + payload.
- **FIB (Forwarding Information Base):** tabla local `prefix → out_port`.
- **RIB (Routing Information Base):** opcional v1.0 (si se implementa aprendizaje). En v1.0 se asume FIB gestionable por config + anuncios mínimos.
- **RD (Routing Domain):** dominio aislado (VRF/VLAN-like).
- **Policy bundle:** artefacto versionado distribuido por Kafka.

---

## 4. Arquitectura (alto nivel)

### 4.1 Capas (monorepo)

- `@json-router/protocol`
  - tipos y validación de frames (JSON schema / TS types),
  - canonicalización de direcciones/prefijos (si aplica),
  - utilidades de header (TTL, hop_count).
- `@json-router/transport-kafka`
  - produce/consume Kafka,
  - reglas de consumer groups para broadcast vs unicast,
  - serialización JSON, compresión opcional.
- `@json-router/sdk`
  - API unificada para **nodos** (bootstrap + send + receive + announce),
  - helpers de `seq` por `stream_id`.
- `@json-router/policy-runtime`
  - carga bundles de policy,
  - eval local (WASM),
  - hot-swap seguro (rollback).
- `@json-router/router`
  - forwarding loop,
  - FIB,
  - registry mínimo (si se usa),
  - HTTP mgmt.

### 4.2 Planos

- **Data-plane (Kafka):** DATA frames.
- **Control-plane (Kafka):** bootstrap, anuncios, distribución de policy.
- **Management-plane (HTTP):** configuración y observabilidad del router.

---

## 5. Kafka: topics (puertos) y convenciones

### 5.1 Concepto de “puerto = cola”
Un puerto del router se representa como un topic Kafka.  
El router “lee” de topics de entrada y “escribe” al topic de salida seleccionado por FIB.

### 5.2 Convención de topics (v1.0)

> Nota: estos nombres son canónicos; pueden parametrizarse, pero **no deben mezclar semánticas**.

- **Puertos de data (por router y dominio):**
  - Entrada al router: `data.in.<router_id>.<rd>`
  - Salida “por puerto”: `data.out.<router_id>.<port_id>.<rd>`

- **Control-plane:**
  - Bootstrap broadcast: `ctrl.bootstrap.<rd>`
  - Bootstrap reply unicast: `ctrl.bootstrap.reply.<rd>.<node_id>`
  - Anuncios/control: `ctrl.route.<rd>` (si se usa)
  - Distribución de policy: `policy.bundle`

- **DLQ (opcional v1.0, recomendado):**
  - `dlq.<router_id>.<rd>`

### 5.3 Ordering (regla obligatoria)
- Para **DATA**: `key = stream.stream_id`.

---

## 6. Trama JSON (wire format)

### 6.1 Header base (obligatorio)

```json
{
  "ver": "1.0",
  "frame_type": "DATA|CTRL|BOOT|ROUTE|POLICY",
  "msg_id": "uuid",
  "ts": "RFC3339",
  "rd": "tenant:acme",
  "hop_count": 0,
  "ttl_hops": 16,
  "src": {
    "node_id": "122EDAD2",
    "role": "AgenteAI",
    "kind": "Anfitrion",
    "tags": ["region:sa-east-1"]
  },
  "dst": {
    "addr": "rd=tenant:acme/site=arba/role=IO/kind=Whatsapp/*",
    "node_id": null,
    "service_id": null
  },
  "meta": {}
}
```

**Reglas:**
- `hop_count` incrementa en cada router; si `hop_count >= ttl_hops` → drop/DLQ.
- `rd` define el dominio: routers deben rechazar o aislar tráfico cruzado por default.
- `dst.addr` es la dirección/prefijo “IP-like” (ver sección 7).  
  Alternativamente se permite `dst.node_id` o `dst.service_id`.

### 6.2 DATA frame

```json
{
  "frame_type": "DATA",
  "stream": { "stream_id": "string", "seq": 12345 },
  "meta": { "type": "event|command|tool_call", "intent": "string", "priority": 3 },
  "payload": { }
}
```

**Orden:**
- El nodo originador es responsable de `seq` monótono por `stream_id`.
- Consumidores finales pueden deduplicar por `(stream_id, seq)` o `msg_id`.

---

## 7. Addressing “IP-like” y prefijos (router clásico)

### 7.1 Requisito
Para tener FIB tipo router se requiere una dirección con **prefijos** para agregación y match “longest prefix”.

### 7.2 Formato recomendado (jerárquico textual)
**Address = path de segmentos `key=value` con wildcard final `/*`.**

Ejemplos:
- `rd=tenant:acme/site=arba/role=IO/kind=Whatsapp/*`
- `rd=tenant:acme/site=arba/role=AgenteAI/*`

### 7.3 Canonicalización
- Convención de path estable (sin espacios).
- Wildcard solo al final: `/*`.

### 7.4 Identidad vs destino (opciones permitidas)
- `dst.addr`: routing por prefijo (“IP routing”).
- `dst.node_id`: host-route (equivalente a /32).
- `dst.service_id`: anycast/pool (selector estilo k8s; el router resuelve a node_id).

---

## 8. FIB (tabla de forwarding) y algoritmo

### 8.1 Estructura
FIB por `rd`, entradas `{ prefix, out_port_id, admin_distance, metric, enabled }`.

### 8.2 Lookup
1. Filtrar por `rd`.
2. **Longest-prefix match** sobre `dst.addr`.
3. Resolver `out_port_id`.
4. Mapear `out_port_id → topic` (`data.out.<router_id>.<port_id>.<rd>`).

### 8.3 Default route
Toda FIB debe tener ruta default por `rd` (o global) hacia DLQ/fallback.

---

## 9. Policies (approve/deny) — distribución global, ejecución local

### 9.1 Qué hace la policy (v1.0)
- Permite o deniega forwarding.
- Impone constraints (opcional).
- **NO** calcula rutas.
- **NO** cambia `dst` ni `out_port`.

### 9.2 Distribución (Kafka)
Topic: `policy.bundle`

```json
{
  "frame_type": "POLICY_BUNDLE",
  "policy": {
    "version": "2026-01-14.1",
    "format": "rego-wasm",
    "sha256": "…",
    "bytes_b64": "…"
  }
}
```

### 9.3 Runtime local
Cada router consume bundles, valida, hot-swap y hace rollback ante fallo.

### 9.4 Entrada/Salida del evaluador
Input recomendado: `{ frame_header, rd, src, dst, out_port_id }`  
Output: `{ allow, reason, constraints? }`

---

## 10. Bootstrap (config mínima por broadcast)

### 10.1 Objetivo
Descubrir routers disponibles y validar/asignar configuración mínima.

### 10.2 Frames
- `BOOT_HELLO` (nodo → `ctrl.bootstrap.<rd>`)
- `BOOT_OFFER` (router → `ctrl.bootstrap.reply.<rd>.<node_id>`)
- `BOOT_ACCEPT` (nodo → router, topic según implementación)

### 10.3 Policy en bootstrap
La policy puede aceptar/denegar nodo y validar `rd`.

---

## 11. Router: forwarding loop (comportamiento)

1. Validar schema + `ver`.
2. Validar TTL (`ttl_hops`/`hop_count`).
3. Lookup FIB → `out_port_id`.
4. Policy local → allow/deny.
5. Deny → DLQ/drop.
6. Allow → `hop_count++` y publicar al topic del puerto de salida.
7. Métricas/logs.

---

## 12. SDK común (nodos y router)

El SDK construye frames, maneja `seq`, bootstrap y Kafka transport.  
El nodo define identidad (`node_id/role/kind/tags`) y `seq` por `stream_id`.

---

## 13. Management HTTP (solo mgmt-plane)

- `GET /health`
- `GET /status`
- `GET /fib`
- `PUT /fib`
- `GET /config` / `PUT /config`
- `GET /metrics`

---

## 14. No objetivos v1.0

NAT, DHCP, OSPF/BGP completos, PKI obligatoria, orden global total.

---

## 15. Config mínima

Nodos: `KAFKA_BROKERS`, `rd` (si aplica), identidad.  
Routers: `KAFKA_BROKERS`, `router_id`, `ports`, FIB inicial, DLQ, policy bundle.

---

**Fin.**
