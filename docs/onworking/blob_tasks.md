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

## Estado de control (2026-03-09)

Fuente única de backlog Blob:
- este archivo (`docs/onworking/blob_tasks.md`) es la lista activa y priorizada.

Documentos complementarios (no duplican backlog):
- `docs/onworking/blob_node_examples.md`: ejemplos y propuesta de API SDK (sin checklist de ejecución).
- `docs/onworking/diagnostics_tasks.md`: inventario/operación de suites E2E y métricas.
- `docs/onworking/sy_orchestrator_v2_tasks.md`: backlog v2 de orchestrator (historial de cierre ya ejecutado para sync blob/dist base).

Alineación para evitar colisiones:
- `BLOB-X10/X11` se coordina con TODO de observabilidad en `diagnostics_tasks.md` (sin crear checklist paralelo).
- `BLOB-X12..X15` se alinea con `SYSTEM_SYNC_HINT` documentado en:
  - `docs/02-protocolo.md` (7.8.4, propuesto v2.x),
  - `docs/onworking/SY.orchestrator — Spec de Cambios v2.md` (5.3/5.4).

## Lista activa inmediata (orden de ejecución)

Track A (objetivo funcional de propagación confirmada):
- [x] A1. Implementar `BLOB-X14` (`SYSTEM_SYNC_HINT` en orchestrator; canal `blob` y `dist`).
  - Avance (2026-03-09): implementado handler system en `sy-orchestrator` (`SYSTEM_SYNC_HINT`/`SYSTEM_SYNC_HINT_RESPONSE`) con estados `ok/sync_pending/error`, trigger de scan por folder y espera opcional `wait_for_idle`.
  - Avance (2026-03-09): expuesto endpoint admin `POST /hives/{id}/sync-hint` y script E2E `scripts/orchestrator_sync_hint_api_e2e.sh`.
- [x] A2. Implementar `BLOB-X12` (API SDK unificada `publish_blob_and_confirm`).
  - Avance (2026-03-09): `fluxbee_sdk::blob::BlobToolkit` agrega `publish_blob_and_confirm(&NodeSender, &mut NodeReceiver, PublishBlobRequest)` con flujo unificado `put_bytes -> promote -> SYSTEM_SYNC_HINT`.
  - Avance (2026-03-09): la API confirma `status=ok` por target (`SY.orchestrator@<hive>` o `<hive>`), con retry sobre `sync_pending` y timeout/error explícito.
- [x] A3. Implementar `BLOB-X13` (gate de envío: sin confirmación no se emite `blob_ref`).
  - Avance (2026-03-09): `blob_sync_diag` productor soporta `BLOB_DIAG_CONFIRM_TARGETS` y usa `publish_blob_and_confirm`; en ese modo no emite `BLOB_REF_JSON` hasta confirmación `ok`.
  - Avance (2026-03-09): `scripts/blob_sync_multi_hive_e2e.sh` activa gate por defecto (`BLOB_DIAG_CONFIRM_REQUIRED=1`) apuntando al `WORKER_HIVE_ID`.
- [x] A4. Implementar `BLOB-X15` (pipeline `dist`: publish -> hint/confirm -> `SYSTEM_UPDATE`).
  - Avance (2026-03-09): `scripts/blob_sync_multi_hive_e2e.sh` agrega paso explícito `SYSTEM_SYNC_HINT(channel=dist)` para local+worker antes de `SYSTEM_UPDATE`.
  - Avance (2026-03-09): `scripts/orchestrator_system_update_api_e2e.sh` valida pipeline con `sync-hint dist -> SYSTEM_UPDATE`.
  - Avance (2026-03-09): `docs/14-runtime-rollout-motherbee.md` actualizado con paso operativo de `sync-hint` previo a `update`.

Track B (cierre operativo y hardening):
- [ ] B1. Resolver `BLOB-X4/X5` (owner/permisos y política explícita de usuario Syncthing).
- [ ] B2. Resolver `BLOB-X6/X7` (GC de `staging/active`).
- [ ] B3. Resolver `BLOB-X8/X9` (límites + errores de contrato).
- [ ] B4. Resolver `BLOB-X10/X11` (métricas + diagnóstico agregable).

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
- [x] X3. E2E real multi-isla (sin modo `copy`): archivo en isla A visible en isla B por Syncthing y consumo con `resolve_with_retry`.
  - Avance (2026-03-09): `scripts/blob_sync_multi_hive_e2e.sh` migrado a flujo productivo (`SYSTEM_UPDATE` + `run_node` local/worker), sin SSH, validando consumer en worker por `resolve_with_retry`.
  - Avance (2026-03-09): hardening del runtime Syncthing en orchestrator para auto-reparar markers `.stfolder` y validar salud por folder (`fluxbee-blob`, `fluxbee-dist`) en watchdog.

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

### BLOB-X6 - Publicación con confirmación (SDK + Orchestrator)
- [x] X12. Definir API SDK unificada para productor Blob (`put/promote + sync hint + confirmación`) en una sola llamada.
  - Avance (2026-03-09): implementado `publish_blob_and_confirm` en `fluxbee_sdk::blob` con request/response tipados y confirmación por target vía `SYSTEM_SYNC_HINT`.
- [x] X13. Asegurar semántica de entrega: el productor no emite mensaje con `blob_ref` hasta confirmar propagación al hive destino (o timeout explícito).
  - Avance (2026-03-09): gate operativo aplicado en runtime/script diagnóstico multi-hive (`blob_sync_diag` + `blob_sync_multi_hive_e2e.sh`) usando `SYSTEM_SYNC_HINT`.
- [x] X14. Definir acción system en orchestrator para `sync hint` por canal (`blob`/`dist`) sin usar SSH, con respuesta estructurada y retry.
  - Avance (2026-03-09): `sy-orchestrator` maneja `SYSTEM_SYNC_HINT`/`SYSTEM_SYNC_HINT_RESPONSE` con estados `ok/sync_pending/error`, scan por folder y espera opcional `wait_for_idle`.
- [x] X15. Integrar `sync hint` en el pipeline de distribución de software (`dist`) operado por plataforma/Admin: publish en motherbee -> hint/confirm en destinos -> `SYSTEM_UPDATE`.
  - Avance (2026-03-09): pipeline aplicado y validado en scripts E2E/operativos (`blob_sync_multi_hive_e2e.sh`, `orchestrator_system_update_api_e2e.sh`) y guía de rollout (`docs/14-runtime-rollout-motherbee.md`).

Salida:
- sincronización por evento (además de watchdog periódico), menor latencia y contrato operativo consistente para blobs y distribución de software.
- contrato explícito de propagación confirmada: sin confirmación no se emite mensaje (`blob`) ni se instala versión (`dist`).

## Fuera de alcance
- API HTTP de blobs.
- Transferencia de blobs por mensaje/router/NATS.
- Gestión de ownership de blobs en `SY.storage`.
