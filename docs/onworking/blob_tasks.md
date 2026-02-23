# Blob - Backlog transversal (2026-02-23)

Alcance acordado:
- `SY.storage` no participa en blobs.
- No se implementa API HTTP de blobs.
- Primero toolkit blob agnóstico del medio; sync con Syncthing va al final.

Referencias:
- `docs/02-protocolo.md` (inline `<64KB` y `blob_ref`)
- `docs/07-operaciones.md` (`storage.path/blob`)

## Fase 1 - Toolkit base

### BLOB-T1 - Contrato mínimo
- [ ] T1. Cerrar `BlobRef` canónico (`type`, `blob_id`, `size`, `mime`, `spool_day` opcional).
- [ ] T2. Cerrar errores canónicos (`BLOB_NOT_FOUND`, `BLOB_INVALID_ID`, `BLOB_IO_ERROR`, `BLOB_ACCESS_DENIED`, `BLOB_TOO_LARGE`).
- [ ] T3. Definir política inline vs `blob_ref`.

Salida:
- contrato estable para implementación y E2E.

### BLOB-T2 - Librería filesystem
- [ ] T4. API interna: `put`, `get`, `stat`, `delete`.
- [ ] T5. Dedupe por `sha256` y validación de tamaño/hash.
- [ ] T6. Escritura atómica (`tmp + rename`) + recovery de temporales.
- [ ] T7. Layout de disco por prefijo hash bajo `blob.path`.

Salida:
- toolkit reutilizable por cualquier nodo/proceso.

### BLOB-T3 - Robustez
- [ ] T8. Tests unitarios de integridad y concurrencia.
- [ ] T9. Retry/backoff para resolución de `blob_ref` en consumidor.
- [ ] T10. Timeouts y mapeo de errores final.

Salida:
- lectura determinista bajo retraso de disponibilidad de archivo.

## Fase 2 - Operación y diagnóstico

### BLOB-O1 - E2E base
- [ ] O1. E2E productor→consumidor con `blob_ref` en filesystem local.
- [ ] O2. E2E con volumen compartido (NFS/infra) sin cambios de código blob.
- [ ] O3. E2E de errores (`not found`, corrupción, permisos).

Salida:
- flujo blob estable en entorno real.
- métricas/diagnóstico blob centralizados en `docs/onworking/diagnostics_tasks.md` (sección Blob Stats).

## Fase 3 - Sync opcional con Syncthing

### BLOB-S1 - Integración en runtime
- [ ] S1. Config runtime: `blob.sync.enabled`.
- [ ] S2. Orchestrator: install/start/health/restart de Syncthing.
- [ ] S3. Documentar puertos, permisos y rutas de operación.

Salida:
- Syncthing mantenido por orchestrator como herramienta auxiliar.

### BLOB-S2 - E2E con sync activo
- [ ] S4. E2E blob con `sync.enabled=true`.
- [ ] S5. Verificar que el contrato blob no cambia entre `sync.enabled=false` y `sync.enabled=true`.

Salida:
- mismo toolkit blob, con o sin capa de sync.

## Fuera de alcance
- API HTTP de blobs (`POST/GET/DELETE /blob`).
- ownership blob en `SY.storage`.
