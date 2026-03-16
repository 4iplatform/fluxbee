#!/usr/bin/env bash
set -euo pipefail

# FR9-T18 E2E (socket):
# validates precedence rule `node_name@hive > payload.target` for node-scoped actions.
#
# Covered:
# - run_node with conflicting target must execute on node_name@hive
# - get_node_status with conflicting target must resolve by node_name@hive
# - kill_node with conflicting target must resolve by node_name@hive
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   ADMIN_TARGET="SY.admin@motherbee" \
#   REQUEST_TARGET_HIVE="motherbee" \
#   NODE_HIVE="worker-220" \
#   RUNTIME="wf.orch.diag" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/admin_internal_node_name_precedence_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
REQUEST_TARGET_HIVE="${REQUEST_TARGET_HIVE:-motherbee}"
NODE_HIVE="${NODE_HIVE:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"
TEST_ID="${TEST_ID:-fr9t18-$(date +%s)-$RANDOM}"
NODE_BASE="${NODE_BASE:-WF.admin.precedence.${TEST_ID}}"
NODE_FQN="$NODE_BASE@$NODE_HIVE"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"

if [[ -z "$TENANT_ID" ]]; then
  echo "FAIL: TENANT_ID is required (format tnt:<uuid>)" >&2
  exit 1
fi
if [[ "$REQUEST_TARGET_HIVE" == "$NODE_HIVE" ]]; then
  echo "FAIL: REQUEST_TARGET_HIVE and NODE_HIVE must differ for precedence test" >&2
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
cleanup() {
  run_admin "cleanup_end" "kill_node" "$REQUEST_TARGET_HIVE" "{\"node_name\":\"$NODE_FQN\"}" >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
}
trap cleanup EXIT

echo "ADMIN internal node_name precedence E2E: ADMIN_TARGET=$ADMIN_TARGET REQUEST_TARGET_HIVE=$REQUEST_TARGET_HIVE NODE_HIVE=$NODE_HIVE RUNTIME=$RUNTIME"

echo "Step 1/7: build admin_internal_command_diag once"
if [[ ! -x "$DIAG_BIN" ]]; then
  (cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: admin_internal_command_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/7: validate runtime exists on NODE_HIVE"
versions_body="$tmpdir/versions.json"
versions_http="$(http_call "GET" "$BASE/hives/$NODE_HIVE/versions" "$versions_body")"
if [[ "$versions_http" != "200" || "$(json_get "status" "$versions_body")" != "ok" ]]; then
  echo "FAIL: GET /hives/$NODE_HIVE/versions failed http=$versions_http" >&2
  cat "$versions_body" >&2
  exit 1
fi
if [[ "$(runtime_exists "$versions_body" "$RUNTIME")" != "1" ]]; then
  echo "FAIL: runtime '$RUNTIME' not present on hive '$NODE_HIVE'" >&2
  cat "$versions_body" >&2
  exit 1
fi

echo "Step 3/7: cleanup baseline node (ignore errors)"
run_admin "cleanup_pre" "kill_node" "$REQUEST_TARGET_HIVE" "{\"node_name\":\"$NODE_FQN\"}" || true

echo "Step 4/7: run_node with conflicting target (must honor node_name@hive)"
run_params="$(cat <<JSON
{"node_name":"$NODE_FQN","runtime":"$RUNTIME","runtime_version":"$RUNTIME_VERSION","tenant_id":"$TENANT_ID"}
JSON
)"
run_admin "run_node" "run_node" "$REQUEST_TARGET_HIVE" "$run_params"
if [[ "$(json_get "status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[run_node]: expected status=ok" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi
run_hive="$(json_get "payload.hive" "$LAST_OUT")"
run_name="$(json_get "payload.node_name" "$LAST_OUT")"
if [[ "$run_hive" != "$NODE_HIVE" || "$run_name" != "$NODE_FQN" ]]; then
  echo "FAIL[run_node]: precedence failed expected hive='$NODE_HIVE' node='$NODE_FQN' got hive='$run_hive' node='$run_name'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 5/7: get_node_status with conflicting target (must resolve by node_name@hive)"
run_admin "get_node_status" "get_node_status" "$REQUEST_TARGET_HIVE" "{\"node_name\":\"$NODE_FQN\"}"
if [[ "$(json_get "status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[get_node_status]: expected status=ok" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi
status_hive="$(json_get "payload.hive" "$LAST_OUT")"
status_name="$(json_get "payload.node_name" "$LAST_OUT")"
if [[ "$status_hive" != "$NODE_HIVE" || "$status_name" != "$NODE_FQN" ]]; then
  echo "FAIL[get_node_status]: precedence failed expected hive='$NODE_HIVE' node='$NODE_FQN' got hive='$status_hive' node='$status_name'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 6/7: kill_node with conflicting target (must resolve by node_name@hive)"
run_admin "kill_node" "kill_node" "$REQUEST_TARGET_HIVE" "{\"node_name\":\"$NODE_FQN\"}"
if [[ "$(json_get "status" "$LAST_OUT")" != "ok" ]]; then
  echo "FAIL[kill_node]: expected status=ok" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi
kill_hive="$(json_get "payload.hive" "$LAST_OUT")"
kill_name="$(json_get "payload.node_name" "$LAST_OUT")"
if [[ "$kill_hive" != "$NODE_HIVE" || "$kill_name" != "$NODE_FQN" ]]; then
  echo "FAIL[kill_node]: precedence failed expected hive='$NODE_HIVE' node='$NODE_FQN' got hive='$kill_hive' node='$kill_name'" >&2
  cat "$LAST_OUT" >&2
  exit 1
fi

echo "Step 7/7: summary"
echo "status=ok"
echo "admin_target=$ADMIN_TARGET"
echo "request_target_hive=$REQUEST_TARGET_HIVE"
echo "node_hive=$NODE_HIVE"
echo "node_name=$NODE_FQN"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "admin internal node_name precedence FR9-T18 E2E passed."
