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

## Fase 3 - Garantias de entrega (JetStream)
- [ ] Migrar de cliente TCP minimo a cliente con soporte JetStream.
- [ ] Durable consumers para `SY.storage`.
- [ ] Ack/retry controlado + metricas de lag.

Criterio de salida:
- Reinicios de router/storage no pierden mensajes en ventana de prueba definida.

## Primer bloque sugerido para arrancar ya

1. Cerrar lifecycle faltante del broker embebido (stop/recovery) y agregar smoke test de restart.
2. Activar productor real de `storage.events/items/reactivation` (SY.cognition) para validar pipeline E2E completo.
3. Preparar migracion de cliente NATS minimo a cliente con soporte de durabilidad (siguiente fase).

Avance:
- Se agrego smoke de lifecycle para NATS embebido: `scripts/nats_embedded_lifecycle_smoke.sh`.
- Este smoke valida `restart` y (opcional) `stop/start` del `rt-gateway` con chequeo del endpoint NATS.

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
