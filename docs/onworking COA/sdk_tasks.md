# Fluxbee SDK - Backlog de migración (`jsr_client` -> `fluxbee_sdk`)

Estado objetivo:
- `fluxbee_sdk` como librería única para nodos (comunicación + blob + futuras herramientas).
- `jsr_client` retirado del workspace principal (migración cerrada).

Regla operativa acordada:
- Todo cambio nuevo se valida compilando y ejecutando por `fluxbee_sdk`.

## Fase M1 - Base de compatibilidad

- [x] M1. Crear crate `fluxbee_sdk`.
- [x] M2. Integrar módulos de comunicación dentro de `fluxbee_sdk` (duplicado de `jsr_client`).
- [x] M3. Exponer API de comunicación desde `fluxbee_sdk` (`connect`, `NodeConfig`, `nats`, `protocol`, etc.).

## Fase M2 - Adopción en repo principal

- [x] M4. Migrar imports del repo principal de `jsr_client` a `fluxbee_sdk`.
- [x] M5. Compilar workspace usando `fluxbee_sdk` como dependencia principal.
- [x] M6. Ajustar scripts/diag para nomenclatura neutral (`jsr_client` -> `fluxbee_sdk`) sin romper compatibilidad temporal.

## Fase M3 - Gate de desarrollo

- [x] M7. Definir gate CI/local obligatorio: `cargo check` + pruebas diag con ruta `fluxbee_sdk` (`scripts/sdk_gate.sh`).
- [x] M8. Prohibir nuevos usos directos de `jsr_client` en código productivo (`scripts/check_no_jsr_client_usage.sh` integrado en `scripts/sdk_gate.sh`).
- [x] M9. Agregar aviso de deprecación en `jsr_client` (README/comentarios/docstring).

## Fase M4 - Integración de blob en SDK

- [x] M10. Implementar blob module canónico de `fluxbee_sdk` según `docs/blob-annex-spec.md`.
- [x] M11. Integrar contrato `text/v1` y utilidades de attachments en `fluxbee_sdk`.
- [x] M12. Validar E2E de comunicación + blob usando bins/scripts que ya consumen `fluxbee_sdk`.

## Fase M5 - Retiro de `jsr_client`

- [x] M13. Eliminar dependencias remanentes de `jsr_client`.
- [x] M14. Retirar crate `crates/jsr_client` del workspace.
- [x] M15. Actualizar documentación final de migración (spec + onworking + changelog).

## Pendientes reales para cerrar migración

- Migración SDK cerrada.
- `fluxbee_sdk` queda como único SDK soportado para nuevos desarrollos.

## Backlog explícito - `fluxbee-go-sdk`

- [x] GO-SDK-1. Formalizar `fluxbee-go-sdk` como módulo top-level reutilizable por nodos first-party y terceros.
- [x] GO-SDK-2. Portar envelope, handshake, framing y lifecycle base de conexión al router.
- [x] GO-SDK-3. Portar helpers canónicos de `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE` y `NODE_STATUS_GET`.
- [x] GO-SDK-4. Portar helpers RPC `system` y tipos reutilizables de `HELP`.
- [x] GO-SDK-5. Agregar resolución de identidad `uuid -> L2` vía router SHM local.
- [x] GO-SDK-6. Corregir la resolución de router para no depender de `gateway_name`:
  - persistir `ANNOUNCE.router_name` en el estado de conexión
  - exponerlo en `NodeSender` / `NodeReceiver`
  - usar ese router efectivo para abrir `state/<router_l2_name>/identity.yaml` y su SHM
- [ ] GO-SDK-7. Revisar si conviene extender `ANNOUNCE` con `shm_name` explícito para evitar lectura adicional de `identity.yaml`.
- [x] GO-SDK-8. Revisar el modelo de múltiples routers por hive y congelar semántica para consumidores SDK:
  - un nodo puede conectarse a cualquier router local
  - la router SHM sigue siendo per-router, no una vista fusionada por hive
  - documentar esto como contrato explícito
- [ ] GO-SDK-9. Portar readers SHM adicionales que hoy siguen faltando respecto del runtime Rust, empezando por los casos con valor real para nodos Go.
- [x] GO-SDK-10. Revisar el surface público del SDK Go y estabilizar política de versionado/compatibilidad para terceros.

Estado actual del cierre del SDK Go:

- la semántica de múltiples routers por hive quedó congelada en el README del SDK:
  - el router efectivo es el informado por `ANNOUNCE.router_name`
  - la SHM usada para `uuid -> L2` es la de ese router efectivo
  - la SHM del router no es una vista fusionada por hive
- la política de compatibilidad v1 también quedó fijada en el README:
  - fixes patch-level backward compatible
  - adiciones permitidas sin romper callers
  - cambios wire-breaking solo con spec + fixtures + migración coordinada
- el surface público estable para v1 quedó explícito:
  - lifecycle
  - envelope/protocol
  - RPC helpers
  - control-plane helpers
  - `HELP`
  - `SY.timer`

## Backlog explícito - `fluxbee_sdk` (Rust) para `SY.timer`

Objetivo:
- llevar al SDK Rust la misma capacidad funcional de integración con `SY.timer` que hoy existe en `fluxbee-go-sdk`
- evitar que `SY.timer` quede como una capacidad “solo Go”
- dejar un surface tipado y reutilizable para `WF.*`, `AI.*`, `IO.*` y futuros nodos Rust

Decisiones ya tomadas:
- `SY.orchestrator` no adopta `SY.timer` internamente en v1 para su control-plane base
- este trabajo es **paridad de SDK**, no un cambio de arquitectura del orchestrator
- la fuente autoritativa sigue siendo el contrato wire del nodo `SY.timer` documentado en `docs/sy-timer.md`

### RUST-TIMER-SDK - Base de contrato y surface público

- [x] RUST-TIMER-SDK-1. Crear módulo canónico `timer` dentro de `crates/fluxbee_sdk/src/`.
- [x] RUST-TIMER-SDK-2. Exponerlo desde [lib.rs](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/lib.rs) y [prelude.rs](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/prelude.rs).
- [x] RUST-TIMER-SDK-3. Congelar constantes de wire:
  - `TIMER_HELP`
  - `TIMER_NOW`
  - `TIMER_NOW_IN`
  - `TIMER_CONVERT`
  - `TIMER_PARSE`
  - `TIMER_FORMAT`
  - `TIMER_SCHEDULE`
  - `TIMER_SCHEDULE_RECURRING`
  - `TIMER_GET`
  - `TIMER_LIST`
  - `TIMER_CANCEL`
  - `TIMER_RESCHEDULE`
  - `TIMER_PURGE_OWNER`
  - `TIMER_FIRED`
  - `TIMER_RESPONSE`
- [x] RUST-TIMER-SDK-4. Definir tipos Rust serializables/deserializables para:
  - `TimerId`
  - `TimerInfo`
  - `FiredEvent`
  - `MissedPolicy`
  - `TimerListFilter`
  - `TimerHelpDescriptor`
  - request/response payloads por verbo
- [x] RUST-TIMER-SDK-5. Definir un error canónico del cliente (`TimerClientError`) que separe:
  - error de transporte
  - timeout / unreachable
  - respuesta de servicio con `code/message`
  - payload inválido / contrato roto

### RUST-TIMER-SDK - Helpers de transporte

- [x] RUST-TIMER-SDK-6. Implementar builder helpers para requests `system` dirigidos a `SY.timer@<hive>`.
- [x] RUST-TIMER-SDK-7. Implementar parser helpers para `TIMER_RESPONSE`.
- [x] RUST-TIMER-SDK-8. Implementar parser helper para `TIMER_FIRED` recibido como evento `system`.
- [x] RUST-TIMER-SDK-9. Reusar la normalización/correlación existente del SDK Rust en lugar de duplicar lógica de envelope.
- [x] RUST-TIMER-SDK-10. Resolver naming del target timer desde `hive_id` con helper canónico, sin hardcodes dispersos.

Estado actual del módulo Rust `timer`:

- nuevo archivo canónico: [timer.rs](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/timer.rs)
- exports ya disponibles desde [lib.rs](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/lib.rs) y [prelude.rs](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/prelude.rs)
- contrato base ya cubierto:
  - constantes `TIMER_*`
  - enums (`TimerKind`, `TimerStatus`, `TimerStatusFilter`, `MissedPolicy`)
  - ids / info / fired event
  - payloads de request/response por verbo
  - `TimerHelpDescriptor`
  - `TimerClientError`
- helpers de transporte ya cubiertos:
  - `timer_node_name(...)`
  - `build_timer_system_request(...)`
  - `build_timer_system_request_with_target(...)`
  - `parse_timer_response(...)`
  - `parse_timer_get_response(...)`
  - `parse_timer_list_response(...)`
  - `parse_timer_help_response(...)`
  - `parse_timer_fired_event(...)`
  - `timer_response_service_error(...)`
  - `map_timer_transport_message(...)`
- verificación actual:
  - `cargo check -p fluxbee-sdk`
  - `cargo test -p fluxbee-sdk timer`

### RUST-TIMER-SDK - Cliente tipado

- [x] RUST-TIMER-SDK-11. Implementar `TimerClient` sobre `NodeSender`.
- [x] RUST-TIMER-SDK-12. Implementar operaciones de tiempo:
  - `now`
  - `now_in`
  - `convert`
  - `parse`
  - `format`
  - `help`
- [x] RUST-TIMER-SDK-13. Implementar operaciones de timers:
  - `schedule`
  - `schedule_in`
  - `schedule_recurring`
  - `get`
  - `list`
  - `cancel`
  - `reschedule`
  - `purge_owner`
- [x] RUST-TIMER-SDK-14. Definir retry policy mínima para operaciones de tiempo, alineada con la semántica ya documentada para Go.
- [x] RUST-TIMER-SDK-15. Definir validación client-side mínima:
  - mínimo de 60s
  - exclusividad entre campos absolutos/relativos
  - shape recurrente
  - `missed_policy` / `missed_within_ms`

Estado actual del cliente Rust `SY.timer`:

- `TimerClient` y `TimerClientConfig` ya existen en [timer.rs](/Users/cagostino/Documents/GitHub/fluxbee/crates/fluxbee_sdk/src/timer.rs).
- surface implementado:
  - `help`
  - `now`
  - `now_in`
  - `convert`
  - `parse`
  - `format`
  - `schedule`
  - `schedule_in`
  - `schedule_recurring`
  - `get`
  - `list`
  - `list_mine`
  - `cancel`
  - `reschedule`
  - `purge_owner`
- validaciones ya activas:
  - mínimo de 60s
  - exclusividad `fire_at_utc_ms` / `fire_in_ms`
  - exclusividad `new_fire_at_utc_ms` / `new_fire_in_ms`
  - cron recurrente de 5 campos
  - `timer_uuid` no vacío
  - `owner_l2_name` / `target_l2_name` como L2 canónico
  - reglas de `missed_policy` / `missed_within_ms`
- retry policy de time ops ya activa:
  - default `100ms / 300ms / 1s`
  - solo para `now`, `now_in`, `convert`, `parse`, `format`
- verificación actual:
  - `cargo check -p fluxbee-sdk`
  - `cargo test -p fluxbee-sdk timer`

### RUST-TIMER-SDK - Tests y compatibilidad

- [ ] RUST-TIMER-SDK-16. Agregar tests unitarios de serialize/deserialize del contrato.
- [ ] RUST-TIMER-SDK-17. Agregar golden tests o fixtures wire compatibles con `SY.timer`.
- [ ] RUST-TIMER-SDK-18. Cruzar compatibilidad semántica con el cliente Go para:
  - errores
  - helpers de tiempo
  - responses de list/get/schedule
- [ ] RUST-TIMER-SDK-19. Agregar un cliente fake/test harness para nodos Rust que quieran testear lógica basada en tiempo sin depender de un hive real.
- [ ] RUST-TIMER-SDK-20. Documentar ejemplos mínimos de uso desde nodos Rust y workflows Rust.

### Notas de alcance

- Esto no obliga a que `SY.admin` o `SY.orchestrator` usen el cliente Rust de `SY.timer` de inmediato.
- El primer objetivo es que el SDK Rust tenga paridad suficiente para cualquier consumidor Rust nuevo.
- Después de eso recién conviene evaluar adopciones puntuales en bins existentes.
