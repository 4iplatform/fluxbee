#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  publish-runtime.sh --runtime <name> --version <version> --binary <path> [options]

Required:
  --runtime <name>         Runtime key in dist manifest (example: AI.chat, IO.slack)
  --version <version>      Runtime version (example: 0.1.0)
  --binary <path>          Built binary to publish

Options:
  --dist-root <path>       Dist root path (default: /var/lib/fluxbee/dist)
  --set-current            Set this version as manifest.runtimes.<runtime>.current
  --sudo                   Use sudo for writes to dist paths
  -h, --help               Show this help

Notes:
  - Publishes binary to: <dist-root>/runtimes/<runtime>/<version>/bin/
  - Creates start script at: <dist-root>/runtimes/<runtime>/<version>/bin/start.sh
  - Updates manifest atomically: <dist-root>/runtimes/manifest.json
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: required command not found: $1" >&2
    exit 1
  fi
}

RUNTIME=""
VERSION=""
BINARY=""
DIST_ROOT="/var/lib/fluxbee/dist"
SET_CURRENT=0
USE_SUDO=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runtime)
      RUNTIME="${2:-}"
      shift 2
      ;;
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --binary)
      BINARY="${2:-}"
      shift 2
      ;;
    --dist-root)
      DIST_ROOT="${2:-}"
      shift 2
      ;;
    --set-current)
      SET_CURRENT=1
      shift
      ;;
    --sudo)
      USE_SUDO=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$RUNTIME" || -z "$VERSION" || -z "$BINARY" ]]; then
  echo "Error: --runtime, --version and --binary are required" >&2
  usage
  exit 1
fi

if [[ ! -f "$BINARY" ]]; then
  echo "Error: binary does not exist: $BINARY" >&2
  exit 1
fi

require_cmd python3
require_cmd sha256sum

SUDO=""
if [[ "$USE_SUDO" == "1" ]]; then
  SUDO="sudo"
fi

RUNTIMES_ROOT="$DIST_ROOT/runtimes"
RUNTIME_DIR="$RUNTIMES_ROOT/$RUNTIME/$VERSION"
BIN_DIR="$RUNTIME_DIR/bin"
MANIFEST_PATH="$RUNTIMES_ROOT/manifest.json"
BINARY_NAME="$(basename "$BINARY")"
START_SH="$BIN_DIR/start.sh"

$SUDO install -d "$BIN_DIR"
$SUDO install -m 0755 "$BINARY" "$BIN_DIR/$BINARY_NAME"

tmp_start="$(mktemp)"
cat >"$tmp_start" <<EOF
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
exec "\$SCRIPT_DIR/$BINARY_NAME" "\$@"
EOF
$SUDO install -m 0755 "$tmp_start" "$START_SH"
rm -f "$tmp_start"

tmp_manifest="$(mktemp)"
python3 - "$MANIFEST_PATH" "$RUNTIME" "$VERSION" "$SET_CURRENT" >"$tmp_manifest" <<'PY'
import datetime
import json
import pathlib
import sys
import time

manifest_path = pathlib.Path(sys.argv[1])
runtime = sys.argv[2]
version = sys.argv[3]
set_current = sys.argv[4] == "1"

doc = {
    "schema_version": 1,
    "version": 0,
    "updated_at": None,
    "runtimes": {},
}

if manifest_path.exists():
    try:
        loaded = json.loads(manifest_path.read_text(encoding="utf-8"))
        if isinstance(loaded, dict):
            doc.update(loaded)
    except Exception:
        pass

schema_version = doc.get("schema_version")
if not isinstance(schema_version, int) or schema_version < 1:
    schema_version = 1
doc["schema_version"] = schema_version
doc["version"] = int(time.time() * 1000)
doc["updated_at"] = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")

runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes

entry = runtimes.get(runtime)
if not isinstance(entry, dict):
    entry = {}

available = entry.get("available")
if not isinstance(available, list):
    available = []
available = [str(v) for v in available if str(v).strip()]
if version not in available:
    available.append(version)
entry["available"] = sorted(set(available))

if set_current or not isinstance(entry.get("current"), str) or not entry.get("current"):
    entry["current"] = version

runtimes[runtime] = entry

print(json.dumps(doc, indent=2, sort_keys=True))
PY

$SUDO install -m 0644 "$tmp_manifest" "$MANIFEST_PATH"
rm -f "$tmp_manifest"

manifest_version="$(python3 - "$MANIFEST_PATH" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    d = json.load(f)
print(int(d.get("version", 0)))
PY
)"
manifest_hash="$($SUDO sha256sum "$MANIFEST_PATH" | awk '{print $1}')"

echo "Published runtime: $RUNTIME@$VERSION"
echo "Binary path: $BIN_DIR/$BINARY_NAME"
echo "Start script: $START_SH"
echo "Manifest: $MANIFEST_PATH"
echo "manifest_version=$manifest_version"
echo "manifest_hash=$manifest_hash"
