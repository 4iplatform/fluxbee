#!/usr/bin/env bash
set -euo pipefail

# PUB-T22 E2E (canonical publish path, full_runtime):
# publish via bundle_upload -> sync/update -> spawn -> running(active)
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/runtime_packaging_pub_t22_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
DEPLOY_STRICT="${DEPLOY_STRICT:-1}"
TEST_ID="${TEST_ID:-pubt22-$(date +%s)-$RANDOM}"
RUNTIME_NAME="${RUNTIME_NAME:-wf.publish.full.diag.$(date +%s)}"
RUNTIME_VERSION="${RUNTIME_VERSION:-1.0.0-$TEST_ID}"
NODE_NAME="${NODE_NAME:-WF.publish.full.$TEST_ID}"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$RUNTIME_NAME"
DIAG_BIN="$ROOT_DIR/target/release/inventory_hold_diag"
BLOB_ROOT="${BLOB_ROOT:-/var/lib/fluxbee/blob}"
BLOB_REL_BASE="${BLOB_REL_BASE:-packages/incoming}"

tmpdir="$(mktemp -d)"
pkg_dir="$tmpdir/pkg"
manifest_backup="$tmpdir/manifest.backup.json"
publish_body="$tmpdir/publish.json"
bundle_zip="$tmpdir/runtime_bundle.zip"
bundle_blob_rel="$BLOB_REL_BASE/$RUNTIME_NAME-$RUNTIME_VERSION.zip"
bundle_blob_abs="$BLOB_ROOT/$bundle_blob_rel"
versions_body="$tmpdir/versions.json"
spawn_body="$tmpdir/spawn.json"
status_body="$tmpdir/status.json"
kill_body="$tmpdir/kill.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -f "$bundle_blob_abs" >/dev/null 2>&1 || true
  as_root_local rm -rf "$DIST_RUNTIME_DIR" >/dev/null 2>&1 || true
  as_root_local rm -rf "$tmpdir" >/dev/null 2>&1 || true
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

wait_runtime_ready() {
  local deadline=$(( $(date +%s) + WAIT_READY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http present exec
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    present="$(jq -r --arg rt "$RUNTIME_NAME" --arg ver "$RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present // false) | tostring' \
      "$versions_body")"
    exec="$(jq -r --arg rt "$RUNTIME_NAME" --arg ver "$RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable // false) | tostring' \
      "$versions_body")"
    if [[ "$present" == "true" && "$exec" == "true" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: runtime not ready on hive '$HIVE_ID' runtime='$RUNTIME_NAME' version='$RUNTIME_VERSION'" >&2
  cat "$versions_body" >&2 || true
  return 1
}

spawn_node() {
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' \
    "$NODE_NAME" "$RUNTIME_NAME" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$payload"
}

wait_node_active() {
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http api_status payload_status lifecycle pid
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" "$status_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    api_status="$(json_get_file "status" "$status_body")"
    payload_status="$(json_get_file "payload.status" "$status_body")"
    lifecycle="$(json_get_file "payload.node_status.lifecycle_state" "$status_body")"
    pid="$(json_get_file "payload.node_status.process.pid" "$status_body")"
    if [[ "$api_status" != "ok" || "$payload_status" != "ok" ]]; then
      sleep 2
      continue
    fi
    if [[ "$lifecycle" == "RUNNING" ]]; then
      echo "$lifecycle"
      return 0
    fi
    if [[ "$lifecycle" == "STARTING" && -n "$pid" && "$pid" != "null" ]]; then
      echo "$lifecycle"
      return 0
    fi
    sleep 2
  done
  echo "FAIL: node did not reach RUNNING/STARTING(active) node='$NODE_NAME'" >&2
  cat "$status_body" >&2 || true
  return 1
}

create_package_fixture() {
  mkdir -p "$pkg_dir/bin" "$pkg_dir/config"
  cat >"$pkg_dir/package.json" <<EOF
{
  "name": "$RUNTIME_NAME",
  "version": "$RUNTIME_VERSION",
  "type": "full_runtime",
  "description": "PUB-T22 full runtime fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$pkg_dir/bin/start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="${INVENTORY_HOLD_NODE_NAME:-${NODE_NAME:-WF.publish.full}}"
export INVENTORY_HOLD_NODE_VERSION="${INVENTORY_HOLD_NODE_VERSION:-0.0.1}"
export INVENTORY_HOLD_SECS="${INVENTORY_HOLD_SECS:-0}"
export NODE_STATUS_DEFAULT_HANDLER_ENABLED="1"
export NODE_STATUS_DEFAULT_HEALTH_STATE="HEALTHY"
exec "$(dirname "${BASH_SOURCE[0]}")/inventory_hold_diag"
EOF
  chmod 0755 "$pkg_dir/bin/start.sh"
  cat >"$pkg_dir/config/default-config.json" <<'EOF'
{
  "diag": {
    "enabled": true
  }
}
EOF
  as_root_local install -m 0755 "$DIAG_BIN" "$pkg_dir/bin/inventory_hold_diag"
}

create_zip_bundle() {
  local src_dir="$1"
  local out_zip="$2"
  local root_name="$3"
  python3 - "$src_dir" "$out_zip" "$root_name" <<'PY'
import os
import sys
import zipfile

src_dir, out_zip, root_name = sys.argv[1:4]
with zipfile.ZipFile(out_zip, "w", compression=zipfile.ZIP_DEFLATED) as zf:
    for dirpath, _, filenames in os.walk(src_dir):
        rel_dir = os.path.relpath(dirpath, src_dir)
        for filename in filenames:
            src_path = os.path.join(dirpath, filename)
            arc_rel = filename if rel_dir == "." else os.path.join(rel_dir, filename)
            arcname = os.path.join(root_name, arc_rel)
            zf.write(src_path, arcname)
PY
}

stage_bundle_in_blob() {
  local blob_dir
  blob_dir="$(dirname "$bundle_blob_abs")"
  as_root_local mkdir -p "$blob_dir"
  as_root_local install -m 0644 "$bundle_zip" "$bundle_blob_abs"
}

publish_bundle_runtime() {
  local out_body="$1"
  local payload
  payload="$(cat <<EOF
{"source":{"kind":"bundle_upload","blob_path":"$bundle_blob_rel"},"sync_to":["$HIVE_ID"],"update_to":["$HIVE_ID"]}
EOF
)"
  http_call "POST" "$BASE/admin/runtime-packages/publish" "$out_body" "$payload"
}

extract_and_validate_publish_summary() {
  local body_file="$1"
  local top_status sync_hint_status update_status update_error_code manifest_version
  top_status="$(json_get_file "status" "$body_file")"
  if [[ "$top_status" != "ok" && "$top_status" != "sync_pending" ]]; then
    echo "FAIL: publish request failed top_status='$top_status'" >&2
    cat "$body_file" >&2 || true
    exit 1
  fi
  sync_hint_status="$(json_get_file "payload.follow_up.sync_hint.0.response.status" "$body_file")"
  update_status="$(json_get_file "payload.follow_up.update.0.response.status" "$body_file")"
  update_error_code="$(json_get_file "payload.follow_up.update.0.response.error_code" "$body_file")"
  manifest_version="$(json_get_file "payload.manifest_version" "$body_file")"
  if [[ "$DEPLOY_STRICT" == "1" ]]; then
    if [[ "$sync_hint_status" == "error" ]]; then
      echo "FAIL: strict deploy requires sync_hint_status != error" >&2
      cat "$body_file" >&2 || true
      exit 1
    fi
    if [[ "$update_status" == "error" ]]; then
      echo "FAIL: strict deploy requires update_status != error (code='$update_error_code')" >&2
      cat "$body_file" >&2 || true
      exit 1
    fi
  fi
  if [[ ! "$manifest_version" =~ ^[0-9]+$ ]]; then
    echo "FAIL: invalid manifest_version='$manifest_version'" >&2
    cat "$body_file" >&2 || true
    exit 1
  fi
  echo "${sync_hint_status:-unknown}|${update_status:-unknown}|${update_error_code:-}|$manifest_version"
}

require_cmd curl
require_cmd jq
require_cmd python3
require_cmd cargo
validate_tenant_id "$TENANT_ID"

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "FAIL: runtime manifest missing at '$MANIFEST_PATH'" >&2
  exit 1
fi

echo "PUB-T22 full_runtime E2E: BASE=$BASE HIVE_ID=$HIVE_ID MOTHER_HIVE_ID=$MOTHER_HIVE_ID RUNTIME=$RUNTIME_NAME VERSION=$RUNTIME_VERSION NODE=$NODE_NAME"

echo "Step 1/10: build binaries (inventory_hold_diag)"
(cd "$ROOT_DIR" && cargo build --release --bin inventory_hold_diag >/dev/null)
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/10: backup manifest + create full_runtime package fixture"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_package_fixture

echo "Step 3/10: build bundle zip and stage it in blob root"
create_zip_bundle "$pkg_dir" "$bundle_zip" "$RUNTIME_NAME"
stage_bundle_in_blob

echo "Step 4/10: publish full_runtime through SY.admin bundle_upload"
publish_http="$(publish_bundle_runtime "$publish_body")"
if [[ "$publish_http" != "200" && "$publish_http" != "202" ]]; then
  echo "FAIL: unexpected publish http=$publish_http" >&2
  cat "$publish_body" >&2 || true
  exit 1
fi

echo "Step 5/10: validate publish summary"
deploy="$(extract_and_validate_publish_summary "$publish_body")"
sync_hint_status="${deploy%%|*}"
rest="${deploy#*|}"
update_status="${rest%%|*}"
rest="${rest#*|}"
update_error_code="${rest%%|*}"
manifest_version="${rest#*|}"

echo "Step 6/10: wait readiness=true on target hive versions"
wait_runtime_ready

echo "Step 7/10: spawn node with published runtime"
spawn_http="$(spawn_node)"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi

echo "Step 8/10: wait node lifecycle RUNNING (or STARTING active)"
lifecycle_observed="$(wait_node_active)"

echo "Step 9/10: cleanup node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null || true

echo "Step 10/10: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME_NAME@$RUNTIME_VERSION"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "sync_hint_status=$sync_hint_status"
echo "update_status=$update_status"
echo "update_error_code=$update_error_code"
echo "manifest_version=$manifest_version"
echo "lifecycle_observed=$lifecycle_observed"
echo "runtime packaging PUB-T22 full_runtime E2E passed."
