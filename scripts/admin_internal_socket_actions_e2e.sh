#!/usr/bin/env bash
set -euo pipefail

# FR9-T9 E2E:
# validates SY.admin internal socket command flow for:
# - sync_hint
# - update
# - run_node
# - get_node_status
# - inventory
# - kill_node
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   ADMIN_TARGET="SY.admin@motherbee" \
#   HIVE_ID="worker-220" \
#   RUNTIME="wf.orch.diag" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/admin_internal_socket_actions_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
HIVE_ID="${HIVE_ID:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"
TEST_ID="${TEST_ID:-fr9t9-$(date +%s)-$RANDOM}"
NODE_NAME="${NODE_NAME:-WF.admin.socket.${TEST_ID}}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"

if [[ -z "$TENANT_ID" ]]; then
  echo "FAIL: TENANT_ID is required (format tnt:<uuid>)" >&2
  exit 1
fi

NODE_FQN="$NODE_NAME@$HIVE_ID"

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
    data = json.load(open(path, "r", encoding="utf-8"))
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
    print("" if value is None else value)
PY
}

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
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

runtime_exists() {
  local file="$1"
  local runtime="$2"
  python3 - "$file" "$runtime" <<'PY'
import json
import sys

path, runtime = sys.argv[1], sys.argv[2]
try:
    data = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("0")
    raise SystemExit(0)

payload = data.get("payload") if isinstance(data, dict) else None
hive = payload.get("hive") if isinstance(payload, dict) else None
runtimes_block = hive.get("runtimes") if isinstance(hive, dict) else None
runtimes_map = runtimes_block.get("runtimes") if isinstance(runtimes_block, dict) else None
if isinstance(runtimes_map, dict) and runtime in runtimes_map:
    print("1")
else:
    print("0")
PY
}

run_admin() {
  local case_id="$1"
  local action="$2"
  local payload_target="$3"
  local params_json="$4"
  local out_file="$tmpdir/${case_id}.json"

  if [[ -n "$payload_target" ]]; then
    ADMIN_ACTION="$action" \
    ADMIN_TARGET="$ADMIN_TARGET" \
    ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
    ADMIN_PAYLOAD_TARGET="$payload_target" \
    ADMIN_PARAMS_JSON="$params_json" \
    "$DIAG_BIN" >"$out_file"
  else
    ADMIN_ACTION="$action" \
    ADMIN_TARGET="$ADMIN_TARGET" \
    ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
    ADMIN_PARAMS_JSON="$params_json" \
    "$DIAG_BIN" >"$out_file"
  fi

  LAST_OUT="$out_file"
}

require_cmd curl
require_cmd python3
if [[ ! -x "$DIAG_BIN" ]]; then
  require_cmd cargo
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "ADMIN internal socket actions E2E: ADMIN_TARGET=$ADMIN_TARGET HIVE_ID=$HIVE_ID RUNTIME=$RUNTIME NODE_NAME=$NODE_NAME"

echo "Step 1/10: build admin_internal_command_diag once"
if [[ ! -x "$DIAG_BIN" ]]; then
  (cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: admin_internal_command_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/10: validate runtime exists and read manifest metadata via HTTP"
versions_body="$tmpdir/versions.json"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" || "$(json_get "status" "$versions_body")" != "ok" ]]; then
  echo "FAIL: GET /hives/$HIVE_ID/versions failed http=$versions_http" >&2
  cat "$versions_body" >&2
  exit 1
fi
if [[ "$(runtime_exists "$versions_body" "$RUNTIME")" != "1" ]]; then
  echo "FAIL: runtime '$RUNTIME' not present on hive '$HIVE_ID'" >&2
  cat "$versions_body" >&2
  exit 1
fi
manifest_version="$(json_get "payload.hive.runtimes.manifest_version" "$versions_body")"
manifest_hash="$(json_get "payload.hive.runtimes.manifest_hash" "$versions_body")"
if [[ -z "$manifest_version" || -z "$manifest_hash" ]]; then
  echo "FAIL: missing manifest metadata in /versions" >&2
  cat "$versions_body" >&2
  exit 1
fi

echo "Step 3/10: cleanup baseline node (ignore errors)"
run_admin "cleanup_pre" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_NAME\"}"

echo "Step 4/10: socket action sync_hint"
run_admin "sync_hint" "sync_hint" "$HIVE_ID" '{"channel":"dist","folder_id":"fluxbee-dist","wait_for_idle":true,"timeout_ms":30000}'
sync_status="$(json_get "status" "$LAST_OUT")"
sync_action="$(json_get "action" "$LAST_OUT")"
if [[ "$sync_action" != "sync_hint" ]]; then
  echo "FAIL[sync_hint]: expected action='sync_hint', got '$sync_action'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi
if [[ "$sync_status" != "ok" && "$sync_status" != "sync_pending" ]]; then
  echo "FAIL[sync_hint]: expected status in {ok,sync_pending}, got '$sync_status'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 5/10: socket action update(runtime)"
update_params="$(cat <<JSON
{"category":"runtime","manifest_version":$manifest_version,"manifest_hash":"$manifest_hash"}
JSON
)"
run_admin "update" "update" "$HIVE_ID" "$update_params"
update_status="$(json_get "status" "$LAST_OUT")"
update_code="$(json_get "error_code" "$LAST_OUT")"
update_action="$(json_get "action" "$LAST_OUT")"
if [[ "$update_action" != "update" ]]; then
  echo "FAIL[update]: expected action='update', got '$update_action'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi
if [[ "$update_status" != "ok" && "$update_status" != "sync_pending" ]]; then
  if [[ "$update_status" == "error" && "$update_code" == "VERSION_MISMATCH" ]]; then
    :
  else
    echo "FAIL[update]: expected status in {ok,sync_pending} or VERSION_MISMATCH, got status='$update_status' code='$update_code'" >&2
    cat "$LAST_OUT" >&2
    exit 1
  fi
fi

echo "Step 6/10: socket action run_node"
run_params="$(cat <<JSON
{"node_name":"$NODE_NAME","runtime":"$RUNTIME","runtime_version":"$RUNTIME_VERSION","tenant_id":"$TENANT_ID"}
JSON
)"
run_admin "run_node" "run_node" "$HIVE_ID" "$run_params"
if [[ "$(json_get "status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[run_node]: expected status='ok'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 7/10: socket action get_node_status (wait STARTING/RUNNING)"
status_ok="0"
observed_lifecycle=""
observed_pid=""
for _ in $(seq 1 20); do
  run_admin "get_node_status" "get_node_status" "$HIVE_ID" "{\"node_name\":\"$NODE_FQN\"}"
  top_status="$(json_get "status" "$LAST_OUT")"
  lifecycle="$(json_get "payload.node_status.lifecycle_state" "$LAST_OUT")"
  pid="$(json_get "payload.node_status.process.pid" "$LAST_OUT")"
  observed_lifecycle="$lifecycle"
  observed_pid="$pid"
  if [[ "$top_status" == "ok" && ( "$lifecycle" == "RUNNING" || "$lifecycle" == "STARTING" ) ]]; then
    status_ok="1"
    break
  fi
  sleep 1
done
if [[ "$status_ok" != "1" ]]; then
  echo "FAIL[get_node_status]: did not reach lifecycle STARTING/RUNNING" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 8/10: socket action inventory (hive scope)"
run_admin "inventory" "inventory" "" "{\"scope\":\"hive\",\"filter_hive\":\"$HIVE_ID\"}"
if [[ "$(json_get "status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[inventory]: expected top status='ok'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi
if [[ "$(json_get "payload.status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[inventory]: expected payload.status='ok'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 9/10: socket action kill_node"
run_admin "kill_node" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_FQN\"}"
if [[ "$(json_get "status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[kill_node]: expected status='ok'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 10/10: summary"
echo "status=ok"
echo "admin_target=$ADMIN_TARGET"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "node_name=$NODE_FQN"
echo "sync_hint_status=$sync_status"
echo "update_status=$update_status"
echo "update_error_code=${update_code:-}"
echo "node_status_lifecycle=$observed_lifecycle"
echo "node_status_pid=$observed_pid"
echo "admin internal socket actions FR9-T9 E2E passed."
