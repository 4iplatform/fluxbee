# IO Relay - Tareas de implementacion

**Estado:** relay comun implementado, config formal del nodo integrada, validacion runtime principal cerrada
**Fecha:** 2026-04-10
**Base normativa:** `docs/onworking NOE/io-relay-spec.md`
**Estado contra la spec:** `docs/onworking NOE/io-relay-spec-status.md`
**Alcance inicial:** `nodes/io/common` + `nodes/io/io-slack`
**Fuera de alcance inicial:** backend durable, cambios en AI SDK, cambios en router/core fuera de IO

---

## Criterio de orden

La secuencia propuesta busca:

1. cerrar primero el contrato comun en `io-common`,
2. implementar despues el backend `memory`,
3. migrar luego `IO.slack` sin romper comportamiento observable,
4. mover la configuracion del relay a la superficie formal del nodo,
5. dejar `durable` explicitamente diferido.

---

## Checklist operativo

### Fase 0 - Congelar el alcance v1

- [x] IO-R0.1 Confirmar que la primera entrega cubre solo `memory` y deja `durable` como fase posterior.
- [x] IO-R0.2 Confirmar el contrato minimo que va a exponer `io-common`: `RelayPolicy`, `RelayStore`, `RelayBuffer`, `RelayFragment`, `AssembledTurn`, `RelayDecision`, `FlushReason`.
- [x] IO-R0.3 Confirmar la metadata minima de salida bajo `meta.context.io.relay.*`.
- [x] IO-R0.4 Confirmar el mapping inicial de Slack hacia `RelayPolicy`.

### Fase 1 - Introducir el relay comun en `io-common`

- [x] IO-R1.1 Crear el modulo comun de relay en `nodes/io/common`.
- [x] IO-R1.2 Definir los tipos normativos base del relay y sus invariantes.
- [x] IO-R1.3 Implementar `InMemoryRelayStore`.
- [x] IO-R1.4 Implementar `RelayBuffer::handle_fragment` con apertura de sesion, acumulacion, dedup best-effort y recalculo de deadline.
- [x] IO-R1.5 Implementar `flush_session` y `flush_expired` con razones de flush explicitas.
- [x] IO-R1.6 Implementar el ensamblado de un unico `text/v1` canonico a partir de los fragmentos acumulados.
- [x] IO-R1.7 Emitir metadata de relay en el mensaje resultante sin romper el contrato actual de `io_context`.
- [x] IO-R1.8 Agregar limites operativos iniciales: sesiones abiertas, fragmentos por sesion y bytes por sesion.
- [ ] IO-R1.9 Agregar logs estructurados y metricas minimas del relay.

### Fase 2 - Integrar el relay con el pipeline inbound comun

- [x] IO-R2.1 Definir el punto exacto de integracion entre `RelayBuffer` y `InboundProcessor`.
- [x] IO-R2.2 Garantizar que identity siga ocurriendo en `flush` y no durante la acumulacion de fragmentos.
- [x] IO-R2.3 Garantizar que `relay_window_ms = 0` haga passthrough inmediato.
- [x] IO-R2.4 Garantizar que el relay no duplique la responsabilidad del SDK sobre `content` vs `content_ref`.
- [x] IO-R2.5 Validar que el mensaje consolidado siga saliendo por `NodeSender::send`.

### Fase 3 - Migrar `IO.slack` al mecanismo comun

- [x] IO-R3.1 Reemplazar `SlackSessionizer` por uso de `RelayBuffer`.
- [x] IO-R3.2 Definir `RelayKey` de Slack con la derivacion estable ya usada hoy.
- [x] IO-R3.3 Mapear la configuracion de Slack al `RelayPolicy` comun.
- [x] IO-R3.4 Adaptar el armado de `RelayFragment` desde los eventos inbound de Slack.
- [ ] IO-R3.5 Mantener el comportamiento actual de dedup/sessionizacion observable salvo donde el spec pida cambio explicito.
- [x] IO-R3.6 Validar que Slack siga enviando al router un unico `text/v1` consolidado por turno cuando la ventana sea mayor a cero.
- [x] IO-R3.7 Mover la configuracion de relay de `IO.slack` desde `SLACK_SESSION_*` a config formal del nodo.
- [x] IO-R3.8 Definir precedencia explicita para config del relay: spawn config persistida y/o `CONFIG_SET`, evitando depender de env vars sueltas.
- [x] IO-R3.9 Extender `IoSlackAdapterConfigContract` para validar/materializar la superficie de relay.
- [x] IO-R3.10 Hacer que `Config::from_env()` lea relay desde config del nodo y deje env solo como compatibilidad temporal o debug.
- [x] IO-R3.11 Validar runtime real del relay usando la superficie formal de config, sin workaround por env file local.

### Fase 4 - Tests de no regresion

- [x] IO-R4.1 Agregar tests unitarios de apertura, acumulacion y flush por timeout.
- [x] IO-R4.2 Agregar tests unitarios de dedup por `fragment_id` dentro de la sesion.
- [x] IO-R4.3 Agregar tests unitarios de limites: capacidad, max_fragments y max_bytes.
- [x] IO-R4.4 Agregar tests de passthrough con `relay_window_ms = 0`.
- [ ] IO-R4.5 Agregar tests de integracion de `IO.slack` cubriendo migracion desde el sessionizer propio al relay comun.

### Fase 5 - Cierre documental y backlog posterior

- [x] IO-R5.1 Actualizar documentacion operativa para reflejar relay comun y config formal del nodo.
- [ ] IO-R5.2 Dejar asentado el backlog de fase posterior para `DurableRelayStore` y rehidratacion acotada.
- [ ] IO-R5.3 Verificar criterio de aceptacion contra `docs/onworking NOE/io-relay-spec.md`.

---

## Estado validado hasta ahora

Validado en Linux:

- `cargo check --manifest-path nodes/io/Cargo.toml -p io-common -p io-slack`
- `cargo test --manifest-path nodes/io/Cargo.toml -p io-common`
- `cargo build --release --manifest-path nodes/io/Cargo.toml -p io-slack`

Observado en runtime:

- passthrough con `window_ms = 0` funciona;
- se ve `relay passthrough immediate`;
- se ve `sent to router`;
- el summary `io-slack relay policy initialized` aparece al boot.
- `CONFIG_SET` con `config.io.relay.*` aplica correctamente en runtime;
- se ve `relay session opened`, `relay fragment buffered` y `relay session flushed`;
- se confirmo consolidacion real al router con `parts=2` en una ventana mayor a cero;
- la validacion efectiva se hizo con credenciales Slack inline en `CONFIG_SET`.

Pendiente en runtime:

- validar no-regresion observable de Slack.

---

## Mapping vigente desde `IO.slack`

La superficie formal actual del relay en `IO.slack` es:

- `config.io.relay.window_ms` -> `relay_window_ms`
- `config.io.relay.max_open_sessions` -> `max_open_sessions`
- `config.io.relay.max_fragments_per_session` -> `max_fragments_per_session`
- `config.io.relay.max_bytes_per_session` -> `max_bytes_per_session`

Compatibilidad temporal:

- `SLACK_SESSION_*` queda solo como fallback de arranque para entornos locales/no canonicos.

Defaults internos vigentes:

- `window_ms`: `0`
- `max_open_sessions`: `10000`
- `max_fragments_per_session`: `8`
- `max_bytes_per_session`: `262144`
- `stale_session_ttl_ms`: derivado interno y mayor o igual a `relay_window_ms`
- `flush_on_attachment`: `false`
- `flush_on_explicit_final`: `true`
- `flush_on_max_fragments`: `true`
- `flush_on_max_bytes`: `true`
- `storage_mode`: `memory`

---

## Politica vigente de logs del relay

- no agregar logs periodicos ni snapshots constantes;
- preferir logs por evento relevante;
- `debug` para apertura de sesion, buffering, flush y dedup;
- `info` para inicializacion o hot-apply de politica;
- `warn` para capacidad, expiracion, drops o fallbacks.

---

## Hallazgos runtime a seguir

- El deploy de runtime con nueva version (`0.1.1`) no hizo que el nodo existente tomara el runtime nuevo; al repetir el flujo con `0.1.0` si aparecieron los cambios del relay.
- Conviene abrir una tarea separada para revisar por que el restart/update del nodo existente no tomo la version nueva publicada.
- La prueba real mostro que cambiar `/etc/fluxbee/io-slack.env` no activo el relay en el nodo spawned existente; ese camino ya no debe usarse como base de validacion.
- El relay ahora entra por `config.io.relay.*`, con bootstrap desde spawn config y hot reload por `CONFIG_SET`.
- La validacion B ya quedo cerrada usando `CONFIG_SET` y logs runtime del nodo real.
- En este entorno, `app_token_ref`/`bot_token_ref` via `env:` no resolvieron como se esperaba para la prueba; con tokens inline el runtime quedo operativo y el relay valido correctamente.

---

## Proximo paso recomendado

1. cerrar la validacion de no-regresion observable de Slack;
2. decidir si conviene persistir formalmente las credenciales/local secrets para no depender de inline en pruebas futuras;
3. dejar separado el analisis de materializacion de version runtime cuando toque retomarlo.
