# Prueba E2E - AI default con CONFIG_SET + reconexión router

## Respuestas rápidas

1. `--config-json` en el primer comando: **no**, ya no es obligatorio para `--spawn` (por default usa `{}`).
2. Nombre del nodo: **sí**, podés usar `AI.chat@motherbee`.
3. Este archivo deja el paso a paso completo para no perderlo en chat.

## Objetivo

Levantar un nodo AI normal (no gov) usando runtime `AI.chat@0.1.1` (pisando esa versión), setear secreto OpenAI por `CONFIG_SET`, y validar hasta ese punto.

## Prerrequisitos

- API admin local: `http://127.0.0.1:8080`
- Hive: `motherbee`
- Runtime publicado: `AI.chat` versión objetivo `0.1.1`
- `jq` instalado
- Tenant válido (reemplazar): `tnt:REPLACE_ME`
- API key OpenAI real (reemplazar): `sk-REPLACE_ME`

## Paso 0 - Publicar runtime `AI.chat` (pisando `0.1.1`)

### 0.1 Publicar + set-current

```bash
bash scripts/publish-ia-runtime.sh \
  --runtime "AI.chat" \
  --version "0.1.1" \
  --mode default \
  --set-current \
  --sudo
```

### 0.2 Propagar runtime al hive (update sin spawn)

```bash
bash scripts/deploy-ia-node.sh \
  --base "http://127.0.0.1:8080" \
  --hive-id "motherbee" \
  --runtime "AI.chat" \
  --version "0.1.1" \
  --mode default \
  --skip-spawn \
  --sync-hint \
  --sudo
```

## Paso 1 - Deploy + spawn del nodo `AI.chat@motherbee` (config mínima)

```bash
bash scripts/deploy-ia-node.sh \
  --base "http://127.0.0.1:8080" \
  --hive-id "motherbee" \
  --runtime "AI.chat" \
  --version "0.1.1" \
  --mode default \
  --node-name "AI.chat@motherbee" \
  --tenant-id "tnt:43d576a3-d712-4d91-9245-5d5463dd693e" \
  --spawn \
  --sync-hint \
  --allow-sync-pending \
  --sudo
```

Notas:
- `runtime` y `node_name` pueden diferir: runtime `AI.chat` + instancia `AI.chat@motherbee`.
- Si ya existía ese nodo, podés agregar `--kill-first`.

## Paso 2 - Leer estado/contrato actual (CONFIG_GET)

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/AI.chat@motherbee/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"manual-test"}' | jq
```

Chequeos esperados:
- `status = "ok"`
- aparece contrato de secret (`config.secrets.openai.api_key`)

## Paso 3 - Aplicar secret por CONFIG_SET

### 3.1 Tomar `config_version` actual y calcular siguiente

```bash
CUR_VER=$(curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/AI.chat@motherbee/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"manual-test"}' | jq -r '.payload.response.config_version // 0')
NEXT_VER=$((CUR_VER + 1))
echo "CUR_VER=$CUR_VER NEXT_VER=$NEXT_VER"
```

### 3.2 Enviar CONFIG_SET con secret

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/AI.chat@motherbee/control/config-set" \
  -H "Content-Type: application/json" \
  -d "{
    \"requested_by\":\"manual-test\",
    \"subsystem\":\"ai_node\",
    \"schema_version\":1,
    \"config_version\":${NEXT_VER},
    \"apply_mode\":\"replace\",
    \"config\":{
      \"behavior\":{\"kind\":\"openai_chat\",\"model\":\"gpt-4.1-mini\"},
      \"secrets\":{\"openai\":{\"api_key\":\"sk-REPLACE_ME\",\"api_key_env\":\"OPENAI_API_KEY\"}}
    }
  }" | jq
```

Chequeos esperados:
- `status = "ok"`
- `payload.response.ok = true`
- no aparece secret en claro en `effective_config`

## Paso 4 - Verificar que el secret quedó persistido

```bash
curl -sS -X POST "http://127.0.0.1:8080/hives/motherbee/nodes/AI.chat@motherbee/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"manual-test"}' | jq '.payload.response | {config_version, api_key_source, contract}'
```

Chequeos esperados:
- `api_key_source = "local_file"`
- `contract.secrets[*].configured = true`

## Paso 5 - Probar reconexión al reiniciar router

### 5.1 Abrir logs del nodo AI

```bash
journalctl -u fluxbee-node-AI.chat-motherbee.service -f
```

### 5.2 Reiniciar router/gateway (en otra terminal)

Ejemplo:
```bash
sudo systemctl restart rt-gateway.service
```

### 5.3 Qué deberías ver en logs

Mensajes esperados en el nodo AI:
- `disconnected from router`
- `reconnecting to router`
- `reconnected to router`

Y que el proceso **siga vivo** (sin salir por error fatal).

## Paso 6 - Smoke post-reconnect

Enviar un mensaje de prueba por el canal que uses normalmente (por ejemplo desde `IO.slack` o `IO.sim`) y verificar que el nodo vuelve a responder.

## Troubleshooting rápido

- `spawn_failed`: revisar nombre del nodo, tenant y runtime/version.
- `stale_config_version` en `CONFIG_SET`: recalcular `NEXT_VER`.
- `missing_secret`: reenviar `CONFIG_SET` con `config.secrets.openai.api_key`.
- Si el servicio no existe en systemd, revisar nombre exacto de unidad:

```bash
systemctl list-units --type=service | rg -i "fluxbee-node-AI\.chat|ai\.chat"
```

## One-shot (cleanup + publish/update + spawn + CONFIG_SET)

```bash
bash -lc '
set -euo pipefail

BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="AI.chat@motherbee"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
RUNTIME="AI.chat"
VERSION="0.1.1"
OPENAI_API_KEY="sk-REPLACE_ME"
LOG_FILE="/tmp/ai_chat_oneshot_$(date +%Y%m%d-%H%M%S).log"

exec > >(tee -a "$LOG_FILE") 2>&1
echo "LOG_FILE=$LOG_FILE"

echo "== cleanup =="
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq || true
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/instance" | jq || true
sudo rm -rf "/var/lib/fluxbee/nodes/AI/$NODE_NAME" || true

echo "== publish/update/spawn =="
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "$RUNTIME" \
  --version "$VERSION" \
  --mode default \
  --node-name "$NODE_NAME" \
  --tenant-id "$TENANT_ID" \
  --spawn \
  --sync-hint \
  --sudo

echo "== config-get =="
GET_RESP="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d "{\"requested_by\":\"manual-test\"}")"
echo "$GET_RESP" | jq

CUR_VER="$(echo "$GET_RESP" | jq -r ".payload.response.config_version // 0")"
NEXT_VER=$((CUR_VER + 1))
echo "CUR_VER=$CUR_VER NEXT_VER=$NEXT_VER"

echo "== config-set (api key) =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d "{
    \"requested_by\":\"manual-test\",
    \"subsystem\":\"ai_node\",
    \"schema_version\":1,
    \"config_version\":${NEXT_VER},
    \"apply_mode\":\"replace\",
    \"config\":{
      \"behavior\":{
        \"kind\":\"openai_chat\",
        \"model\":\"gpt-4.1-mini\",
        \"instructions\":{
          \"source\":\"inline\",
          \"value\":\"Sos un asistente de soporte, ayudá al cliente en lo que te pida y terminá todas las frases con :)\"
        }
      },
      \"secrets\":{\"openai\":{\"api_key\":\"${OPENAI_API_KEY}\",\"api_key_env\":\"OPENAI_API_KEY\"}}
    }
  }" | jq

echo "== config-get post-set =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d "{\"requested_by\":\"manual-test\"}" | jq ".payload.response | {config_version, api_key_source, contract}"

echo "DONE. LOG_FILE=$LOG_FILE"
'
```
