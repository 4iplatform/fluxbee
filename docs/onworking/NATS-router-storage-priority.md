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
- [ ] Acordar productores por subject (router/cognition/u otros).

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
  - [ ] stop
  - [ ] recovery post-crash.
- [x] Alinear config `nats.mode=embedded|client` con comportamiento real.

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
2. Acordar y alinear productores de `storage.events/items/reactivation` al contrato canonico.
3. Preparar migracion de cliente NATS minimo a cliente con soporte de durabilidad (siguiente fase).

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
