#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
BUILD_BIN="${BUILD_BIN:-1}"
RUNTIME="${RUNTIME:-wf.inventory.hold.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-$(date +%Y%m%d%H%M%S)}"
RUNTIME_NODE_VERSION="${RUNTIME_NODE_VERSION:-0.0.1}"
TEST_ID="${TEST_ID:-invd1-$(date +%s)-${RANDOM}}"
NODE_NAME="${NODE_NAME:-WF.inventory.spawnkill.${TEST_ID}}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-1}"
INVENTORY_APPEAR_TIMEOUT_SECS="${INVENTORY_APPEAR_TIMEOUT_SECS:-30}"
INVENTORY_DISAPPEAR_TIMEOUT_SECS="${INVENTORY_DISAPPEAR_TIMEOUT_SECS:-30}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${INVENTORY_HOLD_BIN_PATH:-$ROOT_DIR/target/release/inventory_hold_diag}"
DIST_RUNTIMES_ROOT="/var/lib/fluxbee/dist/runtimes"
MANIFEST_PATH="$DIST_RUNTIMES_ROOT/manifest.json"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
inventory_body="$tmpdir/inventory.json"
spawn_body="$tmpdir/spawn.json"
kill_body="$tmpdir/kill.json"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
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

inventory_node_status() {
  local file="$1"
  local node_l2="$2"
  python3 - "$file" "$node_l2" <<'PY'
import json
import sys

file_path = sys.argv[1]
node_l2 = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

payload = doc.get("payload", {})
nodes = payload.get("nodes", [])
if not isinstance(nodes, list):
    print("")
    raise SystemExit(0)

for node in nodes:
    if not isinstance(node, dict):
        continue
    if node.get("node_name") == node_l2:
        status = node.get("status")
        print("" if status is None else str(status))
        raise SystemExit(0)

print("")
PY
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

wait_for_inventory_state() {
  local node_l2="$1"
  local expected="$2" # present|absent
  local timeout_secs="$3"
  local started_at
  started_at="$(date +%s)"

  while true; do
    local now elapsed http status
    now="$(date +%s)"
    elapsed=$((now - started_at))
    if (( elapsed > timeout_secs )); then
      echo "FAIL: timeout waiting inventory state='$expected' for node '$node_l2'" >&2
      echo "---- spawn response ----" >&2
      cat "$spawn_body" >&2 || true
      echo "---- inventory response ----" >&2
      cat "$inventory_body" >&2 || true
      exit 1
    fi

    http="$(http_call "GET" "$BASE/inventory/$HIVE_ID" "$inventory_body")"
    if [[ "$http" != "200" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    if [[ "$(json_get_file "status" "$inventory_body")" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    if [[ "$(json_get_file "payload.status" "$inventory_body")" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi

    status="$(inventory_node_status "$inventory_body" "$node_l2")"
    if [[ "$expected" == "present" && -n "$status" ]]; then
      return 0
    fi
    if [[ "$expected" == "absent" && -z "$status" ]]; then
      return 0
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

require_cmd curl
require_cmd python3
require_cmd sha256sum
require_cmd sudo

echo "INVENTORY D1 E2E: BASE=$BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME"

echo "Step 1/10: build inventory_hold_diag"
if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  require_cmd cargo
  (cd "$ROOT_DIR" && cargo build --release --bin inventory_hold_diag)
fi
if [[ ! -x "$BIN_PATH" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at $BIN_PATH" >&2
  exit 1
fi

echo "Step 2/10: create runtime fixture in dist"
runtime_bin_dir="$DIST_RUNTIMES_ROOT/$RUNTIME/$RUNTIME_VERSION/bin"
as_root_local mkdir -p "$runtime_bin_dir"
as_root_local install -m 0755 "$BIN_PATH" "$runtime_bin_dir/inventory_hold_diag"
start_tmp="$tmpdir/start.sh"
cat >"$start_tmp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="__INVENTORY_NODE_NAME__"
export INVENTORY_HOLD_NODE_VERSION="__INVENTORY_NODE_VERSION__"
export INVENTORY_HOLD_SECS="0"
exec "$(dirname "${BASH_SOURCE[0]}")/inventory_hold_diag"
EOF
node_name_escaped="$(printf '%s' "$NODE_NAME" | sed 's/[\/&]/\\&/g')"
node_version_escaped="$(printf '%s' "$RUNTIME_NODE_VERSION" | sed 's/[\/&]/\\&/g')"
sed -i'' -e "s/__INVENTORY_NODE_NAME__/${node_name_escaped}/g" "$start_tmp"
sed -i'' -e "s/__INVENTORY_NODE_VERSION__/${node_version_escaped}/g" "$start_tmp"
as_root_local install -m 0755 "$start_tmp" "$runtime_bin_dir/start.sh"

echo "Step 3/10: update runtime manifest"
manifest_tmp="$tmpdir/manifest.json"
as_root_local python3 - "$MANIFEST_PATH" "$RUNTIME" "$RUNTIME_VERSION" >"$manifest_tmp" <<'PY'
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

echo "Step 4/10: wait versions endpoint reachable"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: /hives/$HIVE_ID/versions returned HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 5/10: SYSTEM_SYNC_HINT dist + SYSTEM_UPDATE runtime"
wait_sync_hint_ok "$sync_hint_body"
wait_update_ok "$manifest_version" "$manifest_hash" "$update_body"

echo "Step 6/10: validate runtime exists in /hives/$HIVE_ID/versions"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 7/10: cleanup baseline node (ignore errors)"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "Step 8/10: spawn node and wait until appears in inventory"
spawn_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$NODE_NAME" "$RUNTIME")")"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi
wait_for_inventory_state "$NODE_NAME@$HIVE_ID" "present" "$INVENTORY_APPEAR_TIMEOUT_SECS"

echo "Step 9/10: kill node"
kill_http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}')"
kill_status="$(json_get_file "status" "$kill_body")"
if [[ "$kill_http" != "200" || "$kill_status" != "ok" ]]; then
  echo "FAIL: kill failed http=$kill_http status=$kill_status" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi

echo "Step 10/10: wait until killed node disappears from inventory"
wait_for_inventory_state "$NODE_NAME@$HIVE_ID" "absent" "$INVENTORY_DISAPPEAR_TIMEOUT_SECS"

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "inventory spawn/kill E2E passed."
