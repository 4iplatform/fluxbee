# IO Nodes - Migración de interacción/config al modelo AI (plan de tareas)

## Objetivo

Llevar los nodos IO al mismo modelo operativo de interacción/configuración que hoy usamos en AI Nodes:

- nodo puede nacer con bootstrap mínimo,
- configuración funcional/runtime vía Control Plane del nodo (`CONFIG_GET` / `CONFIG_SET`),
- persistencia dinámica node-owned,
- separación clara entre capa core/orchestrator y capa runtime del nodo.

Este plan está orientado a escalar desde `IO.slack` a futuros adapters (`IO.whatsapp`, `IO.instagram`, `IO.gmail`, `IO.calendar`, etc.).

---

## Decisiones base (a respetar)

1. Mantener dualidad de capas:
- capa core: `set_node_config` / `NODE_CONFIG_SET` / `CONFIG_CHANGED` (infra/bootstrap).
- capa node: `CONFIG_GET` / `CONFIG_SET` / `CONFIG_RESPONSE` (runtime/business).

2. Hot reload canónico para IO:
- `CONFIG_SET` node-owned.

3. `payload.config` es adapter-specific:
- el contrato común fija envelope/metadatos, no el schema de negocio por canal.

4. Secrets:
- no exponer secretos en responses/logs,
- evitar persistir secretos en claro en state dinámico,
- usar metadata de secretos (`contract.secrets[*]`) para discovery operativo.

---

## Plan de trabajo (ordenado)

## Fase 0 - Alineación de contrato (sin romper comportamiento actual)

- [ ] T0.1 Congelar contrato común IO para control-plane:
  - verbos soportados (`CONFIG_GET`, `CONFIG_SET`, `CONFIG_RESPONSE`, `STATUS`, `PING`),
  - campos mínimos comunes (`node_name`, `schema_version`, `config_version`, `apply_mode`, `config`),
  - semántica de errores comunes (`invalid_config`, `stale_config_version`, `config_persist_error`, etc.).
- [ ] T0.2 Definir precedencia efectiva para IO managed:
  - 1) config dinámica persistida del nodo,
  - 2) fallback a `config.json` de orchestrator,
  - 3) `UNCONFIGURED`.
- [ ] T0.3 Acordar política de compatibilidad inicial:
  - `apply_mode=replace` obligatorio en MVP,
  - `merge_patch` opcional en fase posterior.

## Fase 1 - Runtime IO común (base reusable)

- [x] T1.1 Crear módulo común IO para estado/control-plane (en `nodes/io/common`):
  - `IoControlPlaneState` (`UNCONFIGURED` / `CONFIGURED` / `FAILED_CONFIG`),
  - storage del estado efectivo por nodo,
  - validación común de envelope (`CONFIG_SET`/`CONFIG_GET`).
- [x] T1.2 Integrar helpers SDK existentes (`fluxbee_sdk::node_config`):
  - parse/build de mensajes de control-plane,
  - respuesta estándar `CONFIG_RESPONSE`.
- [x] T1.3 Implementar persistencia dinámica común para IO:
  - path canónico por nodo bajo `STATE_DIR`,
  - escritura atómica,
  - carga en bootstrap.
- [x] T1.4 Implementar bootstrap por fallback desde `config.json` de orchestrator cuando no exista estado dinámico.

Nota: T1.x implementado como base reusable en `nodes/io/common`; integración runtime en `io-slack` queda para Fase 3.

## Fase 2 - Contrato adapter-specific (escalable)

- [x] T2.1 Definir interfaz trait para schema por adapter:
  - validar `payload.config`,
  - materializar defaults,
  - redacción de `effective_config` redacted,
  - descriptor de secretos (`contract.secrets`).
- [x] T2.2 Implementar primer adapter sobre el trait (`IO.slack`) como referencia.
- [x] T2.3 Dejar plantilla para nuevos adapters (`whatsapp`, `instagram`, `gmail`, `calendar`) sin acoplarlos a Slack.

## Fase 3 - Migración de IO.slack al flujo node-owned completo

- [x] T3.1 Agregar manejo runtime de `CONFIG_GET`/`CONFIG_SET` en `io-slack`.
- [x] T3.2 Implementar actualización en caliente de parámetros funcionales (sin restart) para campos no estructurales.
- [x] T3.3 Definir qué cambios requieren reinicialización local del adapter (si aplica) y cómo reportarlo en `CONFIG_RESPONSE`.
- [x] T3.4 Resolver política de compatibilidad de spawn (decisión: sin compat legacy).

Nota avance T3.1:
- `io-slack` ya procesa `CONFIG_GET`/`CONFIG_SET` en runtime con el módulo común de IO.
- valida envelope, aplica contrato adapter-specific de Slack, persiste estado dinámico y responde `CONFIG_RESPONSE` redacted.
- T3.2 se completó inicialmente con hot-reload de `io.dst_node`; el resto de campos con impacto de reinicialización queda para T3.3.

Nota avance T3.2:
- `io.dst_node` ya se aplica en caliente sin restart: un `CONFIG_SET` válido actualiza el `InboundProcessor` activo para los próximos mensajes.
- esto cubre el primer campo no estructural del runtime.
- credenciales Slack y parámetros que impliquen reinicialización de componentes quedan explicitados para T3.3.

Nota avance T3.3:
- política aplicada en `io-slack`:
  - hot inmediato: `io.dst_node`
  - reinicialización interna (sin restart de proceso): `slack.*token*` (rotación de credenciales)
  - marcado como `restart_required`: cambios en `identity.*`, `io.blob.*`, `node.*`, `runtime.*`
- `CONFIG_RESPONSE` de `CONFIG_SET` ahora incluye `apply` con:
  - `hot_applied[]`
  - `reinit_performed[]`
  - `restart_required[]`

Nota avance T3.4:
- se eliminó compatibilidad legacy de spawn para `io-slack`.
- contrato activo: `FLUXBEE_NODE_NAME` + lectura estricta de `config.json` en ruta managed canónica.
- se removieron paths/env legacy (`NODE_CONFIG_PATH`, `IO_SLACK_FORCE_CONFIG_PATH`, fallback `NODE_NAME`/`ISLAND_ID` para resolver config de spawn).

## Fase 4 - Integración con core/orchestrator (frontera entre capas)

- [ ] T4.1 Documentar explícitamente que `PUT .../config` no reemplaza `CONFIG_SET` runtime node-owned.
- [ ] T4.2 Definir estrategia para `CONFIG_CHANGED` en IO:
  - opción A: solo señal informativa,
  - opción B: puente interno a `CONFIG_SET` en runtime IO,
  - elegir una y normarla.
- [ ] T4.3 Alinear UX de SY.admin/SY.architect para recomendar `control/config-set` como camino de hot reload IO.

## Fase 5 - Seguridad, observabilidad y operación

- [ ] T5.1 Redaction consistente de secretos en:
  - `CONFIG_RESPONSE`,
  - `STATUS`,
  - logs.
- [ ] T5.2 Métricas mínimas del control-plane IO:
  - count de `CONFIG_SET ok/error`,
  - último `config_version`,
  - estado actual.
- [ ] T5.3 Runbook operativo por adapter:
  - bootstrap mínimo,
  - secuencia `CONFIG_GET -> CONFIG_SET`,
  - recuperación desde `FAILED_CONFIG`.

## Fase 6 - Tests y criterios de aceptación

- [ ] T6.1 Unit tests de control-plane común IO.
- [ ] T6.2 Tests de precedencia (persistida > orchestrator fallback > unconfigured).
- [ ] T6.3 Tests de versionado (`stale_config_version`, idempotencia).
- [ ] T6.4 E2E mínimo en `IO.slack`:
  - spawn sin secrets -> `FAILED_CONFIG`/degradado controlado,
  - `CONFIG_SET` válido -> `CONFIGURED`,
  - restart -> rehidratación correcta.

---

## Cambios de código esperados (mapa de impacto)

- `nodes/io/common/src/*`:
  - nuevo núcleo de control-plane/config state.
- `nodes/io/io-slack/src/main.rs`:
  - adopción del núcleo común y handlers de control-plane.
- `nodes/io/io-sim/src/main.rs`:
  - opcional como adapter de referencia para pruebas.
- `docs/io/*`:
  - actualización de contrato operacional y runbooks.
- `docs/onworking NOE/*`:
  - seguimiento de tareas y decisiones de rollout.

---

## Riesgos a resolver antes de implementación fuerte

- Definir con precisión qué parte de config IO puede aplicarse hot sin reinicio por adapter.
- Evitar duplicar lógica de secrets entre adapters.
- Mantener backward compatibility con deployments que hoy dependen de env/config de spawn.

---

## Propuesta de ejecución por iteraciones

1. Iteración A: Fase 0 + Fase 1 (infra común sin migrar completamente Slack).
2. Iteración B: Fase 2 + Fase 3 en `IO.slack`.
3. Iteración C: Fase 4 + Fase 5 + Fase 6 (cierre operativo y de pruebas).

---

## Fase 1 - Detalle cerrado propuesto (para revisión previa)

## 1) Alcance técnico exacto de Fase 1

- No migrar todavía lógica de Slack business adapter.
- Construir base reusable de control-plane en `nodes/io/common`.
- Dejar `io-slack` e `io-sim` funcionalmente iguales en data-plane.
- Habilitar solo infraestructura para que en Fase 3 sea simple enchufar handlers reales.

## 2) Estructuras comunes propuestas

- `IoNodeLifecycleState`:
  - `UNCONFIGURED`
  - `CONFIGURED`
  - `FAILED_CONFIG`
- `IoControlPlaneState`:
  - `current_state`
  - `config_source` (`dynamic` | `orchestrator_fallback` | `none`)
  - `schema_version`
  - `config_version`
  - `effective_config` (redactable)
  - `last_error` (opcional, redacted-safe)

## 3) Persistencia propuesta (Fase 1)

- Path dinámico por nodo:
  - `${STATE_DIR}/io-nodes/<node@hive>.json`
  - default `STATE_DIR=/var/lib/fluxbee/state`
- Reglas:
  - write atómico,
  - permisos restrictivos (`0600` archivo, `0700` directorio),
  - versión monotónica (`config_version`).
- Precedencia al bootstrap:
  1. dinámico persistido (`io-nodes/<node>.json`)
  2. fallback orchestrator (`/var/lib/fluxbee/nodes/IO/<node>/config.json`)
  3. `UNCONFIGURED`

## 4) Frontera `io-common` vs runtime IO

- `io-common` mantiene su foco data-plane (lookup/provision/forward).
- El nuevo módulo de control-plane en `nodes/io/common` se limita a:
  - envelope/parse/build de config control messages,
  - estado de lifecycle/config,
  - persistencia de config efectiva del nodo.
- No meter en este módulo:
  - colas de negocio durables,
  - sessionización específica de canal,
  - lógica provider-specific.

## 5) Contrato de errores comunes (MVP)

- `invalid_config`
- `stale_config_version`
- `unsupported_apply_mode`
- `config_persist_error`
- `unknown_system_msg`
- `node_not_configured` (para data-plane si estado no operativo)

## 6) Criterios de cierre de Fase 1

- Existe módulo común compilable con tests básicos.
- Puede cargar estado dinámico y fallback de orchestrator sin romper arranque.
- Expone API interna lista para que `io-slack` conecte `CONFIG_GET/SET` en Fase 3.
- No hay cambios de comportamiento productivo en inbound/outbound de Slack todavía.

## 7) Desviaciones explícitas aceptadas

- Se incorpora persistencia local para config runtime de IO (alineación con AI), aunque la redacción vieja de `io-common` hablaba de "sin persistencia local" en sentido general.
- Esta excepción queda acotada a control-plane/config state del nodo, no a colas o storage de negocio.
