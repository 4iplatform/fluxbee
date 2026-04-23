# CORE DIAG - Runtime `ai.common` se borra por `fluxbee-syncthing` (folder `fluxbee-dist`)

Fecha: 2026-03-31  
Ámbito: distribución de runtimes (`dist`) + `SYSTEM_UPDATE` targeted  
Impacto: nodos AI/IO quedan con `runtime_present=false` aunque publish/update haya sido correcto

## 1) Qué se validó (publish/update)

Se ejecutó flujo canónico:

1. `publish-ia-runtime.sh --runtime ai.common --version 0.1.2 --set-current`
2. `POST /hives/{hive}/update` con payload targeted (`runtime` + `runtime_version`)
3. `GET /hives/{hive}/runtimes/ai.common`
4. chequeo de archivo `.../ai.common/0.1.2/bin/start.sh`

Resultado observado:

- Publish: OK (`start.sh` + binario presentes en disco).
- `SYSTEM_UPDATE` targeted: `status=ok`.
- `get_runtime`: `materialized=true` para `ai.common@0.1.2`.

Conclusión: el flujo de publish/update está funcionando correctamente cuando se ejecuta.

## 2) Qué falla (borrado posterior)

Después del publish/update correcto, aparecen en journal entradas explícitas de borrado:

- `syncthing[...] Deleted file ... runtimes/ai.common/0.1.2/bin/ai_node_runner`
- `syncthing[...] Deleted file ... runtimes/ai.common/0.1.2/bin/start.sh`
- `syncthing[...] Deleted directory ... runtimes/ai.common/0.1.2/bin`
- `syncthing[...] Deleted directory ... runtimes/ai.common/0.1.2`

Servicio/proceso involucrado:

- unidad activa: `fluxbee-syncthing.service`
- proceso: `/var/lib/fluxbee/vendor/bin/syncthing`
- folder afectado: `fluxbee-dist` (`folder.type=sendreceive`)

## 3) Evidencia adicional de control-plane

- `sy-orchestrator` relanza automáticamente `fluxbee-syncthing` cuando detecta unhealthy.
- Parar `syncthing.service` no aplica porque la unidad real es `fluxbee-syncthing.service`.
- Con publish/update exitoso y validación inmediata, el runtime se mantiene hasta que sincronización de `fluxbee-dist` aplica un delete remoto/local.

## 4) Expected vs Actual

Expected:

- Si `SYSTEM_UPDATE` targeted responde `ok` y `materialized=true`, el runtime publicado no debería desaparecer por sincronización de dist en la misma convergencia.

Actual:

- Manifest y API pueden reflejar runtime objetivo, pero `fluxbee-syncthing` elimina la carpeta del runtime después.
- Queda estado inconsistente: runtime declarado pero no materializado (`runtime_present=false`, `start_sh_executable=false`).

## 5) Diagnóstico técnico (probable falla core/sync)

No hay evidencia de falla del script de publish/deploy en este caso.

La causa probable está en el plano `dist` + Syncthing:

1. Divergencia de contenido entre peers en folder `fluxbee-dist` (modelo `sendreceive`).
2. El peer remoto/autoritativo no contiene `ai.common/0.1.2`, por lo que converge borrando local.
3. `sy-orchestrator` mantiene activo `fluxbee-syncthing` y reintenta/reinstala, reforzando esa convergencia.

## 6) Qué debería revisar core

1. Política de autoridad de `fluxbee-dist`:
   - quién escribe runtimes nuevos;
   - si `motherbee` debe ser source-of-truth o bidireccional.
2. Configuración Syncthing folder `fluxbee-dist`:
   - tipo (`sendonly`/`receiveonly`/`sendreceive`);
   - ignores/versioning/protección de deletes.
3. Contrato de rollout:
   - asegurar que publish local no sea revertido por peer antes/después de `SYSTEM_UPDATE`.
4. Guardas de orchestrator:
   - si targeted runtime quedó `materialized=true`, evitar que se invalide inmediatamente por drift remoto no confirmado.

## 7) Comandos de reproducción mínima (resumen)

```bash
# publish
bash scripts/publish-ia-runtime.sh --runtime ai.common --version 0.1.2 --mode default --set-current --sudo

# targeted update
MV="$(jq -r '.version' /var/lib/fluxbee/dist/runtimes/manifest.json)"
MH="$(sha256sum /var/lib/fluxbee/dist/runtimes/manifest.json | awk '{print $1}')"
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/update" \
  -H "Content-Type: application/json" \
  -d "{\"category\":\"runtime\",\"manifest_version\":$MV,\"manifest_hash\":\"$MH\",\"runtime\":\"ai.common\",\"runtime_version\":\"0.1.2\"}" | jq

# runtime state
curl -sS "http://127.0.0.1:8080/hives/motherbee/runtimes/ai.common" | jq

# evidencia de borrado
journalctl --since "2 hours ago" | rg -n "syncthing|Deleted file|fluxbee-dist|ai.common"
```
