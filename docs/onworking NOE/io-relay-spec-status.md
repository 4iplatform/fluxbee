# IO Relay - Estado contra la spec

**Fecha:** 2026-04-10
**Referencia normativa:** `docs/onworking NOE/io-relay-spec.md`
**Objetivo:** dejar explicito que partes de la spec del relay ya estan implementadas y cuales siguen pendientes.

---

## Estado actual

Implementado y validado en build/test del workspace IO:

- `io-common` ya expone `RelayPolicy`, `RelayStore`, `InMemoryRelayStore`, `RelayBuffer`, `RelayFragment`, `RelaySession`, `AssembledTurn`, `RelayDecision` y `FlushReason`.
- `InboundProcessor` ya puede consumir `AssembledTurn` y resolver identidad al `flush`.
- `IO.slack` ya dejo de usar `SlackSessionizer` privado y pasa por el relay comun.
- `IO.slack` ya expone relay por config formal del nodo bajo `config.io.relay.*`.
- `meta.context.io.relay.*` ya se emite en el mensaje consolidado.
- `cargo check`, `cargo test --manifest-path nodes/io/Cargo.toml -p io-common` y `cargo build --release --manifest-path nodes/io/Cargo.toml -p io-slack` pasaron en Linux.

Pendiente:

- metricas minimas del relay;
- backend `durable` y rehidratacion acotada;
- cierre de no-regresion observable en runtime para `IO.slack`.

---

## Lectura por secciones de la spec

### Secciones 1 a 3

Estado: alineadas e implementadas en lo esencial para v1-memory.

- el relay vive en `io-common`;
- `IO.slack` ya no tiene sessionizacion privada;
- `relay_window_ms = 0` hace passthrough;
- `relay_window_ms > 0` consolida;
- identity sigue resolviendose al `flush`;
- el relay no toma ownership de cognition, router ni del offload a blob.

### Seccion 4 - Contrato de `io-common`

Estado: mayormente implementada.

Implementado:

- `RelayPolicy`
- `RelayStore`
- `RelayBuffer`
- `InMemoryRelayStore`
- `RelayDecision`

Pendiente:

- parte `durable` del contrato existe en tipos pero no esta activa como backend real;
- la superficie de metricas no esta cerrada.

### Secciones 5 a 8 - Responsabilidades, sesion, flush y metadata

Estado: implementadas para v1-memory.

Implementado:

- el adapter arma `RelayFragment`;
- `io-common` administra sesiones, dedup, limites, deadlines y flush;
- el ensamblado es deterministico y simple;
- el mensaje consolidado incluye `meta.context.io.relay.*`.

Observacion:

- la metadata emitida hoy cubre los campos minimos (`applied`, `reason`, `parts`, `window_ms`) y algunos opcionales cuando estan disponibles.

### Secciones 9 y 10 - Configuracion y capacidad

Estado: parcialmente implementadas.

Implementado:

- `IO.slack` ya mapea `config.io.relay.window_ms`, `config.io.relay.max_open_sessions`, `config.io.relay.max_fragments_per_session` y `config.io.relay.max_bytes_per_session` al relay comun;
- ya hay limites de sesiones, fragmentos y bytes;
- ya existe comportamiento explicito frente a capacidad y limites;
- `CONFIG_SET` ya puede hot-aplicar cambios de relay.
- la validacion runtime del relay activo ya quedo comprobada en nodo real con logs de `relay session opened`, `relay fragment buffered`, `relay session flushed` y `sent to router`.

Pendiente:

- no toda la superficie de configuracion propuesta por la spec esta expuesta todavia como config de adapter;
- el fail-open controlado debe validarse mejor en runtime.

### Secciones 11 a 13 - Durable, rehidratacion y shutdown

Estado: pendientes para fase posterior.

- `durable` no esta implementado;
- la rehidratacion no esta validada en runtime;
- el shutdown/drain existe en la API del relay, pero no esta cerrado como comportamiento validado del nodo Slack en produccion.

### Seccion 14 - Observabilidad

Estado: parcial.

Implementado:

- logs event-driven del relay;
- resumen de inicializacion del relay en `IO.slack`;
- log de hot-apply de politica del relay.

Pendiente:

- metricas minimas del relay;
- validacion runtime real en `journalctl`.

Politica vigente:

- sin logs periodicos;
- `debug` para flujo del relay;
- `info` para inicializacion o eventos poco frecuentes;
- `warn` para capacidad, expiracion, drops y fallback.

### Seccion 15 - Integracion con `IO.slack`

Estado: migracion estructural implementada.

Implementado:

- `SlackSessionizer` fue reemplazado por `RelayBuffer`;
- Slack define `RelayKey`;
- Slack emite `RelayFragment`;
- Slack delega acumulacion y flush al relay comun;
- Slack toma la politica de relay desde config formal del nodo.

Pendiente:

- validacion de no-regresion observable;
- revisar el camino definitivo de credenciales para no depender de inline en pruebas operativas cuando `env:` no resuelva.

### Secciones 16 a 20

Estado:

- `IO.api.*`: pendiente de adopcion;
- plan/checklist: parcialmente cumplidos;
- criterio de aceptacion: parcialmente cubierto, a cerrar con runtime y durable.

---

## Checklist resumido

### `io-common`

- [x] contrato base del relay
- [x] store `memory`
- [x] flush y ensamblado
- [x] metadata `meta.context.io.relay.*`
- [ ] metricas minimas
- [ ] backend `durable`

### `IO.slack`

- [x] migracion desde sessionizer privado
- [x] relay key estable
- [x] passthrough con ventana cero
- [x] config formal bajo `config.io.relay.*`
- [x] hot reload por `CONFIG_SET`
- [x] validacion runtime con consolidacion real
- [ ] validacion no-regresion observable

### Durable / posterior

- [ ] `DurableRelayStore`
- [ ] rehidratacion acotada
- [ ] estrategia de shutdown/drain validada en produccion
