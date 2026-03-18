# IO Slack Deploy Runbook (Fluxbee-compliant path)

## 1) Objetivo

Este runbook describe como desplegar y actualizar `IO.slack` por el camino canónico de Fluxbee:

1. publish runtime en `dist/`
2. `SYSTEM_UPDATE` en el hive destino
3. `SPAWN_NODE` (o restart controlado del nodo)

No depende de `install-io.sh` para el deploy final.

---

## 2) Prerrequisitos

- `sy-orchestrator`, `sy-admin`, `rt-gateway` activos en el hive.
- API admin disponible (`BASE`, por ejemplo `http://127.0.0.1:8080`).
- Binario `io-slack` compilado o compilable.
- `scripts/publish-io-runtime.sh` disponible.

Variables sugeridas:

```bash
BASE="http://127.0.0.1:8080"
HIVE_ID="motherbee"
RUNTIME="IO.slack"
VERSION="0.1.0"
NODE_NAME="IO.slack.T123@$HIVE_ID"
```

---

## 3) Deploy inicial (compliant)

### Paso 1: publish runtime

```bash
bash scripts/publish-io-runtime.sh --kind slack --version "$VERSION" --set-current --sudo
```

Guardar salida:
- `manifest_version=<...>`
- `manifest_hash=<...>`

### Paso 2: (opcional recomendado) sync-hint dist

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
  -H "Content-Type: application/json" \
  -d '{"channel":"dist","wait_for_idle":true,"timeout_ms":30000}'
```

### Paso 3: SYSTEM_UPDATE runtime

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
  -H "Content-Type: application/json" \
  -d "{
    \"category\":\"runtime\",
    \"manifest_version\": $MANIFEST_VERSION,
    \"manifest_hash\": \"$MANIFEST_HASH\"
  }"
```

Debe responder `status=ok` para continuar.

### Paso 4: SPAWN_NODE de IO.slack

Ejemplo minimo con tokens inline (MVP):

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"current\",
    \"config\":{
      \"slack\":{
        \"app_token\":\"xapp-REPLACE_ME\",
        \"bot_token\":\"xoxb-REPLACE_ME\"
      },
      \"identity_target\":\"SY.identity\",
      \"identity_fallback_target\":\"SY.identity@$HIVE_ID\",
      \"identity_timeout_ms\":10000
    }
  }"
```

Notas:
- `IO.slack` soporta tokens por config de spawn:
  - `slack.app_token`, `slack.bot_token`
  - `slack.app_token_ref`, `slack.bot_token_ref` con `env:VAR`
- Si tambien existen `SLACK_APP_TOKEN`/`SLACK_BOT_TOKEN` en env, env tiene precedencia.

---

## 4) Actualizar codigo (nuevo build)

Para cambios de codigo, el flujo correcto no es "pisar /usr/bin" manualmente.

### Secuencia recomendada

1. Publicar nueva version runtime:

```bash
NEW_VERSION="0.1.1"
bash scripts/publish-io-runtime.sh --kind slack --version "$NEW_VERSION" --set-current --sudo
```

2. Ejecutar `SYSTEM_UPDATE` con el nuevo `manifest_version/hash`.
3. Reiniciar proceso del nodo para que tome el runtime nuevo:
   - opcion A: `KILL_NODE` + `SPAWN_NODE` (recomendado, explicito)
   - opcion B: si tu capa admin expone restart directo, usar restart del nodo

Ejemplo A (kill + spawn):

```bash
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME"

curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"IO.slack\",
    \"runtime_version\":\"current\",
    \"config\":{ ... }
  }"
```

---

## 5) Actualizar solo configuracion (sin cambio de binario)

Si solo cambian tokens o parametros:

1. actualizar config del nodo via admin API (si esta disponible `PUT .../config`), o
2. `KILL_NODE` + `SPAWN_NODE` con nuevo bloque `config`.

No hace falta publicar runtime nuevo si el binario no cambio.

---

## 6) Validacion post-deploy

Minimo:
- `status` del nodo en API admin
- logs del runtime en el hive
- prueba real de inbound `app_mention` y outbound `chat.postMessage`

Opcional:
- `GET /hives/{id}/versions` y revisar readiness de `IO.slack` en version actual

---

## 7) Camino rapido local (no canónico)

`scripts/install-io.sh` sigue siendo util para desarrollo local rapido (systemd + env file),
pero no reemplaza el pipeline canónico `publish -> update -> spawn`.

