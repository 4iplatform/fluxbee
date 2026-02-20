#!/usr/bin/env bash
set -euo pipefail

# NATS transport E2E for jsr_client + embedded broker path.
#
# Verifies SY.admin -> NATS -> SY.storage request/reply keeps working across:
# 1) rt-gateway restart (broker reconnect path)
# 2) rt-gateway stop/start (hard down/up path)
# 3) sy-storage restart (subscriber reconnect/resubscribe path)
#
# Usage:
#   BASE="http://127.0.0.1:8080" bash scripts/nats_client_transport_e2e.sh
#
# Optional:
#   ROUTER_SERVICE="rt-gateway"
#   STORAGE_SERVICE="sy-storage"
#   METRICS_TIMEOUT_SECS="120"
#   POLL_INTERVAL_SECS="2"
#   INITIAL_STABILIZE_SECS="2"
#   RUN_ROUTER_RESTART="1"
#   RUN_ROUTER_STOP_START="1"
#   RUN_STORAGE_RESTART="1"

BASE="${BASE:-http://127.0.0.1:8080}"
ROUTER_SERVICE="${ROUTER_SERVICE:-rt-gateway}"
STORAGE_SERVICE="${STORAGE_SERVICE:-sy-storage}"
METRICS_TIMEOUT_SECS="${METRICS_TIMEOUT_SECS:-120}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
INITIAL_STABILIZE_SECS="${INITIAL_STABILIZE_SECS:-2}"
RUN_ROUTER_RESTART="${RUN_ROUTER_RESTART:-1}"
RUN_ROUTER_STOP_START="${RUN_ROUTER_STOP_START:-1}"
RUN_STORAGE_RESTART="${RUN_STORAGE_RESTART:-1}"

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

log_http_request() {
  local method="$1"
  local url="$2"
  echo "[HTTP] -> ${method} ${url}" >&2
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
  log_http_request "$method" "$url"
  curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url"
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

service_active() {
  local svc="$1"
  ${SUDO} systemctl is-active --quiet "$svc"
}

wait_for_service_state() {
  local service="$1"
  local expected="$2" # active|inactive
  local label="$3"
  local start now elapsed
  start="$(date +%s)"
  while true; do
    now="$(date +%s)"
    elapsed=$((now - start))
    if [[ "$expected" == "active" ]] && service_active "$service"; then
      echo "OK: service '$service' is active ($label)"
      return 0
    fi
    if [[ "$expected" == "inactive" ]] && ! service_active "$service"; then
      echo "OK: service '$service' is inactive ($label)"
      return 0
    fi
    if (( elapsed >= METRICS_TIMEOUT_SECS )); then
      echo "FAIL [$label]: timeout waiting service '$service' state '$expected'" >&2
      ${SUDO} systemctl status "$service" --no-pager -l || true
      exit 1
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

wait_for_metrics_ok() {
  local label="$1"
  local body_file="$2"
  local start now elapsed status
  start="$(date +%s)"
  while true; do
    status="$(http_call "GET" "$BASE/config/storage/metrics" "$body_file")"
    log_http_response "$status" "$body_file"
    if [[ "$status" == "200" ]] && \
       [[ "$(json_get "status" "$body_file")" == "ok" ]] && \
       [[ "$(json_get "payload.status" "$body_file")" == "ok" ]]; then
      echo "OK: storage metrics reachable ($label)"
      return 0
    fi

    now="$(date +%s)"
    elapsed=$((now - start))
    if (( elapsed >= METRICS_TIMEOUT_SECS )); then
      echo "FAIL [$label]: timeout waiting /config/storage/metrics healthy" >&2
      cat "$body_file" >&2
      exit 1
    fi
    echo "[POLL][${label}] elapsed=${elapsed}s waiting /config/storage/metrics=200" >&2
    sleep "$POLL_INTERVAL_SECS"
  done
}

require_cmd curl
require_cmd python3
require_cmd systemctl

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "Running NATS transport E2E: BASE=$BASE ROUTER_SERVICE=$ROUTER_SERVICE STORAGE_SERVICE=$STORAGE_SERVICE"

wait_for_service_state "$ROUTER_SERVICE" "active" "initial"
wait_for_service_state "$STORAGE_SERVICE" "active" "initial"
if (( INITIAL_STABILIZE_SECS > 0 )); then
  sleep "$INITIAL_STABILIZE_SECS"
fi

# 0) Health
health_body="$tmpdir/health.json"
status="$(http_call "GET" "$BASE/health" "$health_body")"
log_http_response "$status" "$health_body"
assert_eq "$status" "200" "GET /health/http"

metrics_body="$tmpdir/storage_metrics.json"
wait_for_metrics_ok "initial" "$metrics_body"

# 1) Router restart -> metrics recovery
if [[ "$RUN_ROUTER_RESTART" == "1" ]]; then
  echo "Step: restart service '$ROUTER_SERVICE'..."
  ${SUDO} systemctl restart "$ROUTER_SERVICE"
  wait_for_service_state "$ROUTER_SERVICE" "active" "after_router_restart"
  wait_for_service_state "$STORAGE_SERVICE" "active" "after_router_restart"
  wait_for_metrics_ok "after_router_restart" "$metrics_body"
else
  echo "SKIP: router restart disabled (RUN_ROUTER_RESTART=0)"
fi

# 2) Router hard down/up -> metrics recovery
if [[ "$RUN_ROUTER_STOP_START" == "1" ]]; then
  echo "Step: stop/start service '$ROUTER_SERVICE'..."
  ${SUDO} systemctl stop "$ROUTER_SERVICE"
  wait_for_service_state "$ROUTER_SERVICE" "inactive" "after_router_stop"

  # Expected transient failure while router is down.
  status="$(http_call "GET" "$BASE/config/storage/metrics" "$metrics_body")"
  log_http_response "$status" "$metrics_body"
  if [[ "$status" == "200" ]]; then
    echo "WARN: /config/storage/metrics returned 200 while router is stopped (possible stale response path)" >&2
  else
    echo "OK: /config/storage/metrics not healthy while router is stopped (status=$status)"
  fi

  ${SUDO} systemctl start "$ROUTER_SERVICE"
  wait_for_service_state "$ROUTER_SERVICE" "active" "after_router_start"
  wait_for_service_state "$STORAGE_SERVICE" "active" "after_router_start"
  wait_for_metrics_ok "after_router_start" "$metrics_body"
else
  echo "SKIP: router stop/start disabled (RUN_ROUTER_STOP_START=0)"
fi

# 3) Storage restart -> subscriber reconnect/resubscribe + metrics recovery
if [[ "$RUN_STORAGE_RESTART" == "1" ]]; then
  echo "Step: restart service '$STORAGE_SERVICE'..."
  ${SUDO} systemctl restart "$STORAGE_SERVICE"
  wait_for_service_state "$STORAGE_SERVICE" "active" "after_storage_restart"
  wait_for_metrics_ok "after_storage_restart" "$metrics_body"
else
  echo "SKIP: storage restart disabled (RUN_STORAGE_RESTART=0)"
fi

echo "nats client transport E2E passed."
