# AI.frontdesk.gov

Este directorio quedó temporalmente alineado con el runner AI genérico.

Estado actual:
- contiene una copia del código fuente de `nodes/ai/ai-generic`,
- se mantiene separado solo por orden de repo y ownership,
- el equipo de dev deberá volver a especializarlo para el caso `gov`.

Frontera de dependencias (objetivo):
- `AI.frontdesk.gov` no debe depender de `nodes/ai/common`.
- La capa compartida de gov debe vivir en `nodes/gov/common` + SDKs (`fluxbee_sdk`, `fluxbee_ai_sdk`).
- `AI.common` y `AI.frontdesk.gov` no deben importarse entre sí.

Importante:
- esta duplicación es deliberada y transitoria,
- no expresa todavía una frontera de arquitectura limpia,
- sirve para sacar el runner AI de `crates/` y ubicar los fuentes bajo `nodes/`.

Contrato operativo actual:
- mantiene el mismo `CONFIG_GET` / `CONFIG_SET` que `nodes/ai/ai-generic`.
- persiste la OpenAI key en `secrets.json`.
- usa el campo canónico `config.secrets.openai.api_key`.
- conserva compatibilidad temporal con aliases legacy mientras migra.
- si `behavior.instructions` se omite, el runtime usa un prompt base propio de frontdesk embebido en el runner.
- `behavior.instructions` debe leerse ahora como override opcional, no como requisito para que el nodo tenga identidad funcional.

## Ejecutar local

Desde raíz del repo:

```bash
cargo run -p ai-frontdesk-gov --bin ai_node_runner
```

## Nota para dev

Pendiente posterior a esta reorganización:
- reintroducir comportamiento específico `gov`,
- decidir qué parte vuelve a `nodes/gov/common`,
- eliminar la duplicación con `nodes/ai/ai-generic`.

Prompt/behavior de frontdesk:
- El prompt funcional de frontdesk debe viajar con el runtime `AI.frontdesk.gov` (artefacto/versionado propio).
- No debe depender de mutaciones ad-hoc en runtime común.
- Implementación actual:
  - existe un prompt base runtime-owned en el runner de `AI.frontdesk.gov`
  - la key de provider sigue entrando temporalmente por `CONFIG_SET` y se persiste en `secrets.json`
  - la decisión final sobre surface/privilegios para esa key queda pendiente de `CORE`
