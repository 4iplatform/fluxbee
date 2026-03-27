# Node Source Reorg Checklist

Objetivo: reorganizar solo los fuentes de nodos bajo `nodes/`, sin tocar por ahora la estructura del core ni de los SDK compartidos.

## Convención cerrada

- [x] Mantener `crates/fluxbee_sdk`
- [x] Mantener `crates/fluxbee_ai_sdk`
- [x] Organizar los fuentes de nodos bajo `nodes/`
- [x] Usar cuatro familias:
  - `nodes/ai`
  - `nodes/io`
  - `nodes/gov`
  - `nodes/wf`
- [x] Usar `nodes/<type>/common` como espacio de código compartido por familia
- [x] Usar `nodes/ai/ai-generic` como runtime AI base
- [x] Usar `nodes/wf/wf-generic` como runtime WF base
- [x] Mantener `nodes/io/<node>` para nodos IO con código específico
- [x] Mantener `nodes/gov/<node>-gov` para nodos GOV con código específico

## Cambios realizados

- [x] Se creó `nodes/ai/`
- [x] Se creó `nodes/ai/common/`
- [x] Se creó `nodes/ai/ai-generic/`
- [x] Se creó `nodes/wf/`
- [x] Se creó `nodes/wf/common/`
- [x] Se creó `nodes/wf/wf-generic/`
- [x] Se movió de facto el runner AI desde `crates/fluxbee_ai_nodes` a `nodes/ai/ai-generic`
- [x] Se actualizó el workspace root para apuntar a `nodes/ai/ai-generic`
- [x] Se sobrescribió `nodes/gov/ai-frontdesk-gov` con una copia temporal del runner AI genérico
- [x] Se dejó documentación mínima en `nodes/ai` y `nodes/wf`
- [x] Se documentó el control-plane común de nodos no-`SY` en `docs/node-config-control-plane-spec.md`
- [x] Se creó `nodes/common/README.md` como punto de entrada para contratos transversales de nodos

## Decisiones temporales explícitas

- [x] `nodes/gov/ai-frontdesk-gov` quedó duplicado respecto de `nodes/ai/ai-generic`
- [x] El nombre del paquete `fluxbee-ai-nodes` se conservó en `nodes/ai/ai-generic` para no romper scripts actuales
- [x] `nodes/ai/common` y `nodes/wf/common` quedaron como placeholders de estructura, no como crates productivos todavía
- [x] `nodes/wf/wf-generic` quedó creado como placeholder, sin mover implementación aún

## Pendiente para dev

- [ ] Limpiar duplicación entre `nodes/ai/ai-generic` y `nodes/gov/ai-frontdesk-gov`
- [ ] Reintroducir comportamiento GOV real dentro de `nodes/gov/ai-frontdesk-gov`
- [x] Revisar documentación que todavía referencia `crates/fluxbee_ai_nodes`
- [ ] Seguir corrigiendo documentación funcional que todavía asume `AI.chat` como runtime en vez de instancia
- [ ] Decidir si `nodes/ai/common` y `nodes/wf/common` pasan a crates reales
- [ ] Decidir si `nodes/test` se mantiene dentro de `nodes/` o se mueve a otro árbol
