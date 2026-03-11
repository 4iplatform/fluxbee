#!/usr/bin/env bash
set -euo pipefail

# IO.test runtime E2E (normal orchestrator path, no SSH shortcuts)
# - builds io_test_diag
# - publishes runtime fixture in dist
# - sync-hint + SYSTEM_UPDATE runtime on target hive
# - run_node and validates status/log artifacts
# - kill_node cleanup
#
# Optional env:
#   BASE=http://127.0.0.1:8080
#   HIVE_ID=worker-220
#   BUILD_BIN=1
#   RUNTIME_NAME=io.test.diag
#   RUNTIME_VERSION=diag-<ts>
#   NODE_NAME=IO.test.diag.<id>
#   IO_TEST_CHANNEL_TYPE=whatsapp
#   IO_TEST_ADDRESS=+54911...
#   IO_TEST_ALLOW_PROVISION=1
#   WAIT_SYNC_HINT_SECS=120
#   WAIT_UPDATE_SECS=120
#   WAIT_STATUS_SECS=90
#   SYNC_HINT_TIMEOUT_MS=30000

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-}"
BUILD_BIN="${BUILD_BIN:-1}"
RUNTIME_NAME="${RUNTIME_NAME:-io.test.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-$(date +%Y%m%d%H%M%S)}"
TEST_ID="${TEST_ID:-iotest-$(date +%s)-$RANDOM}"
NODE_NAME="${NODE_NAME:-IO.test.diag.${TEST_ID}}"
IO_TEST_CHANNEL_TYPE="${IO_TEST_CHANNEL_TYPE:-io.test}"
IO_TEST_ADDRESS="${IO_TEST_ADDRESS:-io.test.${TEST_ID}}"
IO_TEST_ALLOW_PROVISION="${IO_TEST_ALLOW_PROVISION:-1}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${IO_TEST_DIAG_BIN_PATH:-$ROOT_DIR/target/release/io_test_diag}"
DIST_RUNTIMES_ROOT="/var/lib/fluxbee/dist/runtimes"
MANIFEST_PATH="$DIST_RUNTIMES_ROOT/manifest.json"
SCENARIO_ROOT="/var/lib/fluxbee/dist/.io-test-node"
SCENARIO_ENV="$SCENARIO_ROOT/scenario.${TEST_ID}.env"
STATUS_FILE="$SCENARIO_ROOT/status.${TEST_ID}.txt"
LOG_FILE="$SCENARIO_ROOT/log.${TEST_ID}.txt"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing command '$1'" >&2
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

post_sync_hint() {
  local out_file="$1"
  local payload
  payload="{\"channel\":\"dist\",\"folder_id\":\"fluxbee-dist\",\"wait_for_idle\":true,\"timeout_ms\":$SYNC_HINT_TIMEOUT_MS}"
  http_call "POST" "$BASE/hives/$HIVE_ID/sync-hint" "$out_file" "$payload"
}

wait_sync_hint_ok() {
  local out_file="$1"
  local deadline=$(( $(date +%s) + WAIT_SYNC_HINT_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status payload_status
    http_status="$(post_sync_hint "$out_file")"
    payload_status="$(json_get "payload.status" "$out_file")"
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: sync-hint status='$payload_status' http=$http_status" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: sync-hint timeout waiting ok" >&2
  cat "$out_file" >&2 || true
  return 1
}

post_runtime_update() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local out_file="$3"
  local payload
  payload="{\"category\":\"runtime\",\"manifest_version\":${manifest_version},\"manifest_hash\":\"${manifest_hash}\"}"
  http_call "POST" "$BASE/hives/$HIVE_ID/update" "$out_file" "$payload"
}

wait_update_ok() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local out_file="$3"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status payload_status error_code error_detail
    http_status="$(post_runtime_update "$manifest_version" "$manifest_hash" "$out_file")"
    payload_status="$(json_get "payload.status" "$out_file")"
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    error_code="$(json_get "error_code" "$out_file")"
    error_detail="$(json_get "error_detail" "$out_file")"
    if [[ "$error_code" == "TRANSPORT_ERROR" && "$error_detail" == *"UNREACHABLE"* ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: update runtime status='$payload_status' http=$http_status" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: update runtime timeout waiting ok" >&2
  cat "$out_file" >&2 || true
  return 1
}

spawn_node() {
  local out_file="$1"
  local payload
  payload="{\"node_name\":\"$NODE_NAME\",\"runtime\":\"$RUNTIME_NAME\",\"runtime_version\":\"current\"}"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

kill_node() {
  local out_file="$1"
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$out_file" "{\"force\":false}"
}

wait_status_ok() {
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    if as_root_local test -f "$STATUS_FILE"; then
      local val
      val="$(as_root_local cat "$STATUS_FILE" 2>/dev/null || true)"
      val="${val//$'\r'/}"
      val="${val//$'\n'/}"
      if [[ "$val" == "ok" ]]; then
        return 0
      fi
      if [[ -n "$val" && "$val" != "pending" ]]; then
        echo "FAIL: status file value '$val' (expected ok)" >&2
        if as_root_local test -f "$LOG_FILE"; then
          echo "---- io.test log (tail) ----" >&2
          as_root_local tail -n 120 "$LOG_FILE" >&2 || true
        fi
        return 1
      fi
    fi
    sleep 2
  done
  echo "FAIL: timeout waiting status file ok: $STATUS_FILE" >&2
  if as_root_local test -f "$LOG_FILE"; then
    echo "---- io.test log (tail) ----" >&2
    as_root_local tail -n 120 "$LOG_FILE" >&2 || true
  fi
  return 1
}

require_cmd cargo
require_cmd curl
require_cmd python3
require_cmd sha256sum
require_cmd sudo

if [[ -z "$HIVE_ID" && -f /etc/fluxbee/hive.yaml ]]; then
  HIVE_ID="$(awk -F': *' '/^hive_id:/ {print $2; exit}' /etc/fluxbee/hive.yaml | tr -d '"')"
fi
if [[ -z "$HIVE_ID" ]]; then
  HIVE_ID="sandbox"
fi

tmpdir="$(mktemp -d)"
cleanup() {
  local _ec=$?
  kill_body="$tmpdir/kill.json"
  kill_node "$kill_body" >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"
spawn_body="$tmpdir/spawn.json"
versions_body="$tmpdir/versions.json"
kill_body="$tmpdir/kill.json"

echo "IO.test E2E: BASE=$BASE HIVE_ID=$HIVE_ID RUNTIME=$RUNTIME_NAME VERSION=$RUNTIME_VERSION NODE_NAME=$NODE_NAME TEST_ID=$TEST_ID"

if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  echo "Step 1/8: build io_test_diag"
  (cd "$ROOT_DIR" && cargo build --release --bin io_test_diag)
else
  echo "Step 1/8: using existing io_test_diag at $BIN_PATH"
fi

echo "Step 2/8: create runtime fixture in dist"
runtime_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_NAME/$RUNTIME_VERSION/bin"
as_root_local mkdir -p "$runtime_bin_dir" "$SCENARIO_ROOT"
as_root_local install -m 0755 "$BIN_PATH" "$runtime_bin_dir/io_test_diag"

start_tmp="$tmpdir/start.sh"
cat >"$start_tmp" <<EOF
#!/usr/bin/env bash
set -euo pipefail
SCENARIO_ENV_FILE="$SCENARIO_ENV"
if [[ ! -f "$SCENARIO_ENV_FILE" ]]; then
  exit 10
fi
set -a
source "$SCENARIO_ENV_FILE"
set +a

: "${IO_TEST_STATUS_FILE:?}"
: "${IO_TEST_LOG_FILE:?}"
: "${IO_TEST_CHANNEL_TYPE:?}"
: "${IO_TEST_ADDRESS:?}"
: "${IO_TEST_ALLOW_PROVISION:=1}"
: "${IO_TEST_TARGET_HIVE:?}"

echo "pending" >"$IO_TEST_STATUS_FILE"
if JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
   IO_TEST_CONFIG_DIR="/etc/fluxbee" \
   IO_TEST_CHANNEL_TYPE="$IO_TEST_CHANNEL_TYPE" \
   IO_TEST_ADDRESS="$IO_TEST_ADDRESS" \
   IO_TEST_ALLOW_PROVISION="$IO_TEST_ALLOW_PROVISION" \
   IO_TEST_IDENTITY_TARGET="SY.identity@${IO_TEST_TARGET_HIVE}" \
   IO_TEST_NODE_NAME="${IO_TEST_NODE_NAME:-IO.test.diag}" \
   IO_TEST_NODE_VERSION="${IO_TEST_NODE_VERSION:-0.0.1}" \
   "$(dirname "${BASH_SOURCE[0]}")/io_test_diag" >"$IO_TEST_LOG_FILE" 2>&1; then
  echo "ok" >"$IO_TEST_STATUS_FILE"
else
  echo "error" >"$IO_TEST_STATUS_FILE"
fi
exec /bin/sleep "${IO_TEST_HOLD_SECS:-3600}"
EOF
as_root_local install -m 0755 "$start_tmp" "$runtime_bin_dir/start.sh"

echo "Step 3/8: update runtime manifest"
manifest_tmp="$tmpdir/manifest.json"
as_root_local python3 - "$MANIFEST_PATH" "$RUNTIME_NAME" "$RUNTIME_VERSION" >"$manifest_tmp" <<'PY'
import json
import pathlib
import sys
import time

manifest_path = pathlib.Path(sys.argv[1])
runtime_name = sys.argv[2]
runtime_version = sys.argv[3]

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

if not isinstance(doc.get("schema_version"), int) or int(doc["schema_version"]) < 1:
    doc["schema_version"] = 1
doc["version"] = int(time.time() * 1000)
doc["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes
entry = runtimes.get(runtime_name)
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
runtimes[runtime_name] = entry
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

echo "Step 4/8: prepare scenario env"
scenario_tmp="$tmpdir/scenario.env"
{
  printf 'IO_TEST_STATUS_FILE=%q\n' "$STATUS_FILE"
  printf 'IO_TEST_LOG_FILE=%q\n' "$LOG_FILE"
  printf 'IO_TEST_CHANNEL_TYPE=%q\n' "$IO_TEST_CHANNEL_TYPE"
  printf 'IO_TEST_ADDRESS=%q\n' "$IO_TEST_ADDRESS"
  printf 'IO_TEST_ALLOW_PROVISION=%q\n' "$IO_TEST_ALLOW_PROVISION"
  printf 'IO_TEST_TARGET_HIVE=%q\n' "$HIVE_ID"
  printf 'IO_TEST_NODE_NAME=%q\n' "$NODE_NAME"
  printf 'IO_TEST_NODE_VERSION=%q\n' "0.0.1"
  printf 'JSR_LOG_LEVEL=%q\n' "info"
  printf 'IO_TEST_HOLD_SECS=%q\n' "3600"
} >"$scenario_tmp"
as_root_local install -m 0644 "$scenario_tmp" "$SCENARIO_ENV"
as_root_local rm -f "$STATUS_FILE" "$LOG_FILE"

echo "Step 5/8: wait versions endpoint reachable"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: /hives/$HIVE_ID/versions returned HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 6/8: SYSTEM_SYNC_HINT dist + SYSTEM_UPDATE runtime"
wait_sync_hint_ok "$sync_hint_body"
wait_update_ok "$manifest_version" "$manifest_hash" "$update_body"

echo "Step 7/8: run IO.test node"
spawn_http="$(spawn_node "$spawn_body")"
spawn_status="$(json_get "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: run_node failed HTTP=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi
wait_status_ok

echo "Step 8/8: summary"
if as_root_local test -f "$LOG_FILE"; then
  echo "---- io.test log (tail) ----"
  as_root_local tail -n 40 "$LOG_FILE" || true
fi
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME_NAME@$RUNTIME_VERSION"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "channel_type=$IO_TEST_CHANNEL_TYPE"
echo "address=$IO_TEST_ADDRESS"
echo "io.test runtime E2E passed."
