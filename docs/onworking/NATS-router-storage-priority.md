# NATS + Router + SY.storage - Plan de prioridad (2026-02-16)

Objetivo: priorizar una base estable para pruebas de `SY.storage`, sin perder el rumbo hacia la spec de NATS embebido en router.

## Estado actual de codigo (baseline)

- Router:
  - Publica solo `storage.turns` (mensajes no-system/admin/query).
  - Usa `src/nats/mod.rs` (cliente TCP NATS minimo, sin JetStream/acks).
  - En `nats.mode=embedded` hoy solo prepara directorio y loguea; no levanta servidor NATS embebido real.
- SY.storage:
  - Consume `storage.turns`, `storage.events`, `storage.items`, `storage.reactivation`.
  - Reintenta suscripcion en loop cuando NATS no esta disponible.
  - Persiste en PostgreSQL con esquema base e inserciones idempotentes.

## Gap principal contra spec v1.16+

1. Falta ownership real de NATS embebido en router.
2. Falta semantica de entrega robusta (JetStream/acks/durable consumers).
3. Falta contrato completo de productores para `events/items/reactivation`.

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
- [ ] Definir y documentar payload canonico para:
  - [ ] `storage.turns`
  - [ ] `storage.events`
  - [ ] `storage.items`
  - [ ] `storage.reactivation`
- [ ] Endurecer validacion en `sy_storage` (rechazo + log estructurado por campo invalido).
- [ ] Acordar productores por subject (router/cognition/u otros).

Criterio de salida:
- Se pueden reproducir tests de ingestion con fixtures validos e invalidos y resultado determinista.

## Fase 2 - NATS embebido real en router
- [ ] Definir mecanismo de embebido (decision tecnica):
  - [ ] Opcion A: proceso `nats-server` gestionado por router/orchestrator.
  - [ ] Opcion B: server embebido en proceso Rust (si hay libreria viable y madura).
- [ ] Implementar lifecycle completo:
  - [ ] start
  - [ ] health
  - [ ] stop
  - [ ] recovery post-crash.
- [ ] Alinear config `nats.mode=embedded|client` con comportamiento real.

Criterio de salida:
- En `embedded`, el router garantiza NATS local operativo sin dependencia manual externa.

## Fase 3 - Garantias de entrega (JetStream)
- [ ] Migrar de cliente TCP minimo a cliente con soporte JetStream.
- [ ] Durable consumers para `SY.storage`.
- [ ] Ack/retry controlado + metricas de lag.

Criterio de salida:
- Reinicios de router/storage no pierden mensajes en ventana de prueba definida.

## Primer bloque sugerido para arrancar ya

1. Implementar readiness NATS en router (`rt-gateway`) y en orchestrator bootstrap.
2. Agregar smoke test de arranque:
   - `systemctl restart rt-gateway sy-storage sy-orchestrator`
   - validar logs y estado estable >= 60s.
3. Documentar payload canonico de `storage.turns` (actual) y dejar fixtures para pruebas de storage.

## Riesgos abiertos

- Si se mantiene cliente TCP minimo, no hay ack real ni durabilidad semantica de consumidor.
- Si `embedded` sigue siendo solo config sin server, pruebas dependen de NATS externo (fragil para QA).
- Hay riesgo de drift entre lo documentado en spec y lo implementado en runtime.
