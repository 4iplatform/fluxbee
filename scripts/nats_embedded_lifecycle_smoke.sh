#!/usr/bin/env bash
set -euo pipefail

# Smoke test for embedded NATS lifecycle tied to rt-gateway.
#
# Validates:
# 1) NATS endpoint is reachable while router service is active.
# 2) After router restart, NATS endpoint becomes reachable again.
# 3) (Optional) After router stop, NATS endpoint goes down; after start, returns.
#
# Usage:
#   bash scripts/nats_embedded_lifecycle_smoke.sh
#
# Optional:
#   SERVICE=rt-gateway
#   NATS_URL=nats://127.0.0.1:4222
#   TIMEOUT_SECS=30
#   POLL_INTERVAL_SECS=1
#   CHECK_STOP_START=1

SERVICE="${SERVICE:-rt-gateway}"
NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
TIMEOUT_SECS="${TIMEOUT_SECS:-30}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-1}"
CHECK_STOP_START="${CHECK_STOP_START:-1}"

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

service_active() {
  ${SUDO} systemctl is-active --quiet "$SERVICE"
}

wait_for_service_state() {
  local expected="$1" # active|inactive
  local label="$2"
  local start now elapsed
  start="$(date +%s)"
  while true; do
    now="$(date +%s)"
    elapsed=$((now - start))
    if [[ "$expected" == "active" ]] && service_active; then
      echo "OK: service '$SERVICE' is active ($label)"
      return 0
    fi
    if [[ "$expected" == "inactive" ]] && ! service_active; then
      echo "OK: service '$SERVICE' is inactive ($label)"
      return 0
    fi
    if (( elapsed >= TIMEOUT_SECS )); then
      echo "FAIL [$label]: timeout waiting service '$SERVICE' state '$expected'" >&2
      ${SUDO} systemctl status "$SERVICE" --no-pager -l || true
      exit 1
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

nats_probe() {
  local endpoint="$1"
  python3 - "$endpoint" <<'PY'
import socket
import sys
from urllib.parse import urlparse

endpoint = sys.argv[1].strip()
if endpoint.startswith("nats://"):
    parsed = urlparse(endpoint)
    host = parsed.hostname or "127.0.0.1"
    port = parsed.port or 4222
else:
    if ":" not in endpoint:
        print("invalid endpoint", file=sys.stderr)
        raise SystemExit(2)
    host, raw_port = endpoint.rsplit(":", 1)
    port = int(raw_port)

sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.settimeout(2.0)
try:
    sock.connect((host, port))
    sock.sendall(b'CONNECT {"lang":"smoke","version":"0.1","verbose":false,"pedantic":false,"tls_required":false}\r\n')
    sock.sendall(b"PING\r\n")
    data = b""
    for _ in range(4):
        chunk = sock.recv(4096)
        if not chunk:
            break
        data += chunk
        if b"\n" in data:
            break
    line = data.splitlines()[0].decode("utf-8", errors="replace") if data else ""
    if line.startswith("INFO "):
        raise SystemExit(0)
    if line.startswith("PONG"):
        # Some endpoints can answer quickly with PONG depending on server behavior.
        raise SystemExit(0)
    raise SystemExit(1)
except Exception:
    raise SystemExit(1)
finally:
    try:
        sock.close()
    except Exception:
        pass
PY
}

wait_for_nats_state() {
  local expected="$1" # up|down
  local label="$2"
  local start now elapsed
  start="$(date +%s)"
  while true; do
    now="$(date +%s)"
    elapsed=$((now - start))
    if nats_probe "$NATS_URL"; then
      if [[ "$expected" == "up" ]]; then
        echo "OK: NATS endpoint reachable ($label): $NATS_URL"
        return 0
      fi
    else
      if [[ "$expected" == "down" ]]; then
        echo "OK: NATS endpoint down ($label): $NATS_URL"
        return 0
      fi
    fi
    if (( elapsed >= TIMEOUT_SECS )); then
      echo "FAIL [$label]: timeout waiting NATS state '$expected' ($NATS_URL)" >&2
      ${SUDO} systemctl status "$SERVICE" --no-pager -l || true
      exit 1
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

require_cmd python3
require_cmd systemctl

echo "Running embedded NATS lifecycle smoke: SERVICE=$SERVICE NATS_URL=$NATS_URL"

if ! service_active; then
  echo "INFO: service '$SERVICE' is inactive, starting it first..."
  ${SUDO} systemctl start "$SERVICE"
fi
wait_for_service_state "active" "initial"
wait_for_nats_state "up" "initial"

echo "Step: restart service '$SERVICE'..."
${SUDO} systemctl restart "$SERVICE"
wait_for_service_state "active" "after_restart"
wait_for_nats_state "up" "after_restart"

if [[ "$CHECK_STOP_START" == "1" ]]; then
  echo "Step: stop/start service '$SERVICE'..."
  ${SUDO} systemctl stop "$SERVICE"
  wait_for_service_state "inactive" "after_stop"
  wait_for_nats_state "down" "after_stop"

  ${SUDO} systemctl start "$SERVICE"
  wait_for_service_state "active" "after_start"
  wait_for_nats_state "up" "after_start"
else
  echo "SKIP: stop/start check disabled (CHECK_STOP_START=0)"
fi

echo "embedded NATS lifecycle smoke passed."
