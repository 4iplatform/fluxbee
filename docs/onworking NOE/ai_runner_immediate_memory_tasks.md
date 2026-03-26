# AI Runner - Immediate Memory Integration Tasks

## Objetivo

Integrar memoria inmediata en `ai_node_runner` para nodos `AI.*` (incluyendo `AI.chat` y `AI.frontdesk.gov`), manteniendo compatibilidad con el comportamiento actual y actualizando deploy/docs.

## Checklist

- [x] 1. Alinear alcance y contratos (sin código): confirmar que `ai_node_runner` construye `FunctionRunInput.immediate_memory` para `openai_chat`, keyed por `src_ilk`, con defaults del spec (`10/8/1600`).
- [x] 2. Definir modelo runtime mínimo en runner:
  - `recent_interactions` persistidas por `src_ilk`.
  - `conversation_summary` persistido por `src_ilk`.
  - `active_operations` inicialmente vacío/default (o solo si el nodo lo reporta).
- [x] 3. Definir política de refresh de summary:
  - refresh por eventos importantes + cada `N` turnos (default `N=3`).
- [x] 4. Definir configuración efectiva por nodo (backward compatible):
  - `immediate_memory.enabled`
  - `immediate_memory.recent_interactions_max`
  - `immediate_memory.active_operations_max`
  - `immediate_memory.summary_max_chars`
  - `immediate_memory.summary_refresh_every_turns`
  - `immediate_memory.trim_noise_enabled`
- [x] 5. Implementar en `ai_node_runner` el armado de `immediate_memory` antes de llamar al SDK, con fallback al path actual cuando `enabled=false`.
- [x] 6. Implementar persistencia local de immediate memory por `src_ilk` en state dir del nodo, con límites y pruning.
- [x] 7. Integrar con `thread_state` existente sin mezclar semánticas:
  - `thread_state` sigue siendo datos duros.
  - `immediate_memory` queda como contexto conversacional corto.
- [x] 8. Agregar observabilidad mínima:
  - memory hit/miss
  - tamaño de ventana usada
  - refresh ejecutado/no ejecutado
  - degradación segura ante fallas de persistencia
- [x] 9. Ajustar deploy para configuración de immediate memory:
  - decidir si `scripts/deploy-ia-node.sh` incorpora flags o si se documenta solo vía `--config-json`.
  - mantener compatibilidad con comandos actuales de spawn (`AI.chat` y `AI.frontdesk.gov`).
- [x] 10. Actualizar ejemplos de config:
  - `AI.chat@motherbee` con immediate memory habilitada.
  - `AI.frontdesk.gov@motherbee` con immediate memory + thread_state.
- [ ] 11. Validar E2E con flujo actual de spawn:
  - continuidad entre turnos
  - aislamiento por `src_ilk`
  - rehidratación tras restart
- [x] 12. Actualizar documentación:
  - `docs/AI_nodes_spec.md`
  - `docs/ai-nodes-deploy-runbook.md`
  - `docs/examples/ai_common_chat.config.json`
  - `docs/examples/ai_frontdesk_gov.config.json`
  - `docs/immediate-conversation-memory-spec.md` (nota de adopción en runner, si aplica)

## Decisiones cerradas (Punto 1)

- El cambio se implementa en `ai_node_runner` solo para `behavior.kind=openai_chat`.
- El runner pasa `FunctionRunInput` al SDK usando `run_with_input(...)`.
- El key canónico para la memoria inmediata es `src_ilk` (con `thread_id` como metadato auxiliar).
- Defaults iniciales adoptados:
  - `recent_interactions_max=10`
  - `active_operations_max=8`
  - `summary_max_chars=1600`

## Decisiones cerradas (Puntos 2-4)

- Modelo runtime inicial por `src_ilk`:
  - `recent_interactions`: ventana acotada de interacciones para continuidad.
  - `conversation_summary`: resumen corto estructurado para foco conversacional.
  - `active_operations`: lista opcional; en v1 del runner se inicializa vacía.
- Política de refresh de summary:
  - refrescar en eventos importantes y cada `N` turnos.
  - default inicial `summary_refresh_every_turns=3`.
  - si falla el refresh, continuar sin cortar la respuesta del turno.
- Config efectiva por nodo (opt-in, backward compatible):
  - `immediate_memory.enabled` (default `false`)
  - `immediate_memory.recent_interactions_max` (default `10`)
  - `immediate_memory.active_operations_max` (default `8`)
  - `immediate_memory.summary_max_chars` (default `1600`)
  - `immediate_memory.summary_refresh_every_turns` (default `3`)
  - `immediate_memory.trim_noise_enabled` (default `true`)

## Decisiones cerradas (Punto 7)

- `thread_state` y `immediate_memory` quedan separados por diseño y almacenamiento:
  - `thread_state`: store privado de tools (`thread_state_*`) para datos duros del flujo.
  - `immediate_memory`: store privado del runner para continuidad conversacional corta.
- No se modificó el contrato de `thread_state_*` ni su scoping actual por `src_ilk`.
- `immediate_memory` no reutiliza ni sobreescribe archivos del store de `thread_state`.

## Decisiones cerradas (Punto 8)

- La observabilidad de `immediate_memory` se implementa con logs orientados a evento:
  - carga de memoria (`memory_hit=true/false`)
  - persistencia de turno (cantidad persistida + límites)
  - fallas de lectura/escritura con degradación segura
- No se agregan logs periódicos ni loops de logging fijo para evitar ruido operativo.
- Estado de refresh de summary se reporta explícitamente como `not_implemented_v1` en esta fase.

## Decisiones cerradas (Punto 9)

- `scripts/deploy-ia-node.sh` incorpora flags opcionales para `runtime.immediate_memory.*`.
- El flujo actual basado en `--config-json` se mantiene sin cambios obligatorios.
- Los flags solo mutan el JSON de spawn cuando se pasan explícitamente.
