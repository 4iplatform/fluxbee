# OPA — Checklist de Pendientes (Spec v1.13)

Estado de implementación contra la especificación actual (docs/01–09 + SY_nodes_spec.md).

## Pendientes críticos (funcionales)
1) **Builtins OPA completos**
   - Implementar el set de builtins esperado por OPA WASM (regex, glob, string ops, etc.).
   - Verificar mapeos correctos en el runtime del router.

2) **SHM OPA dedicada (lectura por router)** ✅
   - Router lee **WASM exclusivamente desde** `/dev/shm/jsr-opa-<island>` (via `OpaRegionReader`).
   - `SY.opa.rules` es el **único writer** de esa región (Go).

3) **Header OPA en SHM** ✅
   - Validar `OPA_MAGIC`, `OPA_VERSION`, `OPA_MAX_WASM_SIZE`.
   - Actualizar `opa_load_status` (LOADING/OK/ERROR) en recargas.

4) **Estado OPA por router** ✅
   - `opa_policy_version` y `opa_load_status` en SHM local del router deben reflejar la carga.
   - `SY.opa.rules` debe poder verificar si los routers aplicaron la versión.

5) **Lock file de sy-opa-rules**
   - Implementar `/var/run/json-router/sy-opa-rules.lock` (evitar procesos duplicados).

## Pendientes de API / Control plane
6) **Endpoints SY.admin (OPA)** ✅
   - Confirmar/implementar:
     - `POST /opa/policy` (compile + apply)
     - `POST /opa/policy/compile` (compile, staged)
     - `POST /opa/policy/check` (alias de compile)
     - `POST /opa/policy/apply` (apply staged)
     - `POST /opa/policy/rollback`
     - `GET /opa/policy`
     - `GET /opa/status`

7) **Códigos de error normalizados** ✅
   - Solo usar: `COMPILE_ERROR`, `NOTHING_STAGED`, `VERSION_MISMATCH`, `NO_BACKUP`, `SHM_ERROR`, `TIMEOUT`.
   - Alinear respuestas de `SY.opa.rules` y `SY.admin`.

## Pendientes de observabilidad
8) **Logs OPA mínimos por evento** ✅
   - Router: loggear acción (`compile/apply/rollback`) y versión al recibir `CONFIG_CHANGED`.
   - `SY.opa.rules`: log de `query` y `config_response` con versión y hash.

## Referencias principales
- `docs/04-routing.md` — Sección OPA
- `docs/03-shm.md` — Región `jsr-opa-<island>`
- `docs/02-protocolo.md` — `CONFIG_CHANGED` + `OPA_RELOAD`
- `docs/SY_nodes_spec.md` — Flujo `SY.opa.rules`
- `docs/09-router-status.md` — Estado de implementación
