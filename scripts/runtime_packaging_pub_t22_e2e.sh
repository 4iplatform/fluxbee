#!/usr/bin/env bash
set -euo pipefail

# PUB-T22 E2E (full_runtime):
# publish -> deploy -> spawn -> running(active)
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
TEST_ID="${TEST_ID:-pubt22-$(date +%s)-$RANDOM}"
RUNTIME_NAME="${RUNTIME_NAME:-wf.publish.full.diag.$(date +%s)}"
RUNTIME_VERSION="${RUNTIME_VERSION:-1.0.0-$TEST_ID}"
NODE_NAME="${NODE_NAME:-WF.publish.full.$TEST_ID}"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$RUNTIME_NAME"
PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"
DIAG_BIN="$ROOT_DIR/target/release/inventory_hold_diag"

tmpdir="$(mktemp -d)"
pkg_dir="$tmpdir/pkg"
manifest_backup="$tmpdir/manifest.backup.json"
publish_log="$tmpdir/publish.log"
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
  "version": "0.0.1",
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

run_publish() {
  as_root_local env \
    FLUXBEE_PUBLISH_BASE="$BASE" \
    FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE_ID" \
    "$PUBLISH_BIN" "$pkg_dir" --version "$RUNTIME_VERSION" --deploy "$HIVE_ID" \
    | tee "$publish_log"
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

echo "Step 1/9: build binaries (fluxbee-publish + inventory_hold_diag)"
(cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish --bin inventory_hold_diag >/dev/null)
if [[ ! -x "$PUBLISH_BIN" ]]; then
  echo "FAIL: publish binary missing at '$PUBLISH_BIN'" >&2
  exit 1
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/9: backup manifest + create full_runtime package fixture"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_package_fixture

echo "Step 3/9: publish package with --version + --deploy"
run_publish

echo "Step 4/9: validate deploy output summary"
if ! grep -q "sync_hint_status=" "$publish_log"; then
  echo "FAIL: publish output missing deploy summary" >&2
  cat "$publish_log" >&2 || true
  exit 1
fi
deploy_line="$(grep -E 'sync_hint_status=' "$publish_log" | tail -n1)"
sync_hint_status="$(echo "$deploy_line" | sed -n 's/.*sync_hint_status=\([^ ]*\).*/\1/p')"
update_status="$(echo "$deploy_line" | sed -n 's/.*update_status=\([^ ]*\).*/\1/p')"
update_error_code="$(echo "$deploy_line" | sed -n 's/.*update_error_code=\(.*\)$/\1/p')"

echo "Step 5/9: wait readiness=true on target hive versions"
wait_runtime_ready

echo "Step 6/9: spawn node with published runtime"
spawn_http="$(spawn_node)"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi

echo "Step 7/9: wait node lifecycle RUNNING (or STARTING active)"
lifecycle_observed="$(wait_node_active)"

echo "Step 8/9: cleanup node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null || true

echo "Step 9/9: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME_NAME@$RUNTIME_VERSION"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "sync_hint_status=$sync_hint_status"
echo "update_status=$update_status"
echo "update_error_code=$update_error_code"
echo "lifecycle_observed=$lifecycle_observed"
echo "runtime packaging PUB-T22 full_runtime E2E passed."
