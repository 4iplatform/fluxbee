#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
ROUTER_SERVICE="${ROUTER_SERVICE:-rt-gateway}"
STALE_TIMEOUT_SECS="${STALE_TIMEOUT_SECS:-120}"
RECOVERY_TIMEOUT_SECS="${RECOVERY_TIMEOUT_SECS:-90}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
AUTO_RECOVER_ON_EXIT="${AUTO_RECOVER_ON_EXIT:-1}"

tmpdir="$(mktemp -d)"
inventory_body="$tmpdir/inventory.json"
kill_body="$tmpdir/kill_router.json"
run_body="$tmpdir/run_router.json"
health_body="$tmpdir/health.json"
hive_body="$tmpdir/hive.json"

router_killed="0"

cleanup() {
  local _ec=$?
  if [[ "$AUTO_RECOVER_ON_EXIT" == "1" && "$router_killed" == "1" ]]; then
    http_call "POST" "$BASE/hives/$HIVE_ID/routers" "$run_body" "{\"service\":\"$ROUTER_SERVICE\"}" >/dev/null 2>&1 || true
  fi
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  }
}

http_call() {
  local method="$1"
  local url="$2"
  local out_file="$3"
  local payload="${4:-}"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$out_file" -w "%{http_code}" -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$out_file" -w "%{http_code}" -X "$method" "$url"
  fi
}

json_get_file() {
  local path="$1"
  local file="$2"
  python3 - "$path" "$file" <<'PY'
import json
import sys

path = sys.argv[1]
file_path = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
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

inventory_hive_status_file() {
  local file="$1"
  local hive_id="$2"
  python3 - "$file" "$hive_id" <<'PY'
import json
import sys

file_path = sys.argv[1]
hive_id = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

payload = doc.get("payload", {})
if isinstance(payload, dict):
    if isinstance(payload.get("hive_status"), str):
        print(payload["hive_status"])
        raise SystemExit(0)
    hives = payload.get("hives", [])
    if isinstance(hives, list):
        for hive in hives:
            if isinstance(hive, dict) and hive.get("hive_id") == hive_id:
                status = hive.get("status")
                print("" if status is None else str(status))
                raise SystemExit(0)

print("")
PY
}

poll_inventory_hive_status() {
  local expected="$1"
  local timeout_secs="$2"
  local label="$3"
  local started_at
  started_at="$(date +%s)"
  local attempts=0

  while true; do
    local now elapsed http hive_status
    now="$(date +%s)"
    elapsed=$((now - started_at))
    attempts=$((attempts + 1))

    if (( elapsed > timeout_secs )); then
      echo "FAIL: timeout waiting inventory hive_status='$expected' ($label)" >&2
      cat "$inventory_body" >&2 || true
      return 1
    fi

    http="$(http_call "GET" "$BASE/inventory/$HIVE_ID" "$inventory_body")"
    if [[ "$http" == "200" && "$(json_get_file "status" "$inventory_body")" == "ok" && "$(json_get_file "payload.status" "$inventory_body")" == "ok" ]]; then
      hive_status="$(inventory_hive_status_file "$inventory_body" "$HIVE_ID")"
      if [[ "$hive_status" == "$expected" ]]; then
        return 0
      fi
      if (( attempts == 1 || attempts % 5 == 0 )); then
        echo "WAIT[$label]: hive_status='${hive_status:-n/a}' expected='$expected' elapsed=${elapsed}s" >&2
      fi
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

run_router_expect_ok() {
  local http
  http="$(http_call "POST" "$BASE/hives/$HIVE_ID/routers" "$run_body" "{\"service\":\"$ROUTER_SERVICE\"}")"
  if [[ "$http" != "200" || "$(json_get_file "status" "$run_body")" != "ok" ]]; then
    echo "FAIL: run router failed http=$http" >&2
    cat "$run_body" >&2 || true
    exit 1
  fi
}

kill_router_expect_ok() {
  local http
  http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/routers/$ROUTER_SERVICE" "$kill_body")"
  if [[ "$http" != "200" || "$(json_get_file "status" "$kill_body")" != "ok" ]]; then
    echo "FAIL: kill router failed http=$http" >&2
    cat "$kill_body" >&2 || true
    exit 1
  fi
}

require_cmd curl
require_cmd python3

echo "INVENTORY D3 E2E: BASE=$BASE HIVE_ID=$HIVE_ID ROUTER_SERVICE=$ROUTER_SERVICE"

echo "Step 1/7: health checks"
health_http="$(http_call "GET" "$BASE/health" "$health_body")"
if [[ "$health_http" != "200" || "$(json_get_file "status" "$health_body")" != "ok" ]]; then
  echo "FAIL: /health not ready" >&2
  cat "$health_body" >&2 || true
  exit 1
fi
hive_http="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$hive_body")"
if [[ "$hive_http" != "200" || "$(json_get_file "status" "$hive_body")" != "ok" ]]; then
  echo "FAIL: hive '$HIVE_ID' not available via /hives/{id}" >&2
  cat "$hive_body" >&2 || true
  exit 1
fi

echo "Step 2/7: ensure inventory hive_status=alive baseline"
if ! poll_inventory_hive_status "alive" 20 "baseline_alive"; then
  run_router_expect_ok
  poll_inventory_hive_status "alive" 60 "baseline_alive_after_run"
fi

echo "Step 3/7: kill remote router"
kill_router_expect_ok
router_killed="1"

echo "Step 4/7: wait until inventory hive_status=stale"
poll_inventory_hive_status "stale" "$STALE_TIMEOUT_SECS" "wait_stale"

echo "Step 5/7: run remote router again"
run_router_expect_ok
router_killed="0"

echo "Step 6/7: wait until inventory hive_status=alive"
poll_inventory_hive_status "alive" "$RECOVERY_TIMEOUT_SECS" "wait_recovery"

echo "Step 7/7: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "router_service=$ROUTER_SERVICE"
echo "inventory stale/recovery E2E passed."
