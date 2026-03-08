# Runtime Rollout Desde Motherbee (v2)

Este instructivo cubre el flujo operativo actual para:

1. publicar un runtime nuevo en motherbee,
2. propagarlo a un hive destino,
3. ejecutar un nodo con `sy-admin`/`sy-orchestrator`.

Aplica al modelo v2 actual: el nodo se corre por API de admin (`/hives/{id}/nodes`) y el versionado se aplica por `SYSTEM_UPDATE` (`/hives/{id}/update`).

## 1. Variables base

```bash
BASE="http://127.0.0.1:8080"
MOTHER_HIVE="sandbox"          # hive local de motherbee
TARGET_HIVE="worker-220"       # hive donde vas a correr el nodo

RUNTIME="wf.demo.task"         # usar nombre exacto (case-sensitive)
VERSION="0.0.1"
NODE_NAME="WF.demo.task.node1" # nombre de nodo (sin @)
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

## 4. Verificar versión/hash local en motherbee

```bash
curl -sS "$BASE/hives/$MOTHER_HIVE/versions" | python3 -m json.tool
```

Verificá que aparezca:

- `payload.hive.runtimes.runtimes.<RUNTIME>`
- `payload.hive.runtimes.manifest_version`
- `payload.hive.runtimes.manifest_hash`

## 5. Propagar al hive destino (`SYSTEM_UPDATE`)

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

## 6. Ejecutar nodo en el hive destino

```bash
curl -sS -X POST "$BASE/hives/$TARGET_HIVE/nodes" \
  -H "Content-Type: application/json" \
  -d "{\"node_name\":\"$NODE_NAME\",\"runtime\":\"$RUNTIME\",\"runtime_version\":\"current\"}"; echo
```

Listar nodos:

```bash
curl -sS "$BASE/hives/$TARGET_HIVE/nodes" | python3 -m json.tool
```

## 7. Detener/eliminar nodo

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
- `RUNTIME_NOT_PRESENT`: existe en manifest pero falta `start.sh` en el hive destino (sync aún no convergió).
- `sync_pending` en `/update`: el destino todavía no tiene el manifest/hash esperado.
- `FORBIDDEN` en `/update`: origen no autorizado para acciones de sistema (revisar allowlist de system messages).
