# SY.orchestrator v2 - Backlog limpio (spec-first, sin legacy)

Fecha de auditoria: 2026-03-07
Fuente de verdad funcional: `docs/onworking/SY.orchestrator — Spec de Cambios v2.md`

## 1. Reglas inmutables (acordadas)

- No backward compatibility.
- No modo legacy.
- SSH solo para bootstrap de `add_hive` cuando el worker no tiene orchestrator operativo.
- Si el worker ya esta online por socket, no se usa SSH.
- Si no comunica por socket para operacion, se trata como no existente/no disponible (sin fallback SSH operativo).

## 2. Estado actual real (codigo)

### 2.1 Ya alineado

- [x] `add_hive`/`remove_hive` solo motherbee.
- [x] Operacion remota (`run_node`/`kill_node`/`update`) por socket L2.
- [x] Lectura remota de versiones (`get_versions`/`list_versions`) por socket L2 (sin SSH operativo).
- [x] `remove_hive` sin fallback SSH: online por socket o cleanup local.
- [x] `add_hive` socket-only no cae a bootstrap SSH si falla finalize (`FINALIZE_FAILED`).
- [x] Se elimino compatibilidad `RUNTIME_UPDATE` en `sy_orchestrator`.
- [x] Se eliminaron aliases legacy de payload en update y nodos:
  - update: `version/hash` (solo `manifest_version/manifest_hash`)
  - nodos: `name` y `version` (solo `node_name` y `runtime_version`)

### 2.2 Desalineaciones abiertas (codigo)

- [ ] Persisten rutas/funciones de dist/core/vendor con fallback legacy en filesystem (`/var/lib/fluxbee/{runtimes,core,vendor}`) en vez de solo `dist/`.
- [ ] Persiste contrato de routers solo en lectura (`list_routers`/`GET /hives/{id}/routers`); acciones mutantes legacy ya removidas.
- [ ] Persisten bloques de codigo muerto/legacy en capas compartidas (`lib`) y algunos textos de compatibilidad, aunque se removio el bloque SSH legacy de sync remoto en orchestrator.
- [ ] Persisten textos/docs internas que hablan de fallback/compat legacy y no reflejan la politica estricta.

## 3. Backlog de limpieza obligatoria (hard-delete)

## Fase A - Contrato estricto (payload/API)

- [x] A1. Eliminar `RUNTIME_UPDATE` del runtime de mensajes (`sy_orchestrator`).
- [x] A2. Eliminar alias `version/hash` en update (`sy_admin` + `sy_orchestrator`).
- [x] A3. Eliminar alias `name/version` en `run_node`/`kill_node`.
- [x] A4. Endurecer validacion HTTP para rechazar payloads legacy con error explicito (`INVALID_REQUEST`) y mensaje canonico.

## Fase B - Eliminar legacy filesystem/sync

- [ ] B1. Quitar fallback de manifests fuera de `dist/`:
  - `local_core_manifest_path` (sin fallback a `CORE_MANIFEST_PATH`)
  - `local_vendor_manifest_path` (sin fallback a `VENDOR_MANIFEST_PATH`)
  - `local_runtime_manifest_paths` (sin fallback a `LEGACY_RUNTIME_ROOT_DIR`)
- [ ] B2. Quitar fallback de binarios/vendor fuera de `dist/`:
  - `local_core_bin_source_path` sin mirror legacy
  - `local_vendor_component_path` sin `VENDOR_ROOT_DIR`
  - `runtime_start_script_legacy` y chequeos duales dist/legacy
- [ ] B3. Eliminar constantes legacy de paths y toda logica asociada.
  - Avance: removidas constantes legacy de `core/manifest`, `core/bin`, `vendor/manifest`, `runtimes` y mirror local/remoto de syncthing fuera de `dist/`.
- [ ] B4. Eliminar sync remoto legacy no usado (`runtime_sync_workers`, `core_sync_workers`, `vendor_sync_workers`, utilidades asociadas).
  - Avance: eliminados `runtime_sync_workers`, `core_sync_workers`, `vendor_sync_workers`, `list_worker_access`, `verify_remote_hash_with_retry`, `sync_runtime_to_worker`.
  - Avance: eliminado bloque de gestión Syncthing remota por SSH post-bootstrap (`ensure_remote_blob_sync_all_hives` y helpers asociados).

## Fase C - Unificar operacion en SPAWN/KILL

- [x] C1. Eliminar acciones system `RUN_ROUTER`/`KILL_ROUTER` en orchestrator.
- [x] C2. Eliminar `run_router_flow`/`kill_router_flow`.
- [ ] C3. Eliminar endpoints admin `/hives/{id}/routers*`.
  - Avance: removidos endpoints mutantes (`POST`/`DELETE`), queda endpoint de lectura (`GET`).
- [ ] C4. Documentar y validar que routers se gestionan como nodos `RT.*` via `SPAWN_NODE`/`KILL_NODE`.

## Fase D - SSH estrictamente bootstrap

- [ ] D1. Revisar y eliminar helpers SSH no usados post-refactor (gate/restrict/fallback que ya no apliquen al flujo final).
- [ ] D1. Revisar y eliminar helpers SSH no usados post-refactor (gate/restrict/fallback que ya no apliquen al flujo final).
  - Avance: removidas rutas SSH operativas para `get_versions/list_versions` y para ensure/reconcile remoto de Syncthing.
- [ ] D2. Mantener solo primitives SSH necesarias para bootstrap minimo (`seed key/sudoers`, copia minima, start bootstrap services).
- [x] D3. Verificar que no quede ninguna ruta de operacion diaria que invoque SSH.

## 4. Alineacion documental obligatoria

- [ ] DOC1. Actualizar `docs/onworking/SY.orchestrator — Spec de Cambios v2.md` en puntos que hoy contradicen la politica acordada (ej. fallback SSH post-bootstrap).
- [ ] DOC2. Actualizar `docs/02-protocolo.md`:
  - remover menciones de compatibilidad `RUNTIME_UPDATE`
  - remover aliases legacy de payload
  - remover/ajustar contrato `RUN_ROUTER`/`KILL_ROUTER` si se completa Fase C
- [ ] DOC3. Actualizar `docs/07-operaciones.md`:
  - quitar fallback SSH en `remove_hive`
  - quitar referencias de operacion legacy/compat
  - reflejar canal unico socket + bootstrap SSH minimo

## 5. E2E gates de cierre (sin legacy)

- [ ] E2E-1 `add_hive` worker limpio: bootstrap SSH minimo + finalize socket `ok`.
- [ ] E2E-2 `add_hive` con worker ya online: `bootstrap_mode=socket_only_existing_orchestrator`, cero pasos SSH.
- [ ] E2E-3 `remove_hive`:
  - online -> `remote_cleanup=socket_ok`
  - offline -> `remote_cleanup in {socket_timeout,local_only}` y `remote_cleanup_via=local_only`
- [ ] E2E-4 `SYSTEM_UPDATE` runtime/core/vendor solo con contrato estricto (sin aliases).
- [ ] E2E-5 `SPAWN_NODE`/`KILL_NODE` para `AI.*`, `IO.*`, `WF.*`, `RT.*` (sin endpoints de routers si se completa Fase C).
- [ ] E2E-6 prueba negativa: payload legacy (`name`, `version`, `hash`, `RUNTIME_UPDATE`) debe fallar explícitamente.
- [ ] E2E-7 prueba negativa: key SSH inutilizable y operacion diaria (`run_node/kill_node/update/remove_hive online`) sigue funcionando por socket.

## 6. Proximo corte de implementacion recomendado

1. Completar Fase B (hard-delete de legacy filesystem).
2. Completar Fase C (quitar contrato paralelo de routers).
3. Actualizar docs (DOC1-3) en el mismo PR de limpieza para no volver a divergir.
4. Ejecutar E2E-1..E2E-7 y cerrar backlog.
