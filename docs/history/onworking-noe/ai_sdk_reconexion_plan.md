# Plan de trabajo - Reconexión automática en `fluxbee_ai_sdk`

## Objetivo
Evitar que los nodos `AI.*` terminen su proceso cuando el router se reinicia o cae temporalmente, implementando reconexión automática robusta en `crates/fluxbee_ai_sdk`.

## Tareas (orden sugerido)

- [x] 1. Diagnóstico fino del flujo actual
  - Revisar `runtime.rs` y `router_client.rs` para ubicar exactamente dónde un error de socket termina el proceso.
  - Confirmar qué errores hoy se tratan como fatales vs recuperables.

- [x] 2. Definir política de reconexión del SDK AI
  - Reintentar en loop ante caída del router (EOF, connection reset, broken pipe, connect fail).
  - Backoff con tope (exponencial con jitter) + logs claros por intento.
  - Mantener fatales solo para errores de configuración/serialización no recuperables.

- [x] 3. Refactor mínimo en el runner AI
  - Evitar que el reader task “mate” el proceso por desconexión temporal.
  - Recrear cliente/conexión y reanudar `HELLO`/registro de nodo automáticamente.

- [x] 4. Endurecer observabilidad
  - Logs diferenciados: `disconnected`, `reconnecting`, `reconnected`, `fatal`.
  - Incluir `node_name`, intento, delay y causa raíz en cada evento.

- [ ] 5. Validación local controlada
  - Levantar un nodo AI, cortar/reiniciar router, verificar que el proceso AI siga vivo y reconecte solo.
  - Verificar que vuelve a procesar mensajes tras reconectar.

- [ ] 6. Validación de no regresión
  - Compilar crates afectados y correr tests existentes del SDK AI (y los que toquen el runtime).
  - Ajustar/crear tests unitarios de reconexión si el diseño actual lo permite sin sobreengineering.

## Hallazgos de Tarea 1 (2026-03-30)

- `RouterClient::connect` solo hace un connect inicial (`connect(&config.into()).await`) y no tiene loop de reconexión propio.
  - Ref: `crates/fluxbee_ai_sdk/src/router_client.rs:46-47`

- En `NodeRuntime` (modo default), cualquier error del `reader_loop` distinto de timeout idle termina `run_with_config` con error fatal.
  - `reader_loop`: `Err(err) => return Err(err)` en lectura.
    - Ref: `crates/fluxbee_ai_sdk/src/runtime.rs:266-277`
  - `run_with_config`: cuando `reader_task` devuelve `Ok(Err(err))`, incrementa `fatal_errors` y retorna error.
    - Ref: `crates/fluxbee_ai_sdk/src/runtime.rs:147-153`

- Aunque `AiSdkError::is_recoverable()` marca `NodeError::Io` y `NodeError::Disconnected` como recuperables, esa clasificación hoy solo se usa para retries de handler/write, no para el loop de lectura/conexión.
  - Ref recoverable map: `crates/fluxbee_ai_sdk/src/errors.rs:25-42`
  - Ref retry scope: `crates/fluxbee_ai_sdk/src/runtime.rs:290-366`

- En `mode=gov` (single connection runtime), la lectura también corta el runtime ante cualquier error no-timeout idle.
  - `Err(err) => return Err(err)` en loop principal.
  - Ref: `nodes/ai/ai-generic/src/bin/ai_node_runner.rs:2014-2064`
  - Ref equivalente: `nodes/gov/ai-frontdesk-gov/src/bin/ai_node_runner.rs:2014-2064`

- Conclusión del diagnóstico: el comportamiento actual sigue siendo “desconexión de router => salida del runtime AI” (sin autorreconexión interna efectiva).

## Definición Tarea 2 - Política de reconexión (2026-03-30)

### Contexto base del core (fuente de verdad)
- `fluxbee_sdk::node_client` ya implementa un `connection_manager_loop` con reconexión automática del socket y nuevo handshake (`HELLO`/`ANNOUNCE`) cuando se corta la conexión.
  - Ref: `crates/fluxbee_sdk/src/node_client.rs:138-195`
- Al cortarse el socket, el receiver puede entregar `NodeError::Io(...)`/`NodeError::Disconnected` en lecturas antes de que se estabilice la reconexión.
  - Ref: `crates/fluxbee_sdk/src/node_client.rs:240-276`, `crates/fluxbee_sdk/src/split.rs:107-128`

### Política acordada para `fluxbee_ai_sdk`

- Lectura runtime (`reader_loop`) en modo default:
  - `NodeError::Timeout`: seguir como idle (sin cambios).
  - `NodeError::Io(_)` y `NodeError::Disconnected`: tratar como transitorios de enlace, no fatales.
  - Acción: registrar `disconnected`, esperar reconexión y reanudar lectura (`wait_connected`) sin terminar el proceso.
  - Cualquier error no recuperable (`Json`, `InvalidAnnounce`, `HandshakeFailed`, etc.): fatal.

- Conexión inicial (`RouterClient::connect`):
  - Mantener comportamiento actual (si no hay conexión inicial, fallar startup).
  - La reconexión automática cubre caídas posteriores de enlace, no ocultar errores de bootstrap/configuración.

- Escritura de respuestas (`writer.write`):
  - Mantener retry existente por `RetryPolicy`.
  - Si se agota en error recuperable, no matar runtime global; contabilizar y seguir con próximos mensajes.
  - Errores no recuperables en write: fatal.

- Backoff de reconexión en runtime AI:
  - Usar un backoff explícito de observabilidad para el loop de espera de reconexión: inicio 200ms, duplicación hasta 5s, con jitter.
  - Se resetea al reconectar exitosamente.
  - Nota: esto complementa, no reemplaza, el backoff del `connection_manager_loop` del core.

- Clasificación de severidad:
  - `transient`: `NodeError::Io`, `NodeError::Disconnected`, `NodeError::Timeout`.
  - `fatal`: `NodeError::Json`, `NodeError::Yaml`, `NodeError::Uuid`, `NodeError::InvalidAnnounce`, `NodeError::HandshakeFailed`, `AiSdkError::Protocol`, errores de payload/json internos no vinculados a enlace.

- Alcance de implementación:
  - Aplicar la misma semántica tanto a `NodeRuntime` (default) como al runtime single-connection usado en `mode=gov`, para no tener comportamiento divergente.

## Avance implementación SDK (2026-03-30)

- Hecho en `fluxbee_ai_sdk` (cubre runtime default):
  - `RouterReader` ahora expone `is_connected()` y `wait_connected()` para esperar reconexión del core sin cerrar proceso.
  - `reader_loop` dejó de tratar `NodeError::Io`/`NodeError::Disconnected` como fatal inmediato.
  - Al detectar desconexión transitoria:
    - log `disconnected`
    - espera reconexión con backoff (200ms -> 5s) + jitter
    - log `reconnecting` por intento
    - log `reconnected` al recuperar enlace
  - Se agregaron métricas de reconexión en runtime (`reconnect_events`, `reconnect_wait_cycles`).

- Hecho también para runtime `single-connection` (modo gov y su variante en `ai-generic`):
  - `run_single_connection_runtime` ahora trata `NodeError::Io`/`NodeError::Disconnected` como transitorios.
  - Se agregó espera de reconexión con backoff+jitter y logs `disconnected`/`reconnecting`/`reconnected`.
  - Se incorporaron contadores locales `reconnect_events` y `reconnect_wait_cycles` en métricas periódicas.
