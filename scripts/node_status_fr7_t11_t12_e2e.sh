#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
BUILD_BIN="${BUILD_BIN:-1}"
TENANT_ID="${TENANT_ID:-}"
TEST_ID="${TEST_ID:-fr7t1112-$(date +%s)-${RANDOM}}"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-fr7-t1112-${TEST_ID}}"
RUNTIME_STABLE="${RUNTIME_STABLE:-wf.node.status.stable.diag}"
RUNTIME_CRASH="${RUNTIME_CRASH:-wf.node.status.crash.diag}"
NODE_FAILED="${NODE_FAILED:-WF.status.failed.${TEST_ID}}"
NODE_MONO="${NODE_MONO:-WF.status.monotonic.${TEST_ID}}"
STATUS_TIMEOUT_SECS="${STATUS_TIMEOUT_SECS:-45}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"
WAIT_ORCH_RESTART_SECS="${WAIT_ORCH_RESTART_SECS:-90}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${NODE_STATUS_DIAG_BIN_PATH:-$ROOT_DIR/target/release/inventory_hold_diag}"
DIST_RUNTIMES_ROOT="/var/lib/fluxbee/dist/runtimes"
MANIFEST_PATH="$DIST_RUNTIMES_ROOT/manifest.json"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_failed_body="$tmpdir/spawn_failed.json"
spawn_mono_body="$tmpdir/spawn_mono.json"
kill_body="$tmpdir/kill.json"
status_failed_body="$tmpdir/status_failed.json"
status_mono_before_body="$tmpdir/status_mono_before.json"
status_mono_after_body="$tmpdir/status_mono_after.json"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"
local_hive_body="$tmpdir/local_hive.json"
hive_status_body="$tmpdir/hive_status.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FAILED" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_MONO" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
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

runtime_current_ready_in_versions() {
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

entry = (
    doc.get("payload", {})
       .get("hive", {})
       .get("runtimes", {})
       .get("runtimes", {})
       .get(runtime)
)
if not isinstance(entry, dict):
    raise SystemExit(1)
current = entry.get("current")
readiness = entry.get("readiness", {})
if not isinstance(current, str) or not isinstance(readiness, dict):
    raise SystemExit(1)
status = readiness.get(current)
if not isinstance(status, dict):
    raise SystemExit(1)
runtime_present = bool(status.get("runtime_present"))
start_sh_executable = bool(status.get("start_sh_executable"))
raise SystemExit(0 if (runtime_present and start_sh_executable) else 1)
PY
}

wait_update_or_runtime_ready() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local runtime="$3"
  local update_out_file="$4"
  local versions_out_file="$5"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))

  while (( $(date +%s) <= deadline )); do
    local payload_status error_code error_detail versions_http
    post_runtime_update "$manifest_version" "$manifest_hash" "$update_out_file" >/dev/null
    payload_status="$(json_get_file "payload.status" "$update_out_file")"
    if [[ "$payload_status" == "ok" ]]; then
      UPDATE_GATE_RESULT="ok"
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_out_file")"
      if [[ "$versions_http" == "200" ]] && runtime_current_ready_in_versions "$runtime" "$versions_out_file"; then
        UPDATE_GATE_RESULT="runtime_ready_only"
        echo "WARN: update stayed sync_pending due unrelated runtime artifacts; continuing because '$runtime' current is ready in /versions." >&2
        return 0
      fi
      sleep 2
      continue
    fi
    error_code="$(json_get_file "error_code" "$update_out_file")"
    error_detail="$(json_get_file "error_detail" "$update_out_file")"
    if [[ "$error_code" == "TRANSPORT_ERROR" && "$error_detail" == *"UNREACHABLE"* ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: update runtime status='$payload_status'" >&2
    cat "$update_out_file" >&2 || true
    return 1
  done
  echo "FAIL: update runtime timeout waiting ok/runtime-ready gate" >&2
  cat "$update_out_file" >&2 || true
  return 1
}

write_runtime_start_sh() {
  local start_path="$1"
  local node_name="$2"
  cat >"$start_path" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="$node_name"
export INVENTORY_HOLD_NODE_VERSION="0.0.1"
export INVENTORY_HOLD_SECS="0"
export NODE_STATUS_DEFAULT_HANDLER_ENABLED="1"
export NODE_STATUS_DEFAULT_HEALTH_STATE="HEALTHY"
exec "\$(dirname "\${BASH_SOURCE[0]}")/inventory_hold_diag"
EOF
}

wait_node_status() {
  local node_name="$1"
  local expected_lifecycle="$2"
  local expected_source="$3"
  local expected_health="$4"
  local out_file="$5"
  local deadline=$(( $(date +%s) + STATUS_TIMEOUT_SECS ))

  while (( $(date +%s) <= deadline )); do
    local http api_status payload_status lifecycle source health
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
    if [[ "$api_status" != "ok" || "$payload_status" != "ok" ]]; then
      sleep 1
      continue
    fi
    if [[ "$lifecycle" == "$expected_lifecycle" && "$source" == "$expected_source" && "$health" == "$expected_health" ]]; then
      return 0
    fi
    sleep 1
  done
  echo "FAIL: node status mismatch node='$node_name' expected_lifecycle='$expected_lifecycle' expected_source='$expected_source' expected_health='$expected_health'" >&2
  cat "$out_file" >&2 || true
  return 1
}

wait_hive_status_ok() {
  local deadline=$(( $(date +%s) + WAIT_ORCH_RESTART_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http status
    http="$(http_call "GET" "$BASE/hive/status" "$hive_status_body")"
    if [[ "$http" != "200" ]]; then
      sleep 1
      continue
    fi
    status="$(json_get_file "status" "$hive_status_body")"
    if [[ "$status" == "ok" ]]; then
      return 0
    fi
    sleep 1
  done
  echo "FAIL: /hive/status did not return ok after orchestrator restart" >&2
  cat "$hive_status_body" >&2 || true
  return 1
}

spawn_node() {
  local node_name="$1"
  local body_file="$2"
  local payload
  if [[ -n "$TENANT_ID" ]]; then
    payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' "$node_name" "$RUNTIME_STABLE" "$TENANT_ID")"
  else
    payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$node_name" "$RUNTIME_STABLE")"
  fi
  local http
  http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$body_file" "$payload")"
  if [[ "$http" != "200" || "$(json_get_file "status" "$body_file")" != "ok" ]]; then
    echo "FAIL: spawn failed node='$node_name' http=$http" >&2
    cat "$body_file" >&2 || true
    return 1
  fi
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
if [[ "$HIVE_ID" != "$LOCAL_HIVE_ID" ]]; then
  echo "FAIL: FR7 T11/T12 requires local hive execution (HIVE_ID='$HIVE_ID', local='$LOCAL_HIVE_ID')" >&2
  exit 1
fi

echo "FR7 T11/T12 E2E: BASE=$BASE HIVE_ID=$HIVE_ID LOCAL_HIVE_ID=$LOCAL_HIVE_ID RUNTIME_STABLE=$RUNTIME_STABLE RUNTIME_CRASH=$RUNTIME_CRASH VERSION=$RUNTIME_VERSION"

echo "Step 1/12: build inventory_hold_diag (status-aware)"
if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  require_cmd cargo
  (cd "$ROOT_DIR" && cargo build --release --bin inventory_hold_diag)
fi
if [[ ! -x "$BIN_PATH" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at $BIN_PATH" >&2
  exit 1
fi

echo "Step 2/12: create runtime fixtures (stable + crash)"
stable_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_STABLE/$RUNTIME_VERSION/bin"
crash_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME_CRASH/$RUNTIME_VERSION/bin"
as_root_local mkdir -p "$stable_bin_dir" "$crash_bin_dir"
as_root_local install -m 0755 "$BIN_PATH" "$stable_bin_dir/inventory_hold_diag"
stable_start="$tmpdir/stable_start.sh"
write_runtime_start_sh "$stable_start" "$NODE_MONO"
as_root_local install -m 0755 "$stable_start" "$stable_bin_dir/start.sh"
cat >"$tmpdir/crash_start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exit 42
EOF
as_root_local install -m 0755 "$tmpdir/crash_start.sh" "$crash_bin_dir/start.sh"

echo "Step 3/12: update runtime manifest"
manifest_tmp="$tmpdir/manifest.json"
as_root_local python3 - "$MANIFEST_PATH" "$RUNTIME_STABLE" "$RUNTIME_CRASH" "$RUNTIME_VERSION" >"$manifest_tmp" <<'PY'
import json
import pathlib
import sys
import time

manifest_path = pathlib.Path(sys.argv[1])
runtime_name = sys.argv[2]
crash_runtime_name = sys.argv[3]
runtime_version = sys.argv[4]

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

for name in (runtime_name, crash_runtime_name):
    entry = runtimes.get(name)
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
    runtimes[name] = entry

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

echo "Step 4/12: SYSTEM_SYNC_HINT dist + SYSTEM_UPDATE runtime"
wait_sync_hint_ok "$sync_hint_body"
UPDATE_GATE_RESULT="unknown"
wait_update_or_runtime_ready "$manifest_version" "$manifest_hash" "$RUNTIME_STABLE" "$update_body" "$versions_body"

echo "Step 5/12: validate runtime presence in versions"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME_STABLE" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_STABLE' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_current_ready_in_versions "$RUNTIME_STABLE" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_STABLE' current version is not ready (runtime_present/start_sh_executable)" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME_CRASH" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_CRASH' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_current_ready_in_versions "$RUNTIME_CRASH" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME_CRASH' current version is not ready (runtime_present/start_sh_executable)" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 6/12: spawn crash node and validate FAILED/UNKNOWN (T11)"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FAILED" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
if [[ -n "$TENANT_ID" ]]; then
  failed_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' "$NODE_FAILED" "$RUNTIME_CRASH" "$TENANT_ID")"
else
  failed_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$NODE_FAILED" "$RUNTIME_CRASH")"
fi
failed_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_failed_body" "$failed_payload")"
if [[ "$failed_http" != "200" || "$(json_get_file "status" "$spawn_failed_body")" != "ok" ]]; then
  echo "FAIL: spawn failed node='$NODE_FAILED' http=$failed_http" >&2
  cat "$spawn_failed_body" >&2 || true
  exit 1
fi
failed_unit="$(json_get_file "payload.unit" "$spawn_failed_body")"
if [[ -z "$failed_unit" ]]; then
  echo "FAIL: missing payload.unit for failed-node scenario" >&2
  cat "$spawn_failed_body" >&2 || true
  exit 1
fi
as_root_local systemctl stop "$failed_unit" >/dev/null 2>&1 || true
as_root_local systemctl reset-failed "$failed_unit" >/dev/null 2>&1 || true
if ! as_root_local systemd-run --replace --unit "$failed_unit" --property Restart=no --property RestartSec=0 /bin/false >/dev/null 2>&1; then
  echo "FAIL: unable to inject failed unit run for '$failed_unit'" >&2
  exit 1
fi
wait_node_status "$NODE_FAILED" "FAILED" "UNKNOWN" "UNKNOWN" "$status_failed_body"

echo "Step 7/12: spawn healthy node for status_version monotonic check (T12)"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_MONO" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
spawn_node "$NODE_MONO" "$spawn_mono_body"
wait_node_status "$NODE_MONO" "RUNNING" "NODE_REPORTED" "HEALTHY" "$status_mono_before_body"
status_version_before="$(json_get_file "payload.node_status.status_version" "$status_mono_before_body")"
if [[ -z "$status_version_before" || ! "$status_version_before" =~ ^[0-9]+$ ]]; then
  echo "FAIL: invalid status_version before orchestrator restart ('$status_version_before')" >&2
  cat "$status_mono_before_body" >&2 || true
  exit 1
fi

echo "Step 8/12: restart sy-orchestrator"
as_root_local systemctl restart sy-orchestrator

echo "Step 9/12: wait control-plane healthy after restart"
wait_hive_status_ok

echo "Step 10/12: read node status after orchestrator restart"
wait_node_status "$NODE_MONO" "RUNNING" "NODE_REPORTED" "HEALTHY" "$status_mono_after_body"
status_version_after="$(json_get_file "payload.node_status.status_version" "$status_mono_after_body")"
if [[ -z "$status_version_after" || ! "$status_version_after" =~ ^[0-9]+$ ]]; then
  echo "FAIL: invalid status_version after orchestrator restart ('$status_version_after')" >&2
  cat "$status_mono_after_body" >&2 || true
  exit 1
fi
if (( status_version_after < status_version_before )); then
  echo "FAIL: status_version regressed across orchestrator restart before=$status_version_before after=$status_version_after" >&2
  cat "$status_mono_before_body" >&2 || true
  cat "$status_mono_after_body" >&2 || true
  exit 1
fi

echo "Step 11/12: cleanup nodes"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_FAILED" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_MONO" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "Step 12/12: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime_stable=$RUNTIME_STABLE@$RUNTIME_VERSION"
echo "runtime_crash=$RUNTIME_CRASH@$RUNTIME_VERSION"
echo "update_gate_result=$UPDATE_GATE_RESULT"
echo "node_failed=$NODE_FAILED@$HIVE_ID"
echo "node_monotonic=$NODE_MONO@$HIVE_ID"
echo "status_version_before=$status_version_before"
echo "status_version_after=$status_version_after"
if [[ -n "$TENANT_ID" ]]; then
  echo "tenant_id=$TENANT_ID"
fi
echo "node status FR7 T11/T12 E2E passed."
