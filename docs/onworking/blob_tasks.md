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
- [x] D6. Incorporar métricas/resultados en `docs/onworking/diagnostics_tasks.md`.

Salida:
- validación operativa documentada.

## Fase B5 - Syncthing (multi-isla)

### BLOB-S1 - Operación
- [x] S1. Config runtime para habilitar sync por herramienta externa.
- [x] S2. Orchestrator: install/start/health/restart de Syncthing.
- [x] S3. Documentar rutas, permisos y puertos operativos.

Salida:
- sync multi-isla gestionado por orchestrator.

### BLOB-S2 - E2E multi-isla
- [x] S4. E2E con Syncthing activo + `resolve_with_retry`.
- [x] S5. Verificar invariancia del contrato blob con/ sin sync.

Salida:
- mismo contrato blob en ambos modos de operación.

## Fase B6 - Cierre funcional pendiente (gap spec vs implementación)

Contexto:
- El backlog B1..B5 quedó cerrado en modo operativo.
- Quedan brechas para considerar Blob "cerrado al 100%" contra `blob-annex-spec.md` y operación real multi-isla.

### BLOB-X1 - Sincronización real de `active/` entre islas
- [x] X1. Configurar topología Syncthing gestionada por orchestrator (devices/folders) en add/remove hive.
  - Avance (2026-03-09): `add_hive` persiste `syncthing_device_id` del worker en `info.yaml` y `remove_hive` desmonta peer Syncthing local (top-level + folders `fluxbee-blob` / `fluxbee-dist`) antes del cleanup local.
- [x] X2. Garantizar que Syncthing replique solo `blob/active` (no `staging`).
  - Avance (2026-03-09): reconciliación de carpeta `fluxbee-blob` en orchestrator apuntando a `<blob.path>/active` (local + peer link), y bootstrap remoto creando `active/` + `staging/`.
- [ ] X3. E2E real multi-isla (sin modo `copy`): archivo en isla A visible en isla B por Syncthing y consumo con `resolve_with_retry`.
  - Avance (2026-03-09): script `scripts/blob_sync_multi_hive_e2e.sh` agregado (producer local en motherbee + consumer remoto en worker con `resolve_with_retry`, sin `copy mode`).

Salida:
- replicación de blobs validada end-to-end con Syncthing real entre hives.

### BLOB-X2 - Política de filesystem y seguridad
- [ ] X4. Alinear owner/permisos de blob con spec (`fluxbee:fluxbee`, dirs `750`, files `640`) en mother/worker.
- [ ] X5. Eliminar fallback silencioso a root para Syncthing o formalizar política explícita y validable.

Salida:
- postura de seguridad de Blob consistente y verificable.

### BLOB-X3 - GC y housekeeping
- [ ] X6. Implementar cleanup de `staging/` huérfano por TTL (24h) con dry-run + modo apply.
- [ ] X7. Definir/implementar GC de `active/` por `spool_day` (fase inicial conservadora, sin borrar referencias recientes).

Salida:
- control de crecimiento de storage Blob en operación continua.

### BLOB-X4 - Contrato de errores y límites
- [ ] X8. Implementar política de tamaño máxima configurable y emitir `BLOB_TOO_LARGE`.
- [ ] X9. Agregar tests de contrato para `BLOB_TOO_LARGE`, `BLOB_NOT_FOUND` con retry agotado, y fallas de permisos.

Salida:
- errores Blob completos y comprobados contra el contrato.

### BLOB-X5 - Observabilidad de Blob
- [ ] X10. Exponer métricas mínimas de Blob (`blob_put_total`, `blob_resolve_total`, `blob_resolve_retry_total`, `blob_errors_total`, bytes).
- [ ] X11. Agregar diagnóstico E2E Blob multi-isla al paquete de diagnósticos operativos.

Salida:
- visibilidad operativa suficiente para soporte y capacity planning.

## Fuera de alcance
- API HTTP de blobs.
- Transferencia de blobs por mensaje/router/NATS.
- Gestión de ownership de blobs en `SY.storage`.
