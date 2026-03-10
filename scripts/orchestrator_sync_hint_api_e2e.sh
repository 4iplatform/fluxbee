#!/usr/bin/env bash
set -euo pipefail

# SYSTEM_SYNC_HINT API E2E
#
# Uso:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" CHANNEL="blob" \
#   WAIT_FOR_IDLE=1 TIMEOUT_MS=30000 bash scripts/orchestrator_sync_hint_api_e2e.sh
#
# Opcionales:
#   CHANNEL="blob|dist|all"         (default: all)
#   WAIT_FOR_IDLE="1|0"             (default: 1)
#   TIMEOUT_MS="<u64>"              (default: 30000)
#   REQUIRE_OK="1|0"                (default: 0; si 1 exige payload.status=ok)

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
CHANNEL="${CHANNEL:-all}" # blob|dist|all
WAIT_FOR_IDLE="${WAIT_FOR_IDLE:-1}"
TIMEOUT_MS="${TIMEOUT_MS:-30000}"
REQUIRE_OK="${REQUIRE_OK:-0}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local json_doc="$1"
  local path="$2"
  JSON_DOC="$json_doc" python3 - "$path" <<'PY'
import json
import os
import sys

path = sys.argv[1]
raw = os.environ.get("JSON_DOC", "")
try:
    doc = json.loads(raw)
except Exception:
    print("")
    raise SystemExit(0)

value = doc
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

post_sync_hint() {
  local ch="$1"
  local folder
  if [[ "$ch" == "blob" ]]; then
    folder="fluxbee-blob"
  else
    folder="fluxbee-dist"
  fi
  curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
    -H "Content-Type: application/json" \
    -d "{\"channel\":\"$ch\",\"folder_id\":\"$folder\",\"wait_for_idle\":$([[ "$WAIT_FOR_IDLE" == "1" ]] && echo true || echo false),\"timeout_ms\":$TIMEOUT_MS}"
}

run_case() {
  local ch="$1"
  echo "Case channel=$ch"
  local resp
  resp="$(post_sync_hint "$ch")"
  echo "$resp"

  local top_status payload_status
  top_status="$(json_get "$resp" "status")"
  payload_status="$(json_get "$resp" "payload.status")"
  if [[ "$top_status" == "error" ]]; then
    echo "FAIL: top-level status=error (channel=$ch)" >&2
    exit 1
  fi
  if [[ "$payload_status" != "ok" && "$payload_status" != "sync_pending" ]]; then
    echo "FAIL: expected payload.status in {ok,sync_pending}, got '$payload_status' (channel=$ch)" >&2
    exit 1
  fi
  if [[ "$REQUIRE_OK" == "1" && "$payload_status" != "ok" ]]; then
    echo "FAIL: REQUIRE_OK=1 and payload.status='$payload_status' (channel=$ch)" >&2
    exit 1
  fi
}

require_cmd curl
require_cmd python3

if [[ "$CHANNEL" != "blob" && "$CHANNEL" != "dist" && "$CHANNEL" != "all" ]]; then
  echo "FAIL: CHANNEL must be blob|dist|all (got '$CHANNEL')" >&2
  exit 1
fi
if ! [[ "$TIMEOUT_MS" =~ ^[0-9]+$ ]] || [[ "$TIMEOUT_MS" -le 0 ]]; then
  echo "FAIL: TIMEOUT_MS must be positive integer (got '$TIMEOUT_MS')" >&2
  exit 1
fi
if [[ "$WAIT_FOR_IDLE" != "0" && "$WAIT_FOR_IDLE" != "1" ]]; then
  echo "FAIL: WAIT_FOR_IDLE must be 0|1 (got '$WAIT_FOR_IDLE')" >&2
  exit 1
fi

echo "SYSTEM_SYNC_HINT API E2E: BASE=$BASE HIVE_ID=$HIVE_ID CHANNEL=$CHANNEL WAIT_FOR_IDLE=$WAIT_FOR_IDLE TIMEOUT_MS=$TIMEOUT_MS REQUIRE_OK=$REQUIRE_OK"

if [[ "$CHANNEL" == "all" ]]; then
  run_case "blob"
  run_case "dist"
else
  run_case "$CHANNEL"
fi

echo "orchestrator SYSTEM_SYNC_HINT API E2E passed."
