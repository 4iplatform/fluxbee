# IO.api + SY.frontdesk.gov - Deploy y E2E operativo

> Nota de deprecacion:
> las referencias operativas históricas a `frontdesk_result` en este runbook deben leerse como legacy.
> Estado actual:
> - sin envelope, `SY.frontdesk.gov` responde `text`
> - con envelope, responde `text` estructurado
> - `frontdesk_result` ya no es contrato de salida vigente para los hops IO

**Fecha:** 2026-04-15  
**Estado:** runbook operativo para Linux  
**Objetivo:** dejar un camino unico para:

- actualizar runtime de `SY.frontdesk.gov` sin borrar nodo ni pisar config;
- reinstalar `IO.api` de forma limpia;
- aplicar `CONFIG_SET` de `IO.api`;
- validar `GET /`;
- validar el camino E2E `IO.api -> SY.frontdesk.gov`.

---

## 1. Direccion operativa

Este runbook trata a los dos nodos distinto a proposito:

- `IO.api` se reinstala como nodo managed comun:
  - delete previo por nombre
  - publish
  - update
  - spawn
  - `CONFIG_SET`
- `SY.frontdesk.gov` se actualiza de forma conservadora:
  - publish runtime
  - update
  - restart del servicio singleton existente
  - sin delete
  - sin respawn
  - sin pisar config

Observacion operativa de este host Linux:

- en este entorno `SY.frontdesk.gov` corre como servicio singleton `sy-frontdesk-gov.service`;
- la unidad observada usa `ExecStart=/usr/bin/sy-frontdesk-gov`;
- por lo tanto, `publish runtime + update + restart` no alcanza por sí solo para asegurar que el proceso activo cambió de binario;
- la verificacion real debe hacerse contra ese servicio y ese binario;
- si queres que el proceso activo refleje el release nuevo, tenes que instalar el binario actualizado en `/usr/bin/sy-frontdesk-gov` o ejecutar el camino equivalente de `scripts/install.sh`.

Paso a paso completo para este host:

1. publicar `SY.frontdesk.gov` a `dist` y ejecutar `SYSTEM_UPDATE` targeted;
2. compilar el binario real:

```bash
cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
```

3. instalarlo en el path usado por la unidad:

```bash
sudo install -m 0755 target/release/sy-frontdesk-gov /usr/bin/sy-frontdesk-gov
```

4. reiniciar el servicio:

```bash
sudo systemctl restart sy-frontdesk-gov.service
sudo systemctl status sy-frontdesk-gov.service --no-pager -l
```

5. validar:

```bash
curl -sS "$BASE/hives/$HIVE_ID/nodes" | jq '.payload.nodes[] | select(.node_name=="SY.frontdesk.gov@motherbee")'
sha256sum /usr/bin/sy-frontdesk-gov
sha256sum "/var/lib/fluxbee/dist/runtimes/SY.frontdesk.gov/$FRONTDESK_VERSION/bin/sy-frontdesk-gov"
```

Si los hashes no coinciden, el servicio singleton no quedó alineado con el artefacto publicado.

La razon es simple:

- `IO.api` hoy se comporta como adapter managed comun;
- `SY.frontdesk.gov` hoy esta alineado como singleton canonico por hive y no conviene resetearlo en updates rutinarios.

---

## 2. Versiones base del repo

Tomadas desde los `Cargo.toml` actuales:

- `IO.api` -> `0.1.0`
- `SY.frontdesk.gov` -> `0.1.0`

Se pueden overridear por variable si queres publicar otra version.

---

## 3. Variables base

```bash
cd /home/administrator/fluxbee-nodes/fluxbee

BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
TENANT_ID="tnt:43d576a3-d712-4d91-9245-5d5463dd693e"

IO_API_VERSION="${IO_API_VERSION:-0.1.0}"
FRONTDESK_VERSION="${FRONTDESK_VERSION:-0.1.0}"

IO_API_NODE_NAME="IO.api.support@$HIVE_ID"
IO_API_TENANT_ID="$TENANT_ID"
LISTEN_ADDRESS="127.0.0.1"
LISTEN_PORT="18081"
API_KEY="tenant_key"
API_KEY_ID="${API_KEY_ID:-tenant-key}"
API_KEY_TENANT_ID="$TENANT_ID"

FRONTDESK_NODE_NAME="SY.frontdesk.gov@$HIVE_ID"
FRONTDESK_TENANT_ID="$TENANT_ID"

IO_API_UNIT="fluxbee-node-${IO_API_NODE_NAME//@/-}.service"
FRONTDESK_UNIT="${FRONTDESK_UNIT:-sy-frontdesk-gov.service}"
```

Notas:

- `API_KEY_ID` no es el secreto; es solo el identificador estable de la key dentro de config.
- `LISTEN_ADDRESS` debe ser `127.0.0.1`, no `http://127.0.0.1`.
- este runbook asume que `SY.frontdesk.gov` ya existe y ya tiene su config sensible aplicada.

---

## 4. Script unico en `/tmp`

Este bloque genera un helper temporal.

Reglas de este helper:

- no usa `set -e`
- sigue ejecutando y resume errores al final
- hace delete best-effort solo para `IO.api`
- si el delete no logra remover completamente la instancia vieja de `IO.api`, hace fallback a `--update-existing`
- no borra `SY.frontdesk.gov`
- no pisa config de `SY.frontdesk.gov`

```bash
cat >/tmp/redeploy_io_api_and_frontdesk.sh <<'EOF'
#!/usr/bin/env bash
set -u -o pipefail

ROOT_DIR="${ROOT_DIR:-/home/administrator/fluxbee-nodes/fluxbee}"
cd "$ROOT_DIR" || exit 1

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-tnt:43d576a3-d712-4d91-9245-5d5463dd693e}"

IO_API_VERSION="${IO_API_VERSION:-0.1.0}"
FRONTDESK_VERSION="${FRONTDESK_VERSION:-0.1.0}"

IO_API_NODE_NAME="${IO_API_NODE_NAME:-IO.api.support@$HIVE_ID}"
IO_API_TENANT_ID="${IO_API_TENANT_ID:-$TENANT_ID}"
LISTEN_ADDRESS="${LISTEN_ADDRESS:-127.0.0.1}"
LISTEN_PORT="${LISTEN_PORT:-18081}"
API_KEY="${API_KEY:-tenant_key}"
API_KEY_ID="${API_KEY_ID:-tenant-key}"
API_KEY_TENANT_ID="${API_KEY_TENANT_ID:-$TENANT_ID}"

FRONTDESK_NODE_NAME="${FRONTDESK_NODE_NAME:-SY.frontdesk.gov@$HIVE_ID}"
FRONTDESK_UNIT="${FRONTDESK_UNIT:-sy-frontdesk-gov.service}"
IO_API_UNIT="${IO_API_UNIT:-fluxbee-node-${IO_API_NODE_NAME//@/-}.service}"

LOG_FILE="${LOG_FILE:-/tmp/redeploy_io_api_and_frontdesk_$(date +%Y%m%d-%H%M%S).log}"
FAILURES=0

log() {
  echo "[$(date -Iseconds)] $*" | tee -a "$LOG_FILE"
}

run_step() {
  local label="$1"
  shift
  log "START $label"
  if "$@" >>"$LOG_FILE" 2>&1; then
    log "OK    $label"
    return 0
  fi
  log "FAIL  $label"
  FAILURES=$((FAILURES + 1))
  return 1
}

best_effort_delete_io_api() {
  curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$IO_API_NODE_NAME" || true
  for _ in 1 2 3 4 5; do
    sleep 2
    curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$IO_API_NODE_NAME/instance" || true
  done
}

deploy_frontdesk_runtime_only() {
  local publish_log update_body
  publish_log="$(mktemp)"
  update_body="$(mktemp)"

  if ! bash scripts/publish-ia-runtime.sh \
      --runtime "SY.frontdesk.gov" \
      --version "$FRONTDESK_VERSION" \
      --set-current \
      --sudo >"$publish_log" 2>&1; then
    cat "$publish_log" >>"$LOG_FILE"
    rm -f "$publish_log" "$update_body"
    return 1
  fi
  cat "$publish_log" >>"$LOG_FILE"

  local manifest_version manifest_hash
  manifest_version="$(sed -n 's/^manifest_version=//p' "$publish_log" | tail -n1)"
  manifest_hash="$(sed -n 's/^manifest_hash=//p' "$publish_log" | tail -n1)"
  if [[ -z "$manifest_version" || -z "$manifest_hash" ]]; then
    log "WARN  frontdesk publish output without manifest_version/hash"
    rm -f "$publish_log" "$update_body"
    return 1
  fi

  curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
    -H "Content-Type: application/json" \
    -d '{"channel":"dist","wait_for_idle":true,"timeout_ms":30000}' \
    >>"$LOG_FILE" 2>&1 || true

  cat >"$update_body" <<JSON
{
  "category":"runtime",
  "manifest_version": $manifest_version,
  "manifest_hash": "$manifest_hash",
  "runtime": "SY.frontdesk.gov",
  "runtime_version": "$FRONTDESK_VERSION"
}
JSON

  if ! curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
      -H "Content-Type: application/json" \
      --data-binary @"$update_body" >>"$LOG_FILE" 2>&1; then
    rm -f "$publish_log" "$update_body"
    return 1
  fi

  if ! sudo systemctl restart "$FRONTDESK_UNIT" >>"$LOG_FILE" 2>&1; then
    rm -f "$publish_log" "$update_body"
    return 1
  fi

  sudo systemctl is-active "$FRONTDESK_UNIT" >>"$LOG_FILE" 2>&1 || true
  rm -f "$publish_log" "$update_body"
  return 0
}

deploy_io_api_full() {
  local node_get body_exists
  node_get="$(mktemp)"
  body_exists=0

  curl -sS "$BASE/hives/$HIVE_ID/nodes/$IO_API_NODE_NAME" >"$node_get" 2>/dev/null || true
  if grep -q '"status":"ok"' "$node_get"; then
    body_exists=1
  fi
  rm -f "$node_get"

  if [[ "$body_exists" == "1" ]]; then
    bash scripts/deploy-io-api.sh \
      --base "$BASE" \
      --hive-id "$HIVE_ID" \
      --version "$IO_API_VERSION" \
      --node-name "$IO_API_NODE_NAME" \
      --update-existing \
      --listen-address "$LISTEN_ADDRESS" \
      --listen-port "$LISTEN_PORT" \
      --api-key "$API_KEY" \
      --api-key-id "$API_KEY_ID" \
      --sync-hint \
      --sudo
  else
    bash scripts/deploy-io-api.sh \
      --base "$BASE" \
      --hive-id "$HIVE_ID" \
      --version "$IO_API_VERSION" \
      --node-name "$IO_API_NODE_NAME" \
      --tenant-id "$IO_API_TENANT_ID" \
      --listen-address "$LISTEN_ADDRESS" \
      --listen-port "$LISTEN_PORT" \
      --api-key "$API_KEY" \
      --api-key-id "$API_KEY_ID" \
      --sync-hint \
      --sudo
  fi
}

io_api_schema_probe() {
  curl -sS "http://$LISTEN_ADDRESS:$LISTEN_PORT/" 
}

run_frontdesk_e2e_ok() {
  curl -i -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $API_KEY" \
    -d "{
      \"subject\": {
        \"external_user_id\": \"support:customer-frontdesk-001\",
        \"display_name\": \"Juan Perez\",
        \"email\": \"juan@example.com\",
        \"company_name\": \"Acme Support\",
        \"attributes\": {
          \"crm_customer_id\": \"crm-123\"
        }
      },
      \"message\": {
        \"text\": \"Necesito ayuda con mi alta\"
      },
      \"options\": {
        \"routing\": {
          \"dst_node\": \"$FRONTDESK_NODE_NAME\"
        },
        \"metadata\": {
          \"conversation_id\": \"frontdesk-case-001\"
        }
      }
    }"
}

run_io_api_invalid_payload() {
  curl -i -sS -X POST "http://$LISTEN_ADDRESS:$LISTEN_PORT/" \
    -H "Content-Type: application/json" \
    -H "Authorization: Bearer $API_KEY" \
    -d "{
      \"subject\": {
        \"external_user_id\": \"support:customer-bad-001\",
        \"display_name\": \"Juan Perez\"
      },
      \"message\": {
        \"text\": \"Payload incompleto\"
      },
      \"options\": {
        \"routing\": {
          \"dst_node\": \"$FRONTDESK_NODE_NAME\"
        }
      }
    }"
}

run_step "best-effort delete IO.api" best_effort_delete_io_api
run_step "deploy frontdesk runtime only" deploy_frontdesk_runtime_only
run_step "deploy IO.api" deploy_io_api_full
run_step "probe IO.api GET /" io_api_schema_probe

log "E2E frontdesk happy path"
run_frontdesk_e2e_ok | tee -a "$LOG_FILE" || FAILURES=$((FAILURES + 1))

log "E2E invalid payload before frontdesk"
run_io_api_invalid_payload | tee -a "$LOG_FILE" || FAILURES=$((FAILURES + 1))

log "LOG_FILE=$LOG_FILE"
if [[ "$FAILURES" -gt 0 ]]; then
  log "SUMMARY failures=$FAILURES"
  exit 1
fi
log "SUMMARY failures=0"
EOF

chmod +x /tmp/redeploy_io_api_and_frontdesk.sh
```

---

## 5. Ejecucion

```bash
BASE="$BASE" \
HIVE_ID="$HIVE_ID" \
TENANT_ID="$TENANT_ID" \
IO_API_VERSION="$IO_API_VERSION" \
FRONTDESK_VERSION="$FRONTDESK_VERSION" \
IO_API_NODE_NAME="$IO_API_NODE_NAME" \
IO_API_TENANT_ID="$IO_API_TENANT_ID" \
LISTEN_ADDRESS="$LISTEN_ADDRESS" \
LISTEN_PORT="$LISTEN_PORT" \
API_KEY="$API_KEY" \
API_KEY_ID="$API_KEY_ID" \
API_KEY_TENANT_ID="$API_KEY_TENANT_ID" \
FRONTDESK_NODE_NAME="$FRONTDESK_NODE_NAME" \
/tmp/redeploy_io_api_and_frontdesk.sh
```

---

## 6. Validaciones esperadas

### 6.1 `SY.frontdesk.gov`

Esperado:

- publish/update exitoso
- restart de `sy-frontdesk-gov.service` o del servicio singleton equivalente
- sin delete
- sin respawn
- sin `CONFIG_SET`
- si el host usa `ExecStart=/usr/bin/sy-frontdesk-gov`, el proceso activo debe reflejar el binario instalado y no solo el runtime publicado en `dist`

Logs utiles:

```bash
sudo journalctl -u "$FRONTDESK_UNIT" --since "10 min ago" --no-pager | rg -n "frontdesk_handoff|frontdesk_result|ilk_register|needs_input|REGISTERED|MISSING_REQUIRED_FIELDS"
```

### 6.2 `IO.api`

Esperado:

- delete best-effort previo
- si el delete no logra remover completamente la instancia vieja, fallback a `--update-existing`
- deploy helper exitoso
- `GET /` responde schema/config
- listener en `127.0.0.1:18081`

Logs utiles:

```bash
sudo journalctl -u "$IO_API_UNIT" --since "10 min ago" --no-pager | rg -n "io-api|frontdesk|pending http request|delivered router reply|invalid_frontdesk_response|frontdesk_timeout|frontdesk_unavailable"
```

### 6.3 E2E happy path de regularizacion via frontdesk

Esperado:

- primer request con `external_user_id` nuevo:
  - `IO.api` registra `identity lookup miss`
  - `IO.api` llama en sync a `SY.frontdesk.gov`
  - `SY.frontdesk.gov` ejecuta `ILK_REGISTER`
  - `IO.api` recibe respuesta estructurada valida
  - el request HTTP continua a su `dst_node` final y responde `202 Accepted`
- segundo request con el mismo `external_user_id`:
  - `IO.api` registra `identity lookup hit`
  - `registration_status=complete`
  - `IO.api` no vuelve a llamar a `SY.frontdesk.gov`
  - el request HTTP continua directo al `dst_node` final y responde `202 Accepted`

Resultado observado en 2026-04-17:

- primer request validado con `external_user_id = support:new-user-001`
- segundo request validado reutilizando el mismo `external_user_id`
- `SY.frontdesk.gov` solo participo en el primer request
- `IO.api` reporto `waiting for structured frontdesk reply` solo en el primer request
- no hubo `invalid_frontdesk_response` despues de corregir la emision opcional de `error_code`

### 6.4 Negative control: payload invalido de `IO.api`

El segundo request del script no apunta a validar frontdesk.  
Valida que `IO.api` siga rechazando el request incompleto antes de enviar handoff.

Esperado:

- HTTP `422`
- `error_code = "invalid_payload"` o `subject_data_incomplete`, segun el punto exacto de validacion

---

## 7. Comandos manuales de soporte

### Ver `GET /` de `IO.api`

```bash
curl -sS "http://$LISTEN_ADDRESS:$LISTEN_PORT/" | jq .
```

### Reiniciar solo `IO.api`

```bash
sudo systemctl restart "$IO_API_UNIT"
```

### Reiniciar solo `SY.frontdesk.gov`

```bash
sudo systemctl restart "$FRONTDESK_UNIT"
```

### Si el update de frontdesk quedo raro

Solo como fallback manual:

```bash
sudo systemctl restart sy-orchestrator
```

No es el camino default de este runbook.

---

## 8. Notas

- este archivo deja todos los pasos en un solo lugar, como pediste;
- para este host, la comprobacion operativa de `SY.frontdesk.gov` debe incluir `systemctl cat sy-frontdesk-gov.service` y, si hace falta, verificacion del binario efectivo en `/usr/bin/sy-frontdesk-gov`;
- si despues queres, se puede separar en:
  - script permanente para `IO.api`
  - script permanente para `SY.frontdesk.gov`
  - runbook de pruebas E2E
- por ahora, el helper temporal en `/tmp` evita tocar `scripts/` del repo.
