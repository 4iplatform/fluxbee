#!/usr/bin/env bash
set -euo pipefail

# Orchestrator partial-failure recovery E2E:
# 1) Baseline runtime_update + spawn/kill cycle.
# 2) Inject partial worker failure (stop rt-gateway + sy-config-routes).
# 3) Trigger runtime_update cycle again.
# 4) Validate auto-recovery:
#    - remote services back to active
#    - core deployment evidence exists for this runtime_update window
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   HIVE_ADDR="192.168.8.220" \
#   BUILD_BIN=0 \
#   REMOTE_SUDO_PASS="magicAI" \
#   bash scripts/orchestrator_partial_failure_recovery_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
ORCH_VERSION="${ORCH_VERSION:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
WAIT_TIMEOUT_SECS="${WAIT_TIMEOUT_SECS:-90}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
BUILD_BIN="${BUILD_BIN:-0}"
REMOTE_SUDO_PASS="${REMOTE_SUDO_PASS:-magicAI}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INFO_FILE="/var/lib/fluxbee/hives/${HIVE_ID}/info.yaml"
LEGACY_KEY_PATH="/var/lib/fluxbee/hives/${HIVE_ID}/ssh.key"
MOTHERBEE_KEY_PATH="/var/lib/fluxbee/ssh/motherbee.key"
KEY_PATH=""

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

epoch_ms() {
  python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
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
elif isinstance(value, bool):
    print("true" if value else "false")
else:
    print(value if value is not None else "")
PY
}

extract_hive_addr() {
  if [[ -n "$HIVE_ADDR" ]]; then
    echo "$HIVE_ADDR"
    return
  fi
  if [[ ! -f "$INFO_FILE" ]]; then
    echo ""
    return
  fi
  awk -F': ' '/^address:/ {gsub(/"/, "", $2); print $2; exit}' "$INFO_FILE"
}

remote_ssh() {
  local remote_cmd="$1"
  local -a ssh_cmd=()
  if [[ "${USE_SUDO_SSH:-0}" == "1" ]]; then
    ssh_cmd+=(sudo)
  fi
  ssh_cmd+=(ssh -i "$KEY_PATH" \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o LogLevel=ERROR \
    -o BatchMode=yes \
    -o ConnectTimeout=10 \
    "administrator@${HIVE_ADDR}" \
    "$remote_cmd")
  "${ssh_cmd[@]}"
}

remote_root() {
  local root_cmd="$1"
  local escaped
  escaped="$(printf "%s" "$root_cmd" | sed "s/'/'\"'\"'/g")"
  if remote_ssh "sudo -n true" >/dev/null 2>&1; then
    remote_ssh "sudo bash -lc '$escaped'"
    return
  fi
  if [[ -z "$REMOTE_SUDO_PASS" ]]; then
    echo "FAIL: remote sudo requires password; set REMOTE_SUDO_PASS" >&2
    return 1
  fi
  remote_ssh "printf '%s\n' '$REMOTE_SUDO_PASS' | sudo -S -p '' bash -lc '$escaped'"
}

service_is_active() {
  local svc="$1"
  remote_root "systemctl is-active --quiet '$svc'"
}

trigger_runtime_update_cycle() {
  (
    cd "$ROOT_DIR"
    TARGET_HIVE="$HIVE_ID" \
    ORCH_RUNTIME="$ORCH_RUNTIME" \
    ORCH_VERSION="$ORCH_VERSION" \
    ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
    ORCH_SEND_RUNTIME_UPDATE=1 \
    ORCH_SEND_KILL=1 \
    BUILD_BIN="$BUILD_BIN" \
    bash scripts/orchestrator_runtime_update_spawn_e2e.sh
  )
}

has_recent_core_deployment_ok() {
  local file="$1"
  local since_ms="$2"
  python3 - "$file" "$since_ms" "$HIVE_ID" <<'PY'
import json
import sys

path = sys.argv[1]
since_ms = int(sys.argv[2])
hive_id = sys.argv[3]

try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("0")
    raise SystemExit(0)

entries = (((data or {}).get("payload") or {}).get("entries")) or []
for item in entries:
    if item.get("category") != "core":
        continue
    if int(item.get("started_at") or 0) < since_ms:
        continue
    workers = item.get("workers") or []
    for worker in workers:
        if worker.get("hive_id") == hive_id and worker.get("status") == "ok":
            print("1")
            raise SystemExit(0)
print("0")
PY
}

require_cmd bash
require_cmd curl
require_cmd python3
require_cmd ssh

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

HIVE_ADDR="$(extract_hive_addr)"
if [[ -z "$HIVE_ADDR" ]]; then
  echo "FAIL: cannot resolve HIVE_ADDR (set HIVE_ADDR or ensure $INFO_FILE exists)" >&2
  exit 1
fi
if [[ -f "$LEGACY_KEY_PATH" ]]; then
  KEY_PATH="$LEGACY_KEY_PATH"
elif [[ -f "$MOTHERBEE_KEY_PATH" ]]; then
  KEY_PATH="$MOTHERBEE_KEY_PATH"
else
  echo "FAIL: missing ssh key file (checked $LEGACY_KEY_PATH and $MOTHERBEE_KEY_PATH)" >&2
  exit 1
fi

USE_SUDO_SSH=0
if [[ ! -r "$KEY_PATH" ]]; then
  if sudo -n true >/dev/null 2>&1; then
    USE_SUDO_SSH=1
    echo "Info: using sudo for local ssh (key not readable by current user): $KEY_PATH" >&2
  else
    echo "FAIL: key file is not readable and sudo -n is unavailable: $KEY_PATH" >&2
    exit 1
  fi
fi

echo "Running partial-failure recovery E2E: BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR"

health_body="$tmpdir/health.json"
curl -sS -o "$health_body" "$BASE/health"
if [[ "$(json_get "status" "$health_body")" != "ok" ]]; then
  echo "FAIL: /health not ok" >&2
  cat "$health_body" >&2
  exit 1
fi

hive_body="$tmpdir/hive.json"
curl -sS -o "$hive_body" "$BASE/hives/$HIVE_ID"
if [[ "$(json_get "status" "$hive_body")" != "ok" ]]; then
  echo "FAIL: hive '$HIVE_ID' not ready in admin API" >&2
  cat "$hive_body" >&2
  exit 1
fi

echo "Step 1/4: baseline runtime update cycle"
trigger_runtime_update_cycle

echo "Step 2/4: inject partial worker failure (stop rt-gateway + sy-config-routes)"
remote_root "systemctl stop rt-gateway || true; systemctl stop sy-config-routes || true"

if service_is_active "rt-gateway"; then
  echo "FAIL: rt-gateway still active after injected failure" >&2
  exit 1
fi
if service_is_active "sy-config-routes"; then
  echo "FAIL: sy-config-routes still active after injected failure" >&2
  exit 1
fi

echo "Step 3/4: trigger runtime_update again to force orchestrator reconciliation"
recovery_started_ms="$(epoch_ms)"
trigger_runtime_update_cycle

echo "Step 4/4: validate services recovered + core deployment evidence"
start_secs="$(date +%s)"
services_ok=0
deploy_ok=0
while true; do
  now_secs="$(date +%s)"
  elapsed=$((now_secs - start_secs))
  if (( elapsed > WAIT_TIMEOUT_SECS )); then
    break
  fi

  if service_is_active "rt-gateway" && service_is_active "sy-config-routes"; then
    services_ok=1
  fi

  deployments_body="$tmpdir/deployments.json"
  curl -sS -o "$deployments_body" "$BASE/deployments?hive=$HIVE_ID&category=core&limit=50"
  if [[ "$(has_recent_core_deployment_ok "$deployments_body" "$recovery_started_ms")" == "1" ]]; then
    deploy_ok=1
  fi

  if [[ "$services_ok" == "1" && "$deploy_ok" == "1" ]]; then
    break
  fi
  echo "[WAIT] elapsed=${elapsed}s services_ok=${services_ok} deploy_ok=${deploy_ok}" >&2
  sleep "$POLL_INTERVAL_SECS"
done

if [[ "$services_ok" != "1" ]]; then
  echo "FAIL: worker services did not recover automatically" >&2
  exit 1
fi
if [[ "$deploy_ok" != "1" ]]; then
  echo "FAIL: no core deployment evidence after recovery trigger" >&2
  cat "$tmpdir/deployments.json" >&2 || true
  exit 1
fi

echo "partial-failure recovery E2E passed."
