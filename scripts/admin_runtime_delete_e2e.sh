#!/usr/bin/env bash
set -euo pipefail

# Runtime delete REST E2E:
# 1) publish a full_runtime twice and verify:
#    - DELETE current -> RUNTIME_CURRENT_CONFLICT
#    - DELETE missing version -> RUNTIME_VERSION_NOT_FOUND
#    - DELETE old non-current version -> ok
# 2) publish an AI runtime twice, spawn version1, and verify:
#    - DELETE old running version -> RUNTIME_IN_USE
# 3) publish a base runtime twice + config_only dependent and verify:
#    - DELETE old base version -> RUNTIME_HAS_DEPENDENTS
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="motherbee" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid-v4>" \
#   bash scripts/admin_runtime_delete_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
TEST_TS="$(date +%s)"
TEST_ID="${TEST_ID:-admindel-${TEST_TS}-$RANDOM}"

DELETE_OK_RUNTIME_NAME="${DELETE_OK_RUNTIME_NAME:-wf.admin.delete.ok.diag.${TEST_TS}}"
DELETE_OK_V1="${DELETE_OK_V1:-1.0.0-${TEST_ID}}"
DELETE_OK_V2="${DELETE_OK_V2:-2.0.0-${TEST_ID}}"
DELETE_OK_MISSING_VERSION="${DELETE_OK_MISSING_VERSION:-9.9.9-${TEST_ID}}"

IN_USE_RUNTIME_NAME="${IN_USE_RUNTIME_NAME:-ai.admin.delete.inuse.diag.${TEST_TS}}"
IN_USE_V1="${IN_USE_V1:-1.0.0-${TEST_ID}}"
IN_USE_V2="${IN_USE_V2:-2.0.0-${TEST_ID}}"
IN_USE_NODE_LOCAL="${IN_USE_NODE_LOCAL:-AI.test.delete.${TEST_ID}}"
IN_USE_NODE_NAME="${IN_USE_NODE_NAME:-${IN_USE_NODE_LOCAL}@${HIVE_ID}}"

BASE_RUNTIME_NAME="${BASE_RUNTIME_NAME:-wf.admin.delete.base.diag.${TEST_TS}}"
BASE_V1="${BASE_V1:-1.0.0-${TEST_ID}}"
BASE_V2="${BASE_V2:-2.0.0-${TEST_ID}}"
DEPENDENT_RUNTIME_NAME="${DEPENDENT_RUNTIME_NAME:-wf.admin.delete.dep.diag.${TEST_TS}}"
DEPENDENT_VERSION="${DEPENDENT_VERSION:-1.0.0-${TEST_ID}}"

PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"
AI_BIN="$ROOT_DIR/target/release/ai-test-gov"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"
MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"

DIST_DELETE_OK_DIR="/var/lib/fluxbee/dist/runtimes/$DELETE_OK_RUNTIME_NAME"
DIST_IN_USE_DIR="/var/lib/fluxbee/dist/runtimes/$IN_USE_RUNTIME_NAME"
DIST_BASE_DIR="/var/lib/fluxbee/dist/runtimes/$BASE_RUNTIME_NAME"
DIST_DEPENDENT_DIR="/var/lib/fluxbee/dist/runtimes/$DEPENDENT_RUNTIME_NAME"

tmpdir="$(mktemp -d)"
delete_ok_pkg="$tmpdir/delete_ok_pkg"
in_use_pkg="$tmpdir/in_use_pkg"
base_pkg="$tmpdir/base_pkg"
dependent_pkg="$tmpdir/dependent_pkg"
manifest_backup="$tmpdir/manifest.backup.json"
versions_body="$tmpdir/versions.json"
runtime_body="$tmpdir/runtime.json"
status_body="$tmpdir/status.json"
spawn_body="$tmpdir/spawn.json"
kill_body="$tmpdir/kill.json"
delete_body="$tmpdir/delete.json"
publish_log="$tmpdir/publish.log"
socket_current_body="$tmpdir/socket_current.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$IN_USE_NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -rf \
    "$DIST_DELETE_OK_DIR" \
    "$DIST_IN_USE_DIR" \
    "$DIST_BASE_DIR" \
    "$DIST_DEPENDENT_DIR" >/dev/null 2>&1 || true
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

validate_tenant_id() {
  local tenant_id="${1:-}"
  if [[ -z "$tenant_id" ]]; then
    echo "FAIL: TENANT_ID is required (format: tnt:<uuid-v4>)" >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected tnt:<uuid-v4>)" >&2
    exit 1
  fi
}

run_publish() {
  local pkg_dir="$1"
  local runtime_version="$2"
  as_root_local env \
    FLUXBEE_PUBLISH_BASE="$BASE" \
    FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE_ID" \
    "$PUBLISH_BIN" "$pkg_dir" --version "$runtime_version" --deploy "$HIVE_ID" \
    | tee "$publish_log"
}

wait_runtime_ready() {
  local runtime_name="$1"
  local runtime_version="$2"
  local deadline=$(( $(date +%s) + WAIT_READY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http present exec
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    present="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present // false) | tostring' \
      "$versions_body")"
    exec="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable // false) | tostring' \
      "$versions_body")"
    if [[ "$present" == "true" && "$exec" == "true" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: runtime not ready runtime='$runtime_name' version='$runtime_version'" >&2
  cat "$versions_body" >&2 || true
  return 1
}

wait_node_running() {
  local node_name="$1"
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http lifecycle api_status payload_status
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$node_name/status" "$status_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    api_status="$(json_get_file "status" "$status_body")"
    payload_status="$(json_get_file "payload.status" "$status_body")"
    lifecycle="$(json_get_file "payload.node_status.lifecycle_state" "$status_body")"
    if [[ "$api_status" == "ok" && "$payload_status" == "ok" && "$lifecycle" == "RUNNING" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: node did not reach RUNNING node='$node_name'" >&2
  cat "$status_body" >&2 || true
  return 1
}

assert_delete_error() {
  local runtime_name="$1"
  local runtime_version="$2"
  local expected_http="$3"
  local expected_code="$4"
  local http
  http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/runtimes/$runtime_name/versions/$runtime_version" "$delete_body")"
  if [[ "$http" != "$expected_http" ]]; then
    echo "FAIL: delete http mismatch runtime='$runtime_name' version='$runtime_version' expected='$expected_http' got='$http'" >&2
    cat "$delete_body" >&2 || true
    exit 1
  fi
  local error_code
  error_code="$(json_get_file "error_code" "$delete_body")"
  if [[ "$error_code" != "$expected_code" ]]; then
    echo "FAIL: delete error_code mismatch runtime='$runtime_name' version='$runtime_version' expected='$expected_code' got='$error_code'" >&2
    cat "$delete_body" >&2 || true
    exit 1
  fi
}

assert_runtime_missing_version() {
  local runtime_name="$1"
  local runtime_version="$2"
  local http
  http="$(http_call "GET" "$BASE/hives/$HIVE_ID/runtimes/$runtime_name" "$runtime_body")"
  if [[ "$http" != "200" ]]; then
    echo "FAIL: get_runtime failed runtime='$runtime_name' http='$http'" >&2
    cat "$runtime_body" >&2 || true
    exit 1
  fi
  if jq -e --arg ver "$runtime_version" '.payload.runtime.available | index($ver)' "$runtime_body" >/dev/null; then
    echo "FAIL: runtime '$runtime_name' still lists deleted version '$runtime_version'" >&2
    cat "$runtime_body" >&2 || true
    exit 1
  fi
}

run_admin_socket() {
  local action="$1"
  local params_json="$2"
  ADMIN_ACTION="$action" \
  ADMIN_TARGET="SY.admin@motherbee" \
  ADMIN_TIMEOUT_SECS="30" \
  ADMIN_PAYLOAD_TARGET="$HIVE_ID" \
  ADMIN_PARAMS_JSON="$params_json" \
  "$DIAG_BIN" >"$socket_current_body"
}

create_delete_ok_package() {
  mkdir -p "$delete_ok_pkg/bin" "$delete_ok_pkg/config"
  cat >"$delete_ok_pkg/package.json" <<EOF
{
  "name": "$DELETE_OK_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "full_runtime",
  "description": "Admin delete OK fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$delete_ok_pkg/config/default-config.json" <<'EOF'
{
  "kind": "admin_delete_ok_fixture"
}
EOF
  cat >"$delete_ok_pkg/bin/start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "admin delete ok fixture"
EOF
  chmod 0755 "$delete_ok_pkg/bin/start.sh"
}

create_in_use_package() {
  mkdir -p "$in_use_pkg/bin" "$in_use_pkg/config"
  cat >"$in_use_pkg/package.json" <<EOF
{
  "name": "$IN_USE_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "full_runtime",
  "description": "Admin delete in-use fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$in_use_pkg/config/default-config.json" <<EOF
{
  "kind": "admin_delete_in_use_fixture",
  "node_name": "$IN_USE_NODE_NAME"
}
EOF
  cat >"$in_use_pkg/bin/start.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec env \
  JSR_LOG_LEVEL="\${JSR_LOG_LEVEL:-info}" \
  AI_TEST_NODE_NAME="$IN_USE_NODE_LOCAL" \
  AI_TEST_NODE_VERSION="$IN_USE_V1" \
  AI_TEST_CONFIG_DIR="/etc/fluxbee" \
  AI_TEST_AUTO_REPLY="0" \
  "\$(dirname "\${BASH_SOURCE[0]}")/ai-test-gov"
EOF
  chmod 0755 "$in_use_pkg/bin/start.sh"
  as_root_local install -m 0755 "$AI_BIN" "$in_use_pkg/bin/ai-test-gov"
}

create_base_package() {
  mkdir -p "$base_pkg/bin" "$base_pkg/config"
  cat >"$base_pkg/package.json" <<EOF
{
  "name": "$BASE_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "full_runtime",
  "description": "Admin delete base runtime fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$base_pkg/config/default-config.json" <<'EOF'
{
  "kind": "admin_delete_base_fixture"
}
EOF
  cat >"$base_pkg/bin/start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
echo "admin delete base fixture"
EOF
  chmod 0755 "$base_pkg/bin/start.sh"
}

create_dependent_package() {
  mkdir -p "$dependent_pkg/config"
  cat >"$dependent_pkg/package.json" <<EOF
{
  "name": "$DEPENDENT_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "config_only",
  "description": "Admin delete dependent runtime fixture",
  "runtime_base": "$BASE_RUNTIME_NAME",
  "config_template": "config/default-config.json"
}
EOF
  cat >"$dependent_pkg/config/default-config.json" <<EOF
{
  "kind": "admin_delete_dependent_fixture",
  "runtime_base": "$BASE_RUNTIME_NAME"
}
EOF
}

spawn_in_use_node() {
  local payload
  payload="$(jq -cn \
    --arg node_name "$IN_USE_NODE_NAME" \
    --arg runtime "$IN_USE_RUNTIME_NAME" \
    --arg runtime_version "$IN_USE_V1" \
    --arg tenant_id "$TENANT_ID" \
    '{node_name:$node_name,runtime:$runtime,runtime_version:$runtime_version,tenant_id:$tenant_id}')"
  local http
  http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$payload")"
  if [[ "$http" != "200" ]]; then
    echo "FAIL: in-use node spawn http='$http'" >&2
    cat "$spawn_body" >&2 || true
    exit 1
  fi
}

require_cmd cargo
require_cmd curl
require_cmd jq
require_cmd python3

if [[ "$HIVE_ID" != "$MOTHER_HIVE_ID" || "$HIVE_ID" != "motherbee" ]]; then
  echo "FAIL: this E2E currently requires HIVE_ID=motherbee and MOTHER_HIVE_ID=motherbee" >&2
  exit 1
fi

validate_tenant_id "$TENANT_ID"

echo "Step 1/14: build fluxbee-publish + ai-test-gov + admin diag"
(cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish -p json-router)
(cd "$ROOT_DIR" && cargo build --release -p ai-test-gov)
(cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
[[ -x "$PUBLISH_BIN" ]] || { echo "FAIL: missing publish binary '$PUBLISH_BIN'" >&2; exit 1; }
[[ -x "$AI_BIN" ]] || { echo "FAIL: missing ai-test-gov binary '$AI_BIN'" >&2; exit 1; }
[[ -x "$DIAG_BIN" ]] || { echo "FAIL: missing admin diag binary '$DIAG_BIN'" >&2; exit 1; }

echo "Step 2/14: backup manifest + create package fixtures"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_delete_ok_package
create_in_use_package
create_base_package
create_dependent_package

echo "Step 3/14: publish success fixture runtime twice"
run_publish "$delete_ok_pkg" "$DELETE_OK_V1"
run_publish "$delete_ok_pkg" "$DELETE_OK_V2"
wait_runtime_ready "$DELETE_OK_RUNTIME_NAME" "$DELETE_OK_V1"
wait_runtime_ready "$DELETE_OK_RUNTIME_NAME" "$DELETE_OK_V2"

echo "Step 4/12: delete non-current version succeeds"
assert_delete_ok "$DELETE_OK_RUNTIME_NAME" "$DELETE_OK_V1"
assert_runtime_missing_version "$DELETE_OK_RUNTIME_NAME" "$DELETE_OK_V1"

echo "Step 5/12: current + missing version negatives"
assert_delete_error "$DELETE_OK_RUNTIME_NAME" "$DELETE_OK_V2" "409" "RUNTIME_CURRENT_CONFLICT"
assert_delete_error "$DELETE_OK_RUNTIME_NAME" "$DELETE_OK_MISSING_VERSION" "404" "RUNTIME_VERSION_NOT_FOUND"

echo "Step 6/12: HTTP/socket parity on current-conflict"
run_admin_socket \
  "remove_runtime_version" \
  "{\"runtime\":\"$DELETE_OK_RUNTIME_NAME\",\"runtime_version\":\"$DELETE_OK_V2\"}"
socket_status="$(json_get_file "status" "$socket_current_body")"
socket_error_code="$(json_get_file "error_code" "$socket_current_body")"
if [[ "$socket_status" != "error" || "$socket_error_code" != "RUNTIME_CURRENT_CONFLICT" ]]; then
  echo "FAIL: socket parity mismatch for current delete" >&2
  cat "$socket_current_body" >&2 || true
  exit 1
fi

echo "Step 7/12: publish in-use fixture runtime twice"
run_publish "$in_use_pkg" "$IN_USE_V1"
run_publish "$in_use_pkg" "$IN_USE_V2"
wait_runtime_ready "$IN_USE_RUNTIME_NAME" "$IN_USE_V1"
wait_runtime_ready "$IN_USE_RUNTIME_NAME" "$IN_USE_V2"

echo "Step 8/12: spawn version-pinned running node"
spawn_in_use_node
wait_node_running "$IN_USE_NODE_NAME"

echo "Step 9/12: running version delete must fail"
assert_delete_error "$IN_USE_RUNTIME_NAME" "$IN_USE_V1" "409" "RUNTIME_IN_USE"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$IN_USE_NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "Step 10/12: publish base runtime twice + dependent runtime"
run_publish "$base_pkg" "$BASE_V1"
run_publish "$base_pkg" "$BASE_V2"
wait_runtime_ready "$BASE_RUNTIME_NAME" "$BASE_V1"
wait_runtime_ready "$BASE_RUNTIME_NAME" "$BASE_V2"
run_publish "$dependent_pkg" "$DEPENDENT_VERSION"

echo "Step 11/12: base runtime delete must fail while dependent exists"
assert_delete_error "$BASE_RUNTIME_NAME" "$BASE_V1" "409" "RUNTIME_HAS_DEPENDENTS"

echo "Step 12/12: summary"
http_call "GET" "$BASE/hives/$HIVE_ID/runtimes/$DELETE_OK_RUNTIME_NAME" "$runtime_body" >/dev/null
delete_ok_manifest_version="$(json_get_file "payload.manifest_version" "$runtime_body")"
http_call "GET" "$BASE/hives/$HIVE_ID/runtimes/$IN_USE_RUNTIME_NAME" "$runtime_body" >/dev/null
in_use_running_count="$(json_get_file "payload.usage_global_visible.running_count" "$runtime_body")"
http_call "GET" "$BASE/hives/$HIVE_ID/runtimes/$BASE_RUNTIME_NAME" "$runtime_body" >/dev/null
base_current="$(json_get_file "payload.runtime.current" "$runtime_body")"
echo "status=ok"
echo "delete_ok_runtime=$DELETE_OK_RUNTIME_NAME@$DELETE_OK_V2"
echo "delete_ok_removed_version=$DELETE_OK_V1"
echo "delete_ok_manifest_version=$delete_ok_manifest_version"
echo "socket_current_error_code=$socket_error_code"
echo "in_use_runtime=$IN_USE_RUNTIME_NAME@$IN_USE_V2"
echo "in_use_blocked_version=$IN_USE_V1"
echo "in_use_running_count_after_kill=$in_use_running_count"
echo "base_runtime=$BASE_RUNTIME_NAME@$BASE_V2"
echo "base_runtime_blocked_version=$BASE_V1"
echo "dependent_runtime=$DEPENDENT_RUNTIME_NAME@$DEPENDENT_VERSION"
echo "base_current=$base_current"
echo "admin runtime delete REST E2E passed."
