# AI Generic

Home actual del runner AI genérico.

Este crate reemplaza la ubicación anterior en `crates/fluxbee_ai_nodes`.

Notas:
- el nombre del paquete se conserva como `fluxbee-ai-nodes` para no romper scripts existentes,
- el bin principal sigue siendo `ai_node_runner`,
- `ai_local_probe` se conserva como bin auxiliar.
- el runtime/package actual del runner genérico se publica como `ai.generic`.
- un nodo vivo usa nombre L2 completo, por ejemplo `AI.chat@motherbee`.

Control-plane y secrets:
- `CONFIG_GET` expone contrato/config redacted del nodo.
- `CONFIG_SET` aplica config funcional del runner.
- la OpenAI key se resuelve exclusivamente desde `SY.vault` con `resource_type=openai`.
- `CONFIG_SET` rechaza campos secret-bearing (`config.secrets.*`, `behavior.api_key`, `behavior.openai`, `behavior.api_key_env`).

Definición cognitiva:
- el nodo arranca con config operativa y prompt default.
- luego lee su propio ILK desde identity SHM por `handler_node`.
- si el ILK tiene hashes de role/skill/handbook, carga assets desde `blob://agent-assets/<hash>.json`.
- recompone el prompt activo sin restart cuando cambia la seq/hash de identity SHM.
- `CONFIG_GET` reporta `definition_state`, hashes cargados/fallidos, truncación y tamaño del prompt.

Build desde raíz:

```bash
cargo check -p fluxbee-ai-nodes --bins
```
