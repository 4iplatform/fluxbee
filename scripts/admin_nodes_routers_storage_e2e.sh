#!/usr/bin/env bash
set -euo pipefail

# SY.admin E2E smoke:
# - /hives/{id}/nodes (list + run/kill)
# - /hives/{id}/routers (list + run/kill)
# - /config/storage (write/read/restore)
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#     bash scripts/admin_nodes_routers_storage_e2e.sh
#
# Optional:
#   ROUTER_SERVICE="rt-gateway"
#   RUN_ROUTER_CYCLE="1"
#   RUN_NODE_CYCLE="1"
#   RUN_STORAGE_CONFIG_CYCLE="1"
#   RUN_STORAGE_METRICS_CHECK="1"
#   STORAGE_METRICS_TIMEOUT_SECS="30"
#   STORAGE_METRICS_POLL_SECS="2"
#   NODE_RUNTIME="wf.echo"
#   NODE_VERSION="current"
#   STORAGE_TEST_PATH="/var/lib/fluxbee/storage-e2e"
#   RESTORE_STORAGE="1"
#   SKIP_NODE_IF_RUNTIME_MISSING="1"

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
ROUTER_SERVICE="${ROUTER_SERVICE:-rt-gateway}"
RUN_ROUTER_CYCLE="${RUN_ROUTER_CYCLE:-1}"
RUN_NODE_CYCLE="${RUN_NODE_CYCLE:-1}"
RUN_STORAGE_CONFIG_CYCLE="${RUN_STORAGE_CONFIG_CYCLE:-1}"
RUN_STORAGE_METRICS_CHECK="${RUN_STORAGE_METRICS_CHECK:-1}"
STORAGE_METRICS_TIMEOUT_SECS="${STORAGE_METRICS_TIMEOUT_SECS:-30}"
STORAGE_METRICS_POLL_SECS="${STORAGE_METRICS_POLL_SECS:-2}"
NODE_RUNTIME="${NODE_RUNTIME:-wf.echo}"
NODE_VERSION="${NODE_VERSION:-current}"
RESTORE_STORAGE="${RESTORE_STORAGE:-1}"
SKIP_NODE_IF_RUNTIME_MISSING="${SKIP_NODE_IF_RUNTIME_MISSING:-1}"

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

assert_eq() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL [$label]: expected '$expected', got '$actual'" >&2
    exit 1
  fi
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

assert_names_suffix() {
  local file="$1"
  local key="$2"
  local suffix="$3"
  python3 - "$file" "$key" "$suffix" <<'PY'
import json
import sys

path, key, suffix = sys.argv[1], sys.argv[2], sys.argv[3]
data = json.load(open(path, "r", encoding="utf-8"))
value = data
for part in key.split("."):
    value = value.get(part, {}) if isinstance(value, dict) else {}
items = value if isinstance(value, list) else []
for item in items:
    name = item.get("name", "")
    if name and not name.endswith("@" + suffix):
        print(name)
        raise SystemExit(1)
print("ok")
PY
}

require_cmd curl
require_cmd python3

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "Running admin E2E against BASE=$BASE HIVE_ID=$HIVE_ID"

# 0) Health
health_body="$tmpdir/health.json"
status="$(http_call "GET" "$BASE/health" "$health_body")"
log_http_response "$status" "$health_body"
assert_eq "$status" "200" "GET /health/http"

# 1) Hive exists
hive_body="$tmpdir/hive.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$hive_body")"
log_http_response "$status" "$hive_body"
assert_eq "$status" "200" "GET /hives/{id}/http"
echo "OK: hive '$HIVE_ID' is available"

# 2) List nodes/routers on target hive
nodes_body="$tmpdir/nodes.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes" "$nodes_body")"
log_http_response "$status" "$nodes_body"
assert_eq "$status" "200" "GET /hives/{id}/nodes/http"
assert_eq "$(json_get "status" "$nodes_body")" "ok" "GET /hives/{id}/nodes/status"
assert_names_suffix "$nodes_body" "payload.nodes" "$HIVE_ID" >/dev/null
echo "OK: /hives/$HIVE_ID/nodes returns target-hive names"

routers_body="$tmpdir/routers.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID/routers" "$routers_body")"
log_http_response "$status" "$routers_body"
assert_eq "$status" "200" "GET /hives/{id}/routers/http"
assert_eq "$(json_get "status" "$routers_body")" "ok" "GET /hives/{id}/routers/status"
assert_names_suffix "$routers_body" "payload.routers" "$HIVE_ID" >/dev/null
echo "OK: /hives/$HIVE_ID/routers returns target-hive names"

# 3) Router run/kill cycle
if [[ "$RUN_ROUTER_CYCLE" == "1" ]]; then
  kill_router_body="$tmpdir/kill_router.json"
  status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/routers/$ROUTER_SERVICE" "$kill_router_body")"
  log_http_response "$status" "$kill_router_body"
  assert_eq "$status" "200" "DELETE /hives/{id}/routers/{service}/http"
  assert_eq "$(json_get "status" "$kill_router_body")" "ok" "DELETE /hives/{id}/routers/{service}/status"

  sleep 1

  run_router_body="$tmpdir/run_router.json"
  status="$(http_call "POST" "$BASE/hives/$HIVE_ID/routers" "$run_router_body" "{\"service\":\"$ROUTER_SERVICE\"}")"
  log_http_response "$status" "$run_router_body"
  assert_eq "$status" "200" "POST /hives/{id}/routers/http"
  assert_eq "$(json_get "status" "$run_router_body")" "ok" "POST /hives/{id}/routers/status"
  echo "OK: router cycle run/kill passed for service '$ROUTER_SERVICE'"
else
  echo "SKIP: router run/kill cycle disabled (RUN_ROUTER_CYCLE=0)"
fi

# 4) Node run/kill cycle
if [[ "$RUN_NODE_CYCLE" == "1" ]]; then
  node_name="e2e-$(date +%s)-$RANDOM"
  node_unit="fluxbee-node-$node_name"
  run_node_body="$tmpdir/run_node.json"
  run_payload="{\"runtime\":\"$NODE_RUNTIME\",\"version\":\"$NODE_VERSION\",\"unit\":\"$node_unit\"}"
  status="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$run_node_body" "$run_payload")"
  log_http_response "$status" "$run_node_body"

  if [[ "$status" != "200" || "$(json_get "status" "$run_node_body")" != "ok" ]]; then
    error_code="$(json_get "error_code" "$run_node_body")"
    if [[ "$SKIP_NODE_IF_RUNTIME_MISSING" == "1" && ( "$error_code" == "RUNTIME_NOT_AVAILABLE" || "$error_code" == "RUNTIME_MANIFEST_MISSING" || "$error_code" == "RUNTIME_NOT_PRESENT" ) ]]; then
      echo "SKIP: node run/kill cycle ($error_code). Provide runtime manifest/assets to enforce this check."
    else
      echo "FAIL: node run/kill cycle failed" >&2
      cat "$run_node_body" >&2
      exit 1
    fi
  else
    kill_node_body="$tmpdir/kill_node.json"
    status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$node_name" "$kill_node_body")"
    log_http_response "$status" "$kill_node_body"
    assert_eq "$status" "200" "DELETE /hives/{id}/nodes/{name}/http"
    assert_eq "$(json_get "status" "$kill_node_body")" "ok" "DELETE /hives/{id}/nodes/{name}/status"
    echo "OK: node cycle run/kill passed (runtime='$NODE_RUNTIME')"
  fi
else
  echo "SKIP: node run/kill cycle disabled (RUN_NODE_CYCLE=0)"
fi

# 5) /config/storage write/read/restore
if [[ "$RUN_STORAGE_CONFIG_CYCLE" == "1" ]]; then
  get_storage_before="$tmpdir/storage_before.json"
  status="$(http_call "GET" "$BASE/config/storage" "$get_storage_before")"
  log_http_response "$status" "$get_storage_before"
  assert_eq "$status" "200" "GET /config/storage/http"
  before_path="$(json_get "payload.path" "$get_storage_before")"
  if [[ -z "$before_path" ]]; then
    before_path="$(json_get "path" "$get_storage_before")"
  fi

  test_path="${STORAGE_TEST_PATH:-/var/lib/fluxbee/storage-e2e-$(date +%s)-$RANDOM}"
  put_storage_body="$tmpdir/storage_put.json"
  status="$(http_call "PUT" "$BASE/config/storage" "$put_storage_body" "{\"path\":\"$test_path\"}")"
  log_http_response "$status" "$put_storage_body"
  assert_eq "$status" "200" "PUT /config/storage/http"
  assert_eq "$(json_get "status" "$put_storage_body")" "ok" "PUT /config/storage/status"

  get_storage_after="$tmpdir/storage_after.json"
  status="$(http_call "GET" "$BASE/config/storage" "$get_storage_after")"
  log_http_response "$status" "$get_storage_after"
  assert_eq "$status" "200" "GET /config/storage after PUT/http"
  after_path="$(json_get "payload.path" "$get_storage_after")"
  if [[ -z "$after_path" ]]; then
    after_path="$(json_get "path" "$get_storage_after")"
  fi
  assert_eq "$after_path" "$test_path" "GET /config/storage after PUT/path"

  if [[ "$RESTORE_STORAGE" == "1" && -n "$before_path" ]]; then
    restore_storage_body="$tmpdir/storage_restore.json"
    status="$(http_call "PUT" "$BASE/config/storage" "$restore_storage_body" "{\"path\":\"$before_path\"}")"
    log_http_response "$status" "$restore_storage_body"
    assert_eq "$status" "200" "PUT /config/storage restore/http"
    assert_eq "$(json_get "status" "$restore_storage_body")" "ok" "PUT /config/storage restore/status"
    echo "OK: /config/storage restored to previous path"
  else
    echo "SKIP: storage restore disabled or previous path empty"
  fi
else
  echo "SKIP: /config/storage write/read/restore disabled (RUN_STORAGE_CONFIG_CYCLE=0)"
fi

# 6) /config/storage/metrics via SY.admin -> NATS -> SY.storage
if [[ "$RUN_STORAGE_METRICS_CHECK" == "1" ]]; then
  storage_metrics_body="$tmpdir/storage_metrics.json"
  metrics_ok="0"
  elapsed="0"
  while (( elapsed <= STORAGE_METRICS_TIMEOUT_SECS )); do
    status="$(http_call "GET" "$BASE/config/storage/metrics" "$storage_metrics_body")"
    log_http_response "$status" "$storage_metrics_body"
    if [[ "$status" == "200" ]] && \
       [[ "$(json_get "status" "$storage_metrics_body")" == "ok" ]] && \
       [[ "$(json_get "payload.status" "$storage_metrics_body")" == "ok" ]]; then
      metrics_ok="1"
      break
    fi
    if (( elapsed == STORAGE_METRICS_TIMEOUT_SECS )); then
      break
    fi
    echo "[POLL][storage_metrics] elapsed=${elapsed}s waiting for /config/storage/metrics=200" >&2
    sleep "$STORAGE_METRICS_POLL_SECS"
    elapsed=$((elapsed + STORAGE_METRICS_POLL_SECS))
  done
  assert_eq "$metrics_ok" "1" "GET /config/storage/metrics/http+status"

  for metric_key in pending pending_with_error oldest_pending_age_s processed_total max_attempts; do
    metric_val="$(json_get "payload.metrics.${metric_key}" "$storage_metrics_body")"
    if [[ -z "$metric_val" ]]; then
      echo "FAIL [GET /config/storage/metrics/${metric_key}]: missing metric value" >&2
      cat "$storage_metrics_body" >&2
      exit 1
    fi
  done
  echo "OK: /config/storage/metrics returns metrics snapshot"
else
  echo "SKIP: storage metrics check disabled (RUN_STORAGE_METRICS_CHECK=0)"
fi

echo "admin nodes/routers/storage E2E passed."
