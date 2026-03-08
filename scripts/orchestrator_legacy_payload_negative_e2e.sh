#!/usr/bin/env bash
set -euo pipefail

# E2E-6: payloads legacy deben fallar explicitamente.
# Cubre:
# - run_node con campo legacy "name"
# - run_node con campo legacy "version"
# - update con campo legacy "version"
# - update con campo legacy "hash"
# - acción legacy "RUNTIME_UPDATE" (debe responder INVALID_REQUEST)
#
# Uso:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#   bash scripts/orchestrator_legacy_payload_negative_e2e.sh
#
# Opcionales:
#   BUILD_BIN=1                      # compila orch_system_diag si hace falta (default: 1)
#   ORCH_TIMEOUT_SECS=30             # timeout para test de SYSTEM_UPDATE/RUNTIME_UPDATE legacy

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
BUILD_BIN="${BUILD_BIN:-1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-30}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing command '$1'" >&2
    exit 1
  fi
}

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url"
  fi
}

json_get() {
  local path="$1"
  local file="$2"
  python3 - "$path" "$file" <<'PY'
import json
import sys

path = sys.argv[1]
file = sys.argv[2]
try:
    with open(file, "r", encoding="utf-8") as fh:
        data = json.load(fh)
except Exception:
    print("")
    raise SystemExit(0)

value = data
for part in path.split("."):
    if not part:
        continue
    if isinstance(value, dict):
        value = value.get(part)
    else:
        value = None
        break

if value is None:
    print("")
elif isinstance(value, bool):
    print("true" if value else "false")
elif isinstance(value, (dict, list)):
    print(json.dumps(value))
else:
    print(str(value))
PY
}

assert_invalid_request() {
  local label="$1"
  local status="$2"
  local body_file="$3"
  echo "[HTTP] status=$status body=$(cat "$body_file")"
  if [[ "$status" != "400" ]]; then
    echo "FAIL[$label]: expected HTTP 400, got $status" >&2
    exit 1
  fi
  local code
  code="$(json_get "error_code" "$body_file")"
  if [[ "$code" != "INVALID_REQUEST" ]]; then
    echo "FAIL[$label]: expected error_code=INVALID_REQUEST, got '$code'" >&2
    exit 1
  fi
}

require_cmd curl
require_cmd python3
require_cmd cargo

echo "E2E-6 legacy payload negative: BASE=$BASE HIVE_ID=$HIVE_ID"

echo "Step 1/5: run_node legacy field 'name' -> INVALID_REQUEST"
status="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$TMPDIR/run-name.json" \
  '{"name":"WF.legacy.bad","runtime":"wf.orch.diag","runtime_version":"current"}')"
assert_invalid_request "run_node-name" "$status" "$TMPDIR/run-name.json"

echo "Step 2/5: run_node legacy field 'version' -> INVALID_REQUEST"
status="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$TMPDIR/run-version.json" \
  '{"node_name":"WF.legacy.bad","runtime":"wf.orch.diag","version":"current"}')"
assert_invalid_request "run_node-version" "$status" "$TMPDIR/run-version.json"

echo "Step 3/5: update legacy field 'version' -> INVALID_REQUEST"
status="$(http_call "POST" "$BASE/hives/$HIVE_ID/update" "$TMPDIR/update-version.json" \
  '{"category":"runtime","version":1,"manifest_hash":"abc"}')"
assert_invalid_request "update-version" "$status" "$TMPDIR/update-version.json"

echo "Step 4/5: update legacy field 'hash' -> INVALID_REQUEST"
status="$(http_call "POST" "$BASE/hives/$HIVE_ID/update" "$TMPDIR/update-hash.json" \
  '{"category":"runtime","manifest_version":1,"hash":"abc"}')"
assert_invalid_request "update-hash" "$status" "$TMPDIR/update-hash.json"

echo "Step 5/5: system legacy action RUNTIME_UPDATE -> INVALID_REQUEST"
if [[ "$BUILD_BIN" == "1" ]]; then
  (cd "$ROOT_DIR" && cargo build --release --bin orch_system_diag >/dev/null)
fi
if [[ ! -x "$ROOT_DIR/target/release/orch_system_diag" ]]; then
  echo "FAIL: missing $ROOT_DIR/target/release/orch_system_diag (set BUILD_BIN=1)" >&2
  exit 1
fi

(
  cd "$ROOT_DIR"
  ORCH_SEND_SYSTEM_UPDATE=0 \
  ORCH_SEND_LEGACY_RUNTIME_UPDATE=1 \
  ORCH_ONLY_SYSTEM_UPDATE=0 \
  ORCH_ONLY_LEGACY_RUNTIME_UPDATE=1 \
  ORCH_EXPECT_LEGACY_RUNTIME_UPDATE_ERROR_CODE="INVALID_REQUEST" \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  ./target/release/orch_system_diag
)

echo "orchestrator legacy payload negative E2E passed."
