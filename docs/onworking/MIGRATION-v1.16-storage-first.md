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

## Actualizacion Iteracion 2 (PostgreSQL real)

- `SY.storage` ahora conecta a PostgreSQL y crea schema base si no existe:
  - `turns`
  - `events`
  - `memory_items`
  - indices de consulta frecuentes
- `SY.storage` ahora consume y procesa en paralelo:
  - `storage.turns`
  - `storage.events`
  - `storage.items`
  - `storage.reactivation`
- Writes idempotentes implementados:
  - `turns`: `ON CONFLICT (ctx, seq) DO NOTHING`
  - `events`: upsert por `event_id` cuando viene informado
  - `memory_items`: `ON CONFLICT (memory_id) DO NOTHING`
- Reactivation implementado:
  - refuerzo/decaimiento de `activation_strength`
  - update de `use_count`, `success_count`, `last_used_at`
- `database.url` se toma de:
  1. `FLUXBEE_DATABASE_URL`
  2. `JSR_DATABASE_URL`
  3. `island.yaml -> database.url`

## Pendiente inmediato (siguiente bloque)

1. Integrar arranque/health de `SY.storage` en servicios operativos (orchestrator/systemd).
2. Endurecer validaciones de payload de `events/items/reactivation` con contratos mas estrictos.
3. Migrar cliente NATS liviano a JetStream/acks cuando se habilite embedded server completo.
4. Reemplazar seqlock -> epoch/RCU (fase siguiente de infraestructura).

## Riesgos conocidos

- El router actualmente publica sobre `storage.turns`; los subjects de `events/items/reactivation` quedan listos para consumo cuando SY.cognition los emita.
- NATS embebido real (server in-process) no esta implementado aun; se asume endpoint NATS operativo segun config.

## Actualizacion Iteracion 3 (Orchestrator + install)

- `SY.orchestrator` incorpora `sy-storage` en lifecycle de motherbee:
  - bootstrap local
  - watchdog de servicios criticos
  - shutdown ordenado
- `add_island` ajustado para workers:
  - ya no instala ni arranca `sy-orchestrator` remoto
  - instala units worker (`rt-gateway`, `sy-config-routes`, `sy-opa-rules`, `sy-identity` opcional)
  - corrige path remoto de config a `/etc/fluxbee/sy-config-routes.yaml`
  - corrige state path remoto a `/var/lib/fluxbee/state/nodes`
- `scripts/install.sh` alineado:
  - instala `sy-storage` en `/usr/bin/sy-storage`
  - crea `sy-storage.service`
  - prepara layout primario `fluxbee` y mantiene layout legacy temporal.

## Actualizacion Iteracion 4 (hardening orchestrator)

- `add_island` corrige bootstrap SSH por clave:
  - `authorized_keys` ahora se configura en el usuario de bootstrap (`administrator`) para que `ssh/scp -i` funcionen de forma consistente.
- `SY.identity` queda en modo opcional temporal en orchestrator:
  - se inicia/watchdog/shutdown solo si existe `/usr/bin/sy-identity` (ctx pendiente fuera de este bloque).
- `scripts/install.sh` valida ejecutables requeridos para worker:
  - `sy-opa-rules` pasa a ser requerido y se instala de forma explicita en `/usr/bin/sy-opa-rules`.
