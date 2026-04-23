# AI Generic

Home actual del runner AI genérico.

Este crate reemplaza la ubicación anterior en `crates/fluxbee_ai_nodes`.

Notas:
- el nombre del paquete se conserva como `fluxbee-ai-nodes` para no romper scripts existentes,
- el bin principal sigue siendo `ai_node_runner`,
- `ai_local_probe` se conserva como bin auxiliar.
- el runtime/package actual del runner genérico se publica como `ai.common`.
- un nodo vivo usa nombre L2 completo, por ejemplo `AI.chat@motherbee`.

Control-plane y secrets:
- `CONFIG_GET` expone contrato/config redacted del nodo.
- `CONFIG_SET` aplica config funcional del runner.
- la OpenAI key canónica entra por `config.secrets.openai.api_key`.
- el secreto se persiste localmente en `secrets.json`.

Build desde raíz:

```bash
cargo check -p fluxbee-ai-nodes --bins
```
