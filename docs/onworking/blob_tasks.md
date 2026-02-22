# Blob - Plan transversal de herramientas (2026-02-22)

Objetivo:
- definir y ejecutar un plan para manejo de blobs como capacidad transversal del sistema,
- habilitar payloads grandes sin romper el contrato actual (payload libre + router/OPA no inspeccionan payload),
- mantener separación: transporte/envelope vs datos binarios de negocio.

Base de spec usada:
- `docs/02-protocolo.md`:
  - sección 4 (`payload` libre),
  - sección 11.1/11.2 (inline `<64KB` y `blob_ref` para tamaños mayores).
- `docs/08-apendices.md`:
  - límite `Mensaje inline max = 64 KB`.
- `docs/07-operaciones.md`:
  - storage incluye carpeta `blob/` bajo `storage.path`.

## Estado actual (spec vs código)

Estado observado:
- existe contrato documental de `blob_ref` en protocolo.
- existe carpeta `blob/` en modelo operativo/documentación.
- no hay aún una implementación transversal cerrada en código para:
  - API de upload/download de blobs,
  - resolución automática de `blob_ref` en nodos,
  - sync/cache inter-isla de blobs,
  - métricas y diagnósticos dedicados de blobs.

Conclusión:
- el gap es mayormente de implementación operativa y tooling transversal, no de concepto en protocolo.

## Principios de diseño (para no bloquear desarrollo)

1. Envelope/transporte:
- sigue siendo problema de router + NATS (ya validado por suites actuales).

2. Payload/binario:
- es problema de productor/consumidor + capa de herramientas blob.

3. Compatibilidad:
- mantener soporte inline `<64KB`,
- promover a `blob_ref` cuando excede límite o por política de productor.

4. Desacople:
- el router no debe interpretar contenido blob; solo enruta mensajes.

## Volumen estimado (alto nivel)

- Bloque A (contrato y librería base): 2-3 PR.
- Bloque B (API admin + storage blob): 3-4 PR.
- Bloque C (sync inter-isla + caché): 2-3 PR.
- Bloque D (diagnóstico, métricas, hardening): 2-3 PR.

Estimación total:
- 9-13 PR medianos,
- esfuerzo aproximado: 3-5 semanas (1 dev full-time), según alcance de seguridad/sync WAN.

## Checklist detallado (orden recomendado)

### BLOB-A - Contrato mínimo y compatibilidad

- [ ] A1. Congelar `BlobRef` canónico en spec operativa:
  - `type=blob_ref`,
  - `blob_id` (`sha256:<hex>`),
  - `size`,
  - `mime`,
  - `spool_day` (opcional o derivable).
- [ ] A2. Definir errores canónicos de blob:
  - `BLOB_NOT_FOUND`,
  - `BLOB_TOO_LARGE`,
  - `BLOB_INVALID_ID`,
  - `BLOB_IO_ERROR`,
  - `BLOB_ACCESS_DENIED`.
- [ ] A3. Definir política explícita de inline vs referencia:
  - default `<64KB` inline,
  - `>=64KB` requerido como `blob_ref` (o auto-promoción por helper).
- [ ] A4. Definir política de retención/GC:
  - por TTL,
  - por referencia activa (si aplica),
  - por cuota de disco.

Criterio de salida:
- contrato operativo de `blob_ref` y errores cerrado en docs onworking + listo para pasar a spec formal.

### BLOB-B - Herramientas/librería transversal de blobs

- [ ] B1. Crear capa compartida (`blob toolkit`) reutilizable por nodos:
  - `put(bytes|file) -> blob_id`,
  - `get(blob_id) -> stream|file`,
  - `stat(blob_id)`,
  - `delete(blob_id)` (si política lo permite).
- [ ] B2. Persistencia local deduplicada por hash (`sha256`):
  - escritura atómica (`tmp + rename`),
  - verificación hash/tamaño al guardar.
- [ ] B3. Layout de disco bajo `storage.path/blob`:
  - particionado por prefijo hash (`ab/cd/<sha256>`),
  - metadatos mínimos (`size`, `mime`, `created_at`, `last_access`).
- [ ] B4. Tests unitarios de integridad:
  - dedupe,
  - hash mismatch,
  - recuperación ante write interrumpida.

Criterio de salida:
- librería usable sin endpoints HTTP aún, con tests robustos.

### BLOB-C - API operacional en SY.admin/SY.storage

- [ ] C1. Definir endpoints HTTP de blob (MVP):
  - `POST /blob` (upload),
  - `HEAD /blob/{blob_id}` (existencia/metadatos),
  - `GET /blob/{blob_id}` (download),
  - `DELETE /blob/{blob_id}` (opcional por política).
- [ ] C2. Integrar rutas en `sy-admin` y backend en `sy-storage` (owner del storage).
- [ ] C3. Validaciones de input:
  - límite de tamaño configurable,
  - `mime` permitido (si aplica),
  - `blob_id` estricto.
- [ ] C4. Respuestas en envelope admin estándar:
  - `status`, `action`, `payload`, `error_code`, `error_detail`.

Criterio de salida:
- API blob funcional local (single-hive) con cobertura básica E2E.

### BLOB-D - Sync/caché inter-isla (mother/worker)

- [ ] D1. Definir estrategia de distribución:
  - pull on-demand (worker pide a mother al resolver `blob_ref` faltante),
  - prefetch opcional para flujos críticos.
- [ ] D2. Implementar caché local en worker:
  - descarga al primer uso,
  - actualización de `last_access`.
- [ ] D3. Integrar con rutas WAN existentes:
  - errores explícitos por conectividad,
  - reintentos con backoff.
- [ ] D4. Definir comportamiento offline:
  - fallback cuando blob no está en caché y mother no accesible.

Criterio de salida:
- `blob_ref` usable en despliegue mother+worker sin NFS compartido obligatorio.

### BLOB-E - Seguridad y política de acceso

- [ ] E1. Definir autorización de operaciones blob:
  - quién puede subir,
  - quién puede leer,
  - quién puede borrar.
- [ ] E2. Revisar superficie de exposición:
  - evitar path traversal,
  - evitar enumeración de IDs,
  - rate limiting básico en endpoints blob.
- [ ] E3. Auditar trazabilidad:
  - actor, blob_id, acción, tamaño, resultado.

Criterio de salida:
- modelo de acceso y seguridad documentado + controles mínimos implementados.

### BLOB-F - Diagnóstico, métricas y suites E2E

- [ ] F1. Agregar métricas blob:
  - `blob_put_total`, `blob_get_total`, `blob_get_hit`, `blob_get_miss`,
  - `blob_bytes_stored`, `blob_bytes_transferred`,
  - `blob_gc_deleted_total`, `blob_gc_reclaimed_bytes`.
- [ ] F2. Agregar script/binario de diagnóstico blob E2E:
  - upload -> ref -> consume -> download -> verify hash.
- [ ] F3. Agregar escenario de restart/replay con `blob_ref` en suite (perf opt-in).
- [ ] F4. Integrar resultados en `docs/onworking/diagnostics_tasks.md`.

Criterio de salida:
- observabilidad y diagnóstico blob comparable a NATS/JetStream actuales.

## Riesgos y mitigaciones

Riesgos:
- crecimiento de storage sin política clara de GC,
- latencia extra por fetch remoto de blobs en worker,
- inconsistencias entre metadata y archivo físico.

Mitigaciones:
- quotas + GC desde MVP,
- caché local con métricas hit/miss,
- verificación hash al leer/escribir + tareas de scrub opcionales.

## Secuencia sugerida para avanzar sin bloquearse

1. Cerrar `BLOB-A`.
2. Implementar `BLOB-B` (toolkit local).
3. Exponer `BLOB-C` (API single-hive).
4. Cerrar `BLOB-F` mínimo (diag local).
5. Implementar `BLOB-D` y `BLOB-E` para despliegue multi-hive endurecido.
