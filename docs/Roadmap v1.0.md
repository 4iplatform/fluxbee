# Roadmap de desarrollo **json-router v1.0** (alineado a la spec) — tareas ejecutables

**Objetivo v1.0:** implementar el router JSON “estilo hardware” con **Kafka como data-plane**, **FIB local** (longest-prefix), **policy distribuida evaluada localmente** (Rego→WASM) y **orden por stream** (stream_id + seq) preservado end-to-end.

> Referencia: `docs/json-router-spec-v1.0-es.md` (Single Source of Truth).  
> Regla: si hay conflicto entre este roadmap y la spec, **manda la spec**.

---

## Principios operativos (v1.0)
- **Single Source of Truth:** el **FrameHeader** y las convenciones de topic son contrato; cambios ⇒ bump de versión.
- **Determinismo por stream:** el router **no reordena**; Kafka `key=stream_id` + nodo asigna `seq`.
- **Policy gate, no router:** policy **solo allow/deny + constraints**; **no cambia destino**.
- **Fail closed:** si no se puede validar, rutear o evaluar policy ⇒ **DLQ** con razón y contexto mínimo.
- **Observabilidad desde el día 1:** counters por razón + health/status + (opcional) metrics.

---

## Entregables v1.0 (lista final)
1. Monorepo con paquetes versionados y CI.
2. `@json-router/protocol`: tipos + validación + canonicalización + fixtures.
3. `@json-router/transport-kafka`: I/O estable + convenciones topics.
4. `@json-router/sdk`: cliente de nodo + seq store + bootstrap v1.
5. `@json-router/router` + `apps/router-daemon`: forwarding loop + FIB + DLQ + allow-all (sin policy).
6. `@json-router/policy-runtime`: carga/eval Rego→WASM + hot-swap + rollback.
7. Management HTTP: config + FIB + status + health (+ metrics opcional).
8. Tests de integración “realistas” (docker Kafka): 1 router + 2 nodos / (opcional) 2 routers.
9. README: “cómo correr local”, “contratos”, “no-goals”, “debug y DLQ”.

---

## Fase 0 — Repo listo (0.1)
### Tareas
- Monorepo (pnpm o npm workspaces) con: `packages/`, `apps/`, `docs/`, `tooling/`.
- Config base:
  - TypeScript (`tsconfig` base + project references si aplica).
  - ESLint + Prettier (reglas mínimas + import/order).
  - Scripts estándar: `build`, `test`, `lint`, `format`.
- CI mínimo: install → build → test → lint.

### Definition of Done
- `pnpm i && pnpm -r build && pnpm -r test` pasa en limpio.
- Se publica/consume un paquete localmente (workspace) sin hacks.

---

## Fase 1 — `@json-router/protocol` (wire format + validación) (0.2)
### Tareas
- Tipos TS (contrato estable):
  - `FrameHeader` (ver, rd, src, dst, stream_id, seq, ts?, hop_count, ttl?, trace_id?).
  - `DataFrame`
  - `PolicyBundleFrame`
  - Frames de bootstrap: `BOOT.HELLO`, `BOOT.OFFER`, `BOOT.ACCEPT` (nombres según spec).
- Runtime validation:
  - zod/ajv (elegir uno) + mensajes de error **claros y mínimos**.
- Canonicalización:
  - `canonicalizeRd(rd)` (segmentos, minúsculas, separadores, etc. según spec).
  - `canonicalizeAddr(addr)` (si aplica).
- Helpers de forwarding:
  - `bumpHopCount(frame)` (y TTL decrement si existe).
  - `assertVersion(frame)` (fail fast).
- Fixtures “golden”:
  - frames válidos por tipo
  - inválidos (missing fields, ver wrong, rd invalid, etc.)

### Definition of Done
- `validateFrame()` retorna errores deterministas y legibles.
- Fixtures cubren ≥80% de casos esperables de “DLQ reasons” (schema/ttl/etc).
- Cambio de contrato rompe tests (apuesta por “contract tests”).

---

## Fase 2 — `@json-router/transport-kafka` (I/O Kafka) (0.3)
### Tareas
- Wrapper productor/consumidor:
  - `publish(topic, key, frame, headers?)`
  - `subscribe(topics, groupId, handler, opts)`
- Conventions helper:
  - `topic.in(router_id, rd)`
  - `topic.out(router_id, port_id, rd)`
  - `topic.bootstrap(rd)` / `topic.bootstrapReply(rd, node_id)`
  - `topic.policyBundle()` / `topic.dlq(router_id, rd)`
- Defaults razonables:
  - acks, retries, backoff, idempotence (si aplica), batch sizes (moderado).
- Serialización:
  - JSON stringify estable (sin canonical JSON todavía, salvo que spec lo pida).
  - (Opcional) compresión.

### Definition of Done
- Publicar/consumir `DataFrame` con key=stream_id funciona estable en Kafka local.
- Tests: mocks + al menos 1 test E2E con docker Kafka (smoke).

---

## Fase 3 — `@json-router/sdk` (cliente para nodos + orden por stream) (0.4)
### Tareas
- `SeqStore` por `stream_id`:
  - `nextSeq(stream_id)` monótono por stream
  - persistencia opcional (archivo JSON/SQLite/kv simple) con interfaz pluggable
- Cliente:
  - `sendData({dst, rd, stream_id, payload, meta})` → produce `DataFrame`
  - `onData(handler)` (si consume inbox; si no, stub)
- Bootstrap v1 (mínimo útil):
  - `sendBootHello(rd, capabilities, tags)`
  - `listenBootOffers(rd, node_id)`
  - `selectOffer(offers)` (heurística simple: menor metric o preferencia por router_id)
  - `sendBootAccept(router_id, rd)`
- Apps ejemplo:
  - `node-example-io` (envía/recibe)
  - `node-example-agent` (envía stream)

### Definition of Done
- Un nodo puede emitir un stream con `seq` correcto aunque reinicie (si persiste).
- Bootstrap permite descubrir router sin hardcode (solo brokers + rd).

---

## Fase 4 — `@json-router/router` + `apps/router-daemon` (forwarding core) (0.5)
### Tareas
- Forwarding loop:
  - consume `data.in.<router_id>.<rd>` (por RD, por defecto una)
  - valida frame (protocol)
  - aplica hop_count/ttl
  - consulta FIB (longest-prefix por `dst` o `dst.service/...` según spec)
  - forward a `data.out.<router_id>.<port_id>.<rd>`
- FIB v1:
  - estructura interna eficiente (segment trie o array con longest-prefix)
  - rutas por rd (tablas separadas o llave compuesta)
  - default route por rd
  - carga inicial desde archivo (json/yaml)
- DLQ:
  - `dlq.<router_id>.<rd>`
  - razones normalizadas (enum) + contexto mínimo (frame_header parcial, error_code)
- Observabilidad mínima:
  - counters: `in_total`, `forward_total`, `dlq_total_by_reason`, `policy_deny_total` (aunque policy sea allow-all aquí)
  - status: router_id, rd(s), commit hash, uptime, kafka lag si disponible

### Definition of Done
- E2E: node-example → router-daemon → topic out correcto.
- DLQ se llena en casos: schema invalid, ttl exceeded, no route.
- No hay “silent drop”.

---

## Fase 5 — `@json-router/policy-runtime` (Rego→WASM) + integración (0.6)
### Tareas
- Contrato de evaluación (según spec):
  - input: `{frame_header, rd, src, dst, out_port_id, router_id, tags?}`
  - output: `{allow: bool, reason: string, constraints?: object}`
- Bundle:
  - frame `PolicyBundleFrame` con `sha256`, `version`, `bytes`/`base64`, `issued_at`
  - consumo de `policy.bundle`
- Runtime:
  - validar sha256
  - cargar wasm
  - hot-swap atómico
  - rollback si falla load/verify
- Router:
  - evaluate **antes** de forward
  - deny → DLQ reason `POLICY_DENY` (con reason de policy)
- Policy ejemplo:
  - allow-all
  - ejemplo real: deny por tags / rd / dst prefix

### Definition of Done
- Un cambio de policy se refleja sin restart.
- Si el bundle está corrupto, router mantiene la policy anterior y reporta error.

---

## Fase 6 — Management HTTP (0.7)
### Tareas
- Endpoints mínimos:
  - `GET /health` (200/500)
  - `GET /status` (router_id, rds, counts, policy sha)
  - `GET /fib` / `PUT /fib` (reemplazo completo o patch; elegir y documentar)
  - `GET /config` / `PUT /config` (ttl, dlq, limits)
  - `GET /metrics` (opcional; Prometheus)
- Validación estricta de payloads (reutilizar protocol/validators).
- Persistencia de config/fib (archivo + atomic write o similar).

### Definition of Done
- Se puede operar el router sin editar código:
  - cambiar rutas
  - ajustar TTL/limits
  - ver policy activa y counters

---

## Fase 7 — Bootstrap completo + “puertos como cables” (0.8)
### Tareas
- Router responde `BOOT.OFFER` con:
  - router_id, rd, in_topic, reply_topic, metric/capacity, ports disponibles (si spec lo incluye)
- Nodo:
  - elige router (selectOffer)
  - `BOOT.ACCEPT` (commit de elección)
- Port wiring (v1):
  - definir cómo un nodo “se ata” a un port_id (si aplica)
  - tests: nodo descubre router → envía data → llega a out_port correcto

### Definition of Done
- Correr sin hardcodear router_id: solo brokers + rd.
- Con dos routers en el bus, el nodo selecciona uno determinísticamente.

---

## Fase 8 — Hardening + release v1.0 (0.9 → 1.0)
### Tareas (prioridad alta)
- Límites y seguridad operativa:
  - max frame size, max payload size
  - rate limit simple (opcional) por src.node_id o rd
- Robustez:
  - backoff/jitter para bootstrap
  - circuit breakers internos (kafka errors)
  - shutdown limpio (drain)
- (Opcional v1.0) Dedupe:
  - estrategia configurable por `(stream_id, seq)` (en router o consumer final)
- Tests de integración:
  - 1 router + 2 nodos (orden por stream preservado)
  - casos DLQ: no route, ttl, policy deny, schema invalid
  - (Opcional) 2 routers / hops: hop_count incrementa y no rompe ordering por stream
- Docs:
  - “How to run locally” (docker compose)
  - “Troubleshooting” (cómo inspeccionar topics y DLQ)
  - “Policy authoring” (disclaimer: gate-only)

### Definition of Done
- Release v1.0 usable en local con docker-compose + 2 nodos.
- Postmortem fácil: cada drop/deny tiene razón y contador.

---

## Matriz de razones DLQ (v1.0)
**Estandarizar enum** (evita caos):
- `SCHEMA_INVALID`
- `VERSION_UNSUPPORTED`
- `TTL_EXCEEDED` / `HOP_EXCEEDED` (según spec)
- `NO_ROUTE`
- `POLICY_DENY`
- `INTERNAL_ERROR` (último recurso; incluye `err_code`)

---

## Backlog post v1.0 (NO scope)
- NAT / DHCP
- OSPF/BGP “real”
- RIB dinámico (learning) más allá de estático
- Orden global total (solo por stream)
- Seguridad avanzada (authN/authZ bus, firmas) salvo que spec lo exija

---

## Checklist rápido de “no drift”
- [ ] Cada paquete tiene tests y fixtures.
- [ ] Protocol changes ⇒ bump versión + migración.
- [ ] Router nunca “droppea” sin DLQ.
- [ ] Policy jamás cambia el destino; solo decide allow/deny.
- [ ] Ordering preservado por stream_id + seq.
