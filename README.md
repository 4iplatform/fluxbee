# json-router

Router de mensajes **JSON** con modelo mental de **router de hardware**:
- **puertos = colas/topics Kafka**
- **FIB local** (tabla de forwarding) con match por prefijo (longest-prefix / jerárquico)
- **policy approve/deny** distribuida globalmente y **evaluada localmente** en el router (Rego→WASM)
- **orden por stream**: `stream_id` + `seq` asignado por el nodo; Kafka `key=stream_id`

> Spec completa: `docs/json-router-spec-v1.0-es.md`

---

## Estado
**v1.0** — alcance cerrado para implementación.

---

## Features v1.0 (mínimo viable)
- Tramas JSON con header estándar
- Kafka como data-plane
- Router: consume por “puerto” (topic), aplica FIB + policy, reenvía por puerto de salida
- Management HTTP para cargar/ver FIB y config
- SDK común para nodos y router
- Distribución de policy bundle por Kafka (`policy.bundle`) y hot-swap en routers

**No v1.0:** NAT, DHCP, OSPF/BGP completos, orden global total (solo por `stream_id`).

---

## Monorepo
```
packages/
  protocol/            # tipos + validación de frames + canonicalización
  transport-kafka/     # wrapper Kafka producer/consumer + conventions
  sdk/                 # cliente para nodos (bootstrap, send/recv, seq)
  policy-runtime/      # carga/eval bundle (rego-wasm) + hot-swap
  router/              # forwarding loop + FIB + mgmt HTTP
apps/
  router-daemon/
  node-example-io/
  node-example-agent/
docs/
  json-router-spec-v1.0-es.md
```

---

## Requisitos
- Node.js 20+
- Docker (recomendado) para levantar Kafka local
- (Opcional) `pnpm` o `npm` (elegir uno)

---

## Quick start (dev)

### 1) Levantar Kafka local (docker-compose)
Crear `docker-compose.yml` (ejemplo mínimo) o usar el que esté en `tooling/`.

Luego:
```bash
docker compose up -d
```

### 2) Instalar dependencias
Con pnpm:
```bash
pnpm install
```
o con npm:
```bash
npm install
```

### 3) Ejecutar router
```bash
# ejemplo (ajustar según package scripts)
pnpm --filter router-daemon dev
```

### 4) Ejecutar un nodo de ejemplo
```bash
pnpm --filter node-example-io dev
```

---

## Variables de entorno (mínimas)

### Router
- `KAFKA_BROKERS` (ej: localhost:9092)
- `ROUTER_ID` (ej: rtr-01)
- `RD_DEFAULT` (ej: tenant:acme)
- `MGMT_PORT` (ej: 8080)
- `FIB_CONFIG` (path o inline JSON/YAML, según implementación)
- `DLQ_TOPIC` (opcional)

### Nodo
- `KAFKA_BROKERS`
- `NODE_ID` (si no se setea, se genera y se debe persistir)
- `RD` (si no se asigna en bootstrap/policy)
- `ROLE`, `KIND`, `TAGS` (ej: `TAGS=region:sa-east-1,pii_ok`)

---

## Topics (convención v1.0)
- Entrada router: `data.in.<router_id>.<rd>`
- Salida por puerto: `data.out.<router_id>.<port_id>.<rd>`
- Bootstrap broadcast: `ctrl.bootstrap.<rd>`
- Bootstrap reply: `ctrl.bootstrap.reply.<rd>.<node_id>`
- Distribución policy: `policy.bundle`
- DLQ: `dlq.<router_id>.<rd>` (recomendado)

---

## API management (router)
- `GET /health`
- `GET /status`
- `GET /fib`
- `PUT /fib` (cargar/actualizar rutas)
- `GET /config`
- `PUT /config`
- `GET /metrics` (si aplica)

---

## Contrato de ordering
- El nodo asigna `stream.seq` monótono por `stream.stream_id`
- Kafka produce DATA con `key = stream.stream_id`
- El router no reordena, solo preserva header y aumenta `hop_count`

---

## Policy (Rego→WASM)
- Autoría “single source of truth”
- Distribución: publicar `POLICY_BUNDLE` en `policy.bundle`
- Routers:
  - validan `sha256`
  - hot-swap
  - rollback si falla
- La policy **NO rutea** y **NO cambia destino**: solo `allow/deny` + constraints

---

## Contribución / reglas internas
- No romper el header del frame (`ver=1.0`) sin bump de versión
- Cambios en `protocol` deben venir con tests de compatibilidad
- No meter features fuera de scope v1.0 sin RFC en `docs/`
