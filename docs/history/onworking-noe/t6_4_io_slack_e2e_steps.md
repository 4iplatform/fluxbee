# T6.4 - E2E IO.slack (camino real, sin bootstrap, runtime 0.1.0)

Este flujo usa el camino operativo pedido:

1. limpieza completa,
2. build nuevo,
3. publish+update targeted pisando `io.slack@0.1.0`,
4. spawn por API con config inline (sin bootstrap file),
5. validación control-plane,
6. restart + rehidratación.

Notas del flujo:

- Se pisa `0.1.0` para evitar borrado de versiones nuevas por runtime deletion en core/syncthing.
- En `CONFIG_SET` no se incluye `io.dst_node` (pedido explícito).

## 0) Variables

```bash
set -euo pipefail

BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_BASE="IO.slack.T126"
NODE_NAME="${NODE_BASE}@${HIVE_ID}"
UNIT="fluxbee-node-${NODE_BASE}-${HIVE_ID}.service"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"

RUNTIME="io.slack"
VERSION="0.1.0"   # se pisa esta version

SLACK_APP_TOKEN="xapp-REPLACE_ME"
SLACK_BOT_TOKEN="xoxb-REPLACE_ME"

LOG_FILE="/tmp/t6_4_io_slack_e2e_${NODE_BASE}_$(date +%Y%m%d-%H%M%S).log"
```

## 1) Cleanup completo

```bash
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq || true
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/instance" | jq || true

sudo systemctl stop "$UNIT" || true
sudo systemctl disable "$UNIT" || true
sudo systemctl reset-failed "$UNIT" || true

sudo rm -rf "/var/lib/fluxbee/nodes/IO/$NODE_NAME" || true
sudo rm -f  "/var/lib/fluxbee/state/io-nodes/${NODE_NAME}.json" || true
```

## 2) Build release

```bash
cd ~/fluxbee-nodes/fluxbee
cargo build --release --manifest-path nodes/io/Cargo.toml -p io-slack
```

## 3) Deploy (publish + targeted update, sin spawn)

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "$RUNTIME" \
  --version "$VERSION" \
  --runtime-version "$VERSION" \
  --update-scope targeted \
  --sync-hint \
  --update-retries 8 \
  --retry-delay-s 2 \
  --skip-spawn \
  --sudo \
  --log-file "$LOG_FILE"
```

## 4) Spawn por API (inline, sin bootstrap file)

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"$VERSION\",
    \"tenant_id\":\"$TENANT_ID\",
    \"config\":{
      \"tenant_id\":\"$TENANT_ID\",
      \"slack\":{
        \"app_token\":\"$SLACK_APP_TOKEN\",
        \"bot_token\":\"$SLACK_BOT_TOKEN\"
      }
    }
  }" | jq
```

## 5) CONFIG_GET inicial

```bash
# puede haber race corto luego del spawn; reintentar hasta 20s
for i in $(seq 1 10); do
  resp="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
    -H "Content-Type: application/json" \
    -d '{"requested_by":"t6.4-initial"}')"
  echo "$resp" | jq
  status="$(echo "$resp" | jq -r '.status // ""')"
  [ "$status" = "ok" ] && break
  sleep 2
done
```

## 6) CONFIG_SET (sin dst_node)

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d "{
    \"schema_version\":1,
    \"config_version\":8,
    \"apply_mode\":\"replace\",
    \"config\":{
      \"slack\":{
        \"app_token\":\"$SLACK_APP_TOKEN\",
        \"bot_token\":\"$SLACK_BOT_TOKEN\"
      }
    }
  }" | jq
```

## 7) CONFIG_GET post-set

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"t6.4-post-set"}' | jq
```

## 8) Restart sin reenviar CONFIG_SET

```bash
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq

# esperar a que el nodo deje de estar visible antes de remove-instance
for i in $(seq 1 20); do
  node_status="$(curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq -r '.status // ""')"
  [ "$node_status" != "ok" ] && break
  sleep 1
done

curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/instance" | jq || true
sudo rm -rf "/var/lib/fluxbee/nodes/IO/$NODE_NAME" || true

curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"$VERSION\",
    \"tenant_id\":\"$TENANT_ID\",
    \"config\":{
      \"tenant_id\":\"$TENANT_ID\",
      \"slack\":{
        \"app_token\":\"$SLACK_APP_TOKEN\",
        \"bot_token\":\"$SLACK_BOT_TOKEN\"
      }
    }
  }" | jq
```

## 9) CONFIG_GET post-restart

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"t6.4-restart"}' | jq
```

## 10) One-shot command (todo junto + logging)

Copiar/pegar y editar solo valores de keys/ids:

```bash
cat >/tmp/t6_4_io_slack_e2e.sh <<'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail

BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
NODE_BASE="IO.slack.T126"
NODE_NAME="${NODE_BASE}@${HIVE_ID}"
UNIT="fluxbee-node-${NODE_BASE}-${HIVE_ID}.service"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"
RUNTIME="io.slack"
VERSION="0.1.0"
SLACK_APP_TOKEN="xapp-REPLACE_ME"
SLACK_BOT_TOKEN="xoxb-REPLACE_ME"
LOG_FILE="/tmp/t6_4_io_slack_e2e_${NODE_BASE}_$(date +%Y%m%d-%H%M%S).log"

exec > >(tee -a "$LOG_FILE") 2>&1
echo "LOG_FILE=$LOG_FILE"

echo "== cleanup =="
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq || true
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/instance" | jq || true
sudo systemctl stop "$UNIT" || true
sudo systemctl disable "$UNIT" || true
sudo systemctl reset-failed "$UNIT" || true
sudo rm -rf "/var/lib/fluxbee/nodes/IO/$NODE_NAME" || true
sudo rm -f  "/var/lib/fluxbee/state/io-nodes/${NODE_NAME}.json" || true

echo "== build =="
cd ~/fluxbee-nodes/fluxbee
cargo build --release --manifest-path nodes/io/Cargo.toml -p io-slack

echo "== deploy (publish+update targeted, no spawn) =="
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --runtime "$RUNTIME" \
  --version "$VERSION" \
  --runtime-version "$VERSION" \
  --update-scope targeted \
  --sync-hint \
  --update-retries 8 \
  --retry-delay-s 2 \
  --skip-spawn \
  --sudo \
  --log-file "$LOG_FILE"

echo "== spawn (inline, no bootstrap file) =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"$VERSION\",
    \"tenant_id\":\"$TENANT_ID\",
    \"config\":{
      \"tenant_id\":\"$TENANT_ID\",
      \"slack\":{
        \"app_token\":\"$SLACK_APP_TOKEN\",
        \"bot_token\":\"$SLACK_BOT_TOKEN\"
      }
    }
  }" | jq

echo "== config-get initial =="
for i in $(seq 1 10); do
  resp="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
    -H "Content-Type: application/json" \
    -d "{\"requested_by\":\"t6.4-initial\"}")"
  echo "$resp" | jq
  status="$(echo "$resp" | jq -r ".status // \"\"")"
  [ "$status" = "ok" ] && break
  sleep 2
done

echo "== config-set (no dst_node) =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d "{
    \"schema_version\":1,
    \"config_version\":8,
    \"apply_mode\":\"replace\",
    \"config\":{
      \"slack\":{
        \"app_token\":\"$SLACK_APP_TOKEN\",
        \"bot_token\":\"$SLACK_BOT_TOKEN\"
      }
    }
  }" | jq

echo "== config-get post-set =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d "{\"requested_by\":\"t6.4-post-set\"}" | jq

echo "== restart =="
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq || true
for i in $(seq 1 20); do
  node_status="$(curl -sS "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | jq -r ".status // \"\"")"
  [ "$node_status" != "ok" ] && break
  sleep 1
done
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/instance" | jq || true
sudo rm -rf "/var/lib/fluxbee/nodes/IO/$NODE_NAME" || true

curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"$VERSION\",
    \"tenant_id\":\"$TENANT_ID\",
    \"config\":{
      \"tenant_id\":\"$TENANT_ID\",
      \"slack\":{
        \"app_token\":\"$SLACK_APP_TOKEN\",
        \"bot_token\":\"$SLACK_BOT_TOKEN\"
      }
    }
  }" | jq

echo "== config-get post-restart =="
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d "{\"requested_by\":\"t6.4-restart\"}" | jq

echo "DONE. LOG_FILE=$LOG_FILE"
SCRIPT

chmod +x /tmp/t6_4_io_slack_e2e.sh
/tmp/t6_4_io_slack_e2e.sh
```

## 11) Validacion payload >64KB (offload automatico a blob)

Objetivo: verificar que la conversion a `content_ref` ocurra en `io-common` (no en el adapter).

```bash
# ejemplo: enviar un texto grande por Slack (app mention) y luego revisar logs de io-slack
journalctl -u "$UNIT" --since "10 min ago" --no-pager | rg -n "inbound text/v1 normalized|inbound text/v1 processed as blob|offload_to_blob"
```

Resultado esperado:

- aparece `inbound text/v1 normalized`.
- para texto grande, `offload_to_blob=true`.
- aparece `inbound text/v1 processed as blob` con `blob_name`/`blob_size`.

Nota:

- la decision de offload por tamano se aplica en el pipeline comun (`io-common::InboundProcessor`).
- `io-slack` solo construye payload `text/v1` base + adjuntos.
- para validar de forma determinística casos de texto extremo (`>64KB`), preferir `io-sim`.
- en Slack real, `chat.postMessage` trunca texto por encima de 40.000 caracteres:
  https://docs.slack.dev/reference/methods/chat.postMessage/ (Truncating content).
