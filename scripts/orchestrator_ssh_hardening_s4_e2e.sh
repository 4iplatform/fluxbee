#!/usr/bin/env bash
set -euo pipefail

# S4 E2E runner (SSH hardening pre-cut):
# - Optional add/remove hive exercise
# - Runtime versioning E2E (stale reject + rollout + drift)
# - Vendor E2E (drift + rollback)
# - Final versions/deployments snapshot
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   HIVE_ADDR="192.168.8.220" \
#   BUILD_BIN=0 \
#   REMOTE_SUDO_PASS="magicAI" \
#   bash scripts/orchestrator_ssh_hardening_s4_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-192.168.8.220}"
BUILD_BIN="${BUILD_BIN:-0}"
REMOTE_SUDO_PASS="${REMOTE_SUDO_PASS:-magicAI}"
EXERCISE_ADD_REMOVE="${EXERCISE_ADD_REMOVE:-1}"
SKIP_VENDOR="${SKIP_VENDOR:-0}"
REQUIRE_DRIFT_ALERT="${REQUIRE_DRIFT_ALERT:-0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local key="$1"
  local payload="$2"
  python3 - "$key" "$payload" <<'PY'
import json
import sys

key = sys.argv[1]
payload = sys.argv[2]
try:
    data = json.loads(payload)
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

run_step() {
  local name="$1"
  shift
  echo ""
  echo "===== ${name} ====="
  "$@"
}

require_cmd bash
require_cmd curl
require_cmd python3

echo "S4 runner config: BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR BUILD_BIN=$BUILD_BIN SKIP_VENDOR=$SKIP_VENDOR"

health="$(curl -sS "$BASE/health" || true)"
if [[ "$(json_get "status" "$health")" != "ok" ]]; then
  echo "FAIL: $BASE/health is not ok: $health" >&2
  exit 1
fi

if [[ "$EXERCISE_ADD_REMOVE" == "1" ]]; then
  run_step "precheck remove_hive" bash -lc "curl -sS -X DELETE '$BASE/hives/$HIVE_ID'; echo"
  run_step "precheck add_hive" bash -lc "curl -sS -X POST '$BASE/hives' -H 'Content-Type: application/json' -d '{\"hive_id\":\"$HIVE_ID\",\"address\":\"$HIVE_ADDR\"}'; echo"
  hive_body="$(curl -sS "$BASE/hives/$HIVE_ID" || true)"
  if [[ "$(json_get "status" "$hive_body")" != "ok" ]]; then
    echo "FAIL: hive '$HIVE_ID' is not ready after add_hive: $hive_body" >&2
    exit 1
  fi
fi

run_step "runtime stale reject" \
  bash -lc "cd '$ROOT_DIR' && BASE='$BASE' HIVE_ID='$HIVE_ID' BUILD_BIN='$BUILD_BIN' bash scripts/orchestrator_runtime_update_stale_e2e.sh"

run_step "runtime rollout canary/global/rollback" \
  bash -lc "cd '$ROOT_DIR' && BASE='$BASE' HIVE_ID='$HIVE_ID' BUILD_BIN='$BUILD_BIN' bash scripts/orchestrator_runtime_rollout_e2e.sh"

run_step "runtime drift reconcile" \
  bash -lc "cd '$ROOT_DIR' && BASE='$BASE' HIVE_ID='$HIVE_ID' HIVE_ADDR='$HIVE_ADDR' BUILD_BIN='$BUILD_BIN' REMOTE_SUDO_PASS='$REMOTE_SUDO_PASS' REQUIRE_DRIFT_ALERT='$REQUIRE_DRIFT_ALERT' bash scripts/orchestrator_drift_runtime_e2e.sh"

if [[ "$SKIP_VENDOR" != "1" ]]; then
  run_step "vendor drift reconcile" \
    bash -lc "cd '$ROOT_DIR' && BASE='$BASE' HIVE_ID='$HIVE_ID' HIVE_ADDR='$HIVE_ADDR' BUILD_BIN='$BUILD_BIN' REMOTE_SUDO_PASS='$REMOTE_SUDO_PASS' bash scripts/orchestrator_drift_vendor_e2e.sh"

  run_step "vendor rollback" \
    bash -lc "cd '$ROOT_DIR' && BASE='$BASE' HIVE_ID='$HIVE_ID' HIVE_ADDR='$HIVE_ADDR' BUILD_BIN='$BUILD_BIN' REMOTE_SUDO_PASS='$REMOTE_SUDO_PASS' bash scripts/orchestrator_vendor_rollback_e2e.sh"
else
  echo "Info: SKIP_VENDOR=1 -> vendor tests skipped"
fi

echo ""
echo "===== final snapshots ====="
curl -sS "$BASE/versions?hive=$HIVE_ID"; echo
curl -sS "$BASE/deployments?hive=$HIVE_ID&limit=20"; echo
curl -sS "$BASE/drift_alerts?hive=$HIVE_ID&limit=20"; echo

echo ""
echo "S4 E2E runner passed."
