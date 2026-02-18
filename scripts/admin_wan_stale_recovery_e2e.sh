#!/usr/bin/env bash
set -euo pipefail

# SY.admin WAN stale/recovery E2E:
# - Validates remote router status transition alive -> stale -> alive
# - Uses only admin/orchestrator HTTP API (no direct SSH in this script)
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#     bash scripts/admin_wan_stale_recovery_e2e.sh
#
# Optional:
#   ROUTER_SERVICE="rt-gateway"
#   STALE_TIMEOUT_SECS="90"
#   RECOVERY_TIMEOUT_SECS="60"
#   POLL_INTERVAL_SECS="2"
#   AUTO_RECOVER_ON_EXIT="1"

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
ROUTER_SERVICE="${ROUTER_SERVICE:-rt-gateway}"
STALE_TIMEOUT_SECS="${STALE_TIMEOUT_SECS:-90}"
RECOVERY_TIMEOUT_SECS="${RECOVERY_TIMEOUT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
AUTO_RECOVER_ON_EXIT="${AUTO_RECOVER_ON_EXIT:-1}"

ROUTER_KILLED="0"

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

run_router_best_effort() {
  local body="$1"
  local status
  status="$(http_call "POST" "$BASE/hives/$HIVE_ID/routers" "$body" "{\"service\":\"$ROUTER_SERVICE\"}")" || true
  if [[ -n "${status:-}" ]]; then
    log_http_response "$status" "$body" || true
  fi
}

cleanup() {
  if [[ "$AUTO_RECOVER_ON_EXIT" == "1" && "$ROUTER_KILLED" == "1" ]]; then
    echo "Cleanup: attempting router recovery on hive '$HIVE_ID'..." >&2
    local body
    body="$(mktemp)"
    run_router_best_effort "$body"
    rm -f "$body"
  fi
}

poll_router_status() {
  local expected="$1"
  local timeout_secs="$2"
  local label="$3"
  local start now elapsed
  local body="$tmpdir/poll_${label}.json"
  local status_code router_status api_status router_uuid

  start="$(date +%s)"
  while true; do
    status_code="$(http_call "GET" "$BASE/hives/$HIVE_ID/routers" "$body")"
    log_http_response "$status_code" "$body"

    if [[ "$status_code" == "200" ]]; then
      api_status="$(json_get "status" "$body")"
      router_status="$(json_get "payload.routers.0.status" "$body")"
      router_uuid="$(json_get "payload.routers.0.uuid" "$body")"
      if [[ "$api_status" == "ok" && "$router_status" == "$expected" ]]; then
        echo "OK: router status reached '$expected' (uuid=$router_uuid)"
        return 0
      fi
    fi

    now="$(date +%s)"
    elapsed=$((now - start))
    if (( elapsed >= timeout_secs )); then
      echo "FAIL [$label]: timeout waiting router status '$expected' (last_http=$status_code, last_status=${router_status:-})" >&2
      cat "$body" >&2
      return 1
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

trap cleanup EXIT

require_cmd curl
require_cmd python3

tmpdir="$(mktemp -d)"
trap 'cleanup; rm -rf "$tmpdir"' EXIT

echo "Running WAN stale/recovery E2E against BASE=$BASE HIVE_ID=$HIVE_ID"

# 0) Health
health_body="$tmpdir/health.json"
status="$(http_call "GET" "$BASE/health" "$health_body")"
log_http_response "$status" "$health_body"
assert_eq "$status" "200" "GET /health/http"

# 1) Ensure target hive exists
hive_body="$tmpdir/hive.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$hive_body")"
log_http_response "$status" "$hive_body"
assert_eq "$status" "200" "GET /hives/{id}/http"
assert_eq "$(json_get "status" "$hive_body")" "ok" "GET /hives/{id}/status"

# 2) Ensure router alive before transition test
if ! poll_router_status "alive" 20 "ensure_alive"; then
  echo "Router not alive initially, trying run_router before stale test..."
  run_body="$tmpdir/run_router_before.json"
  status="$(http_call "POST" "$BASE/hives/$HIVE_ID/routers" "$run_body" "{\"service\":\"$ROUTER_SERVICE\"}")"
  log_http_response "$status" "$run_body"
  assert_eq "$status" "200" "POST /hives/{id}/routers before test/http"
  assert_eq "$(json_get "status" "$run_body")" "ok" "POST /hives/{id}/routers before test/status"
  poll_router_status "alive" "$RECOVERY_TIMEOUT_SECS" "ensure_alive_after_run"
fi

# 3) Kill router and wait stale
kill_body="$tmpdir/kill_router.json"
status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/routers/$ROUTER_SERVICE" "$kill_body")"
log_http_response "$status" "$kill_body"
assert_eq "$status" "200" "DELETE /hives/{id}/routers/{service}/http"
assert_eq "$(json_get "status" "$kill_body")" "ok" "DELETE /hives/{id}/routers/{service}/status"
ROUTER_KILLED="1"

poll_router_status "stale" "$STALE_TIMEOUT_SECS" "wait_stale"

# 4) Run router and wait alive again
run_body="$tmpdir/run_router.json"
status="$(http_call "POST" "$BASE/hives/$HIVE_ID/routers" "$run_body" "{\"service\":\"$ROUTER_SERVICE\"}")"
log_http_response "$status" "$run_body"
assert_eq "$status" "200" "POST /hives/{id}/routers/http"
assert_eq "$(json_get "status" "$run_body")" "ok" "POST /hives/{id}/routers/status"
ROUTER_KILLED="0"

poll_router_status "alive" "$RECOVERY_TIMEOUT_SECS" "wait_recovery"

final_body="$tmpdir/final_router.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID/routers" "$final_body")"
log_http_response "$status" "$final_body"
assert_eq "$status" "200" "final GET /hives/{id}/routers/http"
router_uuid="$(json_get "payload.routers.0.uuid" "$final_body")"
if [[ -z "$router_uuid" || "$router_uuid" == "00000000-0000-0000-0000-000000000000" ]]; then
  echo "FAIL [final_uuid]: expected non-nil remote router uuid, got '$router_uuid'" >&2
  exit 1
fi

echo "WAN stale/recovery E2E passed."
