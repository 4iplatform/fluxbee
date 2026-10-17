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
- `ai.generic` es el único runtime AI; los nodos `AI.*` son instancias suyas.
- las instancias (`AI.sales@motherbee`, etc.) no viven en el repo ni arrancan con la instalación
  base: se crean con `run_node` cuando hacen falta.
- `SY.frontdesk.gov` no es un runtime: es un nodo de sistema del core (`nodes/gov/ai-frontdesk-gov`).
- acá viven solo los fuentes del runtime y sus especializaciones.

Nota:
- `ai.generic` no tiene modos: el frontdesk de identidad es otro binario (`SY.frontdesk.gov`).
- `ai.generic` es el runtime base actual para agentes AI configurables; el "alma" cognitiva se carga por identidad con hashes de role/skill/handbook.

Contrato operativo actual:
- configuración funcional ví­a `CONFIG_GET` / `CONFIG_SET`.
- secrets de provider persistidos localmente en `secrets.json`, no en `hive.yaml`.
- el campo canónico actual para OpenAI es `config.secrets.openai.api_key`.

Referencias:
- [`docs/AI_nodes_spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/AI_nodes_spec.md)
- [`docs/node-config-control-plane-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/node-config-control-plane-spec.md)
- [`docs/onworking COA/node-secret-config-spec.md`](/Users/cagostino/Documents/GitHub/fluxbee/docs/onworking%20COA/node-secret-config-spec.md)
