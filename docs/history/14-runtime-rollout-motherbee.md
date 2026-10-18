# Runtime Rollout Desde Motherbee (v2)

Este instructivo cubre el flujo operativo actual para:

1. publicar un runtime nuevo en motherbee,
2. propagarlo a un hive destino,
3. ejecutar un nodo con `sy-admin`/`sy-orchestrator`.

Aplica al modelo v2 actual: el nodo se corre por API de admin (`/hives/{id}/nodes`) y el versionado se aplica por `SYSTEM_UPDATE` (`/hives/{id}/update`).
Referencia canónica de lifecycle: `docs/onworking/runtime-lifecycle-spec.md`.

## 1. Variables base

```bash
BASE="http://127.0.0.1:8080"
MOTHER_HIVE="motherbee"        # hive local de motherbee (fijo por convención)
TARGET_HIVE="worker-220"       # hive donde vas a correr el nodo

RUNTIME="wf.demo.task"         # usar nombre exacto (case-sensitive)
VERSION="0.0.1"
NODE_NAME="WF.demo.task.node1" # nombre de nodo (sin @)
TENANT_ID="tnt:xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" # requerido si identity strict aplica al runtime
```

## 2. Publicar ejecutable/runtime en motherbee

El orchestrator busca `start.sh` en:

- `/var/lib/fluxbee/dist/runtimes/<runtime>/<version>/bin/start.sh`

Publicá primero en `dist`:

```bash
RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$RUNTIME/$VERSION"
sudo mkdir -p "$RUNTIME_DIR/bin"

# Ejemplo mínimo: runtime de prueba
cat <<'EOF' | sudo tee "$RUNTIME_DIR/bin/start.sh" >/dev/null
#!/usr/bin/env bash
set -euo pipefail
exec /bin/sleep 3600
EOF

sudo chmod +x "$RUNTIME_DIR/bin/start.sh"
```

## 3. Actualizar manifest de runtimes en motherbee

Archivo esperado: `/var/lib/fluxbee/dist/runtimes/manifest.json`.

```bash
MANIFEST="/var/lib/fluxbee/dist/runtimes/manifest.json"
sudo mkdir -p /var/lib/fluxbee/dist/runtimes

sudo env RUNTIME="$RUNTIME" VERSION="$VERSION" python3 - <<'PY'
import json, os, datetime

manifest_path = "/var/lib/fluxbee/dist/runtimes/manifest.json"
runtime = os.environ["RUNTIME"]
version = os.environ["VERSION"]

try:
    with open(manifest_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except FileNotFoundError:
    doc = {
        "schema_version": 1,
        "version": 1,
        "updated_at": None,
        "runtimes": {}
    }

doc.setdefault("schema_version", 1)
doc["version"] = int(datetime.datetime.now(datetime.timezone.utc).timestamp() * 1000)
doc["updated_at"] = datetime.datetime.now(datetime.timezone.utc).isoformat()
doc.setdefault("runtimes", {})
entry = doc["runtimes"].setdefault(runtime, {"available": [], "current": version})
entry.setdefault("available", [])
if version not in entry["available"]:
    entry["available"].append(version)
entry["current"] = version

tmp = manifest_path + ".tmp"
with open(tmp, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=2, sort_keys=True)
    f.write("\n")
os.replace(tmp, manifest_path)
print("manifest updated:", manifest_path)
print("manifest_version:", doc["version"])
PY
```

## 4. Verificar manifest + readiness local en motherbee

```bash
resp="$(curl -sS "$BASE/hives/$MOTHER_HIVE/versions")"
echo "$resp" | python3 -m json.tool

echo "$resp" | jq -e --arg rt "$RUNTIME" --arg ver "$VERSION" '
  .payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present == true and
  .payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable == true
' >/dev/null || {
  echo "runtime no listo en motherbee (readiness false): $RUNTIME@$VERSION" >&2
  exit 1
}
```

Verificá que aparezca:

- `payload.hive.runtimes.runtimes.<RUNTIME>`
- `payload.hive.runtimes.runtimes.<RUNTIME>.readiness.<VERSION>`
- `payload.hive.runtimes.manifest_version`
- `payload.hive.runtimes.manifest_hash`

## 5. Confirmar convergencia `dist` en destino (`SYSTEM_SYNC_HINT`)

Antes de aplicar `SYSTEM_UPDATE`, confirmar convergencia del canal `dist` en el hive destino:

```bash
SYNC_HINT_TIMEOUT_MS=30000
SYNC_HINT_WAIT_SECS=120
deadline=$(( $(date +%s) + SYNC_HINT_WAIT_SECS ))
while (( $(date +%s) <= deadline )); do
  resp="$(curl -sS -X POST "$BASE/hives/$TARGET_HIVE/sync-hint" \
    -H "Content-Type: application/json" \
    -d "{\"channel\":\"dist\",\"folder_id\":\"fluxbee-dist\",\"wait_for_idle\":true,\"timeout_ms\":$SYNC_HINT_TIMEOUT_MS}")"
  status="$(python3 - <<'PY' "$resp"
import json,sys
try:
    d=json.loads(sys.argv[1])
except Exception:
    print("")
    raise SystemExit(0)
print(d.get("payload",{}).get("status",""))
PY
)"
  if [[ "$status" == "ok" ]]; then
    echo "$resp"
    break
  fi
  if [[ "$status" == "sync_pending" ]]; then
    sleep 2
    continue
  fi
  echo "sync-hint failed: $resp" >&2
  exit 1
done
```

## 6. Propagar al hive destino (`SYSTEM_UPDATE`)

Tomar `manifest_version` y `manifest_hash` del hive fuente (motherbee) y aplicarlo al destino:

```bash
read MANIFEST_VERSION MANIFEST_HASH < <(
  curl -sS "$BASE/hives/$MOTHER_HIVE/versions" | python3 - <<'PY'
import json,sys
d=json.load(sys.stdin)
r=d["payload"]["hive"]["runtimes"]
print(int(r.get("manifest_version",0)), r["manifest_hash"])
PY
)

echo "manifest_version=$MANIFEST_VERSION"
echo "manifest_hash=$MANIFEST_HASH"

curl -sS -X POST "$BASE/hives/$TARGET_HIVE/update" \
  -H "Content-Type: application/json" \
  -d "{\"category\":\"runtime\",\"manifest_version\":$MANIFEST_VERSION,\"manifest_hash\":\"$MANIFEST_HASH\"}"; echo
```

Si devuelve `payload.status=sync_pending`, esperá unos segundos y repetí el `POST /update`.
Si devuelve `payload.status=error` con `error_code=VERSION_MISMATCH`, no reintentes con la misma versión pedida: tomá el `manifest_version` vigente desde motherbee y reenviá el update.

Semántica actual:

- sin `runtime`, `SYSTEM_UPDATE category=runtime` valida el estado global de los runtimes `current` del manifest local;
- con `runtime` y opcionalmente `runtime_version`, usa **targeted readiness** para ese runtime/version puntual.

Ejemplo targeted:

```bash
curl -sS -X POST "$BASE/hives/$TARGET_HIVE/update" \
  -H "Content-Type: application/json" \
  -d "{\"category\":\"runtime\",\"manifest_version\":$MANIFEST_VERSION,\"manifest_hash\":\"$MANIFEST_HASH\",\"runtime\":\"$RUNTIME\",\"runtime_version\":\"$RUNTIME_VERSION\"}"; echo
```

En modo targeted, la respuesta puede seguir trayendo `global_runtime_health` como contexto, pero ese degradado global no debe bloquear el deploy puntual del runtime objetivo.

Loop recomendado:

```bash
UPDATE_WAIT_SECS=180
deadline=$(( $(date +%s) + UPDATE_WAIT_SECS ))
while (( $(date +%s) <= deadline )); do
  upd="$(curl -sS -X POST "$BASE/hives/$TARGET_HIVE/update" \
    -H "Content-Type: application/json" \
    -d "{\"category\":\"runtime\",\"manifest_version\":$MANIFEST_VERSION,\"manifest_hash\":\"$MANIFEST_HASH\"}")"
  status="$(echo "$upd" | jq -r '.payload.status // .status // ""')"
  code="$(echo "$upd" | jq -r '.error_code // .payload.error_code // ""')"
  if [[ "$status" == "ok" ]]; then
    echo "$upd"
    break
  fi
  if [[ "$status" == "sync_pending" ]]; then
    sleep 2
    continue
  fi
  echo "$upd" >&2
  if [[ "$code" == "VERSION_MISMATCH" ]]; then
    echo "update rechazado por VERSION_MISMATCH (local > requested)" >&2
  fi
  exit 1
done
```

## 6.1 Verificar readiness en destino antes de spawn

```bash
resp_target="$(curl -sS "$BASE/hives/$TARGET_HIVE/versions")"
echo "$resp_target" | jq -e --arg rt "$RUNTIME" '
  .payload.hive.runtimes.runtimes[$rt].current as $cv |
  .payload.hive.runtimes.runtimes[$rt].readiness[$cv].runtime_present == true and
  .payload.hive.runtimes.runtimes[$rt].readiness[$cv].start_sh_executable == true
' >/dev/null || {
  echo "runtime no listo en target hive para current: $RUNTIME" >&2
  exit 1
}
```

Para diagnóstico fino del runtime puntual:

```bash
curl -sS "$BASE/hives/$TARGET_HIVE/runtimes/$RUNTIME" | jq .
```

Ese endpoint expone:

- `readiness`
- `materialization`
- `targeted_runtime_health`
- `global_runtime_health`

## 7. Ejecutar nodo en el hive destino

```bash
curl -sS -X POST "$BASE/hives/$TARGET_HIVE/nodes" \
  -H "Content-Type: application/json" \
  -d "{\"node_name\":\"$NODE_NAME\",\"runtime\":\"$RUNTIME\",\"runtime_version\":\"current\",\"tenant_id\":\"$TENANT_ID\"}"; echo
```

Listar nodos:

```bash
curl -sS "$BASE/hives/$TARGET_HIVE/nodes" | python3 -m json.tool
```

## 8. Detener/eliminar nodo

```bash
curl -sS -X DELETE "$BASE/hives/$TARGET_HIVE/nodes/$NODE_NAME" \
  -H "Content-Type: application/json" \
  -d '{"force":false}'; echo
```

Forzado:

```bash
curl -sS -X DELETE "$BASE/hives/$TARGET_HIVE/nodes/$NODE_NAME" \
  -H "Content-Type: application/json" \
  -d '{"force":true}'; echo
```

## Troubleshooting rápido

- `RUNTIME_NOT_AVAILABLE`: el `runtime` enviado no existe en manifest (revisar nombre exacto/case).
- `RUNTIME_NOT_PRESENT`: existe en manifest pero la materialización local no está lista; revisar `materialization.blocking_reason` y `targeted_runtime_health`.
- `sync_pending` en `/sync-hint`: Syncthing aún no llegó a `idle` para `fluxbee-dist`.
- `sync_pending` en `/update`: el destino todavía no tiene el manifest/hash esperado o faltan artefactos del scope validado.
- `VERSION_MISMATCH` en `/update`: el destino tiene manifest local más nuevo que el solicitado (`local_manifest_version > manifest_version`).
- `IDENTITY_REGISTER_FAILED`: faltó `tenant_id` en `POST /nodes` (runtime con identity strict).
- `FORBIDDEN` en `/update`: origen no autorizado para acciones de sistema (revisar allowlist de system messages).
