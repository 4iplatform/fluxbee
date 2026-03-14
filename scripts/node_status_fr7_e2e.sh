#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
BUILD_BIN="${BUILD_BIN:-1}"
TENANT_ID="${TENANT_ID:-}"
TEST_ID="${TEST_ID:-fr7-$(date +%s)-${RANDOM}}"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-fr7-${TEST_ID}}"
RUNTIME_REPORTED="${RUNTIME_REPORTED:-wf.node.status.report.diag}"
RUNTIME_FALLBACK="${RUNTIME_FALLBACK:-wf.node.status.fallback.diag}"
RUNTIME_ERROR="${RUNTIME_ERROR:-wf.node.status.error.diag}"
NODE_REPORTED="${NODE_REPORTED:-WF.status.report.${TEST_ID}}"
NODE_FALLBACK="${NODE_FALLBACK:-WF.status.fallback.${TEST_ID}}"
NODE_ERROR="${NODE_ERROR:-WF.status.error.${TEST_ID}}"
STATUS_TIMEOUT_SECS="${STATUS_TIMEOUT_SECS:-30}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${NODE_STATUS_DIAG_BIN_PATH:-$ROOT_DIR/target/release/inventory_hold_diag}"
DIST_RUNTIMES_ROOT="/var/lib/fluxbee/dist/runtimes"
MANIFEST_PATH="$DIST_RUNTIMES_ROOT/manifest.json"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_report_body="$tmpdir/spawn_report.json"
spawn_fallback_body="$tmpdir/spawn_fallback.json"
spawn_error_body="$tmpdir/spawn_error.json"
kill_body="$tmpdir/kill.json"
status_report_body="$tmpdir/status_report.json"
status_fallback_body="$tmpdir/status_fallback.json"
status_stopped_body="$tmpdir/status_stopped.json"
status_error_body="$tmpdir/status_error.json"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"
local_hive_body="$tmpdir/local_hive.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_REPORTED" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FALLBACK" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_ERROR" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  }
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
  local out_file="$3"
  local payload="${4:-}"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$out_file" -w "%{http_code}" -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$out_file" -w "%{http_code}" -X "$method" "$url"
  fi
}

json_get_file() {
  local path="$1"
  local file="$2"
  python3 - "$path" "$file" <<'PY'
import json
import sys

path = sys.argv[1]
file_path = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

value = doc
for part in path.split("."):
    if not part:
        continue
    if isinstance(value, dict):
        value = value.get(part)
    else:
        value = None
        break

if value is None:
    print("")
elif isinstance(value, bool):
    print("true" if value else "false")
elif isinstance(value, (dict, list)):
    print(json.dumps(value))
else:
    print(str(value))
PY
}

runtime_exists_in_manifest() {
  local runtime="$1"
  local file="$2"
  python3 - "$runtime" "$file" <<'PY'
import json
import sys

runtime = sys.argv[1]
file_path = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    raise SystemExit(2)

runtimes = (
    doc.get("payload", {})
       .get("hive", {})
       .get("runtimes", {})
       .get("runtimes", {})
)
if not isinstance(runtimes, dict):
    raise SystemExit(2)
raise SystemExit(0 if runtime in runtimes else 1)
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
    local payload_status
    post_sync_hint "$out_file" >/dev/null
    payload_status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: sync-hint status='$payload_status'" >&2
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
    local payload_status error_code error_detail
    post_runtime_update "$manifest_version" "$manifest_hash" "$out_file" >/dev/null
    payload_status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    error_code="$(json_get_file "error_code" "$out_file")"
    error_detail="$(json_get_file "error_detail" "$out_file")"
    if [[ "$error_code" == "TRANSPORT_ERROR" && "$error_detail" == *"UNREACHABLE"* ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: update runtime status='$payload_status'" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: update runtime timeout waiting ok" >&2
  cat "$out_file" >&2 || true
  return 1
}

write_runtime_start_sh() {
  local start_path="$1"
  local node_name="$2"
  local handler_enabled="$3"
  local health_state="$4"
  cat >"$start_path" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="$node_name"
export INVENTORY_HOLD_NODE_VERSION="0.0.1"
export INVENTORY_HOLD_SECS="0"
export NODE_STATUS_DEFAULT_HANDLER_ENABLED="$handler_enabled"
export NODE_STATUS_DEFAULT_HEALTH_STATE="$health_state"
exec "\$(dirname "\${BASH_SOURCE[0]}")/inventory_hold_diag"
EOF
}

wait_node_status() {
  local node_name="$1"
  local expected_lifecycle="$2"
  local expected_config_valid="$3"
  local expected_source="$4"
  local expected_health="$5"
  local out_file="$6"
  local deadline=$(( $(date +%s) + STATUS_TIMEOUT_SECS ))

  while (( $(date +%s) <= deadline )); do
    local http api_status payload_status lifecycle source health config_valid
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$node_name/status" "$out_file")"
    if [[ "$http" != "200" ]]; then
      sleep 1
      continue
    fi
    api_status="$(json_get_file "status" "$out_file")"
    payload_status="$(json_get_file "payload.status" "$out_file")"
    lifecycle="$(json_get_file "payload.node_status.lifecycle_state" "$out_file")"
    source="$(json_get_file "payload.node_status.health_source" "$out_file")"
    health="$(json_get_file "payload.node_status.health_state" "$out_file")"
    config_valid="$(json_get_file "payload.node_status.config.valid" "$out_file")"
    if [[ "$api_status" != "ok" || "$payload_status" != "ok" ]]; then
      sleep 1
      continue
    fi
    if [[ "$lifecycle" == "$expected_lifecycle" && "$source" == "$expected_source" && "$health" == "$expected_health" && "$config_valid" == "$expected_config_valid" ]]; then
      return 0
    fi
    sleep 1
  done
  echo "FAIL: node status mismatch node='$node_name' expected_lifecycle='$expected_lifecycle' expected_source='$expected_source' expected_health='$expected_health' expected_config_valid='$expected_config_valid'" >&2
  cat "$out_file" >&2 || true
  return 1
}

require_cmd curl
require_cmd python3
require_cmd sha256sum
require_cmd sudo

local_hive_http="$(http_call "GET" "$BASE/hive/status" "$local_hive_body")"
if [[ "$local_hive_http" != "200" || "$(json_get_file "status" "$local_hive_body")" != "ok" ]]; then
  echo "FAIL: cannot resolve local hive from /hive/status (http=$local_hive_http)" >&2
  cat "$local_hive_body" >&2 || true
  exit 1
fi
LOCAL_HIVE_ID="$(json_get_file "payload.hive_id" "$local_hive_body")"
if [[ -z "$LOCAL_HIVE_ID" ]]; then
  echo "FAIL: /hive/status returned empty payload.hive_id" >&2
  cat "$local_hive_body" >&2 || true
  exit 1
fi

echo "FR7 T7/T8/T9/T10 E2E: BASE=$BASE HIVE_ID=$HIVE_ID LOCAL_HIVE_ID=$LOCAL_HIVE_ID REPORT_RUNTIME=$RUNTIME_REPORTED FALLBACK_RUNTIME=$RUNTIME_FALLBACK ERROR_RUNTIME=$RUNTIME_ERROR"

echo "Step 1/13: build inventory_hold_diag (status-aware)"
if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  require_cmd cargo
  (cd "$ROOT_DIR" && cargo build --release --bin inventory_hold_diag)
fi
if [[ ! -x "$BIN_PATH" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at $BIN_PATH" >&2
  exit 1
fi

echo "Step 2/13: create runtime fixtures for NODE_REPORTED, fallback-inferred, and NODE_REPORTED=ERROR"
report_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_REPORTED/$RUNTIME_VERSION/bin"
fallback_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_FALLBACK/$RUNTIME_VERSION/bin"
error_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_ERROR/$RUNTIME_VERSION/bin"
as_root_local mkdir -p "$report_bin_dir" "$fallback_bin_dir" "$error_bin_dir"
as_root_local install -m 0755 "$BIN_PATH" "$report_bin_dir/inventory_hold_diag"
as_root_local install -m 0755 "$BIN_PATH" "$fallback_bin_dir/inventory_hold_diag"
as_root_local install -m 0755 "$BIN_PATH" "$error_bin_dir/inventory_hold_diag"
report_start="$tmpdir/report_start.sh"
fallback_start="$tmpdir/fallback_start.sh"
error_start="$tmpdir/error_start.sh"
write_runtime_start_sh "$report_start" "$NODE_REPORTED" "1" "HEALTHY"
write_runtime_start_sh "$fallback_start" "$NODE_FALLBACK" "0" "HEALTHY"
write_runtime_start_sh "$error_start" "$NODE_ERROR" "1" "ERROR"
as_root_local install -m 0755 "$report_start" "$report_bin_dir/start.sh"
as_root_local install -m 0755 "$fallback_start" "$fallback_bin_dir/start.sh"
as_root_local install -m 0755 "$error_start" "$error_bin_dir/start.sh"

echo "Step 3/13: update runtime manifest"
manifest_tmp="$tmpdir/manifest.json"
as_root_local python3 - "$MANIFEST_PATH" "$RUNTIME_REPORTED" "$RUNTIME_FALLBACK" "$RUNTIME_ERROR" "$RUNTIME_VERSION" >"$manifest_tmp" <<'PY'
import json
import pathlib
import sys
import time

manifest_path = pathlib.Path(sys.argv[1])
runtime_report = sys.argv[2]
runtime_fallback = sys.argv[3]
runtime_error = sys.argv[4]
runtime_version = sys.argv[5]

doc = {"schema_version": 1, "version": 0, "updated_at": None, "runtimes": {}}
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

for runtime_name in (runtime_report, runtime_fallback, runtime_error):
    entry = runtimes.get(runtime_name)
    if not isinstance(entry, dict):
        entry = {}
    available = entry.get("available")
    if not isinstance(available, list):
        available = []
    available = [str(v).strip() for v in available if str(v).strip()]
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

echo "Step 4/13: SYSTEM_SYNC_HINT dist + SYSTEM_UPDATE runtime"
wait_sync_hint_ok "$sync_hint_body"
wait_update_ok "$manifest_version" "$manifest_hash" "$update_body"

echo "Step 5/13: validate runtime presence in versions"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME_REPORTED" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_REPORTED' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME_FALLBACK" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_FALLBACK' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME_ERROR" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_ERROR' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 6/13: spawn reported-status node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_REPORTED" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
if [[ -n "$TENANT_ID" ]]; then
  spawn_report_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' "$NODE_REPORTED" "$RUNTIME_REPORTED" "$TENANT_ID")"
else
  spawn_report_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$NODE_REPORTED" "$RUNTIME_REPORTED")"
fi
spawn_report_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_report_body" "$spawn_report_payload")"
if [[ "$spawn_report_http" != "200" || "$(json_get_file "status" "$spawn_report_body")" != "ok" ]]; then
  echo "FAIL: reported node spawn failed http=$spawn_report_http" >&2
  cat "$spawn_report_body" >&2 || true
  exit 1
fi

echo "Step 7/13: validate NODE_REPORTED precedence"
wait_node_status "$NODE_REPORTED" "RUNNING" "true" "NODE_REPORTED" "HEALTHY" "$status_report_body"

echo "Step 8/13: spawn fallback-status node and validate inferred fallback"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FALLBACK" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
if [[ -n "$TENANT_ID" ]]; then
  spawn_fallback_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' "$NODE_FALLBACK" "$RUNTIME_FALLBACK" "$TENANT_ID")"
else
  spawn_fallback_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$NODE_FALLBACK" "$RUNTIME_FALLBACK")"
fi
spawn_fallback_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_fallback_body" "$spawn_fallback_payload")"
if [[ "$spawn_fallback_http" != "200" || "$(json_get_file "status" "$spawn_fallback_body")" != "ok" ]]; then
  echo "FAIL: fallback node spawn failed http=$spawn_fallback_http" >&2
  cat "$spawn_fallback_body" >&2 || true
  exit 1
fi
wait_node_status "$NODE_FALLBACK" "RUNNING" "true" "ORCHESTRATOR_INFERRED" "HEALTHY" "$status_fallback_body"

echo "Step 9/13: stop fallback node and validate STOPPED/UNKNOWN"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FALLBACK" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
wait_node_status "$NODE_FALLBACK" "STOPPED" "true" "UNKNOWN" "UNKNOWN" "$status_stopped_body"

echo "Step 10/13: spawn error-reporting node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_ERROR" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
if [[ -n "$TENANT_ID" ]]; then
  spawn_error_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' "$NODE_ERROR" "$RUNTIME_ERROR" "$TENANT_ID")"
else
  spawn_error_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$NODE_ERROR" "$RUNTIME_ERROR")"
fi
spawn_error_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_error_body" "$spawn_error_payload")"
if [[ "$spawn_error_http" != "200" || "$(json_get_file "status" "$spawn_error_body")" != "ok" ]]; then
  echo "FAIL: error node spawn failed http=$spawn_error_http" >&2
  cat "$spawn_error_body" >&2 || true
  exit 1
fi

echo "Step 11/13: corrupt config.json to force config.valid=false"
error_config_path="$(json_get_file "payload.config.path" "$spawn_error_body")"
if [[ -z "$error_config_path" ]]; then
  echo "FAIL: missing config path for error node" >&2
  cat "$spawn_error_body" >&2 || true
  exit 1
fi
T10_MODE="full"
if [[ "$HIVE_ID" != "$LOCAL_HIVE_ID" ]]; then
  echo "INFO: target hive '$HIVE_ID' is remote from local '$LOCAL_HIVE_ID'; skipping on-disk corruption step." >&2
  echo "INFO: validating NODE_REPORTED=ERROR with config.valid=true (remote-safe path)." >&2
  T10_MODE="remote_safe"
else
  printf '{invalid_json: true' >"$tmpdir/invalid_config.json"
  as_root_local install -m 0600 "$tmpdir/invalid_config.json" "$error_config_path"
fi

if [[ "$T10_MODE" == "full" ]]; then
  echo "Step 12/13: validate RUNNING + NODE_REPORTED=ERROR + config.valid=false"
  wait_node_status "$NODE_ERROR" "RUNNING" "false" "NODE_REPORTED" "ERROR" "$status_error_body"
else
  echo "Step 12/13: validate RUNNING + NODE_REPORTED=ERROR + config.valid=true (remote-safe)"
  wait_node_status "$NODE_ERROR" "RUNNING" "true" "NODE_REPORTED" "ERROR" "$status_error_body"
fi

echo "Step 13/13: cleanup + summary"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_REPORTED" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FALLBACK" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_ERROR" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime_reported=$RUNTIME_REPORTED@$RUNTIME_VERSION"
echo "runtime_fallback=$RUNTIME_FALLBACK@$RUNTIME_VERSION"
echo "runtime_error=$RUNTIME_ERROR@$RUNTIME_VERSION"
echo "node_reported=$NODE_REPORTED@$HIVE_ID"
echo "node_fallback=$NODE_FALLBACK@$HIVE_ID"
echo "node_error=$NODE_ERROR@$HIVE_ID"
echo "t10_mode=$T10_MODE"
if [[ -n "$TENANT_ID" ]]; then
  echo "tenant_id=$TENANT_ID"
fi
echo "node status FR7 T7/T8/T9/T10 E2E passed."
