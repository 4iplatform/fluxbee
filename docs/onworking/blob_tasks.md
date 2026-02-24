# Blob - Backlog transversal (2026-02-24)

Alcance acordado:
- `SY.storage` no administra blobs; solo persiste metadata de mensajes.
- No se implementa API HTTP de blobs.
- El camino oficial de desarrollo es `fluxbee_sdk` (migración detallada en `docs/onworking/sdk_tasks.md`).
- Sync multi-isla se resuelve con Syncthing gestionado por orchestrator (sin cambiar contrato blob).

Referencias:
- `docs/blob-annex-spec.md`
- `docs/02-protocolo.md`
- `docs/07-operaciones.md`
- `docs/onworking/sdk_tasks.md`
- `docs/onworking/diagnostics_tasks.md`
- `docs/onworking/blob_node_examples.md`

## Fase B1 - Contrato canónico en SDK

### BLOB-A1 - Modelo de datos
- [x] A1. Alinear `BlobRef` canónico (`type=blob_ref`, `blob_name`, `size`, `mime`, `filename_original`, `spool_day`).
- [x] A2. Alinear `BlobConfig`, `BlobStat`, `ResolveRetryConfig` y constantes del módulo.
- [x] A3. Implementar validaciones de contrato (`blob_name` y campos requeridos).

Salida:
- API estable de `fluxbee_sdk::blob` acorde a spec.

### BLOB-A2 - Naming y sanitización
- [x] A4. Implementar sanitización de nombre fuente (allowed set, colapso `_`, truncado, fallback `blob`).
- [x] A5. Implementar `blob_name` canónico `<name>_<hash16>.<ext>`.
- [x] A6. Implementar `prefix(blob_name)` (2 chars del hash embebido).

Salida:
- naming determinista y compatible con FS.

## Fase B2 - Toolkit filesystem (staging/active)

### BLOB-B1 - Escritura y promote
- [x] B1. Implementar `put(source_path, original_filename)` escribiendo en `staging/<prefix>/`.
- [x] B2. Implementar `put_bytes(data, original_filename, mime)` para productores en memoria.
- [x] B3. Implementar `promote(blob_ref)` con `rename` atómico `staging -> active`.

Salida:
- flujo productor completo listo para uso de nodos.

### BLOB-B2 - Lectura y retry
- [x] B4. Implementar `resolve`, `exists`, `stat` sobre `active/`.
- [x] B5. Implementar `resolve_with_retry` async con backoff exponencial.
- [x] B6. Definir timeout defaults y mapeo de errores canónicos.

Salida:
- consumo robusto en single-isla y multi-isla.

## Fase B3 - Integración con payload de nodos

### BLOB-C1 - Contrato `text/v1`
- [x] C1. Helpers para payload `text/v1` (`content` + `attachments[]`).
- [x] C2. Regla de 64KB: soporte `content_ref` cuando texto no entra inline.
- [x] C3. Ejemplos y contratos de uso en nodos IO/AI/WF.

Salida:
- contrato de aplicación consistente para attachments.

## Fase B4 - Pruebas

### BLOB-D1 - Unit/integration
- [x] D1. Unit tests: sanitización, `blob_name`, `prefix`, errores.
- [x] D2. Integration tests: `put -> promote -> resolve`.
- [x] D3. Integration tests: retry/backoff con archivo tardío.

Salida:
- garantía funcional del módulo blob.

### BLOB-D2 - E2E operativos
- [x] D4. E2E local productor->consumidor con `blob_ref`.
- [x] D5. E2E errores (`BLOB_NOT_FOUND`, permisos, IO).
- [ ] D6. Incorporar métricas/resultados en `docs/onworking/diagnostics_tasks.md`.

Salida:
- validación operativa documentada.

## Fase B5 - Syncthing (multi-isla)

### BLOB-S1 - Operación
- [ ] S1. Config runtime para habilitar sync por herramienta externa.
- [ ] S2. Orchestrator: install/start/health/restart de Syncthing.
- [ ] S3. Documentar rutas, permisos y puertos operativos.

Salida:
- sync multi-isla gestionado por orchestrator.

### BLOB-S2 - E2E multi-isla
- [ ] S4. E2E con Syncthing activo + `resolve_with_retry`.
- [ ] S5. Verificar invariancia del contrato blob con/ sin sync.

Salida:
- mismo contrato blob en ambos modos de operación.

## Fuera de alcance
- API HTTP de blobs.
- Transferencia de blobs por mensaje/router/NATS.
- Gestión de ownership de blobs en `SY.storage`.
