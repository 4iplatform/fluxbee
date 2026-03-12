# Onworking - Índice por proceso

Documentos operativos activos organizados por proceso/componente:

- `docs/onworking/sy_admin_tasks.md`
- `docs/onworking/sy_orchestrator_v2_tasks.md`
- `docs/onworking/sy_orchestrator_tasks.md`
- `docs/onworking/sy_router_tasks.md`
- `docs/onworking/sy_storage_tasks.md`
- `docs/onworking/nats_tasks.md`
- `docs/onworking/blob_tasks.md`
- `docs/onworking/sdk_tasks.md`
- `docs/onworking/blob_node_examples.md`
- `docs/onworking/orchestrator_frictions.md`

Convención aplicada:
- nodos/procesos: archivo por proceso con patrón `<proceso>_tasks.md` (ej. `sy_admin_tasks.md`, `ai_<name>_tasks.md`).
- capacidades transversales: archivo `<capacidad>_tasks.md` sin prefijo de nodo (ej. `nats_tasks.md`).
- cada archivo concentra tareas, notas técnicas y checklists del ámbito correspondiente.

Documentos transversales (no de un solo proceso):

- `docs/onworking/CHANGELOG-v1.15-to-v1.16.md`
- `docs/onworking/MIGRATION-v1.16-storage-first.md`
- `docs/onworking/diagnostics_tasks.md`
- `docs/onworking/blob_tasks.md`
- `docs/onworking/sdk_tasks.md`
- `docs/onworking/blob_node_examples.md`
- `docs/onworking/orchestrator_frictions.md`

Nota:
- `docs/onworking/sy_orchestrator_tasks.md` y `docs/onworking/CHANGELOG-v1.15-to-v1.16.md` se mantienen como históricos.
- Para operación vigente de orchestrator usar `docs/onworking/sy_orchestrator_v2_tasks.md` y `docs/onworking/SY.orchestrator — Spec de Cambios v2.md`.
- Para Blob, la fuente única de backlog es `docs/onworking/blob_tasks.md` (mientras que `blob_node_examples.md` y `diagnostics_tasks.md` son documentos complementarios).
