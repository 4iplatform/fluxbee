#!/usr/bin/env bash
set -euo pipefail

# FR9-T10 E2E:
# parity check for critical subset between HTTP and internal socket admin paths.
# Covered actions:
# - inventory
# - sync_hint
# - update
# - run_node
# - get_node_status
# - kill_node
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   ADMIN_TARGET="SY.admin@motherbee" \
#   HIVE_ID="worker-220" \
#   RUNTIME="wf.orch.diag" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/admin_http_socket_parity_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
HIVE_ID="${HIVE_ID:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"
TEST_ID="${TEST_ID:-fr9t10-$(date +%s)-$RANDOM}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"

NODE_HTTP="${NODE_HTTP:-WF.admin.parity.http.${TEST_ID}}"
NODE_SOCKET="${NODE_SOCKET:-WF.admin.parity.sock.${TEST_ID}}"
NODE_HTTP_FQN="$NODE_HTTP@$HIVE_ID"
NODE_SOCKET_FQN="$NODE_SOCKET@$HIVE_ID"

if [[ -z "$TENANT_ID" ]]; then
  echo "FAIL: TENANT_ID is required (format tnt:<uuid>)" >&2
  exit 1
fi

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

run_admin_socket() {
  local case_id="$1"
  local action="$2"
  local payload_target="$3"
  local params_json="$4"
  local out_file="$tmpdir/socket_${case_id}.json"

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
  LAST_SOCKET_OUT="$out_file"
}

is_ok_like() {
  local status="$1"
  [[ "$status" == "ok" || "$status" == "sync_pending" ]]
}

is_update_valid() {
  local status="$1"
  local code="$2"
  if [[ "$status" == "ok" || "$status" == "sync_pending" ]]; then
    return 0
  fi
  [[ "$status" == "error" && "$code" == "VERSION_MISMATCH" ]]
}

wait_http_node_started() {
  local node_name="$1"
  local out_file="$2"
  local lifecycle=""
  for _ in $(seq 1 20); do
    local http
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$node_name/status" "$out_file")"
    if [[ "$http" == "200" && "$(json_get "status" "$out_file")" == "ok" ]]; then
      lifecycle="$(json_get "payload.node_status.lifecycle_state" "$out_file")"
      if [[ "$lifecycle" == "STARTING" || "$lifecycle" == "RUNNING" ]]; then
        echo "$lifecycle"
        return 0
      fi
    fi
    sleep 1
  done
  return 1
}

wait_socket_node_started() {
  local node_fqn="$1"
  local case_prefix="$2"
  local lifecycle=""
  for i in $(seq 1 20); do
    run_admin_socket "${case_prefix}_${i}" "get_node_status" "$HIVE_ID" "{\"node_name\":\"$node_fqn\"}"
    if [[ "$(json_get "status" "$LAST_SOCKET_OUT")" == "ok" ]]; then
      lifecycle="$(json_get "payload.node_status.lifecycle_state" "$LAST_SOCKET_OUT")"
      if [[ "$lifecycle" == "STARTING" || "$lifecycle" == "RUNNING" ]]; then
        echo "$lifecycle"
        return 0
      fi
    fi
    sleep 1
  done
  return 1
}

require_cmd curl
require_cmd python3
if [[ ! -x "$DIAG_BIN" ]]; then
  require_cmd cargo
fi

tmpdir="$(mktemp -d)"
cleanup() {
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_HTTP" "$tmpdir/cleanup_http.json" >/dev/null || true
  run_admin_socket "cleanup_sock" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_SOCKET\"}" >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
}
trap cleanup EXIT

echo "ADMIN HTTP vs SOCKET parity E2E: ADMIN_TARGET=$ADMIN_TARGET HIVE_ID=$HIVE_ID RUNTIME=$RUNTIME TEST_ID=$TEST_ID"

echo "Step 1/11: build admin_internal_command_diag once"
if [[ ! -x "$DIAG_BIN" ]]; then
  (cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: admin_internal_command_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/11: validate runtime exists and read manifest metadata"
versions_http_file="$tmpdir/versions.json"
versions_http_code="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_http_file")"
if [[ "$versions_http_code" != "200" || "$(json_get "status" "$versions_http_file")" != "ok" ]]; then
  echo "FAIL: versions endpoint failed http=$versions_http_code" >&2
  cat "$versions_http_file" >&2
  exit 1
fi
if [[ "$(runtime_exists "$versions_http_file" "$RUNTIME")" != "1" ]]; then
  echo "FAIL: runtime '$RUNTIME' not present on hive '$HIVE_ID'" >&2
  cat "$versions_http_file" >&2
  exit 1
fi
manifest_version="$(json_get "payload.hive.runtimes.manifest_version" "$versions_http_file")"
manifest_hash="$(json_get "payload.hive.runtimes.manifest_hash" "$versions_http_file")"

echo "Step 3/11: cleanup baseline nodes (ignore errors)"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_HTTP" "$tmpdir/prekill_http.json" >/dev/null || true
run_admin_socket "prekill_sock" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_SOCKET\"}" || true

echo "Step 4/11: parity inventory (HTTP vs socket)"
inventory_http_file="$tmpdir/inventory_http.json"
inventory_http_code="$(http_call "GET" "$BASE/inventory/$HIVE_ID" "$inventory_http_file")"
if [[ "$inventory_http_code" != "200" ]]; then
  echo "FAIL[inventory-http]: http=$inventory_http_code" >&2
  cat "$inventory_http_file" >&2
  exit 1
fi
run_admin_socket "inventory" "inventory" "" "{\"scope\":\"hive\",\"filter_hive\":\"$HIVE_ID\"}"
inventory_http_status="$(json_get "status" "$inventory_http_file")"
inventory_socket_status="$(json_get "status" "$LAST_SOCKET_OUT")"
inventory_http_payload_status="$(json_get "payload.status" "$inventory_http_file")"
inventory_socket_payload_status="$(json_get "payload.status" "$LAST_SOCKET_OUT")"
if [[ "$inventory_http_status" != "ok" || "$inventory_socket_status" != "ok" ]]; then
  echo "FAIL[inventory-parity]: top-level status mismatch http='$inventory_http_status' socket='$inventory_socket_status'" >&2
  cat "$inventory_http_file" >&2
  cat "$LAST_SOCKET_OUT" >&2
  exit 1
fi
if [[ "$inventory_http_payload_status" != "ok" || "$inventory_socket_payload_status" != "ok" ]]; then
  echo "FAIL[inventory-parity]: payload.status mismatch http='$inventory_http_payload_status' socket='$inventory_socket_payload_status'" >&2
  cat "$inventory_http_file" >&2
  cat "$LAST_SOCKET_OUT" >&2
  exit 1
fi

echo "Step 5/11: parity sync_hint (HTTP vs socket)"
sync_params='{"channel":"dist","folder_id":"fluxbee-dist","wait_for_idle":true,"timeout_ms":30000}'
sync_http_file="$tmpdir/sync_http.json"
sync_http_code="$(http_call "POST" "$BASE/hives/$HIVE_ID/sync-hint" "$sync_http_file" "$sync_params")"
if [[ "$sync_http_code" != "200" ]]; then
  echo "FAIL[sync_hint-http]: http=$sync_http_code" >&2
  cat "$sync_http_file" >&2
  exit 1
fi
run_admin_socket "sync_hint" "sync_hint" "$HIVE_ID" "$sync_params"
sync_http_status="$(json_get "status" "$sync_http_file")"
sync_socket_status="$(json_get "status" "$LAST_SOCKET_OUT")"
if ! is_ok_like "$sync_http_status" || ! is_ok_like "$sync_socket_status"; then
  echo "FAIL[sync_hint-parity]: invalid statuses http='$sync_http_status' socket='$sync_socket_status'" >&2
  cat "$sync_http_file" >&2
  cat "$LAST_SOCKET_OUT" >&2
  exit 1
fi

echo "Step 6/11: parity update(runtime) (HTTP vs socket)"
update_params="$(cat <<JSON
{"category":"runtime","manifest_version":$manifest_version,"manifest_hash":"$manifest_hash"}
JSON
)"
update_http_file="$tmpdir/update_http.json"
update_http_code="$(http_call "POST" "$BASE/hives/$HIVE_ID/update" "$update_http_file" "$update_params")"
if [[ "$update_http_code" != "200" ]]; then
  echo "FAIL[update-http]: http=$update_http_code" >&2
  cat "$update_http_file" >&2
  exit 1
fi
run_admin_socket "update" "update" "$HIVE_ID" "$update_params"
update_http_status="$(json_get "status" "$update_http_file")"
update_http_code_field="$(json_get "error_code" "$update_http_file")"
update_socket_status="$(json_get "status" "$LAST_SOCKET_OUT")"
update_socket_code_field="$(json_get "error_code" "$LAST_SOCKET_OUT")"
if ! is_update_valid "$update_http_status" "$update_http_code_field" || ! is_update_valid "$update_socket_status" "$update_socket_code_field"; then
  echo "FAIL[update-parity]: invalid statuses/codes http=($update_http_status,$update_http_code_field) socket=($update_socket_status,$update_socket_code_field)" >&2
  cat "$update_http_file" >&2
  cat "$LAST_SOCKET_OUT" >&2
  exit 1
fi

echo "Step 7/11: run_node via HTTP and socket"
run_http_payload="$(cat <<JSON
{"node_name":"$NODE_HTTP","runtime":"$RUNTIME","runtime_version":"$RUNTIME_VERSION","tenant_id":"$TENANT_ID"}
JSON
)"
run_http_file="$tmpdir/run_http.json"
run_http_code="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$run_http_file" "$run_http_payload")"
if [[ "$run_http_code" != "200" || "$(json_get "status" "$run_http_file")" != "ok" ]]; then
  echo "FAIL[run_node-http]" >&2
  cat "$run_http_file" >&2
  exit 1
fi
run_socket_payload="$(cat <<JSON
{"node_name":"$NODE_SOCKET","runtime":"$RUNTIME","runtime_version":"$RUNTIME_VERSION","tenant_id":"$TENANT_ID"}
JSON
)"
run_admin_socket "run_node" "run_node" "$HIVE_ID" "$run_socket_payload"
if [[ "$(json_get "status" "$LAST_SOCKET_OUT")" != "ok" ]]; then
  echo "FAIL[run_node-socket]" >&2
  cat "$LAST_SOCKET_OUT" >&2
  exit 1
fi

echo "Step 8/11: get_node_status via HTTP and socket"
status_http_file="$tmpdir/status_http.json"
http_lifecycle="$(wait_http_node_started "$NODE_HTTP" "$status_http_file" || true)"
socket_lifecycle="$(wait_socket_node_started "$NODE_SOCKET_FQN" "status_socket" || true)"
if [[ -z "$http_lifecycle" || -z "$socket_lifecycle" ]]; then
  echo "FAIL[get_node_status-parity]: HTTP or socket node did not reach STARTING/RUNNING" >&2
  cat "$status_http_file" >&2 || true
  cat "$LAST_SOCKET_OUT" >&2 || true
  exit 1
fi

echo "Step 9/11: kill_node via HTTP and socket"
kill_http_file="$tmpdir/kill_http.json"
kill_http_code="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_HTTP" "$kill_http_file")"
if [[ "$kill_http_code" != "200" || "$(json_get "status" "$kill_http_file")" != "ok" ]]; then
  echo "FAIL[kill_node-http]" >&2
  cat "$kill_http_file" >&2
  exit 1
fi
run_admin_socket "kill_node" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_SOCKET_FQN\"}"
if [[ "$(json_get "status" "$LAST_SOCKET_OUT")" != "ok" ]]; then
  echo "FAIL[kill_node-socket]" >&2
  cat "$LAST_SOCKET_OUT" >&2
  exit 1
fi

echo "Step 10/11: parity assertions summary"
echo "parity_inventory=ok"
echo "parity_sync_hint=ok_like(http=$sync_http_status,socket=$sync_socket_status)"
echo "parity_update=valid(http=$update_http_status/$update_http_code_field,socket=$update_socket_status/$update_socket_code_field)"
echo "parity_run_node=ok"
echo "parity_get_node_status=ok(http=$http_lifecycle,socket=$socket_lifecycle)"
echo "parity_kill_node=ok"

echo "Step 11/11: final summary"
echo "status=ok"
echo "admin_target=$ADMIN_TARGET"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "node_http=$NODE_HTTP_FQN"
echo "node_socket=$NODE_SOCKET_FQN"
echo "admin HTTP vs socket parity FR9-T10 E2E passed."
