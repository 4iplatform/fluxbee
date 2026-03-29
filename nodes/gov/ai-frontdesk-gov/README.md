# AI.frontdesk.gov

Este directorio quedó temporalmente alineado con el runner AI genérico.

Estado actual:
- contiene una copia del código fuente de `nodes/ai/ai-generic`,
- se mantiene separado solo por orden de repo y ownership,
- el equipo de dev deberá volver a especializarlo para el caso `gov`.

Importante:
- esta duplicación es deliberada y transitoria,
- no expresa todavía una frontera de arquitectura limpia,
- sirve para sacar el runner AI de `crates/` y ubicar los fuentes bajo `nodes/`.

Contrato operativo actual:
- mantiene el mismo `CONFIG_GET` / `CONFIG_SET` que `nodes/ai/ai-generic`.
- persiste la OpenAI key en `secrets.json`.
- usa el campo canónico `config.secrets.openai.api_key`.
- conserva compatibilidad temporal con aliases legacy mientras migra.

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
