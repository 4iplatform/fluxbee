# AI Nodes Deploy Runbook (publish/update/spawn)

## 1) Objetivo

Desplegar y actualizar nodos IA por el camino canónico de Fluxbee:

1. publish runtime en `dist/`
2. `SYSTEM_UPDATE` en hive destino
3. `SPAWN_NODE` o restart controlado

Incluye dos perfiles:
- nodo IA común (`AI.common`)
- frontdesk gov (`SY.frontdesk.gov`)

Importante:
- la separación de comportamiento es por runtime (`AI.common` vs `SY.frontdesk.gov`), no por flags de modo.

---

## 2) Scripts disponibles

- `scripts/publish-ia-runtime.sh`
- `scripts/deploy-ia-node.sh`

`publish-ia-runtime.sh`:
- selecciona el perfil de runtime por `--runtime`.

`deploy-ia-node.sh` ahora soporta:
- `--update-scope <targeted|global>` (default: `targeted`).
- En `targeted`, `SYSTEM_UPDATE category=runtime` se envía con `runtime + runtime_version` para no bloquear deploy puntual por drift global de runtimes ajenos.
- Spawn sin `--config-json` (genera config base `{}` y usa `tenant_id` como fallback si se pasa por flag).

Decisión de ownership:
- El prompt/flujo de `SY.frontdesk.gov` pertenece al runtime frontdesk.
- `AI.common` no debe transportar lógica específica gov/frontdesk.
- El data-plane `text/v1` (incluyendo resolución de `content_ref`/`attachments` y offload de respuesta a `content_ref` por tamaño) pertenece al SDK AI compartido.

Nota operativa vigente para frontdesk:
- `SY.frontdesk.gov` sigue usando hoy el camino general de deploy de runtimes AI, pero debe leerse como runtime singleton `system-default` por hive.
- el prompt funcional base del runtime no debería depender de que el operador lo cargue por `CONFIG_SET`.
- la key de provider puede seguir entrando temporalmente por `CONFIG_SET` y persistirse en `secrets.json` mientras `CORE` define el modelo final para secretos de un AI de sistema crítico.

---

## 3) Prerrequisitos

- `sy-orchestrator`, `sy-admin`, `rt-gateway` activos.
- API admin accesible (ejemplo: `BASE=http://127.0.0.1:8080`).
- Permisos para escribir en `/var/lib/fluxbee/dist` si publicás local.

Variables sugeridas:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
```

---

## 4) Deploy inicial de runtime IA

### 4.1 Nodo IA común

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.0" \
  --sync-hint \
  --sudo
```

### 4.2 Frontdesk gov

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "SY.frontdesk.gov" \
  --version "0.1.0" \
  --sync-hint \
  --sudo
```

---

## 5) Spawn de nodo (config opcional)

Ejemplo frontdesk:

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "SY.frontdesk.gov" \
  --version "0.1.0" \
  --node-name "SY.frontdesk.gov@$HIVE_ID" \
  --spawn \
  --sync-hint \
  --sudo
```

Si no envías `--config-json`, el script hace spawn con `config={}` (más `tenant_id` si fue enviado).
El detalle funcional (prompt/key/model/etc.) se recomienda cargarlo por `POST .../control/config-set`.

### 5.1 Bootstrap canónico por FLUXBEE_NODE_NAME (dos nodos)

Con el contrato actual de core, `spawn` inyecta `FLUXBEE_NODE_NAME=<node@hive>` y el runtime deriva su `config.json` canónico. No hace falta path fijo.

```bash
# GOV
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "SY.frontdesk.gov" \
  --version "0.1.0" \
  --sync-hint \
  --sudo

# COMUN
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.0" \
  --sync-hint \
  --sudo
```

Los flags `--forced-node-name` y `--forced-dynamic-config-dir` quedan solo para compatibilidad de emergencia.

---

## 6) Update de código de nodo existente

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "SY.frontdesk.gov" \
  --version "0.1.1" \
  --node-name "SY.frontdesk.gov@$HIVE_ID" \
  --update-existing \
  --sync-hint \
  --sudo
```

`--update-existing` hace:
- publish runtime nuevo
- update con retries (`targeted` por defecto)
- `GET config` del nodo actual
- `DELETE` + `POST` reutilizando esa config

### 6.1 Scope recomendado de update

- Recomendado (default): `--update-scope targeted`
- Solo para diagnóstico de salud general: `--update-scope global`

Ejemplo forzando modo global:

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.2" \
  --update-scope global \
  --sync-hint \
  --sudo
```

---

## 7) Prompts/config obligatorias

Para `AI.common`, los prompts/config obligatorias pueden versionarse en el `config` del nodo.

Para `SY.frontdesk.gov`, la dirección vigente es distinta:
- el prompt funcional base pertenece al runtime frontdesk,
- `CONFIG_SET` queda preferentemente para modelo, timeouts, flags operativos y secrets,
- no para transportar obligatoriamente todo el prompt funcional del nodo.

Ejemplo base para `openai_chat`:

```json
{
  "behavior": {
    "kind": "openai_chat",
    "model": "gpt-4.1-mini",
    "instructions": {
      "source": "inline",
      "value": "Prompt obligatorio del nodo...",
      "trim": true
    },
    "api_key_env": "OPENAI_API_KEY",
    "model_settings": {
      "temperature": 0.0,
      "top_p": 1.0,
      "max_output_tokens": 400
    }
  },
  "runtime": {
    "handler_timeout_ms": 60000,
    "worker_pool_size": 4,
    "queue_capacity": 128
  }
}
```

Variables recomendadas para frontdesk gov:
- `OPENAI_API_KEY`
- `GOV_IDENTITY_TARGET`
- `GOV_IDENTITY_TIMEOUT_MS`

---

## 8) Importante: estado actual del runner IA

Hoy hay dos persistencias distintas por capa:
- `config.json` de orchestrator en `/var/lib/fluxbee/nodes/<TYPE>/<node@hive>/config.json` (infra/bootstrap).
- estado dinámico del nodo en `/var/lib/fluxbee/state/ai-nodes/<node>.json` (runtime/business, vía `CONFIG_SET`).

Precedencia efectiva actual del runner AI (flujo managed-node):
1. estado dinámico persistido por `CONFIG_SET`
2. fallback a `config.json` de orchestrator
3. `UNCONFIGURED`

Hot reload actual:
- confiable/canónico: `POST .../control/config-set` (`CONFIG_SET`).
- `CONFIG_CHANGED` (enviado por core después de `PUT .../config`) todavía no está cerrado end-to-end como hot reload AI.

Conclusión operativa:
- para código: `publish -> update -> spawn` (canónico) ya sirve.
- para prompt/key/behavior en runtime: usar `CONFIG_SET` (control plane del nodo).
- para update de runtime puntual, mantener `deploy-ia-node.sh` en scope `targeted` (default).

Nota sobre attachments AI (estado 2026-04-06):
- `AI.*` ya consume `attachments[]`/`content_ref` vía SDK AI compartido.
- imágenes soportadas (`png`/`jpeg`/`webp`) viajan como `input_image.image_url`.
- archivos no imagen, cuando `multimodal=true`, viajan como `input_file.file_data` con formato `data:<mime>;base64,...` y `filename`.
- `file_id` / `file_url` siguen diferidos hasta que Fluxbee/Core defina metadata suficiente para soportarlos de forma canónica.
- esto no debe leerse como "cualquier tipo de archivo ya quedó validado end-to-end":
  - evidencia E2E asentada hoy: imagen, PDF y `xlsx`
  - siguen faltando pruebas por familias de tipos adicionales (`docx`, audio, otros binarios y mezcla de adjuntos)
- esto tampoco debe leerse como "AI ya devuelve archivos al usuario final":
  - el contrato de salida soporta `attachments[]`
  - `IO.slack` sabe publicar adjuntos salientes si existen
  - pero los runners `AI.*` actuales responden texto y offload de texto largo a `content_ref`
  - la generación de PDF/XLSX/imagen saliente como adjunto no está cerrada hoy como flujo general

---

## 9) Immediate memory options for spawn/deploy

`deploy-ia-node.sh` now supports optional flags to inject `runtime.immediate_memory` into spawn config.

Optional flags:
- `--immediate-memory-enabled <true|false>`
- `--immediate-memory-recent-max <n>`
- `--immediate-memory-active-max <n>`
- `--immediate-memory-summary-max-chars <n>`
- `--immediate-memory-refresh-every-turns <n>`
- `--immediate-memory-trim-noise <true|false>`

Notes:
- These flags are opt-in. Existing commands keep working unchanged.
- If you do not pass these flags, config remains exactly as provided by `--config-json`.

Example (`AI.chat@motherbee`):

```bash
bash scripts/deploy-ia-node.sh \
  --base "http://127.0.0.1:8080" \
  --hive-id "motherbee" \
  --runtime "AI.chat" \
  --version "0.1.1" \
  --node-name "AI.chat@motherbee" \
  --tenant-id "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" \
  --config-json /tmp/ai_common_chat.config.json \
  --immediate-memory-enabled true \
  --immediate-memory-recent-max 10 \
  --immediate-memory-active-max 8 \
  --immediate-memory-summary-max-chars 1600 \
  --immediate-memory-refresh-every-turns 3 \
  --immediate-memory-trim-noise true \
  --spawn \
  --sync-hint \
  --allow-sync-pending \
  --sudo
```

Example (`SY.frontdesk.gov@motherbee`):

```bash
bash scripts/deploy-ia-node.sh \
  --base "http://127.0.0.1:8080" \
  --hive-id "motherbee" \
  --runtime "SY.frontdesk.gov" \
  --version "0.1.0" \
  --node-name "SY.frontdesk.gov@motherbee" \
  --tenant-id "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" \
  --config-json /tmp/ai_frontdesk_gov.spawn.json \
  --immediate-memory-enabled true \
  --immediate-memory-recent-max 10 \
  --immediate-memory-active-max 8 \
  --immediate-memory-summary-max-chars 1600 \
  --immediate-memory-refresh-every-turns 3 \
  --immediate-memory-trim-noise true \
  --spawn \
  --sync-hint \
  --allow-sync-pending \
  --sudo
```
