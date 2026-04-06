# E2E rapido - Slack -> AI.chat multimodal (text/v1 + blob refs)

Este archivo NO reemplaza `ai_chattest_reconnect_test.md`.
Es un flujo corto para validar runtime + config + multimodal en `AI.chat`.

## Objetivo

Validar que `AI.chat` (runtime `AI.common`) hoy:

1. levanta con deploy limpio,
2. recibe config por `CONFIG_SET`,
3. procesa imagen cuando `multimodal=true`,
4. devuelve error canonico de proveedor cuando OpenAI falla.

## Supuestos

- `IO.slack` ya esta activo y enruta hacia `AI.chat@motherbee`.
- El `thread_id` llega correctamente desde Slack adapter.

## Importante

Si cambiaste codigo del runner/SDK AI, SI conviene redeploy del runtime antes de probar.

## One-shot oficial (cleanup + publish/update/spawn + config-set)

```bash
set -euo pipefail

cd /home/administrator/fluxbee-nodes/fluxbee

BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_NAME="AI.chat@motherbee"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
RUNTIME="AI.common"
VERSION="0.1.1"   # overwrite de version existente
OPENAI_API_KEY="sk-REPLACE_ME"
LOG_FILE="/tmp/ai_chat_multimodal_e2e_$(date +%Y%m%d-%H%M%S).log"

exec > >(tee -a "$LOG_FILE") 2>&1
echo "LOG_FILE=$LOG_FILE"

echo "== cleanup node =="
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq || true
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/instance" | jq || true
sudo rm -rf "/var/lib/fluxbee/nodes/AI/$NODE_NAME" || true
sudo rm -f "/var/lib/fluxbee/state/ai-nodes/${NODE_NAME}.json" || true

echo "== deploy runtime + spawn =="
bash scripts/deploy-ia-node.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "$RUNTIME" \
  --version "$VERSION" \
  --node-name "$NODE_NAME" \
  --tenant-id "$TENANT_ID" \
  --spawn \
  --sync-hint \
  --allow-sync-pending \
  --sudo

echo "== config-get pre-set =="
cat > /tmp/ai_chat_config_get.json <<'JSON'
{"requested_by":"ai-multimodal-e2e"}
JSON

GET_RESP="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  --data-binary @/tmp/ai_chat_config_get.json)"
echo "$GET_RESP" | jq

CUR_VER="$(echo "$GET_RESP" | jq -r ".payload.response.config_version // 0")"
NEXT_VER=$((CUR_VER + 1))
echo "CUR_VER=$CUR_VER NEXT_VER=$NEXT_VER"

echo "== config-set (multimodal on) =="
cat > /tmp/ai_chat_config_set.json <<JSON
{
  "requested_by": "ai-multimodal-e2e",
  "subsystem": "ai_node",
  "schema_version": 1,
  "config_version": ${NEXT_VER},
  "apply_mode": "replace",
  "config": {
    "behavior": {
      "kind": "openai_chat",
      "model": "gpt-4.1-mini",
      "instructions": {
        "source": "inline",
        "value": "Sos un asistente de soporte. Responde breve y claro."
      },
      "capabilities": {
        "multimodal": true
      }
    },
    "secrets": {
      "openai": {
        "api_key": "${OPENAI_API_KEY}",
        "api_key_env": "OPENAI_API_KEY"
      }
    }
  }
}
JSON

curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  --data-binary @/tmp/ai_chat_config_set.json | jq

echo "== config-get post-set =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  --data-binary @/tmp/ai_chat_config_get.json \
  | jq ".payload.response | {config_version, state, config_source, contract, effective_config}"

echo "== service status =="
systemctl status fluxbee-node-AI.chat-motherbee.service --no-pager -l || true

echo "DONE. LOG_FILE=$LOG_FILE"
```

## Prueba manual desde Slack

### Caso A: texto + imagen (esperado: camino multimodal OK)

1. Envia texto + imagen (`png/jpg/webp`) al thread/canal que enruta a `AI.chat`.
2. Esperado:
- el nodo sigue `active (running)`,
- respuesta util del AI (no `invalid_text_payload`),
- logs con `incoming user message`.

Comando util:

```bash
journalctl -u fluxbee-node-AI.chat-motherbee.service --since "10 min ago" --no-pager | rg -n "incoming user message|unsupported_attachment_mime|provider_"
```

### Caso B: forzar error de proveedor (esperado: error canonico)

Objetivo: validar el nuevo mapeo M3.3.

1. Hace `CONFIG_SET` temporal con `api_key` invalida.
2. Envia un mensaje desde Slack.
3. Esperado: payload de error canonico (`provider_auth_error` o similar), no error crudo interno.
4. Restituir `api_key` valida con otro `CONFIG_SET`.

## Criterio de exito

- Imagen llega al nodo AI sin `unsupported_attachment_mime` en modo multimodal.
- Ante falla de OpenAI, se ve error canonico (`provider_*` o `ai_runtime_error`) con `retryable`.
- El servicio `fluxbee-node-AI.chat-motherbee.service` queda en `active (running)`.
