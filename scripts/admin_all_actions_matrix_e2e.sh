#!/usr/bin/env bash
set -euo pipefail

# FR9-T24 E2E:
# Executes an internal socket matrix across all actions exposed by
# list_admin_actions, including inventory, with safe/controlled params.
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   ADMIN_TARGET="SY.admin@motherbee" \
#   HIVE_ID="worker-220" \
#   RUNTIME="wf.orch.diag" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/admin_all_actions_matrix_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
HIVE_ID="${HIVE_ID:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"
TEST_ID="${TEST_ID:-fr9t24-$(date +%s)-$RANDOM}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"

if [[ -z "$TENANT_ID" ]]; then
  echo "FAIL: TENANT_ID is required (format tnt:<uuid>)" >&2
  exit 1
fi

NODE_BASE="${NODE_BASE:-WF.admin.matrix.${TEST_ID}}"
NODE_FQN="$NODE_BASE@$HIVE_ID"
ROUTE_PREFIX="${ROUTE_PREFIX:-WF.admin.matrix.${TEST_ID}.*}"
VPN_PATTERN="${VPN_PATTERN:-WF.admin.matrix.${TEST_ID}.*}"
FAKE_HIVE_ID="${FAKE_HIVE_ID:-hive-fr9t24-nonexistent-${TEST_ID}}"

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

catalog_actions_file_to_list() {
  local catalog_file="$1"
  local out_list="$2"
  python3 - "$catalog_file" "$out_list" <<'PY'
import json
import sys

catalog_path, out_path = sys.argv[1], sys.argv[2]
doc = json.load(open(catalog_path, "r", encoding="utf-8"))
actions = doc.get("payload", {}).get("actions", [])
if not isinstance(actions, list):
    raise SystemExit("payload.actions is not a list")
with open(out_path, "w", encoding="utf-8") as out:
    for row in actions:
        if isinstance(row, dict):
            a = row.get("action")
            if isinstance(a, str) and a.strip():
                out.write(a.strip() + "\n")
PY
}

runtime_exists() {
  local versions_file="$1"
  local runtime="$2"
  python3 - "$versions_file" "$runtime" <<'PY'
import json
import sys

path, runtime = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("0")
    raise SystemExit(0)

rt = (
    doc.get("payload", {})
       .get("hive", {})
       .get("runtimes", {})
       .get("runtimes", {})
)
print("1" if isinstance(rt, dict) and runtime in rt else "0")
PY
}

run_socket_action() {
  local case_id="$1"
  local action="$2"
  local target="$3"
  local params_json="$4"
  local out_file="$tmpdir/${case_id}.json"

  if [[ -n "$target" ]]; then
    ADMIN_ACTION="$action" \
    ADMIN_TARGET="$ADMIN_TARGET" \
    ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
    ADMIN_PAYLOAD_TARGET="$target" \
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

assert_valid_matrix_response() {
  local requested_action="$1"
  local file="$2"

  local got_action status error_code error_detail
  got_action="$(json_get "action" "$file")"
  status="$(json_get "status" "$file")"
  error_code="$(json_get "error_code" "$file")"
  error_detail="$(json_get "error_detail" "$file")"

  if [[ "$got_action" != "$requested_action" ]]; then
    echo "FAIL[$requested_action]: action mismatch got='$got_action'" >&2
    cat "$file" >&2
    exit 1
  fi

  case "$status" in
    ok|sync_pending|error) ;;
    *)
      echo "FAIL[$requested_action]: invalid status='$status'" >&2
      cat "$file" >&2
      exit 1
      ;;
  esac

  local detail_lc
  detail_lc="$(printf '%s' "$error_detail" | tr '[:upper:]' '[:lower:]')"
  if [[ "$detail_lc" == *"unknown action"* || "$detail_lc" == *"legacy router action"* ]]; then
    echo "FAIL[$requested_action]: dispatcher mapping failure detected" >&2
    cat "$file" >&2
    exit 1
  fi
  if [[ "$error_code" == "INVALID_REQUEST" ]] && [[ "$detail_lc" == *"missing target"* || "$detail_lc" == *"legacy field 'hive_id'"* ]]; then
    echo "FAIL[$requested_action]: gateway contract invalid for matrix scenario" >&2
    cat "$file" >&2
    exit 1
  fi
}

require_cmd curl
require_cmd python3
if [[ ! -x "$DIAG_BIN" ]]; then
  require_cmd cargo
fi

tmpdir="$(mktemp -d)"
LAST_OUT=""
SUMMARY_FILE="$tmpdir/summary.tsv"
STORAGE_PATH=""
MANIFEST_VERSION=""
MANIFEST_HASH=""

cleanup() {
  run_socket_action "cleanup_kill" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_FQN\"}" >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
}
trap cleanup EXIT

echo "ADMIN all-actions matrix E2E: ADMIN_TARGET=$ADMIN_TARGET HIVE_ID=$HIVE_ID RUNTIME=$RUNTIME TEST_ID=$TEST_ID"

echo "Step 1/8: build admin_internal_command_diag once"
if [[ ! -x "$DIAG_BIN" ]]; then
  (cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: admin_internal_command_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/8: fetch admin action catalog + runtime metadata"
catalog_http="$tmpdir/catalog_http.json"
catalog_http_code="$(http_call "GET" "$BASE/admin/actions" "$catalog_http")"
if [[ "$catalog_http_code" != "200" || "$(json_get "status" "$catalog_http")" != "ok" ]]; then
  echo "FAIL: /admin/actions failed http=$catalog_http_code" >&2
  cat "$catalog_http" >&2
  exit 1
fi

versions_http="$tmpdir/versions_http.json"
versions_http_code="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_http")"
if [[ "$versions_http_code" != "200" || "$(json_get "status" "$versions_http")" != "ok" ]]; then
  echo "FAIL: /hives/$HIVE_ID/versions failed http=$versions_http_code" >&2
  cat "$versions_http" >&2
  exit 1
fi
if [[ "$(runtime_exists "$versions_http" "$RUNTIME")" != "1" ]]; then
  echo "FAIL: runtime '$RUNTIME' not present on hive '$HIVE_ID'" >&2
  cat "$versions_http" >&2
  exit 1
fi
MANIFEST_VERSION="$(json_get "payload.hive.runtimes.manifest_version" "$versions_http")"
MANIFEST_HASH="$(json_get "payload.hive.runtimes.manifest_hash" "$versions_http")"

actions_list="$tmpdir/actions.list"
catalog_actions_file_to_list "$catalog_http" "$actions_list"
if [[ ! -s "$actions_list" ]]; then
  echo "FAIL: empty action catalog from /admin/actions" >&2
  cat "$catalog_http" >&2
  exit 1
fi

# Prepare storage path for set_storage idempotent call.
run_socket_action "pre_get_storage" "get_storage" "" "{}"
assert_valid_matrix_response "get_storage" "$LAST_OUT"
STORAGE_PATH="$(json_get "payload.path" "$LAST_OUT")"
if [[ -z "$STORAGE_PATH" ]]; then
  STORAGE_PATH="/var/lib/fluxbee/storage"
fi

echo "Step 3/8: execute socket matrix over catalog actions"

# Ensure node-scoped actions run in deterministic order.
node_actions=(run_node get_node_status get_node_config set_node_config get_node_state kill_node)

# Build ordered list: catalog without node actions + node actions appended.
ordered_actions="$tmpdir/ordered_actions.list"
python3 - "$actions_list" "$ordered_actions" <<'PY'
import sys
src, dst = sys.argv[1], sys.argv[2]
node = ["run_node","get_node_status","get_node_config","set_node_config","get_node_state","kill_node"]
items = [line.strip() for line in open(src, "r", encoding="utf-8") if line.strip()]
base = [a for a in items if a not in node]
with open(dst, "w", encoding="utf-8") as out:
    for a in base:
        out.write(a + "\n")
    for a in node:
        if a in items:
            out.write(a + "\n")
PY

index=0
while IFS= read -r action; do
  [[ -n "$action" ]] || continue
  index=$((index + 1))
  target=""
  params='{}'

  case "$action" in
    hive_status|list_hives|list_admin_actions|get_storage)
      target=""
      params='{}'
      ;;
    list_versions|get_versions|list_routes|list_vpns|list_nodes|list_deployments|get_deployments|list_drift_alerts|get_drift_alerts)
      target="$HIVE_ID"
      params='{}'
      ;;
    add_hive)
      target=""
      params='{"hive_id":""}'
      ;;
    get_hive)
      target=""
      params="{\"hive_id\":\"$HIVE_ID\"}"
      ;;
    remove_hive)
      target=""
      params="{\"hive_id\":\"$FAKE_HIVE_ID\"}"
      ;;
    add_route)
      target="$HIVE_ID"
      params='{}'
      ;;
    delete_route)
      target="$HIVE_ID"
      params="{\"prefix\":\"$ROUTE_PREFIX\"}"
      ;;
    add_vpn)
      target="$HIVE_ID"
      params='{}'
      ;;
    delete_vpn)
      target="$HIVE_ID"
      params="{\"pattern\":\"$VPN_PATTERN\"}"
      ;;
    run_node)
      target="$HIVE_ID"
      params="{\"node_name\":\"$NODE_FQN\",\"runtime\":\"$RUNTIME\",\"runtime_version\":\"$RUNTIME_VERSION\",\"tenant_id\":\"$TENANT_ID\"}"
      ;;
    get_node_status|get_node_config|get_node_state|kill_node)
      target="$HIVE_ID"
      params="{\"node_name\":\"$NODE_FQN\"}"
      ;;
    set_node_config)
      target="$HIVE_ID"
      params="{\"node_name\":\"$NODE_FQN\",\"config\":{\"fr9_t24_probe\":\"$TEST_ID\"},\"replace\":false,\"notify\":false}"
      ;;
    set_storage)
      target=""
      params="{\"path\":\"$STORAGE_PATH\"}"
      ;;
    update)
      target="$HIVE_ID"
      params="{\"category\":\"runtime\",\"manifest_version\":$MANIFEST_VERSION,\"manifest_hash\":\"$MANIFEST_HASH\"}"
      ;;
    sync_hint)
      target="$HIVE_ID"
      params='{"channel":"dist","folder_id":"fluxbee-dist","wait_for_idle":true,"timeout_ms":30000}'
      ;;
    inventory)
      target=""
      params="{\"scope\":\"hive\",\"filter_hive\":\"$HIVE_ID\"}"
      ;;
    opa_compile_apply|opa_compile|opa_apply|opa_rollback|opa_check)
      target="$HIVE_ID"
      params='{}'
      ;;
    opa_get_policy|opa_get_status)
      target="$HIVE_ID"
      params='{}'
      ;;
    *)
      echo "FAIL: action '$action' from catalog has no matrix mapping (update script)" >&2
      exit 1
      ;;
  esac

  run_socket_action "matrix_${index}_${action}" "$action" "$target" "$params"
  assert_valid_matrix_response "$action" "$LAST_OUT"

  status="$(json_get "status" "$LAST_OUT")"
  code="$(json_get "error_code" "$LAST_OUT")"
  echo -e "${action}\t${status}\t${code}" >>"$SUMMARY_FILE"

done < "$ordered_actions"

echo "Step 4/8: matrix coverage assertions"
expected_count="$(wc -l < "$actions_list" | tr -d ' ')"
actual_count="$(wc -l < "$SUMMARY_FILE" | tr -d ' ')"
if [[ "$expected_count" != "$actual_count" ]]; then
  echo "FAIL: matrix count mismatch expected=$expected_count actual=$actual_count" >&2
  cat "$SUMMARY_FILE" >&2
  exit 1
fi
if ! grep -q '^inventory\t' "$SUMMARY_FILE"; then
  echo "FAIL: inventory action was not executed in matrix" >&2
  cat "$SUMMARY_FILE" >&2
  exit 1
fi

echo "Step 5/8: print compact action result matrix"
column -t -s $'\t' "$SUMMARY_FILE"

echo "Step 6/8: explicit cleanup kill_node (idempotent)"
run_socket_action "cleanup_end" "kill_node" "$HIVE_ID" "{\"node_name\":\"$NODE_FQN\"}" || true

echo "Step 7/8: summary metrics"
ok_count="$(awk -F'\t' '$2=="ok"{c++} END{print c+0}' "$SUMMARY_FILE")"
sync_pending_count="$(awk -F'\t' '$2=="sync_pending"{c++} END{print c+0}' "$SUMMARY_FILE")"
error_count="$(awk -F'\t' '$2=="error"{c++} END{print c+0}' "$SUMMARY_FILE")"

echo "Step 8/8: final summary"
echo "status=ok"
echo "admin_target=$ADMIN_TARGET"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "actions_total=$actual_count"
echo "status_ok=$ok_count"
echo "status_sync_pending=$sync_pending_count"
echo "status_error=$error_count"
echo "admin all-actions matrix FR9-T24 E2E passed."
