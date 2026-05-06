# AI Nodes

Fuentes de nodos AI del repo.

Convención actual:
- `common/`: código compartido de la familia AI.
- `ai-generic/`: runner AI base e instanciable.

Frontera de dependencias (decisión vigente):
- `nodes/ai/*` no debe depender de `nodes/gov/*`.
- `SY.frontdesk.gov` no debe consumir `nodes/ai/common`.
- `nodes/ai/common` es exclusivo de la familia AI no-gov.

Regla:
- `ai.generic`, `ai.chat`, `sy.frontdesk.gov` son nombres de runtime/package.
- las instancias (`AI.chat@motherbee`, etc.) no viven en el repo.
- acá viven solo los fuentes del runtime y sus especializaciones.

Nota:
- La separación de comportamiento es por runtime (`ai.generic` vs `sy.frontdesk.gov`), no por `AI_NODE_MODE`.
- `ai.generic` es el runtime base actual para agentes AI configurables; el "alma" cognitiva se carga por identidad con hashes de role/skill/handbook.

Contrato operativo actual:
- configuración funcional ví­a `CONFIG_GET` / `CONFIG_SET`.
- secrets de provider persistidos localmente en `secrets.json`, no en `hive.yaml`.
- el campo canónico actual para OpenAI es `config.secrets.openai.api_key`.

Referencias:
- [`docs/AI_nodes_spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/AI_nodes_spec.md)
- [`docs/node-config-control-plane-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/node-config-control-plane-spec.md)
- [`docs/onworking COA/node-secret-config-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/onworking%20COA/node-secret-config-spec.md)
