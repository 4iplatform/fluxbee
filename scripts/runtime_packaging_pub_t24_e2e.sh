#!/usr/bin/env bash
set -euo pipefail

# PUB-T24 E2E (workflow):
# 1) publish workflow package (runtime_base=wf.engine by default)
# 2) deploy to target hive
# 3) spawn node and verify runtime + _system wiring
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/runtime_packaging_pub_t24_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
DEPLOY_STRICT="${DEPLOY_STRICT:-1}"
TEST_ID="${TEST_ID:-pubt24-$(date +%s)-$RANDOM}"
WORKFLOW_RUNTIME_NAME="${WORKFLOW_RUNTIME_NAME:-wf.publish.workflow.diag.$(date +%s)}"
WORKFLOW_RUNTIME_VERSION="${WORKFLOW_RUNTIME_VERSION:-1.0.0-$TEST_ID}"
RUNTIME_BASE="${RUNTIME_BASE:-wf.engine}"
NODE_NAME="${NODE_NAME:-WF.publish.workflow.$TEST_ID}"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_WORKFLOW_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$WORKFLOW_RUNTIME_NAME"
PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"

tmpdir="$(mktemp -d)"
workflow_pkg_dir="$tmpdir/workflow_pkg"
manifest_backup="$tmpdir/manifest.backup.json"
publish_log="$tmpdir/publish_workflow.log"
versions_body="$tmpdir/versions.json"
spawn_body="$tmpdir/spawn.json"
status_body="$tmpdir/status.json"
config_body="$tmpdir/config.json"
kill_body="$tmpdir/kill.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -rf "$DIST_WORKFLOW_RUNTIME_DIR" >/dev/null 2>&1 || true
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

assert_runtime_base_ready() {
  local deadline=$(( $(date +%s) + WAIT_READY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http runtime_exists base_current present exec
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    runtime_exists="$(jq -r --arg rt "$RUNTIME_BASE" \
      '(.payload.hive.runtimes.runtimes | has($rt)) // false | tostring' \
      "$versions_body")"
    if [[ "$runtime_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    base_current="$(jq -r --arg rt "$RUNTIME_BASE" \
      '(.payload.hive.runtimes.runtimes[$rt].current // "") | tostring' \
      "$versions_body")"
    if [[ -z "$base_current" ]]; then
      sleep 2
      continue
    fi
    present="$(jq -r --arg rt "$RUNTIME_BASE" --arg ver "$base_current" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present // false) | tostring' \
      "$versions_body")"
    exec="$(jq -r --arg rt "$RUNTIME_BASE" --arg ver "$base_current" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable // false) | tostring' \
      "$versions_body")"
    if [[ "$present" == "true" && "$exec" == "true" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: runtime_base '$RUNTIME_BASE' is not ready on hive '$HIVE_ID'" >&2
  echo "HINT: publish/deploy the base runtime first or override RUNTIME_BASE for this environment." >&2
  cat "$versions_body" >&2 || true
  return 1
}

wait_workflow_readiness() {
  local expected_manifest_version="${1:-0}"
  local deadline=$(( $(date +%s) + WAIT_READY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http manifest_version runtime_exists readiness_exists present exec base_ready has_base_ready
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    manifest_version="$(jq -r '.payload.hive.runtimes.manifest_version // 0 | tostring' "$versions_body")"
    if [[ "$expected_manifest_version" =~ ^[0-9]+$ && "$expected_manifest_version" -gt 0 ]]; then
      if [[ "$manifest_version" =~ ^[0-9]+$ ]]; then
        if (( manifest_version < expected_manifest_version )); then
          sleep 2
          continue
        fi
      fi
    fi
    runtime_exists="$(jq -r --arg rt "$WORKFLOW_RUNTIME_NAME" \
      '(.payload.hive.runtimes.runtimes | has($rt)) // false | tostring' \
      "$versions_body")"
    if [[ "$runtime_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    readiness_exists="$(jq -r --arg rt "$WORKFLOW_RUNTIME_NAME" --arg ver "$WORKFLOW_RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness | has($ver)) // false | tostring' \
      "$versions_body")"
    if [[ "$readiness_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    has_base_ready="$(jq -r --arg rt "$WORKFLOW_RUNTIME_NAME" --arg ver "$WORKFLOW_RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver] | has("base_runtime_ready")) // false | tostring' \
      "$versions_body")"
    if [[ "$has_base_ready" != "true" ]]; then
      echo "FAIL: target hive '$HIVE_ID' runtime '$WORKFLOW_RUNTIME_NAME' readiness for version '$WORKFLOW_RUNTIME_VERSION' is present but missing base_runtime_ready." >&2
      echo "HINT: verify worker sy-orchestrator build has package-aware readiness (PUB-T10+)." >&2
      cat "$versions_body" >&2 || true
      return 1
    fi
    present="$(jq -r --arg rt "$WORKFLOW_RUNTIME_NAME" --arg ver "$WORKFLOW_RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present // false) | tostring' \
      "$versions_body")"
    exec="$(jq -r --arg rt "$WORKFLOW_RUNTIME_NAME" --arg ver "$WORKFLOW_RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable // false) | tostring' \
      "$versions_body")"
    base_ready="$(jq -r --arg rt "$WORKFLOW_RUNTIME_NAME" --arg ver "$WORKFLOW_RUNTIME_VERSION" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].base_runtime_ready // false) | tostring' \
      "$versions_body")"
    if [[ "$present" == "true" && "$exec" == "true" && "$base_ready" == "true" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: workflow readiness timeout runtime='$WORKFLOW_RUNTIME_NAME' version='$WORKFLOW_RUNTIME_VERSION' expected_manifest_version>=$expected_manifest_version" >&2
  cat "$versions_body" >&2 || true
  return 1
}

spawn_node() {
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current","tenant_id":"%s"}' \
    "$NODE_NAME" "$WORKFLOW_RUNTIME_NAME" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$payload"
}

wait_node_active_with_runtime() {
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http api_status payload_status lifecycle pid runtime_name runtime_version
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" "$status_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    api_status="$(json_get_file "status" "$status_body")"
    payload_status="$(json_get_file "payload.status" "$status_body")"
    lifecycle="$(json_get_file "payload.node_status.lifecycle_state" "$status_body")"
    pid="$(json_get_file "payload.node_status.process.pid" "$status_body")"
    runtime_name="$(json_get_file "payload.node_status.runtime.name" "$status_body")"
    runtime_version="$(json_get_file "payload.node_status.runtime.resolved_version" "$status_body")"
    if [[ "$api_status" != "ok" || "$payload_status" != "ok" ]]; then
      sleep 2
      continue
    fi
    if [[ "$runtime_name" != "$WORKFLOW_RUNTIME_NAME" || "$runtime_version" != "$WORKFLOW_RUNTIME_VERSION" ]]; then
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
  echo "FAIL: node did not reach active lifecycle with expected workflow runtime node='$NODE_NAME'" >&2
  cat "$status_body" >&2 || true
  return 1
}

assert_node_system_runtime_base() {
  local http api_status payload_status cfg_runtime_base cfg_package_path
  http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" "$config_body")"
  api_status="$(json_get_file "status" "$config_body")"
  payload_status="$(json_get_file "payload.status" "$config_body")"
  if [[ "$http" != "200" || "$api_status" != "ok" || "$payload_status" != "ok" ]]; then
    echo "FAIL: get config failed http=$http status=$api_status payload_status=$payload_status" >&2
    cat "$config_body" >&2 || true
    return 1
  fi
  cfg_runtime_base="$(json_get_file "payload.config._system.runtime_base" "$config_body")"
  cfg_package_path="$(json_get_file "payload.config._system.package_path" "$config_body")"
  if [[ "$cfg_runtime_base" != "$RUNTIME_BASE" ]]; then
    echo "FAIL: unexpected payload.config._system.runtime_base expected='$RUNTIME_BASE' got='$cfg_runtime_base'" >&2
    cat "$config_body" >&2 || true
    return 1
  fi
  if [[ -z "$cfg_package_path" ]]; then
    echo "FAIL: payload.config._system.package_path is empty" >&2
    cat "$config_body" >&2 || true
    return 1
  fi
  echo "$cfg_package_path"
}

create_workflow_package_fixture() {
  mkdir -p "$workflow_pkg_dir/config" "$workflow_pkg_dir/flow" "$workflow_pkg_dir/assets"
  cat >"$workflow_pkg_dir/package.json" <<EOF
{
  "name": "$WORKFLOW_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "workflow",
  "description": "PUB-T24 workflow fixture",
  "runtime_base": "$RUNTIME_BASE",
  "config_template": "config/default-config.json"
}
EOF
  cat >"$workflow_pkg_dir/config/default-config.json" <<'EOF'
{
  "diag": {
    "kind": "workflow",
    "from_template": true
  }
}
EOF
  cat >"$workflow_pkg_dir/flow/main.yaml" <<'EOF'
id: demo-workflow
version: 1
steps:
  - id: start
    type: noop
EOF
  cat >"$workflow_pkg_dir/assets/readme.txt" <<'EOF'
workflow fixture payload
EOF
}

run_publish() {
  local pkg_path="$1"
  local version="$2"
  local out_log="$3"
  as_root_local env \
    FLUXBEE_PUBLISH_BASE="$BASE" \
    FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE_ID" \
    "$PUBLISH_BIN" "$pkg_path" --version "$version" --deploy "$HIVE_ID" \
    | tee "$out_log"
}

extract_and_validate_deploy_summary() {
  local out_log="$1"
  if ! grep -q "sync_hint_status=" "$out_log"; then
    echo "FAIL: publish output missing deploy summary" >&2
    cat "$out_log" >&2 || true
    exit 1
  fi
  local deploy_line sync_hint_status update_status update_error_code manifest_version
  deploy_line="$(grep -E 'sync_hint_status=' "$out_log" | tail -n1)"
  sync_hint_status="$(echo "$deploy_line" | sed -n 's/.*sync_hint_status=\([^ ]*\).*/\1/p')"
  update_status="$(echo "$deploy_line" | sed -n 's/.*update_status=\([^ ]*\).*/\1/p')"
  update_error_code="$(echo "$deploy_line" | sed -n 's/.*update_error_code=\(.*\)$/\1/p')"
  manifest_version="$(sed -n 's/.*Manifest: .* (version=\([0-9][0-9]*\)).*/\1/p' "$out_log" | tail -n1)"
  if [[ -z "$manifest_version" ]]; then
    echo "FAIL: unable to parse manifest version from publish output" >&2
    cat "$out_log" >&2 || true
    exit 1
  fi
  if [[ "$DEPLOY_STRICT" == "1" ]]; then
    if [[ "$sync_hint_status" == "error" ]]; then
      echo "FAIL: strict deploy requires sync_hint_status != error" >&2
      cat "$out_log" >&2 || true
      exit 1
    fi
    if [[ "$update_status" == "error" ]]; then
      echo "FAIL: strict deploy requires update_status != error (code='$update_error_code')" >&2
      cat "$out_log" >&2 || true
      exit 1
    fi
  fi
  echo "$sync_hint_status|$update_status|$update_error_code|$manifest_version"
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

echo "PUB-T24 workflow E2E: BASE=$BASE HIVE_ID=$HIVE_ID MOTHER_HIVE_ID=$MOTHER_HIVE_ID RUNTIME_BASE=$RUNTIME_BASE WORKFLOW_RUNTIME=$WORKFLOW_RUNTIME_NAME@$WORKFLOW_RUNTIME_VERSION NODE=$NODE_NAME"

echo "Step 1/11: build binary (fluxbee-publish)"
(cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish >/dev/null)
if [[ ! -x "$PUBLISH_BIN" ]]; then
  echo "FAIL: publish binary missing at '$PUBLISH_BIN'" >&2
  exit 1
fi

echo "Step 2/11: validate runtime_base readiness on target hive"
assert_runtime_base_ready

echo "Step 3/11: backup manifest + create workflow package fixture"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_workflow_package_fixture

echo "Step 4/11: publish workflow package with deploy"
run_publish "$workflow_pkg_dir" "$WORKFLOW_RUNTIME_VERSION" "$publish_log"

echo "Step 5/11: validate deploy summary"
deploy_summary="$(extract_and_validate_deploy_summary "$publish_log")"
sync_hint_status="${deploy_summary%%|*}"
rest="${deploy_summary#*|}"
update_status="${rest%%|*}"
rest="${rest#*|}"
update_error_code="${rest%%|*}"
manifest_version="${rest#*|}"

echo "Step 6/11: wait workflow readiness (including base_runtime_ready)"
wait_workflow_readiness "$manifest_version"

echo "Step 7/11: spawn node with workflow runtime"
spawn_http="$(spawn_node)"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi

echo "Step 8/11: wait node lifecycle RUNNING (or STARTING active)"
lifecycle_observed="$(wait_node_active_with_runtime)"

echo "Step 9/11: verify _system.runtime_base and _system.package_path"
package_path_observed="$(assert_node_system_runtime_base)"

echo "Step 10/11: cleanup node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null || true

echo "Step 11/11: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime_base=$RUNTIME_BASE"
echo "workflow_runtime=$WORKFLOW_RUNTIME_NAME@$WORKFLOW_RUNTIME_VERSION"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "sync_hint_status=$sync_hint_status"
echo "update_status=$update_status"
echo "update_error_code=$update_error_code"
echo "manifest_version=$manifest_version"
echo "lifecycle_observed=$lifecycle_observed"
echo "package_path_observed=$package_path_observed"
echo "runtime packaging PUB-T24 workflow E2E passed."
