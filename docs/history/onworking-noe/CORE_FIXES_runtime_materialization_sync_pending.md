# CORE_FIXES - runtime materialization + sync_pending global

## Resumen corto

Hay un problema en la capa core/orchestrator/dist que impacta deploy de runtimes nuevos:

- publish local de `io.slack@<version>` crea carpeta y `start.sh`,
- `SYSTEM_UPDATE` responde `sync_pending` por faltantes de otros runtimes no relacionados,
- spawn termina en `RUNTIME_NOT_PRESENT`,
- en algunos casos la carpeta runtime recien publicada desaparece.

Esto no es un bug del control-plane `CONFIG_GET/CONFIG_SET` de `IO.slack`.

## Evidencia observada

1. `SYSTEM_UPDATE` devuelve `sync_pending` con `errors` que incluyen faltantes globales (`AI.*`, `wf.*`) y tambien la version de `io.slack` publicada.
2. spawn falla con:
   - `error_code=RUNTIME_NOT_PRESENT`
   - `expected_path=/var/lib/fluxbee/dist/runtimes/io.slack/<version>/bin/start.sh`
3. logs systemd de nodo muestran `Failed at step EXEC ... start.sh: No such file or directory`.
4. Caso reproducido tambien con `ai.common@0.1.2`:
   - `publish` reporta OK con `start.sh` materializado en `.../runtimes/ai.common/0.1.2/bin/start.sh`.
   - durante retries de `SYSTEM_UPDATE` aparece `runtime package missing directory runtime='ai.common' version='0.1.2'`.
   - `spawn` termina en `RUNTIME_NOT_PRESENT` para el mismo `expected_path`.

## Lectura tecnica en codigo core

En `src/bin/sy_orchestrator.rs`:

- `apply_system_update_local(..., "runtime")` ejecuta:
  1) `apply_runtime_retention(&manifest)`
  2) `verify_runtime_current_artifacts(&manifest)`
  3) si hay faltantes => `status="sync_pending"`

- `verify_runtime_current_artifacts` valida los `current` de todo el manifest runtime, no solo el runtime objetivo.

- `apply_runtime_retention` puede podar versiones/directorios no retenidos por manifest/persisted nodes.

Con manifest global inconsistente, el update runtime queda atrapado en `sync_pending` y el deploy de un runtime puntual queda bloqueado.

## Impacto operativo

- No se puede validar ni desplegar un runtime puntual (ej. `io.slack`) de forma confiable.
- Produce falsos negativos operativos sobre nodos (`RUNTIME_NOT_PRESENT`) aunque el publish local haya sido exitoso.
- Se observa desmaterializacion transitoria o podado prematuro del runtime recien publicado durante ventana de `sync_pending`.

## Datos adicionales recomendados para triage Core

1. **Correlacion temporal completa**
- Adjuntar timestamps de:
  - fin de `publish`,
  - primer `update sync_pending`,
  - primer `missing directory` del runtime objetivo,
  - `spawn_failed` con `RUNTIME_NOT_PRESENT`.

2. **Payloads completos de `SYSTEM_UPDATE`**
- Incluir `payload.errors[]` completos en varios intentos para mostrar bloqueo cruzado por runtimes no relacionados.

3. **Estado de manifest runtime antes/despues**
- Guardar snapshot de `/var/lib/fluxbee/dist/runtimes/manifest.json` antes de `publish`, despues de `publish`, y tras `sync_pending`.
- Confirmar si `current/available` queda apuntando a versiones no materializadas en disco.

4. **Verificacion de path objetivo en vivo**
- Registrar chequeos de existencia del runtime objetivo en paralelo a retries:
  - `test -f /var/lib/fluxbee/dist/runtimes/<runtime>/<version>/bin/start.sh`
- Esto permite detectar exactamente en que intento desaparece el artifact.

## Fixes sugeridos para Core

1. **Validacion por alcance runtime (targeted readiness)**
- Para `SYSTEM_UPDATE category=runtime`, permitir modo de validacion por runtime/version solicitado.
- No bloquear `io.slack` por faltantes de `AI.*` / `wf.*` ajenos.

2. **No podar durante estado no convergente**
- Evitar `apply_runtime_retention` destructivo mientras el estado sea `sync_pending`.
- Alternativa: retencion no destructiva hasta convergencia de sync.

3. **Semantica clara de materializacion**
- Exponer estado "materialized on worker" por `runtime/version`.
- Spawn deberia poder consultar ese estado previo.

4. **Separar consistencia global vs deploy puntual**
- Mantener chequeos globales para salud general, pero no como bloqueo duro para runtime puntual si su artifact esta presente y ejecutable.

## Workaround actual (mientras no se fije Core)

Para cerrar pruebas IO:

- usar runtime estable existente (`0.1.0`) y pisar solo binario `io-slack` con build nuevo,
- spawnear con `runtime_version="0.1.0"`.

Esto evita el bloqueo por `sync_pending` global del manifest.
