# NATS + Router + SY.storage - Plan de prioridad (2026-02-16)

Objetivo: priorizar una base estable para pruebas de `SY.storage`, sin perder el rumbo hacia la spec de NATS embebido en router.

## Estado actual de codigo (baseline)

- Router:
  - Publica solo `storage.turns` (mensajes no-system/admin/query).
  - Usa `src/nats/mod.rs` (cliente TCP NATS minimo, sin JetStream/acks).
  - En `nats.mode=embedded` ahora levanta broker NATS minimo en el propio proceso del router (sin `nats-server` externo).
- SY.storage:
  - Consume `storage.turns`, `storage.events`, `storage.items`, `storage.reactivation`.
  - Reintenta suscripcion en loop cuando NATS no esta disponible.
  - Persiste en PostgreSQL con esquema base e inserciones idempotentes.

## Revision spec vs codigo (2026-02-19)

Resumen contra `docs/13-storage.md` y estado real del repo:

- Alineado:
  - Router con `nats.mode=embedded` levanta broker in-process (`start_embedded_broker`).
  - Readiness de NATS en router y orchestrator implementada.
  - `SY.storage` consume los 4 subjects y aplica validacion/ingesta robusta.
- Parcial:
  - Lifecycle embebido:
    - `start`: implementado.
    - `health`: implementado.
    - `stop/recovery` explicito: pendiente de contrato formal (hoy depende del lifecycle del proceso/router + systemd).
- No alineado todavia con spec objetivo:
  - No hay JetStream/durable consumers/acks.
  - No hay garantias de entrega at-least-once en reinicios.
  - Productores de `storage.events/items/reactivation` no estan activos end-to-end en runtime (aunque el contrato de `SY.storage` ya esta listo).

## Gap principal contra spec v1.16+

1. Falta semantica de entrega robusta (JetStream/acks/durable consumers).
2. Falta contrato completo de productores para `events/items/reactivation`.
3. Falta completar lifecycle de NATS embebido para shutdown/recovery explicito.

## Prioridad recomendada (orden de ejecucion)

## Fase 0 - Estabilizacion para pruebas (inmediata)
- [x] Router: readiness explicito de endpoint NATS al inicio (falla clara si no conecta).
- [x] Orchestrator: readiness explicito de NATS + `sy-storage` activo en bootstrap motherbee.
- [x] Observabilidad minima:
  - [x] logs por subject (publish/subscribe failures)
  - [x] contador simple de errores NATS en router y storage.

Criterio de salida:
- Con `nats://127.0.0.1:4222` disponible, `rt-gateway + sy-storage` levantan y quedan estables.
- Si NATS no esta disponible, error explicito y accionable en logs.

Nota actual:
- El chequeo de `sy-storage` valida estado `systemd active`; queda pendiente chequeo explicito de conexion DB al bootstrap.

## Fase 1 - Contrato de datos de storage
- [x] Definir y documentar payload canonico para:
  - [x] `storage.turns`
  - [x] `storage.events`
  - [x] `storage.items`
  - [x] `storage.reactivation`
- [x] Endurecer validacion en `sy_storage` (rechazo + log estructurado por campo invalido).
- [x] Acordar productores por subject (router/cognition/u otros).
  - `storage.turns`: `rt-gateway` (publicacion actual implementada).
  - `storage.events`: `SY.cognition` (productor objetivo; `SY.storage` ya preparado para consumir).
  - `storage.items`: `SY.cognition` (productor objetivo; `SY.storage` ya preparado para consumir).
  - `storage.reactivation`: `SY.cognition` (productor objetivo; `SY.storage` ya preparado para consumir).
  - Nota operativa: hasta cerrar el pipeline cognitivo/WAN completo, el unico flujo E2E validado es `storage.turns`.

Criterio de salida:
- Se pueden reproducir tests de ingestion con fixtures validos e invalidos y resultado determinista.

Avance de implementacion:
- Contrato canonico documentado en `docs/onworking/storage_subject_contract.md`.
- Validacion endurecida aplicada en parser de `SY.storage` para events/items/reactivation y campos minimos de turns.

## Fase 2 - NATS embebido real en router
- [x] Definir mecanismo de embebido (decision tecnica):
  - [ ] Opcion A: proceso `nats-server` gestionado por router/orchestrator.
  - [x] Opcion B: broker NATS minimo embebido en proceso Rust del router.
- [ ] Implementar lifecycle completo:
  - [x] start
  - [x] health
  - [x] stop
  - [x] recovery post-crash.
- [x] Alinear config `nats.mode=embedded|client` con comportamiento real.

Avance Fase 2 (2026-02-19):
- `stop` explicito implementado en broker embebido (`stop_embedded_broker`) y conectado a `SIGTERM/SIGINT` de `rt-gateway`.
- Recovery post-crash/end-point down: loop de autorecuperacion en router (`embedded`), con health-check periodico + restart del broker cuando cae.
- Recovery de estado inconsistente: `start_embedded_broker` ahora recupera instancia registrada no saludable.

Criterio de salida:
- En `embedded`, el router garantiza NATS local operativo sin dependencia manual externa.

## Fase 3A - Infra de entrega (JetStream, independiente del contrato final)
- [x] Migrar de cliente TCP minimo a base JetStream embebida (work-queue durable sobre broker in-process actual).
- [x] Streams + consumers durables para subjects de storage.
- [x] Ack/retry controlado + metricas de lag.
- [x] Ack/retry basico en broker embebido actual (pre-JetStream):
  - [x] `reply-to` de ack por mensaje y ack automatico post-handler exitoso en subscriber.
  - [x] Redelivery en broker cuando no llega ack dentro de timeout.
  - [x] Test unitario de redelivery por falta de ack.
- [x] Observabilidad operativa de lag/reintentos (pre-JetStream):
  - [x] loop periodico en `SY.storage` con metricas de `storage_inbox` (pendientes, pendientes con error, edad del mas viejo).
  - [x] warning por umbral de backlog/edad para deteccion temprana.
  - [x] exposicion de contadores acumulados (`nats_subscribe_failures`, `storage_handler_failures`) en logs de metricas.
  - [x] endpoint admin `GET /config/storage/metrics` para consultar backlog/edad de inbox via API.
    - `SY.admin` ya no consulta PostgreSQL directo para este endpoint; usa request/reply NATS (`storage.metrics.get`) y `SY.storage` responde como gateway de DB.
  - [x] hardening de request/reply para metricas:
    - `jsr_client::nats::publish` ahora sincroniza con broker (`PING/PONG`) antes de cerrar socket en conexiones cortas.
    - evita timeouts intermitentes donde `SY.storage` procesaba en ~1-2ms pero `SY.admin` no recibia reply.
    - trazas finas quedan disponibles en nivel `debug` (`jsr_client::nats`, `sy_admin`, `sy_storage`) para diagnostico puntual sin ruido operativo por defecto.
- [x] Base de ack post-persistencia en `SY.storage` (sin JetStream aun):
  - [x] `storage_inbox` durable en PostgreSQL para registrar mensajes recibidos.
  - [x] Replay automatico de pendientes al bootstrap de `SY.storage`.
  - [x] Dedupe por mensaje (`dedupe_key`) y guardado de `last_error`.
  - [x] Dedupe de `storage.reactivation` por `(dedupe_key, event_id)` para evitar doble aplicacion en retries.
- [x] Reforzar idempotencia de ingesta para tolerar redeliveries:
  - [x] `storage.events` sin `event_id` ahora hace upsert por clave natural (`ctx/start_seq/end_seq/boundary_reason`) en `SY.storage`.

Criterio de salida:
- Reinicios de router/storage no pierden mensajes en ventana de prueba definida.
- Reintentos/redeliveries no generan duplicados no deseados en tablas de storage.
- Nota: se implemento durabilidad base en broker embebido; queda pendiente alineacion completa al protocolo JetStream oficial.

Avance JetStream-base (2026-02-19):
- Broker embebido persiste estado durable por endpoint en `nats.storage_dir` (`embedded-js-<endpoint>.json`):
  - stream por subject (storage.*) con secuencia monotona.
  - consumer durable por `SUB` con queue `durable.*` (ej. `durable.sy-storage.turns`).
  - cursor `acked_seq` persistente y replay post-restart.
- `SY.storage` consume `storage.turns/events/items/reactivation` con durable queues en `embedded`.
- Ack explicito post-handler exitoso mantiene semantica at-least-once y replay en reconnect/restart.
- Test unitario agregado para replay durable cross-restart del broker.

Incidencia operativa cerrada (2026-02-19, storage metrics):
- Sintoma: `GET /config/storage/metrics` podia fallar con timeout de 8s en varios retries, aunque el siguiente intento respondia en milisegundos.
- Causa: falta de sincronizacion en `publish` de cliente NATS sobre sockets efimeros (publish-and-close), con perdida intermitente de frame en el cierre.
- Correccion aplicada: sync `PING/PONG` post-`PUB` en `jsr_client::nats::publish`.
- Alcance:
  - afecta camino NATS del cliente `jsr_client` (admin/storage en este flujo).
  - no modifica el modulo NATS del router (`src/nats/mod.rs`), ni cambia contratos de subjects.

## Checklist de cierre - Libreria cliente (socket + NATS)

Regla de arquitectura objetivo:
- todo trafico entre nodos pasa por router local (socket y broker NATS embebido del `rt-gateway` local), nunca proceso-a-proceso directo.

Estado y pendientes:
- [x] Exponer modulo NATS compartido en `jsr_client` (`publish`, `request`, `NatsSubscriber`).
- [x] Migrar `SY.admin` storage metrics a request/reply NATS (`storage.metrics.get`) via `SY.storage`.
- [x] Definir API strict router-local en libreria (`request_local`, `publish_local`, `subscribe_local`) para no depender de endpoint raw pasado por caller.
- [x] Resolver `NATS endpoint` local desde config del nodo (`hive.yaml`) dentro de la libreria (alineado a discovery de socket local).
- [x] Rechazar endpoints no locales en modo strict (evitar bypass de router).
- [x] Unificar config de cliente para socket + nats en una sola estructura de libreria.
- [x] Cliente NATS persistente con reconnect/backoff (evitar connect por operacion).
- [x] Reescribir `request` NATS para usar una sola conexion por request/reply (sin `request -> publish` en segundo socket).
- [x] Mantener inbox/reply subjects en sesion persistente (multiplexado), evitando handshake TCP por cada operacion local.
- [ ] Objetivo operativo de este bloque: eliminar jitter/picos locales observados (~40ms) por connect/close repetido.
- [x] Auto-resubscribe robusto para subscribers tras caida/restart de broker.
- [x] Correlacion request/reply robusta (inbox por sesion + `trace_id`).
- [x] Envelope comun de mensajes (versionado/meta minima) para callers NATS.
- [x] Metricas del cliente NATS (timeouts, reconnects, in-flight, last error).
- [x] Tests de integracion de libreria contra broker embebido (restart, reconnect, req/reply, sub).
- [ ] Migrar nodos callers a API strict (evitar uso directo de endpoint string).
- [ ] Documentar quickstart de nodo nuevo (`AI.test`) usando libreria para socket + NATS.

Nota de estado:
- La API strict ya existe en `jsr_client::nats`; queda pendiente migrar callers (`SY.*`/otros nodos) para eliminar uso de endpoint string directo.
- `jsr_client::nats::request` ya publica el request en el mismo socket donde espera el reply (se elimino el segundo socket interno de `publish`).
- `SY.admin` (`/config/storage/metrics`) ya usa `jsr_client::nats::NatsClient` persistente con reconnect/backoff.
- `SY.admin` storage metrics ahora usa `reply_subject` por request sobre inbox de sesion (`_INBOX.JSR.<session>.<trace_id>`), evitando colisiones entre procesos.
- `jsr_client::nats::NatsSubscriber` ahora incluye `run_with_reconnect` (backoff exponencial) para re-suscribir automaticamente tras caidas/restarts del broker.
- `jsr_client::nats::NatsClient` ahora genera inbox de sesion (`_INBOX.JSR.<session>.*`) y correlaciona requests con `reply_subject` por `trace_id` dentro de esa sesion compartida.
- `jsr_client::nats::NatsClient` expone `metrics_snapshot()` con `timeouts/reconnects/in_flight/last_error` para observabilidad del caller.

## Cierre admin - prueba recomendada (estado actual)

Smoke recomendado:

```bash
BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" bash scripts/admin_nodes_routers_storage_e2e.sh
```

Este E2E ahora cubre tambien:
- `GET /config/storage/metrics` (camino `SY.admin -> NATS -> SY.storage -> DB`).

Chequeo puntual rapido:

```bash
curl -sS http://127.0.0.1:8080/config/storage/metrics
```

## Fase 3B - Cierre de contrato (cuando modelo cognitivo quede congelado)
- [ ] Versionado de payload (`schema_version`) y politica de compatibilidad.
- [ ] Validaciones estrictas de campos finales por subject.
- [ ] Ajuste final de esquema/indices/constraints segun contrato definitivo.

Criterio de salida:
- Contrato de producers y storage cerrado, con compatibilidad y migracion definidas.

## Proximo bloque sugerido (orden de ejecucion)

1. Implementar JetStream base (streams + durable consumers + ack explicito post-persistencia) sin depender del cierre final de payload.
2. Activar productor real de `storage.events/items/reactivation` (SY.cognition) para validar pipeline E2E completo sobre infraestructura durable.
3. Cerrar contrato final (Fase 3B) con versionado y validaciones estrictas.

Avance:
- Se agrego smoke de lifecycle para NATS embebido: `scripts/nats_embedded_lifecycle_smoke.sh`.
- Este smoke valida `restart` y (opcional) `stop/start` del `rt-gateway` con chequeo del endpoint NATS.
- Se agrego `scripts/nats_client_transport_e2e.sh` para validar request/reply (`SY.admin -> NATS -> SY.storage`) en escenarios de restart/reconnect.
- Se agrego `scripts/nats_full_suite.sh` para ejecutar en orden lifecycle + transport + admin E2E en una sola corrida.

## Riesgos abiertos

- El broker embebido actual es minimo y no cubre semantica JetStream.
- Sin ack/durable consumer todavia puede haber perdida de mensajes ante reinicios.
- Hay riesgo de drift entre lo documentado en spec y lo implementado en runtime si no se cierran pruebas E2E de subjects.

## Smoke E2E (storage.turns)

Para validar rapidamente el camino `NATS -> SY.storage -> PostgreSQL` con ciclo completo `write -> read -> delete`:

```bash
sudo bash scripts/storage_smoke.sh
```

Opciones utiles:

```bash
sudo HIVE_CONFIG=/etc/fluxbee/hive.yaml SMOKE_TIMEOUT_SECS=20 bash scripts/storage_smoke.sh
sudo DB_URL='postgresql://fluxbee:magicAI@127.0.0.1:5432/fluxbee' NATS_URL='nats://127.0.0.1:4222' bash scripts/storage_smoke.sh
```

Salida esperada:
- `OK: insert observed in turns ...`
- `OK: read observed in turns`
- `OK: delete observed and verified ...`

Si queres conservar la fila para inspeccion manual:

```bash
sudo SMOKE_KEEP_ROW=1 bash scripts/storage_smoke.sh
```

## Smoke E2E (storage.events/items/reactivation)

Para validar ingestion completa de subjects de storage (fixtures validos):

```bash
sudo bash scripts/storage_subjects_e2e.sh
```

Opciones utiles:

```bash
sudo HIVE_CONFIG=/etc/fluxbee/hive.yaml SMOKE_TIMEOUT_SECS=30 bash scripts/storage_subjects_e2e.sh
sudo DB_URL='postgresql://fluxbee:magicAI@127.0.0.1:5432/fluxbee' NATS_URL='nats://127.0.0.1:4222' bash scripts/storage_subjects_e2e.sh
```

Salida esperada:
- `OK: event inserted in events table`
- `OK: item inserted in memory_items table`
- `OK: reactivation applied in events table`
- `OK: cleanup verified (...)`

Si queres conservar filas para inspeccion manual:

```bash
sudo SMOKE_KEEP_ROWS=1 bash scripts/storage_subjects_e2e.sh
```

## Suite E2E (storage.turns + storage subjects)

Para correr ambos tests de ingestion en una sola pasada:

```bash
sudo bash scripts/storage_ingestion_suite.sh
```

Opciones utiles:

```bash
sudo HIVE_CONFIG=/etc/fluxbee/hive.yaml SMOKE_TIMEOUT_SECS=30 bash scripts/storage_ingestion_suite.sh
sudo DB_URL='postgresql://fluxbee:magicAI@127.0.0.1:5432/fluxbee' NATS_URL='nats://127.0.0.1:4222' bash scripts/storage_ingestion_suite.sh
```

## Smoke lifecycle NATS embebido (router)

Para validar rapido el lifecycle de NATS embebido atado al router:

```bash
bash scripts/nats_embedded_lifecycle_smoke.sh
```

Opciones utiles:

```bash
SERVICE=rt-gateway NATS_URL=nats://127.0.0.1:4222 TIMEOUT_SECS=30 bash scripts/nats_embedded_lifecycle_smoke.sh
CHECK_STOP_START=0 bash scripts/nats_embedded_lifecycle_smoke.sh
```

## E2E transporte NATS (cliente/libreria)

Para validar reconnect/request-reply/resubscribe en camino `SY.admin -> NATS -> SY.storage`:

```bash
BASE="http://127.0.0.1:8080" bash scripts/nats_client_transport_e2e.sh
```

Opciones utiles:

```bash
BASE="http://127.0.0.1:8080" METRICS_TIMEOUT_SECS=60 POLL_INTERVAL_SECS=2 bash scripts/nats_client_transport_e2e.sh
RUN_ROUTER_STOP_START=0 RUN_STORAGE_RESTART=0 bash scripts/nats_client_transport_e2e.sh
```

## Suite NATS completa (lifecycle + transporte + admin)

Para ejecutar todo junto en una sola corrida:

```bash
BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" bash scripts/nats_full_suite.sh
```
