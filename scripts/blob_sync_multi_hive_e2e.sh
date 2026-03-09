#!/usr/bin/env bash
set -euo pipefail

# Blob sync real multi-hive E2E (no SSH path):
# - creates 2 runtime fixtures in dist:
#     wf.blob.produce.diag / wf.blob.consume.diag
# - publishes via SYSTEM_UPDATE (runtime) on local + worker
# - runs producer node on local hive and consumer node on worker hive
# - validates consumer result through dist-shared status files (Syncthing real)
#
# Optional:
#   BASE=http://127.0.0.1:8080
#   WORKER_HIVE_ID=worker-220
#   LOCAL_HIVE_ID=sandbox
#   BUILD_BIN=1
#   BLOB_DIAG_BIN_PATH=./target/release/blob_sync_diag
#   BLOB_ROOT_LOCAL=/var/lib/fluxbee/blob
#   BLOB_ROOT_REMOTE=/var/lib/fluxbee/blob
#   TEST_ID=blobmh-<custom>
#   WAIT_STATUS_SECS=240
#   BLOB_DIAG_CONFIRM_REQUIRED=1
#   BLOB_DIAG_CONFIRM_TIMEOUT_MS=120000
#   WAIT_SYNC_HINT_SECS=120
#   SYNC_HINT_TIMEOUT_MS=30000
#   WAIT_UPDATE_SECS=120
#   WAIT_RUNTIME_READY_SECS=180
#   SHOW_FULL_LOGS=0

BASE="${BASE:-http://127.0.0.1:8080}"
WORKER_HIVE_ID="${WORKER_HIVE_ID:-worker-220}"
LOCAL_HIVE_ID="${LOCAL_HIVE_ID:-}"
BUILD_BIN="${BUILD_BIN:-1}"
BLOB_ROOT_LOCAL="${BLOB_ROOT_LOCAL:-/var/lib/fluxbee/blob}"
BLOB_ROOT_REMOTE="${BLOB_ROOT_REMOTE:-/var/lib/fluxbee/blob}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-240}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"
WAIT_RUNTIME_READY_SECS="${WAIT_RUNTIME_READY_SECS:-180}"
WAIT_SPAWN_READY_SECS="${WAIT_SPAWN_READY_SECS:-180}"
SHOW_FULL_LOGS="${SHOW_FULL_LOGS:-0}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${BLOB_DIAG_BIN_PATH:-$ROOT_DIR/target/release/blob_sync_diag}"
BLOB_DIAG_RETRY_MAX_WAIT_MS="${BLOB_DIAG_RETRY_MAX_WAIT_MS:-180000}"
BLOB_DIAG_RETRY_INITIAL_MS="${BLOB_DIAG_RETRY_INITIAL_MS:-200}"
BLOB_DIAG_RETRY_BACKOFF="${BLOB_DIAG_RETRY_BACKOFF:-1.8}"
BLOB_DIAG_CONFIRM_REQUIRED="${BLOB_DIAG_CONFIRM_REQUIRED:-1}"
BLOB_DIAG_CONFIRM_TIMEOUT_MS="${BLOB_DIAG_CONFIRM_TIMEOUT_MS:-120000}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
BLOB_SYNC_API_PORT="${BLOB_SYNC_API_PORT:-8384}"
BLOB_DIAG_CONTENT="${BLOB_DIAG_CONTENT:-}"
BLOB_DIAG_FILENAME="${BLOB_DIAG_FILENAME:-}"
BLOB_DIAG_MIME="${BLOB_DIAG_MIME:-text/plain}"
BLOB_DIAG_PAYLOAD_TEXT="${BLOB_DIAG_PAYLOAD_TEXT:-blob multi hive producer}"

RUNTIME_PRODUCER="wf.blob.produce.diag"
RUNTIME_CONSUMER="wf.blob.consume.diag"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-$(date +%Y%m%d%H%M%S)}"
TEST_ID="${TEST_ID:-blobmh-$(date +%s)-$RANDOM}"

DIST_RUNTIMES_ROOT="/var/lib/fluxbee/dist/runtimes"
MANIFEST_PATH="$DIST_RUNTIMES_ROOT/manifest.json"
SCENARIO_ROOT="/var/lib/fluxbee/dist/.blob-sync-multi-hive"
SCENARIO_ENV="$SCENARIO_ROOT/scenario.env"

PRODUCER_NODE_NAME="WF.blob.producer.${TEST_ID}"
CONSUMER_NODE_NAME="WF.blob.consumer.${TEST_ID}"

PRODUCER_STATUS_FILE="$SCENARIO_ROOT/producer.${TEST_ID}.status"
PRODUCER_LOG_FILE="$SCENARIO_ROOT/producer.${TEST_ID}.log"
PRODUCER_REF_FILE="$SCENARIO_ROOT/blob_ref.${TEST_ID}.json"
PRODUCER_CONTRACT_FILE="$SCENARIO_ROOT/contract.${TEST_ID}.txt"
PRODUCER_ACTIVE_FILE="$SCENARIO_ROOT/active_path.${TEST_ID}.txt"

CONSUMER_STATUS_FILE="$SCENARIO_ROOT/consumer.${TEST_ID}.status"
CONSUMER_LOG_FILE="$SCENARIO_ROOT/consumer.${TEST_ID}.log"
CONSUMER_ELAPSED_FILE="$SCENARIO_ROOT/consumer_elapsed.${TEST_ID}.txt"
CONSUMER_PATH_FILE="$SCENARIO_ROOT/consumer_path.${TEST_ID}.txt"

CONTENT="${BLOB_DIAG_CONTENT:-fluxbee-blob-sync-multi-hive-${TEST_ID}}"
FILENAME="${BLOB_DIAG_FILENAME:-blob-sync-multi-hive-${TEST_ID}.txt}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

as_root_local() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    "$@"
  else
    sudo -n "$@"
  fi
}

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
  local status
  if [[ -n "$payload" ]]; then
    status="$(curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url" \
      -H "Content-Type: application/json" -d "$payload")"
  else
    status="$(curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url")"
  fi
  echo "$status"
}

json_get() {
  local expr="$1"
  local file="$2"
  python3 - "$expr" "$file" <<'PY'
import json
import sys
expr = sys.argv[1]
path = sys.argv[2]
try:
    data = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)
cur = data
for part in expr.split('.'):
    if isinstance(cur, dict):
        cur = cur.get(part)
    else:
        cur = None
        break
if cur is None:
    print("")
elif isinstance(cur, (dict, list)):
    print(json.dumps(cur, separators=(",", ":")))
else:
    print(cur)
PY
}

post_runtime_update() {
  local hive="$1"
  local version="$2"
  local hash="$3"
  local out_file="$4"
  local payload
  payload="{\"category\":\"runtime\",\"manifest_version\":${version},\"manifest_hash\":\"${hash}\"}"
  local status
  status="$(http_call "POST" "$BASE/hives/$hive/update" "$out_file" "$payload")"
  echo "$status"
  return 0
}

post_sync_hint() {
  local hive="$1"
  local channel="$2"
  local timeout_ms="$3"
  local out_file="$4"
  local payload
  payload="{\"channel\":\"$channel\",\"folder_id\":\"fluxbee-$channel\",\"wait_for_idle\":true,\"timeout_ms\":$timeout_ms}"
  local status
  status="$(http_call "POST" "$BASE/hives/$hive/sync-hint" "$out_file" "$payload")"
  echo "$status"
  return 0
}

wait_sync_hint_ok() {
  local hive="$1"
  local channel="$2"
  local timeout_ms="$3"
  local out_file="$4"
  local deadline=$(( $(date +%s) + WAIT_SYNC_HINT_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status payload_status error_code error_detail
    http_status="$(post_sync_hint "$hive" "$channel" "$timeout_ms" "$out_file")"
    payload_status="$(json_get "payload.status" "$out_file")"
    if [[ -z "$payload_status" ]]; then
      payload_status="$(json_get "status" "$out_file")"
    fi
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    error_code="$(json_get "error_code" "$out_file")"
    if [[ -z "$error_code" ]]; then
      error_code="$(json_get "payload.error_code" "$out_file")"
    fi
    error_detail="$(json_get "error_detail" "$out_file")"
    if [[ -z "$error_detail" ]]; then
      error_detail="$(json_get "payload.message" "$out_file")"
    fi
    if [[ "$payload_status" == "sync_pending" || "$error_code" == "TRANSPORT_ERROR" || "$error_detail" == *"UNREACHABLE"* ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: sync-hint status '$payload_status' (hive=$hive channel=$channel http=$http_status)" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: sync-hint timeout waiting ok (hive=$hive channel=$channel)" >&2
  cat "$out_file" >&2 || true
  return 1
}

wait_update_ok() {
  local hive="$1"
  local version="$2"
  local hash="$3"
  local out_file="$4"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status
    http_status="$(post_runtime_update "$hive" "$version" "$hash" "$out_file")"
    local payload_status
    payload_status="$(json_get "payload.status" "$out_file")"
    if [[ -z "$payload_status" ]]; then
      payload_status="$(json_get "status" "$out_file")"
    fi
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    local payload_error_code payload_error_detail
    payload_error_code="$(json_get "error_code" "$out_file")"
    payload_error_detail="$(json_get "error_detail" "$out_file")"
    if [[ "$payload_error_code" == "TRANSPORT_ERROR" && "$payload_error_detail" == *"UNREACHABLE"* ]]; then
      echo "WARN: transient transport unreachable during runtime update (hive=$hive http=$http_status), retrying..." >&2
      sleep 2
      continue
    fi
    echo "FAIL: update runtime status '$payload_status' (hive=$hive http=$http_status)" >&2
    if [[ -n "$http_status" && "$http_status" != "200" ]]; then
      echo "INFO: runtime update HTTP $http_status (hive=$hive)" >&2
    fi
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: runtime update timeout waiting ok (hive=$hive)" >&2
  cat "$out_file" >&2 || true
  return 1
}

wait_hive_versions_reachable() {
  local hive="$1"
  local out_file="$2"
  local timeout_secs="$3"
  local deadline=$(( $(date +%s) + timeout_secs ))
  while (( $(date +%s) <= deadline )); do
    local status
    status="$(http_call "GET" "$BASE/hives/$hive/versions" "$out_file")"
    if [[ "$status" == "200" ]]; then
      local top_status error_code error_detail
      top_status="$(json_get "status" "$out_file")"
      error_code="$(json_get "error_code" "$out_file")"
      error_detail="$(json_get "error_detail" "$out_file")"
      if [[ "$top_status" == "ok" ]]; then
        return 0
      fi
      if [[ "$error_code" == "VERSIONS_FAILED" || "$error_code" == "TRANSPORT_ERROR" || "$error_detail" == *"UNREACHABLE"* ]]; then
        sleep 2
        continue
      fi
    fi
    sleep 2
  done
  echo "FAIL: versions endpoint not reachable for hive '$hive' within ${timeout_secs}s" >&2
  cat "$out_file" >&2 || true
  return 1
}

wait_runtime_ready() {
  local hive="$1"
  local runtime="$2"
  local deadline=$(( $(date +%s) + WAIT_RUNTIME_READY_SECS ))
  local out_file="$3"
  while (( $(date +%s) <= deadline )); do
    local status
    status="$(http_call "GET" "$BASE/hives/$hive/versions" "$out_file")"
    if [[ "$status" == "200" ]]; then
      local current
      current="$(python3 - "$runtime" "$out_file" <<'PY'
import json,sys
runtime=sys.argv[1]
path=sys.argv[2]
try:
    doc=json.load(open(path,"r",encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)
r=doc.get("payload",{}).get("hive",{}).get("runtimes",{}).get("runtimes",{}).get(runtime,{})
cur=r.get("current","")
print(cur if isinstance(cur,str) else "")
PY
)"
      if [[ -n "$current" ]]; then
        return 0
      fi
    fi
    sleep 2
  done
  echo "FAIL: runtime '$runtime' not ready in hive '$hive'" >&2
  cat "$out_file" >&2 || true
  return 1
}

spawn_node() {
  local hive="$1"
  local node_name="$2"
  local runtime="$3"
  local out_file="$4"
  local payload
  payload="{\"node_name\":\"$node_name\",\"runtime\":\"$runtime\",\"runtime_version\":\"current\"}"
  local deadline=$(( $(date +%s) + WAIT_SPAWN_READY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local status top_status error_code error_detail
    status="$(http_call "POST" "$BASE/hives/$hive/nodes" "$out_file" "$payload")"
    top_status="$(json_get "status" "$out_file")"
    if [[ "$status" == "200" && "$top_status" == "ok" ]]; then
      return 0
    fi
    error_code="$(json_get "error_code" "$out_file")"
    if [[ -z "$error_code" ]]; then
      error_code="$(json_get "payload.error_code" "$out_file")"
    fi
    error_detail="$(json_get "error_detail" "$out_file")"
    if [[ -z "$error_detail" ]]; then
      error_detail="$(json_get "payload.message" "$out_file")"
    fi
    if [[ "$error_code" == "RUNTIME_NOT_PRESENT" || "$error_code" == "TRANSPORT_ERROR" || "$error_detail" == *"UNREACHABLE"* ]]; then
      echo "WARN: run_node transient '$error_code' (hive=$hive node=$node_name), retrying..." >&2
      sleep 2
      continue
    fi
    echo "FAIL: run_node failed (hive=$hive node=$node_name runtime=$runtime)" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: run_node timeout waiting ready (hive=$hive node=$node_name runtime=$runtime)" >&2
  cat "$out_file" >&2 || true
  return 1
}

kill_node() {
  local hive="$1"
  local node_name="$2"
  local out_file="$3"
  local status
  status="$(http_call "DELETE" "$BASE/hives/$hive/nodes/$node_name" "$out_file")"
  if [[ "$status" != "200" ]]; then
    echo "WARN: kill_node HTTP $status (hive=$hive node=$node_name)" >&2
    cat "$out_file" >&2 || true
    return 0
  fi
  return 0
}

wait_status_file_value() {
  local file_path="$1"
  local expected="$2"
  local timeout_secs="$3"
  local status_basename status_stem status_log_file
  status_basename="$(basename "$file_path")"
  status_stem="${status_basename%.status}"
  status_log_file="$SCENARIO_ROOT/${status_stem}.log"
  local deadline=$(( $(date +%s) + timeout_secs ))
  while (( $(date +%s) <= deadline )); do
    if as_root_local test -f "$file_path"; then
      local val
      val="$(as_root_local cat "$file_path" 2>/dev/null || true)"
      val="${val//$'\r'/}"
      val="${val//$'\n'/}"
      if [[ "$val" == "$expected" ]]; then
        return 0
      fi
      if [[ -n "$val" && "$val" != "pending" ]]; then
        echo "FAIL: status file '$file_path' has value '$val' (expected '$expected')" >&2
        if as_root_local test -f "$status_log_file"; then
          echo "---- diag log: $status_log_file (tail) ----" >&2
          as_root_local tail -n 120 "$status_log_file" >&2 || true
        fi
        if [[ "$status_basename" == consumer.*.status ]]; then
          local test_id producer_status producer_log
          test_id="${status_basename#consumer.}"
          test_id="${test_id%.status}"
          producer_status="$SCENARIO_ROOT/producer.${test_id}.status"
          producer_log="$SCENARIO_ROOT/producer.${test_id}.log"
          if as_root_local test -f "$producer_status"; then
            echo "---- diag producer status: $producer_status ----" >&2
            as_root_local cat "$producer_status" >&2 || true
          fi
          if as_root_local test -f "$producer_log"; then
            echo "---- diag producer log: $producer_log (tail) ----" >&2
            as_root_local tail -n 120 "$producer_log" >&2 || true
          fi
        fi
        return 1
      fi
    fi
    sleep 2
  done
  echo "FAIL: timeout waiting status '$expected' in $file_path" >&2
  if as_root_local test -d "$SCENARIO_ROOT"; then
    echo "---- diag scenario dir: $SCENARIO_ROOT ----" >&2
    as_root_local ls -la "$SCENARIO_ROOT" >&2 || true
  else
    echo "---- diag: scenario dir missing: $SCENARIO_ROOT ----" >&2
  fi
  if as_root_local test -f "$status_log_file"; then
    echo "---- diag log: $status_log_file (tail) ----" >&2
    as_root_local tail -n 120 "$status_log_file" >&2 || true
  fi
  if [[ "$status_basename" == consumer.*.status ]]; then
    local test_id producer_status producer_log
    test_id="${status_basename#consumer.}"
    test_id="${test_id%.status}"
    producer_status="$SCENARIO_ROOT/producer.${test_id}.status"
    producer_log="$SCENARIO_ROOT/producer.${test_id}.log"
    if as_root_local test -f "$producer_status"; then
      echo "---- diag producer status: $producer_status ----" >&2
      as_root_local cat "$producer_status" >&2 || true
    fi
    if as_root_local test -f "$producer_log"; then
      echo "---- diag producer log: $producer_log (tail) ----" >&2
      as_root_local tail -n 120 "$producer_log" >&2 || true
    fi
  fi
  return 1
}

require_cmd cargo
require_cmd curl
require_cmd python3
require_cmd sha256sum
require_cmd sudo
require_cmd awk

if [[ -z "$LOCAL_HIVE_ID" ]]; then
  if [[ -f /etc/fluxbee/hive.yaml ]]; then
    LOCAL_HIVE_ID="$(awk -F': *' '/^hive_id:/ {print $2; exit}' /etc/fluxbee/hive.yaml | tr -d '"')"
  fi
fi
if [[ -z "$LOCAL_HIVE_ID" ]]; then
  LOCAL_HIVE_ID="sandbox"
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

update_local_body="$tmpdir/update_local.json"
update_worker_body="$tmpdir/update_worker.json"
sync_hint_local_body="$tmpdir/sync_hint_local.json"
sync_hint_worker_body="$tmpdir/sync_hint_worker.json"
versions_local_body="$tmpdir/versions_local.json"
versions_worker_body="$tmpdir/versions_worker.json"
spawn_local_body="$tmpdir/spawn_local.json"
spawn_worker_body="$tmpdir/spawn_worker.json"
kill_local_body="$tmpdir/kill_local.json"
kill_worker_body="$tmpdir/kill_worker.json"

echo "Blob multi-hive sync E2E (no SSH): BASE=$BASE LOCAL_HIVE_ID=$LOCAL_HIVE_ID WORKER_HIVE_ID=$WORKER_HIVE_ID TEST_ID=$TEST_ID"

if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  echo "Step 1/11: build blob_sync_diag"
  (cd "$ROOT_DIR" && cargo build --release --bin blob_sync_diag)
else
  echo "Step 1/11: using existing blob_sync_diag at $BIN_PATH"
fi

echo "Step 2/11: create runtime fixtures in dist"
producer_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_PRODUCER/$RUNTIME_VERSION/bin"
consumer_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_CONSUMER/$RUNTIME_VERSION/bin"
as_root_local mkdir -p "$producer_dir" "$consumer_dir" "$SCENARIO_ROOT"
as_root_local install -m 0755 "$BIN_PATH" "$producer_dir/blob_sync_diag"
as_root_local install -m 0755 "$BIN_PATH" "$consumer_dir/blob_sync_diag"

producer_start_tmp="$tmpdir/producer_start.sh"
cat >"$producer_start_tmp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCENARIO_ROOT="/var/lib/fluxbee/dist/.blob-sync-multi-hive"
SCENARIO_ENV="$SCENARIO_ROOT/scenario.env"
if [[ ! -f "$SCENARIO_ENV" ]]; then
  echo "missing scenario.env" >"$SCENARIO_ROOT/producer.missing_scenario.log"
  exec /bin/sleep 3600
fi
set -a
source "$SCENARIO_ENV"
set +a
: "${BLOB_TEST_ID:?}"
: "${BLOB_DIAG_CONTENT:?}"
: "${BLOB_DIAG_FILENAME:?}"
: "${BLOB_DIAG_MIME:=text/plain}"
: "${BLOB_DIAG_PAYLOAD_TEXT:=blob multi hive producer}"

status_file="$SCENARIO_ROOT/producer.${BLOB_TEST_ID}.status"
log_file="$SCENARIO_ROOT/producer.${BLOB_TEST_ID}.log"
ref_file="$SCENARIO_ROOT/blob_ref.${BLOB_TEST_ID}.json"
contract_file="$SCENARIO_ROOT/contract.${BLOB_TEST_ID}.txt"
active_file="$SCENARIO_ROOT/active_path.${BLOB_TEST_ID}.txt"

echo "pending" >"$status_file"

if BLOB_SYNC_DIAG_MODE=produce \
   BLOB_ROOT="${BLOB_ROOT_LOCAL:-/var/lib/fluxbee/blob}" \
   BLOB_DIAG_FILENAME="$BLOB_DIAG_FILENAME" \
   BLOB_DIAG_CONTENT="$BLOB_DIAG_CONTENT" \
   BLOB_DIAG_MIME="$BLOB_DIAG_MIME" \
   BLOB_DIAG_PAYLOAD_TEXT="$BLOB_DIAG_PAYLOAD_TEXT" \
   BLOB_DIAG_CONFIRM_TARGETS="$WORKER_HIVE_ID" \
   BLOB_DIAG_CONFIRM_REQUIRED="${BLOB_DIAG_CONFIRM_REQUIRED:-1}" \
   BLOB_DIAG_CONFIRM_WAIT_FOR_IDLE=true \
   BLOB_DIAG_CONFIRM_TIMEOUT_MS="${BLOB_DIAG_CONFIRM_TIMEOUT_MS:-120000}" \
   BLOB_DIAG_SYSTEM_NODE_NAME="${BLOB_DIAG_SYSTEM_NODE_NAME:-WF.orch.diag}" \
   JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
   "$SCRIPT_DIR/blob_sync_diag" >"$log_file" 2>&1; then
  awk -F= '/^BLOB_REF_JSON=/{print substr($0, index($0, "=")+1)}' "$log_file" | tail -n1 >"$ref_file"
  awk -F= '/^CONTRACT_SIGNATURE=/{print substr($0, index($0, "=")+1)}' "$log_file" | tail -n1 >"$contract_file"
  awk -F= '/^ACTIVE_PATH=/{print substr($0, index($0, "=")+1)}' "$log_file" | tail -n1 >"$active_file"
  echo "ok" >"$status_file"
else
  echo "error" >"$status_file"
fi

exec /bin/sleep "${BLOB_DIAG_HOLD_SECS:-3600}"
EOF

consumer_start_tmp="$tmpdir/consumer_start.sh"
cat >"$consumer_start_tmp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCENARIO_ROOT="/var/lib/fluxbee/dist/.blob-sync-multi-hive"
SCENARIO_ENV="$SCENARIO_ROOT/scenario.env"

wait_env_secs="${BLOB_DIAG_WAIT_SCENARIO_SECS:-180}"
deadline=$(( $(date +%s) + wait_env_secs ))
while [[ ! -f "$SCENARIO_ENV" && $(date +%s) -le $deadline ]]; do
  sleep 1
done
if [[ ! -f "$SCENARIO_ENV" ]]; then
  echo "missing scenario.env after wait" >"$SCENARIO_ROOT/consumer.missing_scenario.log"
  exec /bin/sleep 3600
fi

set -a
source "$SCENARIO_ENV"
set +a
: "${BLOB_TEST_ID:?}"
: "${BLOB_DIAG_CONTENT:?}"

status_file="$SCENARIO_ROOT/consumer.${BLOB_TEST_ID}.status"
log_file="$SCENARIO_ROOT/consumer.${BLOB_TEST_ID}.log"
elapsed_file="$SCENARIO_ROOT/consumer_elapsed.${BLOB_TEST_ID}.txt"
path_file="$SCENARIO_ROOT/consumer_path.${BLOB_TEST_ID}.txt"
ref_file="$SCENARIO_ROOT/blob_ref.${BLOB_TEST_ID}.json"

echo "pending" >"$status_file"

wait_ref_secs="${BLOB_DIAG_WAIT_REF_SECS:-240}"
deadline=$(( $(date +%s) + wait_ref_secs ))
while [[ ! -s "$ref_file" && $(date +%s) -le $deadline ]]; do
  sleep 1
done
if [[ ! -s "$ref_file" ]]; then
  echo "error_ref_missing" >"$status_file"
  exec /bin/sleep "${BLOB_DIAG_HOLD_SECS:-3600}"
fi

blob_ref_json="$(cat "$ref_file")"
if BLOB_SYNC_DIAG_MODE=consume \
   BLOB_ROOT="${BLOB_ROOT_REMOTE:-/var/lib/fluxbee/blob}" \
   BLOB_DIAG_BLOB_REF_JSON="$blob_ref_json" \
   BLOB_DIAG_EXPECT_CONTENT="$BLOB_DIAG_CONTENT" \
   BLOB_DIAG_RETRY_MAX_WAIT_MS="${BLOB_DIAG_RETRY_MAX_WAIT_MS:-180000}" \
   BLOB_DIAG_RETRY_INITIAL_MS="${BLOB_DIAG_RETRY_INITIAL_MS:-200}" \
   BLOB_DIAG_RETRY_BACKOFF="${BLOB_DIAG_RETRY_BACKOFF:-1.8}" \
   JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
   "$SCRIPT_DIR/blob_sync_diag" >"$log_file" 2>&1; then
  awk -F= '/^ELAPSED_MS=/{print substr($0, index($0, "=")+1)}' "$log_file" | tail -n1 >"$elapsed_file"
  awk -F= '/^RESOLVED_PATH=/{print substr($0, index($0, "=")+1)}' "$log_file" | tail -n1 >"$path_file"
  echo "ok" >"$status_file"
else
  {
    echo "---- consumer diag ----"
    date -Is || true
    python3 - "$blob_ref_json" "${BLOB_ROOT_REMOTE:-/var/lib/fluxbee/blob}" <<'PY'
import json,sys,pathlib
raw = sys.argv[1]
root = pathlib.Path(sys.argv[2])
try:
    ref = json.loads(raw)
except Exception as exc:
    print(f"diag: failed to parse blob_ref_json: {exc}")
    raise SystemExit(0)
name = str(ref.get("blob_name","")).strip()
if not name:
    print("diag: blob_name missing in blob_ref_json")
    raise SystemExit(0)
parts = name.rsplit("_", 1)
prefix = "00"
if len(parts) == 2 and "." in parts[1]:
    hash16 = parts[1].split(".", 1)[0]
    if len(hash16) >= 2:
        prefix = hash16[:2]
path = root / "active" / prefix / name
print(f"diag: expected_blob_path={path}")
print(f"diag: expected_blob_exists={path.exists()}")
PY
    cfg="/var/lib/fluxbee/syncthing/config.xml"
    if [[ -f "$cfg" ]]; then
      api="$(grep -oPm1 '(?<=<apikey>)[^<]+' "$cfg" || true)"
      if [[ -n "${api:-}" ]]; then
        port="${BLOB_SYNC_API_PORT:-8384}"
        echo "diag: syncthing_api_port=$port"
        echo "diag: syncthing_db_status_fluxbee_blob"
        curl -sS -H "X-API-Key: $api" \
          "http://127.0.0.1:${port}/rest/db/status?folder=fluxbee-blob" || true
        echo
        echo "diag: syncthing_system_connections"
        curl -sS -H "X-API-Key: $api" \
          "http://127.0.0.1:${port}/rest/system/connections" || true
        echo
      else
        echo "diag: syncthing API key not found in $cfg"
      fi
    else
      echo "diag: syncthing config missing at $cfg"
    fi
  } >>"$log_file" 2>&1
  echo "error" >"$status_file"
fi

exec /bin/sleep "${BLOB_DIAG_HOLD_SECS:-3600}"
EOF

as_root_local install -m 0755 "$producer_start_tmp" "$producer_dir/start.sh"
as_root_local install -m 0755 "$consumer_start_tmp" "$consumer_dir/start.sh"

echo "Step 3/11: update runtime manifest with producer/consumer runtimes"
manifest_tmp="$tmpdir/manifest.json"
as_root_local python3 - "$MANIFEST_PATH" "$RUNTIME_PRODUCER" "$RUNTIME_CONSUMER" "$RUNTIME_VERSION" >"$manifest_tmp" <<'PY'
import json
import pathlib
import sys
import time

manifest_path = pathlib.Path(sys.argv[1])
runtime_producer = sys.argv[2]
runtime_consumer = sys.argv[3]
runtime_version = sys.argv[4]

doc = {
    "schema_version": 1,
    "version": 0,
    "updated_at": None,
    "runtimes": {},
}
if manifest_path.exists():
    try:
        loaded = json.loads(manifest_path.read_text(encoding="utf-8"))
        if isinstance(loaded, dict):
            doc.update(loaded)
    except Exception:
        pass

schema_version = doc.get("schema_version")
if not isinstance(schema_version, int) or schema_version < 1:
    schema_version = 1
doc["schema_version"] = schema_version
doc["version"] = int(time.time() * 1000)
doc["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes

def upsert(name: str):
    entry = runtimes.get(name)
    if not isinstance(entry, dict):
        entry = {}
    available = entry.get("available")
    if not isinstance(available, list):
        available = []
    available = [str(v) for v in available if str(v).strip()]
    if runtime_version not in available:
        available.append(runtime_version)
    entry["available"] = sorted(set(available))
    entry["current"] = runtime_version
    runtimes[name] = entry

upsert(runtime_producer)
upsert(runtime_consumer)
print(json.dumps(doc, indent=2, sort_keys=True))
PY
as_root_local install -m 0644 "$manifest_tmp" "$MANIFEST_PATH"

manifest_version="$(as_root_local python3 - "$MANIFEST_PATH" <<'PY'
import json,sys
doc=json.load(open(sys.argv[1],"r",encoding="utf-8"))
print(int(doc.get("version",0) or 0))
PY
)"
manifest_hash="$(as_root_local sha256sum "$MANIFEST_PATH" | awk '{print $1}')"
if [[ -z "$manifest_version" || "$manifest_version" == "0" || -z "$manifest_hash" ]]; then
  echo "FAIL: cannot resolve runtime manifest version/hash" >&2
  exit 1
fi
echo "runtime manifest version=$manifest_version hash=$manifest_hash"

echo "Step 4/11: prepare scenario files"
as_root_local mkdir -p "$SCENARIO_ROOT"
as_root_local rm -f \
  "$PRODUCER_STATUS_FILE" "$PRODUCER_LOG_FILE" "$PRODUCER_REF_FILE" "$PRODUCER_CONTRACT_FILE" "$PRODUCER_ACTIVE_FILE" \
  "$CONSUMER_STATUS_FILE" "$CONSUMER_LOG_FILE" "$CONSUMER_ELAPSED_FILE" "$CONSUMER_PATH_FILE"
scenario_tmp="$tmpdir/scenario.env"
{
  printf 'BLOB_TEST_ID=%q\n' "$TEST_ID"
  printf 'BLOB_ROOT_LOCAL=%q\n' "$BLOB_ROOT_LOCAL"
  printf 'BLOB_ROOT_REMOTE=%q\n' "$BLOB_ROOT_REMOTE"
  printf 'BLOB_DIAG_CONTENT=%q\n' "$CONTENT"
  printf 'BLOB_DIAG_FILENAME=%q\n' "$FILENAME"
  printf 'BLOB_DIAG_MIME=%q\n' "$BLOB_DIAG_MIME"
  printf 'BLOB_DIAG_PAYLOAD_TEXT=%q\n' "$BLOB_DIAG_PAYLOAD_TEXT"
  printf 'BLOB_DIAG_RETRY_MAX_WAIT_MS=%q\n' "$BLOB_DIAG_RETRY_MAX_WAIT_MS"
  printf 'BLOB_DIAG_RETRY_INITIAL_MS=%q\n' "$BLOB_DIAG_RETRY_INITIAL_MS"
  printf 'BLOB_DIAG_RETRY_BACKOFF=%q\n' "$BLOB_DIAG_RETRY_BACKOFF"
  printf 'BLOB_DIAG_CONFIRM_REQUIRED=%q\n' "$BLOB_DIAG_CONFIRM_REQUIRED"
  printf 'BLOB_DIAG_CONFIRM_TIMEOUT_MS=%q\n' "$BLOB_DIAG_CONFIRM_TIMEOUT_MS"
  printf 'WORKER_HIVE_ID=%q\n' "$WORKER_HIVE_ID"
  printf 'BLOB_SYNC_API_PORT=%q\n' "$BLOB_SYNC_API_PORT"
  printf 'JSR_LOG_LEVEL=%q\n' "info"
} >"$scenario_tmp"
as_root_local install -m 0644 "$scenario_tmp" "$SCENARIO_ENV"

echo "Step 5/11: wait hives reachable in versions endpoint"
wait_hive_versions_reachable "$LOCAL_HIVE_ID" "$versions_local_body" "$WAIT_RUNTIME_READY_SECS"
wait_hive_versions_reachable "$WORKER_HIVE_ID" "$versions_worker_body" "$WAIT_RUNTIME_READY_SECS"

echo "Step 6/11: SYSTEM_SYNC_HINT dist on local + worker"
wait_sync_hint_ok "$LOCAL_HIVE_ID" "dist" "$SYNC_HINT_TIMEOUT_MS" "$sync_hint_local_body"
wait_sync_hint_ok "$WORKER_HIVE_ID" "dist" "$SYNC_HINT_TIMEOUT_MS" "$sync_hint_worker_body"

echo "Step 7/11: SYSTEM_UPDATE runtime on local + worker"
wait_update_ok "$LOCAL_HIVE_ID" "$manifest_version" "$manifest_hash" "$update_local_body"
wait_update_ok "$WORKER_HIVE_ID" "$manifest_version" "$manifest_hash" "$update_worker_body"

echo "Step 8/11: wait runtimes ready in versions endpoint"
wait_runtime_ready "$LOCAL_HIVE_ID" "$RUNTIME_PRODUCER" "$versions_local_body"
wait_runtime_ready "$WORKER_HIVE_ID" "$RUNTIME_CONSUMER" "$versions_worker_body"

echo "Step 9/11: run producer (local) and consumer (worker)"
spawn_node "$LOCAL_HIVE_ID" "$PRODUCER_NODE_NAME" "$RUNTIME_PRODUCER" "$spawn_local_body"
spawn_node "$WORKER_HIVE_ID" "$CONSUMER_NODE_NAME" "$RUNTIME_CONSUMER" "$spawn_worker_body"

cleanup_nodes() {
  kill_node "$LOCAL_HIVE_ID" "$PRODUCER_NODE_NAME" "$kill_local_body" || true
  kill_node "$WORKER_HIVE_ID" "$CONSUMER_NODE_NAME" "$kill_worker_body" || true
}
trap 'cleanup_nodes; rm -rf "$tmpdir"' EXIT

echo "Step 10/11: wait producer/consumer status (via dist sync)"
wait_status_file_value "$PRODUCER_STATUS_FILE" "ok" "$WAIT_STATUS_SECS"
wait_status_file_value "$CONSUMER_STATUS_FILE" "ok" "$WAIT_STATUS_SECS"

echo "Step 11/11: summary + cleanup"
consumer_elapsed="$(as_root_local cat "$CONSUMER_ELAPSED_FILE" 2>/dev/null || true)"
consumer_path="$(as_root_local cat "$CONSUMER_PATH_FILE" 2>/dev/null || true)"
contract_signature="$(as_root_local cat "$PRODUCER_CONTRACT_FILE" 2>/dev/null || true)"
active_local="$(as_root_local cat "$PRODUCER_ACTIVE_FILE" 2>/dev/null || true)"

echo "---- Blob multi-hive sync summary ----"
echo "status=ok"
echo "mode=real_syncthing_multi_hive_via_run_node"
echo "local_hive_id=$LOCAL_HIVE_ID"
echo "worker_hive_id=$WORKER_HIVE_ID"
echo "runtime_producer=$RUNTIME_PRODUCER@$RUNTIME_VERSION"
echo "runtime_consumer=$RUNTIME_CONSUMER@$RUNTIME_VERSION"
echo "active_path_local=$active_local"
echo "resolved_path_remote=$consumer_path"
echo "contract_signature=$contract_signature"
echo "consumer_retry_elapsed_ms=$consumer_elapsed"

if [[ "$SHOW_FULL_LOGS" == "1" ]]; then
  echo "---- producer log ----"
  as_root_local cat "$PRODUCER_LOG_FILE" || true
  echo "---- consumer log ----"
  as_root_local cat "$CONSUMER_LOG_FILE" || true
fi

cleanup_nodes
echo "blob sync multi-hive E2E passed."
