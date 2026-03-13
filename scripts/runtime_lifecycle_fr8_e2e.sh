#!/usr/bin/env bash
set -euo pipefail

# FR-08 E2E (T11 + T12):
# - T11: runtime en manifest sin start.sh -> readiness=false + spawn RUNTIME_NOT_PRESENT
# - T12: crear start.sh + sync-hint + update -> readiness=true + spawn ok
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/runtime_lifecycle_fr8_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
RUNTIME_NAME="${RUNTIME_NAME:-wf.runtime.lifecycle.fr8.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-fr8-$(date +%s)-$RANDOM}"
NODE_NAME_A="${NODE_NAME_A:-WF.fr8.t11.$(date +%s)}"
NODE_NAME_B="${NODE_NAME_B:-WF.fr8.t12.$(date +%s)}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-90}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-90}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$RUNTIME_NAME/$RUNTIME_VERSION"
START_SCRIPT="$RUNTIME_DIR/bin/start.sh"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_a_body="$tmpdir/spawn_a.json"
spawn_b_body="$tmpdir/spawn_b.json"
kill_body="$tmpdir/kill.json"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"
manifest_backup="$tmpdir/manifest.backup.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME_A@$HIVE_ID" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME_B@$HIVE_ID" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -rf "$RUNTIME_DIR" >/dev/null 2>&1 || true
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
    if ! command -v sudo >/dev/null 2>&1; then
      echo "FAIL: sudo is required when not running as root" >&2
      exit 1
    fi
    sudo -n "$@"
  fi
}

validate_tenant_id() {
  local tenant_id="$1"
  if [[ -z "$tenant_id" ]]; then
    echo "FAIL: TENANT_ID is required (format: tnt:<uuid-v4>)" >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected tnt:<uuid-v4>)" >&2
    exit 1
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

manifest_version_and_hash() {
  local out_file="$1"
  as_root_local python3 - "$MANIFEST_PATH" "$out_file" <<'PY'
import json
import hashlib
import sys

manifest = sys.argv[1]
out = sys.argv[2]
with open(manifest, "rb") as f:
    raw = f.read()
doc = json.loads(raw.decode("utf-8"))
version = int(doc.get("version", 0) or 0)
sha = hashlib.sha256(raw).hexdigest()
with open(out, "w", encoding="utf-8") as f:
    f.write(f"{version}\n{sha}\n")
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
    local status
    post_sync_hint "$out_file" >/dev/null
    status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$status" == "ok" ]]; then
      return 0
    fi
    if [[ "$status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: sync-hint status='$status'" >&2
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

wait_update_ok_or_sync_pending() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local out_file="$3"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))
  while (( $(date +%s) <= deadline )); do
    local status
    post_runtime_update "$manifest_version" "$manifest_hash" "$out_file" >/dev/null
    status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$status" == "ok" || "$status" == "sync_pending" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: update runtime timeout waiting ok|sync_pending" >&2
  cat "$out_file" >&2 || true
  return 1
}

set_manifest_runtime_current() {
  local runtime="$1"
  local version="$2"
  as_root_local python3 - "$MANIFEST_PATH" "$runtime" "$version" <<'PY'
import json
import sys
import time

manifest_path = sys.argv[1]
runtime = sys.argv[2]
version = sys.argv[3]

with open(manifest_path, "r", encoding="utf-8") as f:
    doc = json.load(f)

runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes
entry = runtimes.get(runtime)
if not isinstance(entry, dict):
    entry = {}
available = entry.get("available")
if not isinstance(available, list):
    available = []
available = [str(v).strip() for v in available if str(v).strip()]
available = [v for v in available if v != "current"]
if version not in available:
    available.append(version)
entry["available"] = sorted(set(available))
entry["current"] = version
runtimes[runtime] = entry

doc["version"] = int(time.time() * 1000)
doc["updated_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())

with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(doc, f, indent=2, sort_keys=True)
PY
}

assert_readiness_value() {
  local runtime="$1"
  local version="$2"
  local expected_present="$3"
  local expected_exec="$4"
  local file="$5"
  local present exec
  present="$(jq -r --arg runtime "$runtime" --arg version "$version" \
    '.payload.hive.runtimes.runtimes[$runtime].readiness[$version].runtime_present // empty' \
    "$file")"
  exec="$(jq -r --arg runtime "$runtime" --arg version "$version" \
    '.payload.hive.runtimes.runtimes[$runtime].readiness[$version].start_sh_executable // empty' \
    "$file")"
  if [[ "$present" != "$expected_present" || "$exec" != "$expected_exec" ]]; then
    echo "FAIL: readiness mismatch runtime=$runtime version=$version expected=($expected_present,$expected_exec) got=($present,$exec)" >&2
    cat "$file" >&2 || true
    exit 1
  fi
}

spawn_node() {
  local node_name="$1"
  local out_file="$2"
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' \
    "$node_name" "$RUNTIME_NAME" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

kill_node_if_exists() {
  local node_name="$1"
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$node_name@$HIVE_ID" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
}

require_cmd curl
require_cmd jq
require_cmd python3
validate_tenant_id "$TENANT_ID"

echo "FR8 T11/T12 E2E: BASE=$BASE HIVE_ID=$HIVE_ID RUNTIME=$RUNTIME_NAME VERSION=$RUNTIME_VERSION"

echo "Step 1/10: backup runtime manifest"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"

echo "Step 2/10: prepare manifest entry pointing current to test version (without start.sh)"
as_root_local rm -rf "$RUNTIME_DIR"
set_manifest_runtime_current "$RUNTIME_NAME" "$RUNTIME_VERSION"

meta_file="$tmpdir/meta.txt"
manifest_version_and_hash "$meta_file"
mapfile -t meta < "$meta_file"
manifest_version="${meta[0]}"
manifest_hash="${meta[1]}"

echo "Step 3/10: sync-hint + update after manifest change (expect sync_pending or ok)"
wait_sync_hint_ok "$sync_hint_body"
wait_update_ok_or_sync_pending "$manifest_version" "$manifest_hash" "$update_body"

echo "Step 4/10: T11 readiness must be false/false for test runtime current"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
assert_readiness_value "$RUNTIME_NAME" "$RUNTIME_VERSION" "false" "false" "$versions_body"

echo "Step 5/10: T11 spawn must fail with RUNTIME_NOT_PRESENT"
kill_node_if_exists "$NODE_NAME_A"
spawn_http_a="$(spawn_node "$NODE_NAME_A" "$spawn_a_body")"
status_a="$(json_get_file "status" "$spawn_a_body")"
code_a="$(json_get_file "error_code" "$spawn_a_body")"
if [[ "$spawn_http_a" != "500" || "$status_a" != "error" || "$code_a" != "RUNTIME_NOT_PRESENT" ]]; then
  echo "FAIL: expected spawn failure RUNTIME_NOT_PRESENT (http=500), got http=$spawn_http_a status=$status_a code=$code_a" >&2
  cat "$spawn_a_body" >&2 || true
  exit 1
fi

echo "Step 6/10: materialize runtime start.sh and mark executable"
as_root_local mkdir -p "$RUNTIME_DIR/bin"
start_tmp="$tmpdir/start.sh"
cat >"$start_tmp" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
exec /bin/sleep 3600
EOF
as_root_local install -m 0755 "$start_tmp" "$START_SCRIPT"

echo "Step 7/10: sync-hint + update after artifact materialization"
wait_sync_hint_ok "$sync_hint_body"
wait_update_ok_or_sync_pending "$manifest_version" "$manifest_hash" "$update_body"

echo "Step 8/10: T12 readiness must be true/true for test runtime current"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
assert_readiness_value "$RUNTIME_NAME" "$RUNTIME_VERSION" "true" "true" "$versions_body"

echo "Step 9/10: T12 spawn must succeed"
kill_node_if_exists "$NODE_NAME_B"
spawn_http_b="$(spawn_node "$NODE_NAME_B" "$spawn_b_body")"
status_b="$(json_get_file "status" "$spawn_b_body")"
if [[ "$spawn_http_b" != "200" || "$status_b" != "ok" ]]; then
  echo "FAIL: expected spawn success after materialization, got http=$spawn_http_b status=$status_b" >&2
  cat "$spawn_b_body" >&2 || true
  exit 1
fi

echo "Step 10/10: cleanup spawned node + summary"
kill_node_if_exists "$NODE_NAME_B"

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME_NAME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "node_t11=$NODE_NAME_A@$HIVE_ID"
echo "node_t12=$NODE_NAME_B@$HIVE_ID"
echo "runtime lifecycle FR8 T11/T12 E2E passed."
