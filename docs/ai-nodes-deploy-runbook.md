# AI Nodes Deploy Runbook (publish/update/spawn)

## 1) Objetivo

Desplegar y actualizar nodos IA por el camino canónico de Fluxbee:

1. publish runtime en `dist/`
2. `SYSTEM_UPDATE` en hive destino
3. `SPAWN_NODE` o restart controlado

Incluye dos perfiles:
- nodo IA común (`mode=default`)
- frontdesk gov (`mode=gov`)

---

## 2) Scripts disponibles

- `scripts/publish-ia-runtime.sh`
- `scripts/deploy-ia-node.sh`

`publish-ia-runtime.sh` ahora soporta:
- `--mode default|gov` para fijar `AI_NODE_MODE` por defecto en `start.sh` del runtime publicado.

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

### 4.1 Nodo IA común (`mode=default`)

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.0" \
  --mode default \
  --sync-hint \
  --sudo
```

### 4.2 Frontdesk gov (`mode=gov`)

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.frontdesk.gov" \
  --version "0.1.0" \
  --mode gov \
  --sync-hint \
  --sudo
```

---

## 5) Spawn de nodo (con config)

Ejemplo frontdesk:

```bash
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.frontdesk.gov" \
  --version "0.1.0" \
  --mode gov \
  --node-name "AI.frontdesk.gov@$HIVE_ID" \
  --config-json /tmp/ai_frontdesk_config.json \
  --spawn \
  --sync-hint \
  --sudo
```

El JSON se envía como `config` en `POST /hives/{id}/nodes`.

### 5.1 Bootstrap canonico por FLUXBEE_NODE_NAME (dos nodos)

Con el contrato actual de core, `spawn` inyecta `FLUXBEE_NODE_NAME=<node@hive>` y el runtime deriva su `config.json` canonico. No hace falta path fijo.

```bash
# GOV
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.frontdesk.gov" \
  --version "0.1.0" \
  --mode gov \
  --sync-hint \
  --sudo

# COMUN
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "AI.common" \
  --version "0.1.0" \
  --mode default \
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
  --runtime "AI.frontdesk.gov" \
  --version "0.1.1" \
  --mode gov \
  --node-name "AI.frontdesk.gov@$HIVE_ID" \
  --update-existing \
  --sync-hint \
  --sudo
```

`--update-existing` hace:
- publish runtime nuevo
- update con retries en `sync_pending`
- `GET config` del nodo actual
- `DELETE` + `POST` reutilizando esa config

---

## 7) Prompts/config obligatorias

Para imponer prompts/config obligatorias, versioná ese bloque en el `config` del nodo (spawn o update de config).

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

Hoy `ai_node_runner` soporta:
- YAML (`--config ...`) y
- estado dinámico persistido en `/var/lib/fluxbee/state/ai-nodes/<node>.json` (Control Plane `CONFIG_SET`)

No tiene todavía una integración cerrada de hot-reload por `CONFIG_CHANGED` basada en `config.json` de orchestrator equivalente a IO Slack.

Conclusión operativa:
- para código: `publish -> update -> spawn` (canónico) ya sirve.
- para config/prompt obligatoria en runtime: usá `CONFIG_SET`/estado dinámico del runner o flujo systemd+YAML hasta cerrar el hot-reload end-to-end de `config.json`.


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
  --mode default \
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

Example (`AI.frontdesk.gov@motherbee`):

```bash
bash scripts/deploy-ia-node.sh \
  --base "http://127.0.0.1:8080" \
  --hive-id "motherbee" \
  --runtime "AI.frontdesk.gov" \
  --version "0.1.0" \
  --mode gov \
  --node-name "AI.frontdesk.gov@motherbee" \
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
