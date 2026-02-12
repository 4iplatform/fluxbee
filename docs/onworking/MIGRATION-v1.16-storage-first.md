# Fluxbee - Plan de migracion v1.16+ (storage-first)

Fecha: 2026-02-12

## Decisiones acordadas

1. Roles canonicos: `motherbee` y `worker`.
2. Nombre consolidado: `fluxbee` (manteniendo compatibilidad temporal con rutas legacy `json-router`).
3. No iniciar por `ctx`/`memory_package` ni cambios cognitivos.
4. Prioridad inicial: base de `NATS + SY.storage`.

## Objetivo de esta fase

Entregar una base funcional y estable para desacoplar el router de persistencia:

- Router publica eventos de turns a NATS.
- SY.storage consume de NATS y persiste localmente (fase 1).
- La parte PostgreSQL se agrega en la siguiente iteracion.

## Cambios implementados en este bloque

- Paths primarios migrados a `fluxbee` con fallback automatico a legacy:
  - `/etc/fluxbee`
  - `/var/lib/fluxbee`
  - `/var/run/fluxbee`
- `SY.admin` y `SY.orchestrator` validados para `role=motherbee` (aceptando `mother` solo como compatibilidad transitoria).
- `add_island` actualizado para generar worker con:
  - `role: worker`
  - bloque `nats` embebido.
- `RouterConfig` extendido con configuracion NATS:
  - `nats.mode`
  - `nats.port`
  - `nats.url`
  - `nats.storage_dir`
- Router inicia base NATS (configuracion/directorios) y publica mensajes no-sistema a subject `storage.turns`.
- Nuevo modulo `src/nats/mod.rs` (cliente NATS liviano por TCP para `PUB`/`SUB`).
- Nuevo binario `SY.storage` (`src/bin/sy_storage.rs`) que:
  - corre solo en motherbee,
  - consume `storage.turns`,
  - persiste en `turns.ndjson` (fase 1).

## Pendiente inmediato (siguiente bloque)

1. `SY.storage`: persistencia PostgreSQL real (writer unico).
2. Retry/backoff mas robusto y manejo de errores NATS (telemetria).
3. Streams/sujetos adicionales (`storage.events`, `storage.items`, `storage.reactivation`).
4. Integrar arranque/health de `SY.storage` en servicios operativos.
5. Reemplazar seqlock -> epoch/RCU (fase siguiente de infraestructura).

## Riesgos conocidos

- En esta fase, `SY.storage` escribe a NDJSON (no DB aun).
- NATS embebido real (server in-process) no esta implementado en este bloque; se asume endpoint NATS operativo segun config.
