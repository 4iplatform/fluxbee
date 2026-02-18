# SY.admin - Checklist E2E (curl)

Checklist operativo corto para validar `SY.admin -> SY.orchestrator` via API.

## 0) Base URL

Usar una de estas dos:

```bash
# directo al servicio local
BASE="http://127.0.0.1:8080"

# via nginx reverse proxy (debe strippear /control)
# BASE="https://fluxbee.ai/control"
```

## 1) Health y estado local

```bash
curl -sS "$BASE/health"
curl -sS "$BASE/hive/status"
curl -sS "$BASE/hives"
```

Esperado:
- `{"status":"ok"}` en `/health`
- `status=ok` en `hive/status`
- lista JSON de hives en `/hives`

## 2) Alta/baja de hive (orchestrator)

```bash
# ajustar address al worker real
HIVE_ID="worker-test"
HIVE_ADDR="192.168.8.50"

curl -sS -X POST "$BASE/hives" \
  -H 'Content-Type: application/json' \
  -d "{\"hive_id\":\"$HIVE_ID\",\"address\":\"$HIVE_ADDR\"}"

curl -sS "$BASE/hives/$HIVE_ID"

# cleanup
curl -sS -X DELETE "$BASE/hives/$HIVE_ID"
```

Esperado:
- `status=ok` en alta
- `status=ok` + datos en get hive
- `status=ok` en delete
- `delete` detiene y deshabilita servicios Fluxbee remotos del worker (`rt-gateway`, `sy-config-routes`, `sy-opa-rules`, `sy-identity`) antes de borrar metadata local.

### 2.1 Matriz de errores add_hive (rápido)

Script automático:

```bash
bash scripts/admin_add_hive_matrix.sh
```

Casos cubiertos por script:
- `INVALID_ADDRESS` -> HTTP `400`
- `INVALID_HIVE_ID` -> HTTP `400`
- `HIVE_EXISTS` -> HTTP `409` (si existe al menos una hive)
- `SSH_*` -> HTTP `502/504`

Caso manual adicional:
- `WAN_NOT_AUTHORIZED` -> HTTP `409`
  - Cuando motherbee tiene `wan.authorized_hives` no vacio y falta el `hive_id` solicitado.
- `WAN_TIMEOUT` -> HTTP `504`
  - Requiere host remoto que bootstrappee pero no logre conexión WAN dentro de 60s.

## 3) Router remoto por hive

```bash
HIVE_ID="worker-test"

curl -sS "$BASE/hives/$HIVE_ID/routers"

curl -sS -X POST "$BASE/hives/$HIVE_ID/routers" \
  -H 'Content-Type: application/json' \
  -d '{"service":"rt-gateway"}'

curl -sS -X DELETE "$BASE/hives/$HIVE_ID/routers/rt-gateway"
```

Esperado:
- `list_routers` responde `status=ok`
- en hive remoto, el `name` debe quedar con sufijo `@{hive}` (ej: `RT.gateway@worker-220`), no `@sandbox`.
- start/stop router responde `status=ok`

## 4) Node remoto por hive

```bash
HIVE_ID="worker-test"

# ejemplo: runtime declarado en runtime-manifest.json
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H 'Content-Type: application/json' \
  -d '{"runtime":"wf.echo","version":"current"}'

curl -sS "$BASE/hives/$HIVE_ID/nodes"
```

Nota:
- `DELETE /hives/{id}/nodes/{name}` existe, pero para casos con unit generado automaticamente conviene validar primero el identificador operativo a matar.
- en hive remoto, los `name` listados deben pertenecer al hive target (ej: `SY.config.routes@worker-220`), no al motherbee.

## 5) Config storage (broadcast)

```bash
curl -sS "$BASE/config/storage"

curl -sS -X PUT "$BASE/config/storage" \
  -H 'Content-Type: application/json' \
  -d '{"path":"/var/lib/fluxbee/storage"}'
```

Esperado:
- `GET` devuelve `status=ok` y `path`
- `PUT` devuelve `status=ok`
- `version` es opcional; si se envia manual y no es mayor al actual, responde `409 VERSION_MISMATCH`

## 6) Config VPN (sanity)

```bash
curl -sS -X PUT "$BASE/config/vpns" \
  -H 'Content-Type: application/json' \
  -d '{"vpns":[
    {"pattern":"WF.echo","match_kind":"PREFIX","vpn_id":20},
    {"pattern":"WF.listen","match_kind":"PREFIX","vpn_id":20}
  ]}'
```

Esperado:
- `{"status":"ok"}`

## 7) Verificacion por logs

```bash
sudo journalctl -u sy-admin -n 120 --no-pager
sudo journalctl -u sy-orchestrator -n 120 --no-pager
```

Buscar:
- acciones admin recibidas en orchestrator
- respuestas `status=ok`/`status=error` con `error_code` cuando aplique

## 8) Nota Nginx

Si con proxy falla `/control/*` con `not_found`, revisar que el `location /control/` haga `proxy_pass` con `/` final para remover prefijo:

```nginx
location /control/ {
  proxy_pass http://127.0.0.1:8080/;
}
```

## 9) Sin legacy

Estos endpoints legacy ya no existen y deben devolver `404`:

```bash
curl -sS "$BASE/nodes"
curl -sS "$BASE/routers"
```
