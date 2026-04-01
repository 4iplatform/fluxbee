# IO Slack Deploy Runbook (Fluxbee-compliant path)

## 1) Objetivo

Este runbook describe cómo desplegar y operar `IO.slack` por el camino canónico de Fluxbee:

1. publicar runtime en `dist/`
2. ejecutar `SYSTEM_UPDATE` en el hive destino
3. `SPAWN_NODE` (o restart controlado del nodo)
4. configurar runtime del nodo por control-plane (`CONFIG_GET` / `CONFIG_SET`)

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

Contrato de spawn vigente en `io-slack`:

- requiere `FLUXBEE_NODE_NAME` provisto por la plataforma
- usa ruta managed canónica para `config.json`
- no usa fallback/env legacy de spawn

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

Ejemplo mínimo con bootstrap básico:

```bash
curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "{
    \"node_name\":\"$NODE_NAME\",
    \"runtime\":\"$RUNTIME\",
    \"runtime_version\":\"current\",
    \"tenant_id\":\"<TENANT_ID>\",
    \"config\":{
      \"tenant_id\":\"<TENANT_ID>\"
    }
  }"
```

Notas:

- En hives con registro de identidad obligatorio en spawn, `tenant_id` es requerido.
- El nodo puede arrancar sin tokens/prompt funcionales y quedar no operativo (`FAILED_CONFIG` o equivalente) hasta `CONFIG_SET` válido.

---

## 4) Actualizar código (nuevo build)

Para cambios de código, el flujo correcto no es pisar `/usr/bin` manualmente.

### Secuencia recomendada

1. Publicar nueva versión runtime:

```bash
NEW_VERSION="0.1.1"
bash scripts/publish-io-runtime.sh --kind slack --version "$NEW_VERSION" --set-current --sudo
```

2. Ejecutar `SYSTEM_UPDATE` con el nuevo `manifest_version/hash`.
3. Reiniciar proceso del nodo para que tome el runtime nuevo:
   - opción A: `KILL_NODE` + `SPAWN_NODE` (recomendado, explícito)
   - opción B: si tu capa admin expone restart directo, usar restart del nodo

Atajo automatizado (recomendado):

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "$NEW_VERSION" \
  --node-name "$NODE_NAME" \
  --update-existing \
  --sync-hint \
  --sudo
```

`--update-existing` hace:

- publish runtime nuevo
- update runtime con retries en `sync_pending`
- GET config actual del nodo
- kill-first + spawn reutilizando esa config

---

## 5) Actualizar solo configuración (sin cambio de binario)

Para hot reload de configuración funcional de `IO.slack`, el camino canónico es control-plane del nodo:

1. `CONFIG_GET` para ver estado/config efectiva redacted.
2. `CONFIG_SET` con `config_version` monotónica y `apply_mode=replace`.
3. verificar `CONFIG_RESPONSE` (`ok/error`) y campos `apply`.

Ejemplo `CONFIG_GET`:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" \
  -H "Content-Type: application/json" \
  -d '{"requested_by":"archi"}'
```

Ejemplo `CONFIG_SET`:

```bash
curl -sS -X POST \
  "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
  -H "Content-Type: application/json" \
  -d '{
    "schema_version":1,
    "config_version":8,
    "apply_mode":"replace",
    "config":{
      "io":{"dst_node":"AI.chat@motherbee"},
      "slack":{"app_token_ref":"env:SLACK_APP_TOKEN","bot_token_ref":"env:SLACK_BOT_TOKEN"}
    }
  }'
```

Interpretación de `apply` en `CONFIG_RESPONSE`:

- `hot_applied`: cambios aplicados en caliente (sin reinicio de proceso)
- `reinit_performed`: reinicialización interna del adapter realizada
- `restart_required`: cambios que requieren restart de proceso para efecto completo

Importante:

- `PUT /hives/{hive}/nodes/{node}/config` (core) persiste `config.json` administrada, pero no reemplaza `CONFIG_SET` como contrato de hot reload runtime en IO.
- `CONFIG_CHANGED` se trata como señal informativa en IO v1.

---

## 6) Recuperación desde `FAILED_CONFIG`

Caso típico: nodo arrancó con bootstrap mínimo o config inválida y quedó en estado no operativo.

Secuencia de recuperación:

1. enviar `CONFIG_GET` y confirmar estado + último error.
2. corregir payload y enviar `CONFIG_SET` con versión mayor.
3. validar `CONFIG_RESPONSE status=ok` y transición de estado.
4. ejecutar prueba funcional real (inbound/outbound).

Si `CONFIG_SET` responde error:

- `invalid_config`: revisar shape y campos obligatorios del adapter Slack.
- `stale_config_version`: reenviar con `config_version` mayor a la última aplicada.
- `config_persist_error`: revisar permisos/path de persistencia del state dinámico.

---

## 7) Validación post-deploy

Mínimo:

- `status` del nodo en API admin
- logs del runtime en el hive
- prueba real de inbound `app_mention` y outbound `chat.postMessage`

Opcional:

- `CONFIG_GET` y revisar `metrics.control_plane`
- `GET /hives/{id}/versions` y revisar readiness de `IO.slack` en versión actual

---

## 8) Camino rápido local (no canónico)

`scripts/install-io.sh` sigue siendo útil para desarrollo local rápido (systemd + env file),
pero no reemplaza el pipeline canónico `publish -> update -> spawn`.

---

## 9) Script automatizado (publish + update + spawn opcional)

Se agregó:

- `scripts/deploy-io-slack.sh`

Ejemplo (deploy + spawn):

```bash
bash scripts/deploy-io-slack.sh \
  --base "$BASE" \
  --hive-id "$HIVE_ID" \
  --version "0.1.0" \
  --node-name "IO.slack.T123@$HIVE_ID" \
  --tenant-id "<TENANT_ID>" \
  --app-token "xapp-REPLACE_ME" \
  --bot-token "xoxb-REPLACE_ME" \
  --identity-target "SY.identity@$HIVE_ID" \
  --identity-timeout-ms 10000 \
  --sync-hint \
  --kill-first \
  --sudo
```

El script:

- parsea `manifest_version`/`manifest_hash` desde publish,
- reintenta `update` si recibe `sync_pending`,
- loguea cada paso en archivo (`/tmp/deploy-io-slack-<ts>.log` por defecto),
- soporta `--update-existing` para actualizar código de nodo existente reutilizando su config.
- opcionalmente permite continuar aun con `sync_pending` vía `--allow-sync-pending` (solo para entornos de prueba con manifest global inconsistente).

### Nota sobre `sync_pending` permanente

Si `update` queda en `sync_pending` por faltantes de otros runtimes del manifest (por ejemplo `wf.node.status.*.diag`), el deploy de `IO.slack` puede bloquearse aunque `IO.slack` se haya publicado correctamente.

En entornos de prueba podés usar:

```bash
bash scripts/deploy-io-slack.sh ... --allow-sync-pending
```

Esto permite continuar a `spawn`, pero no corrige el problema de fondo del manifest global.

---

## 10) Nota de contrato `text/v1` (offload > limite)

En estado actual, la regla de offload por tamano para `text/v1` esta centralizada en `fluxbee_sdk::NodeSender::send`:

- `io-slack` arma payload base (`content` + `attachments`).
- `NodeSender::send` decide inline vs `content_ref` usando defaults internos del SDK (`max_message_bytes=64KB`, `message_overhead_bytes=2048`).

Logs esperados:

- `sdk send text/v1 normalized` (debug)
- `sdk send text/v1 processed as blob` (info, solo cuando hubo offload)

Esto evita divergencias entre adapters IO y mantiene comportamiento uniforme de `text/v1`.

Importante:
- `io.blob.*` configurado por control-plane del nodo no aplica automaticamente al enforcement de `NodeSender::send` en esta etapa.

Nota de alcance de pruebas:

- Para validar de forma determinística payloads de texto muy grandes (ejemplo `>64KB`), usar `io-sim`.
- En Slack real, `chat.postMessage` tiene límites propios del canal y trunca texto por encima de 40.000 caracteres, por lo que no es un canal confiable para probar umbrales internos extremos.
  - Fuente: https://docs.slack.dev/reference/methods/chat.postMessage/ ("Truncating content").

Estado actual outbound a considerar en pruebas:

- Si `io-slack` recibe texto largo via `content_ref`, hoy lo resuelve y lo publica como texto por `chat.postMessage`.
- Por lo tanto, textos muy largos pueden quedar truncados por el límite de Slack.
- Validación de fallback canónico (archivo/snippet/chunking) queda pendiente de norma específica del canal.
