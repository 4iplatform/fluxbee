#!/usr/bin/env bash
set -euo pipefail

# SY.admin / SY.orchestrator WAN integration suite:
# - add_hive with WAN freshness gate (expects wan_connected=true)
# - remote WAN stale/recovery cycle
# - remove_hive cleanup and NOT_FOUND verification
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   HIVE_ADDR="192.168.8.220" \
#   bash scripts/admin_wan_integration_suite.sh
#
# Optional:
#   STALE_TIMEOUT_SECS=120
#   RECOVERY_TIMEOUT_SECS=60
#   POLL_INTERVAL_SECS=2

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-192.168.8.220}"
STALE_TIMEOUT_SECS="${STALE_TIMEOUT_SECS:-120}"
RECOVERY_TIMEOUT_SECS="${RECOVERY_TIMEOUT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local key="$1"
  local file="$2"
  python3 - "$key" "$file" <<'PY'
import json
import sys

key = sys.argv[1]
path = sys.argv[2]
try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

value = data
for part in key.split("."):
    if isinstance(value, dict):
        value = value.get(part, "")
    elif isinstance(value, list):
        if part.isdigit():
            idx = int(part)
            if 0 <= idx < len(value):
                value = value[idx]
            else:
                value = ""
                break
        else:
            value = ""
            break
    else:
        value = ""
        break

if isinstance(value, (dict, list)):
    print(json.dumps(value))
else:
    print(value if value is not None else "")
PY
}

log_http_request() {
  local method="$1"
  local url="$2"
  local payload="${3:-}"
  echo "[HTTP] -> ${method} ${url}" >&2
  if [[ -n "$payload" ]]; then
    echo "[HTTP]    payload: ${payload}" >&2
  fi
}

log_http_response() {
  local status="$1"
  local body_file="$2"
  local compact
  compact="$(tr '\n' ' ' < "$body_file" | sed 's/[[:space:]]\+/ /g')"
  echo "[HTTP] <- status=${status} body=${compact}" >&2
}

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
  log_http_request "$method" "$url" "$payload"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$body_file" -w "%{http_code}" \
      -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$body_file" -w "%{http_code}" \
      -X "$method" "$url"
  fi
}

assert_eq() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL [$label]: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

require_cmd curl
require_cmd python3
require_cmd bash

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "Running WAN integration suite against BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR"

# 0) Health
health_body="$tmpdir/health.json"
status="$(http_call "GET" "$BASE/health" "$health_body")"
log_http_response "$status" "$health_body"
assert_eq "$status" "200" "GET /health/http"

# 1) Ensure clean starting point (delete if hive exists)
get_before="$tmpdir/get_before.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$get_before")"
log_http_response "$status" "$get_before"
if [[ "$status" == "200" && "$(json_get "status" "$get_before")" == "ok" ]]; then
  echo "INFO: hive '$HIVE_ID' exists, removing before add_hive integration check..."
  del_before="$tmpdir/del_before.json"
  status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID" "$del_before")"
  log_http_response "$status" "$del_before"
  assert_eq "$status" "200" "DELETE /hives/{id} pre-clean/http"
  assert_eq "$(json_get "status" "$del_before")" "ok" "DELETE /hives/{id} pre-clean/status"
fi

# 2) add_hive + WAN freshness gate
add_body="$tmpdir/add.json"
add_payload="{\"hive_id\":\"$HIVE_ID\",\"address\":\"$HIVE_ADDR\"}"
status="$(http_call "POST" "$BASE/hives" "$add_body" "$add_payload")"
log_http_response "$status" "$add_body"
assert_eq "$status" "200" "POST /hives/http"
assert_eq "$(json_get "status" "$add_body")" "ok" "POST /hives/status"
assert_eq "$(json_get "payload.wan_connected" "$add_body")" "true" "POST /hives/payload.wan_connected"
echo "OK: add_hive passed with WAN freshness (wan_connected=true)"

# 3) Stale/recovery integration
BASE="$BASE" \
HIVE_ID="$HIVE_ID" \
STALE_TIMEOUT_SECS="$STALE_TIMEOUT_SECS" \
RECOVERY_TIMEOUT_SECS="$RECOVERY_TIMEOUT_SECS" \
POLL_INTERVAL_SECS="$POLL_INTERVAL_SECS" \
bash scripts/admin_wan_stale_recovery_e2e.sh
echo "OK: WAN stale/recovery cycle passed"

# 4) remove_hive + NOT_FOUND check
remove_body="$tmpdir/remove.json"
status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID" "$remove_body")"
log_http_response "$status" "$remove_body"
assert_eq "$status" "200" "DELETE /hives/{id}/http"
assert_eq "$(json_get "status" "$remove_body")" "ok" "DELETE /hives/{id}/status"

get_after="$tmpdir/get_after.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$get_after")"
log_http_response "$status" "$get_after"
if [[ "$status" != "404" ]]; then
  echo "FAIL [GET /hives/{id} after remove/http]: expected 404, got '$status'" >&2
  exit 1
fi
error_code="$(json_get "error_code" "$get_after")"
if [[ -z "$error_code" ]]; then
  error_code="$(json_get "payload.error_code" "$get_after")"
fi
assert_eq "$error_code" "NOT_FOUND" "GET /hives/{id} after remove/error_code"
echo "OK: remove_hive cleanup validated (NOT_FOUND after delete)"

echo "WAN integration suite passed."
