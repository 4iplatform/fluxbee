# Roadmap de desarrollo (v1.0) — en tareas concretas

**Objetivo:** que puedas implementar una vez siguiendo la spec, sin re-arquitectura después.

---

## Fase 0 — Repo listo (0.1)

- Crear monorepo (pnpm o npm workspaces) con carpetas `packages/`, `apps/`, `docs/`, `tooling/`
- Agregar `docs/json-router-spec-v1.0-es.md` al repo
- Configurar lint/format (eslint + prettier) y TS config base (tsconfig references)
- Pipeline CI mínimo: install + build + test

**Deliverable:** repo compila y testea en limpio.

---

## Fase 1 — @json-router/protocol (wire format y validación) (0.2)

- Definir TypeScript types para:
  - `FrameHeader`, `DataFrame`, `PolicyBundleFrame`, `BootFrames` (HELLO/OFFER/ACCEPT)
- Definir JSON Schema para cada frame (o zod/ajv como runtime validator)
- Implementar:
  - `validateFrame(frame)` (errores claros)
  - `canonicalizeRd(rd)`, `canonicalizeAddr(addr)` (reglas simples v1)
  - helpers TTL: `bumpHopCount(frame)`
- Tests: golden fixtures de frames válidos/invalidos

**Deliverable:** contrato de trama estable y validado.

---

## Fase 2 — @json-router/transport-kafka (I/O Kafka) (0.3)

- Wrapper de producer/consumer:
  - `publish(topic, key, frame)`
  - `subscribe(topics, groupId, handler)`
- Manejo de serialización JSON + (opcional) compresión
- Config de retries/acks (defaults razonables)
- Utilidad para naming de topics (convenciones de spec)
- Tests con Kafka local (docker) o mocks + contract tests

**Deliverable:** transporte estable, sin lógica de router.

---

## Fase 3 — @json-router/sdk (cliente para nodos) (0.4)

- `SeqStore` por `stream_id`:
  - `nextSeq(stream_id)` con persistencia opcional
- `MeshClient` / `JsonRouterClient`:
  - `sendData(dst, stream_id, payload, meta)`
  - `onData(handler)`
- Bootstrap v1:
  - `sendBootHello(rd)`
  - `listenBootOffers(node_id)`
  - `selectOffer(offers)`
  - `sendBootAccept(router_id)`
- Ejemplos en `apps/node-example-*`

**Deliverable:** nodo genera frames válidos y DATA ordenado.

---

## Fase 4 — apps/router-daemon + @json-router/router (0.5)

- Router process:
  - consume `data.in.<router_id>.<rd>`
  - valida frames
  - TTL / hop_count
  - lookup FIB → `out_port_id`
  - forward a `data.out.<router_id>.<port_id>.<rd>`
- FIB v1:
  - longest-prefix match
  - default route por rd
- DLQ:
  - `dlq.<router_id>.<rd>`
- Observabilidad mínima:
  - counters por motivo
  - lag de consumer (si es posible)

**Deliverable:** forwarding end-to-end sin policy (allow-all).

---

## Fase 5 — @json-router/policy-runtime (Rego→WASM) (0.6)

- Contrato de evaluación:
  - input: `{frame_header, rd, src, dst, out_port_id}`
  - output: `{allow, reason, constraints?}`
- Consumir `policy.bundle` y hot-swap:
  - validar sha256
  - cargar wasm
  - rollback en error
- Integración en router:
  - evaluar antes de forward
  - deny → DLQ
- Policy stub (allow-all) + ejemplo real

**Deliverable:** policy gate funcional.

---

## Fase 6 — Management HTTP (0.7)

- `GET /health`, `GET /status`
- `GET /fib`, `PUT /fib`
- `GET /config`, `PUT /config`
- `GET /metrics` (opcional)

**Deliverable:** router operable sin tocar código.

---

## Fase 7 — Bootstrap completo + puertos (0.8)

- Implementar BOOT frames completos
- Registrar puerto de entrada del nodo
- Validar bootstrap con policy (opcional v1)
- Tests end-to-end de bootstrap

**Deliverable:** nodo se conecta sin hardcodear router.

---

## Fase 8 — Hardening (0.9 → 1.0)

- Dedupe opcional por `(stream_id, seq)`
- Límites:
  - tamaño máximo de frame/payload
  - rate limit básico por nodo
- Robustez:
  - backoff/jitter
  - timeouts y circuit breakers
- Tests de integración:
  - 1 router + 2 nodos
  - 2 routers + forwarding multi-hop
- Documentar “How to run locally”

**Deliverable:** release v1.0 usable.

---

## Backlog (post v1.0)

- RIB dinámico
- Multi-router avanzado
- Observabilidad distribuida
- Seguridad (authN/authZ, firmas)
