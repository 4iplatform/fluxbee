#!/usr/bin/env bash
set -euo pipefail

# Custom managed node reboot/reconcile E2E.
# - builds fluxbee-publish + ai-test-gov + io-test
# - publishes temporary full_runtime fixtures for AI.test.gov and IO.test
# - spawns AI.test.gov as managed custom node
# - kills the node and waits until it is no longer visible in router inventory
# - restarts sy-orchestrator to trigger bootstrap reconcile
# - verifies AI.test.gov is relaunched from persisted config.json
# - spawns IO.test and validates a real routed reply handled by AI.test.gov
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="motherbee" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid-v4>" \
#   bash scripts/custom_node_reboot_reconcile_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
LOCAL_HIVE_YAML="/etc/fluxbee/hive.yaml"
LOCAL_HIVE_ID="${LOCAL_HIVE_ID:-}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-}"
HIVE_ID="${HIVE_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
WAIT_STOP_SECS="${WAIT_STOP_SECS:-30}"
WAIT_IO_RESULT_SECS="${WAIT_IO_RESULT_SECS:-30}"
WAIT_PROBE_READY_SECS="${WAIT_PROBE_READY_SECS:-30}"
IO_TEST_WAIT_REPLY_MS="${IO_TEST_WAIT_REPLY_MS:-8000}"
TEST_TS="$(date +%s)"
TEST_ID="${TEST_ID:-rebootreconcile-${TEST_TS}-$RANDOM}"
AI_RUNTIME_NAME="${AI_RUNTIME_NAME:-ai.test.gov.reboot.${TEST_TS}}"
AI_RUNTIME_VERSION="${AI_RUNTIME_VERSION:-1.0.0-${TEST_ID}}"
IO_RUNTIME_NAME="${IO_RUNTIME_NAME:-io.test.reboot.${TEST_TS}}"
IO_RUNTIME_VERSION="${IO_RUNTIME_VERSION:-1.0.0-${TEST_ID}}"
IO_TEST_CHANNEL_TYPE="${IO_TEST_CHANNEL_TYPE:-io.test.reboot}"
IO_TEST_ADDRESS="${IO_TEST_ADDRESS:-io.test.reboot.${TEST_ID}}"
PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"
AI_BIN="$ROOT_DIR/target/release/ai-test-gov"
IO_BIN="$ROOT_DIR/target/release/io-test"
MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_AI_DIR="/var/lib/fluxbee/dist/runtimes/$AI_RUNTIME_NAME"
DIST_IO_DIR="/var/lib/fluxbee/dist/runtimes/$IO_RUNTIME_NAME"
SCENARIO_ROOT="/var/lib/fluxbee/dist/.custom-node-reboot-reconcile"
SCENARIO_DIR="$SCENARIO_ROOT/$TEST_ID"
IO_STATUS_FILE="$SCENARIO_DIR/io-test.status"
IO_LOG_FILE="$SCENARIO_DIR/io-test.log"

tmpdir="$(mktemp -d)"
ai_pkg_dir="$tmpdir/ai_pkg"
io_pkg_dir="$tmpdir/io_pkg"
manifest_backup="$tmpdir/manifest.backup.json"
ai_publish_log="$tmpdir/ai.publish.log"
io_publish_log="$tmpdir/io.publish.log"
versions_body="$tmpdir/versions.json"
ai_spawn_body="$tmpdir/ai.spawn.json"
io_spawn_body="$tmpdir/io.spawn.json"
status_body="$tmpdir/status.json"
nodes_body="$tmpdir/nodes.json"
config_body="$tmpdir/config.json"
kill_body="$tmpdir/kill.json"
remove_body="$tmpdir/remove.json"

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
    echo "FAIL: TENANT_ID is required for this E2E because the frontdesk node is identity-managed and spawn requires tenant_id (format: tnt:<uuid-v4>)" >&2
    exit 1
  fi
  if [[ "$tenant_id" == *"<"* || "$tenant_id" == *">"* ]]; then
    echo "FAIL: TENANT_ID looks like placeholder ('$tenant_id'). Use real tnt:<uuid-v4>." >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected tnt:<uuid-v4>)" >&2
    exit 1
  fi
}

read_hive_id_from_config() {
  if [[ ! -f "$LOCAL_HIVE_YAML" ]]; then
    return 0
  fi
  awk -F': *' '/^hive_id:/ {print $2; exit}' "$LOCAL_HIVE_YAML" | tr -d '"'
}

node_local_name() {
  local raw="$1"
  echo "${raw%@*}"
}

node_hive_name() {
  local raw="$1"
  if [[ "$raw" == *"@"* ]]; then
    echo "${raw##*@}"
  else
    echo ""
  fi
}

normalize_node_name() {
  local raw="$1"
  local hive_id="$2"
  if [[ "$raw" == *"@"* ]]; then
    echo "$raw"
  else
    echo "${raw}@${hive_id}"
  fi
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
  echo "FAIL: runtime not ready on hive '$HIVE_ID' runtime='$runtime_name' version='$runtime_version'" >&2
  cat "$versions_body" >&2 || true
  return 1
}

spawn_node_payload() {
  local node_name="$1"
  local runtime_name="$2"
  local runtime_version="$3"
  jq -cn \
    --arg node_name "$node_name" \
    --arg runtime "$runtime_name" \
    --arg runtime_version "$runtime_version" \
    --arg tenant_id "$TENANT_ID" \
    '{node_name:$node_name,runtime:$runtime,runtime_version:$runtime_version,tenant_id:$tenant_id}'
}

wait_node_active() {
  local node_name="$1"
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http api_status payload_status lifecycle pid
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$node_name/status" "$status_body")"
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
  echo "FAIL: node did not reach RUNNING/STARTING(active) node='$node_name'" >&2
  cat "$status_body" >&2 || true
  return 1
}

wait_admin_ready() {
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes" "$nodes_body" || true)"
    if [[ "$http" == "200" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: admin API did not recover after orchestrator restart" >&2
  return 1
}

wait_node_visible_in_router() {
  local node_name="$1"
  local deadline=$(( $(date +%s) + WAIT_STATUS_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http lifecycle visible
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes" "$nodes_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    lifecycle="$(jq -r --arg node "$node_name" \
      '.payload.nodes[] | select(.node_name == $node) | .lifecycle_state // empty' \
      "$nodes_body" | head -n1)"
    visible="$(jq -r --arg node "$node_name" \
      '.payload.nodes[] | select(.node_name == $node) | (.visible_in_router // false | tostring)' \
      "$nodes_body" | head -n1)"
    if [[ "$visible" == "true" && ( "$lifecycle" == "RUNNING" || "$lifecycle" == "STARTING" ) ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: node did not become visible in router node='$node_name'" >&2
  cat "$nodes_body" >&2 || true
  return 1
}

wait_node_stopped_and_not_visible() {
  local node_name="$1"
  local deadline=$(( $(date +%s) + WAIT_STOP_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http lifecycle visible
    http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes" "$nodes_body")"
    if [[ "$http" != "200" ]]; then
      sleep 2
      continue
    fi
    lifecycle="$(jq -r --arg node "$node_name" \
      '.payload.nodes[] | select(.node_name == $node) | .lifecycle_state // empty' \
      "$nodes_body" | head -n1)"
    visible="$(jq -r --arg node "$node_name" \
      '.payload.nodes[] | select(.node_name == $node) | (.visible_in_router // false | tostring)' \
      "$nodes_body" | head -n1)"
    if [[ "$lifecycle" == "STOPPED" && "$visible" == "false" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: node did not reach STOPPED and invisible state node='$node_name'" >&2
  cat "$nodes_body" >&2 || true
  return 1
}

wait_io_result() {
  local deadline=$(( $(date +%s) + WAIT_IO_RESULT_SECS ))
  while (( $(date +%s) <= deadline )); do
    if as_root_local test -f "$IO_STATUS_FILE"; then
      local status
      status="$(as_root_local cat "$IO_STATUS_FILE" 2>/dev/null || true)"
      status="${status//$'\r'/}"
      status="${status//$'\n'/}"
      if [[ "$status" == "ok" ]]; then
        return 0
      fi
      if [[ "$status" == "error" ]]; then
        echo "FAIL: IO.test wrapper reported error" >&2
        if as_root_local test -f "$IO_LOG_FILE"; then
          as_root_local tail -n 200 "$IO_LOG_FILE" >&2 || true
        fi
        return 1
      fi
    fi
    sleep 1
  done
  echo "FAIL: timeout waiting IO.test status file '$IO_STATUS_FILE'" >&2
  if as_root_local test -f "$IO_LOG_FILE"; then
    as_root_local tail -n 200 "$IO_LOG_FILE" >&2 || true
  fi
  return 1
}

run_io_probe_until_success() {
  local deadline=$(( $(date +%s) + WAIT_PROBE_READY_SECS ))
  local attempt=0
  while (( $(date +%s) <= deadline )); do
    attempt=$((attempt + 1))
    as_root_local rm -f "$IO_STATUS_FILE" "$IO_LOG_FILE"
    kill_node "$IO_NODE_NAME"
    remove_node_instance "$IO_NODE_NAME"

    local io_spawn_payload io_spawn_http io_spawn_status
    io_spawn_payload="$(spawn_node_payload "$IO_NODE_NAME" "$IO_RUNTIME_NAME" "current")"
    io_spawn_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$io_spawn_body" "$io_spawn_payload")"
    io_spawn_status="$(json_get_file "payload.status" "$io_spawn_body")"
    if [[ "$io_spawn_http" != "200" || "$io_spawn_status" != "ok" ]]; then
      echo "FAIL: IO.test spawn http=$io_spawn_http attempt=$attempt" >&2
      cat "$io_spawn_body" >&2 || true
      return 1
    fi

    if wait_io_result && assert_io_log "$FRONTDESK_NODE_NAME"; then
      return 0
    fi

    echo "WARN: IO.test probe attempt $attempt failed after orchestrator restart; retrying" >&2

    sleep 2
  done

  echo "FAIL: IO.test probe never observed routed reply after orchestrator restart" >&2
  if as_root_local test -f "$IO_LOG_FILE"; then
    as_root_local tail -n 200 "$IO_LOG_FILE" >&2 || true
  fi
  return 1
}

assert_io_log() {
  local expected_handled_by="$1"
  if ! as_root_local test -f "$IO_LOG_FILE"; then
    echo "FAIL: IO.test log missing '$IO_LOG_FILE'" >&2
    return 1
  fi
  local status handled_by
  status="$(as_root_local awk -F= '/^STATUS=/{print $2; exit}' "$IO_LOG_FILE" 2>/dev/null || true)"
  handled_by="$(as_root_local awk -F= '/^HANDLED_BY=/{print $2; exit}' "$IO_LOG_FILE" 2>/dev/null || true)"
  if [[ "$status" != "ok" ]]; then
    echo "FAIL: IO.test log missing STATUS=ok" >&2
    as_root_local tail -n 200 "$IO_LOG_FILE" >&2 || true
    return 1
  fi
  if [[ "$handled_by" != "$expected_handled_by" ]]; then
    echo "FAIL: unexpected HANDLED_BY='$handled_by' expected '$expected_handled_by'" >&2
    as_root_local tail -n 200 "$IO_LOG_FILE" >&2 || true
    return 1
  fi
}

run_publish() {
  local pkg_dir="$1"
  local runtime_version="$2"
  local publish_log="$3"
  as_root_local env \
    FLUXBEE_PUBLISH_BASE="$BASE" \
    FLUXBEE_PUBLISH_MOTHER_HIVE_ID="$MOTHER_HIVE_ID" \
    "$PUBLISH_BIN" "$pkg_dir" --version "$runtime_version" --deploy "$HIVE_ID" \
    | tee "$publish_log"
}

kill_node() {
  local node_name="$1"
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$node_name" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
}

remove_node_instance() {
  local node_name="$1"
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$node_name/instance" "$remove_body" >/dev/null 2>&1 || true
}

create_ai_package_fixture() {
  mkdir -p "$ai_pkg_dir/bin" "$ai_pkg_dir/config"
  cat >"$ai_pkg_dir/package.json" <<EOF
{
  "name": "$AI_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "full_runtime",
  "description": "Reboot reconcile frontdesk runtime fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$ai_pkg_dir/config/default-config.json" <<EOF
{
  "kind": "reboot_reconcile_frontdesk",
  "node_name": "$FRONTDESK_NODE_NAME"
}
EOF
  cat >"$ai_pkg_dir/bin/start.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
exec env \
  JSR_LOG_LEVEL="\${JSR_LOG_LEVEL:-info}" \
  AI_TEST_NODE_NAME="${FRONTDESK_NODE_LOCAL}" \
  AI_TEST_NODE_VERSION="${AI_RUNTIME_VERSION}" \
  AI_TEST_CONFIG_DIR="/etc/fluxbee" \
  AI_TEST_AUTO_REPLY="1" \
  "\$(dirname "\${BASH_SOURCE[0]}")/ai-test-gov"
EOF
  chmod 0755 "$ai_pkg_dir/bin/start.sh"
  as_root_local install -m 0755 "$AI_BIN" "$ai_pkg_dir/bin/ai-test-gov"
}

create_io_package_fixture() {
  mkdir -p "$io_pkg_dir/bin" "$io_pkg_dir/config"
  cat >"$io_pkg_dir/package.json" <<EOF
{
  "name": "$IO_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "full_runtime",
  "description": "Reboot reconcile IO runtime fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$io_pkg_dir/config/default-config.json" <<EOF
{
  "kind": "reboot_reconcile_io",
  "frontdesk_node_name": "$FRONTDESK_NODE_NAME",
  "channel_type": "$IO_TEST_CHANNEL_TYPE"
}
EOF
  cat >"$io_pkg_dir/bin/start.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
STATUS_FILE="$IO_STATUS_FILE"
LOG_FILE="$IO_LOG_FILE"
mkdir -p "\$(dirname "\$STATUS_FILE")"
echo "pending" >"\$STATUS_FILE"
if env \
  JSR_LOG_LEVEL="\${JSR_LOG_LEVEL:-info}" \
  IO_TEST_NODE_NAME="${IO_NODE_LOCAL}" \
  IO_TEST_NODE_VERSION="${IO_RUNTIME_VERSION}" \
  IO_TEST_CONFIG_DIR="/etc/fluxbee" \
  IO_TEST_CHANNEL_TYPE="${IO_TEST_CHANNEL_TYPE}" \
  IO_TEST_ADDRESS="${IO_TEST_ADDRESS}" \
  IO_TEST_ALLOW_PROVISION="1" \
  IO_TEST_IDENTITY_TARGET="SY.identity@${HIVE_ID}" \
  IO_TEST_WAIT_REPLY_MS="${IO_TEST_WAIT_REPLY_MS}" \
  "\$(dirname "\${BASH_SOURCE[0]}")/io-test" >"\$LOG_FILE" 2>&1; then
  echo "ok" >"\$STATUS_FILE"
  exec /bin/sleep "\${IO_TEST_HOLD_SECS:-3600}"
else
  rc="\$?"
  echo "error" >"\$STATUS_FILE"
  echo "EXIT_CODE=\$rc" >>"\$LOG_FILE"
  exit "\$rc"
fi
EOF
  chmod 0755 "$io_pkg_dir/bin/start.sh"
  as_root_local install -m 0755 "$IO_BIN" "$io_pkg_dir/bin/io-test"
}

cleanup() {
  local _ec=$?
  kill_node "$IO_NODE_NAME"
  kill_node "$FRONTDESK_NODE_NAME"
  remove_node_instance "$IO_NODE_NAME"
  remove_node_instance "$FRONTDESK_NODE_NAME"
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -rf "$DIST_AI_DIR" "$DIST_IO_DIR" "$SCENARIO_DIR" >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

require_cmd cargo
require_cmd curl
require_cmd jq
require_cmd python3
require_cmd sudo
validate_tenant_id "${TENANT_ID:-}"

if [[ -z "$LOCAL_HIVE_ID" ]]; then
  LOCAL_HIVE_ID="$(read_hive_id_from_config)"
fi
if [[ -z "$LOCAL_HIVE_ID" ]]; then
  echo "FAIL: unable to resolve local hive_id from '$LOCAL_HIVE_YAML'" >&2
  exit 1
fi
if [[ -z "$MOTHER_HIVE_ID" ]]; then
  MOTHER_HIVE_ID="$LOCAL_HIVE_ID"
fi
if [[ -z "$HIVE_ID" ]]; then
  HIVE_ID="$LOCAL_HIVE_ID"
fi
if [[ "$HIVE_ID" != "$LOCAL_HIVE_ID" || "$MOTHER_HIVE_ID" != "$LOCAL_HIVE_ID" ]]; then
  echo "FAIL: this E2E is local-hive only; expected HIVE_ID=MOTHER_HIVE_ID=LOCAL_HIVE_ID='$LOCAL_HIVE_ID'" >&2
  exit 1
fi

FRONTDESK_NODE_NAME="${FRONTDESK_NODE_NAME:-AI.test.gov@$HIVE_ID}"
FRONTDESK_NODE_NAME="$(normalize_node_name "$FRONTDESK_NODE_NAME" "$HIVE_ID")"
FRONTDESK_NODE_LOCAL="$(node_local_name "$FRONTDESK_NODE_NAME")"
FRONTDESK_NODE_HIVE="$(node_hive_name "$FRONTDESK_NODE_NAME")"
if [[ "$FRONTDESK_NODE_HIVE" != "$HIVE_ID" ]]; then
  echo "FAIL: FRONTDESK_NODE_NAME must target local hive '$HIVE_ID', got '$FRONTDESK_NODE_NAME'" >&2
  exit 1
fi

IO_NODE_NAME="${IO_NODE_NAME:-IO.test@$HIVE_ID}"
IO_NODE_NAME="$(normalize_node_name "$IO_NODE_NAME" "$HIVE_ID")"
IO_NODE_LOCAL="$(node_local_name "$IO_NODE_NAME")"

if [[ ! -f "$MANIFEST_PATH" ]]; then
  echo "FAIL: runtime manifest missing at '$MANIFEST_PATH'" >&2
  exit 1
fi

echo "Custom node reboot/reconcile E2E: BASE=$BASE HIVE_ID=$HIVE_ID FRONTDESK=$FRONTDESK_NODE_NAME IO=$IO_NODE_NAME TEST_ID=$TEST_ID"

echo "Step 1/11: build binaries (fluxbee-publish + ai-test-gov + io-test)"
(cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish -p json-router >/dev/null)
(cd "$ROOT_DIR" && cargo build --release -p ai-test-gov -p io-test >/dev/null)
if [[ ! -x "$PUBLISH_BIN" || ! -x "$AI_BIN" || ! -x "$IO_BIN" ]]; then
  echo "FAIL: expected release binaries missing" >&2
  exit 1
fi

echo "Step 2/11: backup manifest + create package fixtures"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
as_root_local mkdir -p "$SCENARIO_DIR"
create_ai_package_fixture
create_io_package_fixture

echo "Step 3/11: publish AI.test.gov and IO.test runtimes with deploy"
run_publish "$ai_pkg_dir" "$AI_RUNTIME_VERSION" "$ai_publish_log"
run_publish "$io_pkg_dir" "$IO_RUNTIME_VERSION" "$io_publish_log"

echo "Step 4/11: wait runtime readiness"
wait_runtime_ready "$AI_RUNTIME_NAME" "$AI_RUNTIME_VERSION"
wait_runtime_ready "$IO_RUNTIME_NAME" "$IO_RUNTIME_VERSION"

echo "Step 5/11: cleanup any previous managed nodes and spawn frontdesk"
kill_node "$IO_NODE_NAME"
kill_node "$FRONTDESK_NODE_NAME"
remove_node_instance "$IO_NODE_NAME"
remove_node_instance "$FRONTDESK_NODE_NAME"
ai_spawn_payload="$(spawn_node_payload "$FRONTDESK_NODE_NAME" "$AI_RUNTIME_NAME" "current")"
ai_spawn_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$ai_spawn_body" "$ai_spawn_payload")"
ai_spawn_status="$(json_get_file "payload.status" "$ai_spawn_body")"
if [[ "$ai_spawn_http" != "200" || "$ai_spawn_status" != "ok" ]]; then
  echo "FAIL: frontdesk spawn http=$ai_spawn_http" >&2
  cat "$ai_spawn_body" >&2 || true
  exit 1
fi

echo "Step 6/11: wait frontdesk active before simulated reboot"
frontdesk_lifecycle_before="$(wait_node_active "$FRONTDESK_NODE_NAME")"

echo "Step 7/11: kill frontdesk and wait until STOPPED + not visible"
kill_node "$FRONTDESK_NODE_NAME"
wait_node_stopped_and_not_visible "$FRONTDESK_NODE_NAME"

echo "Step 8/11: restart sy-orchestrator to trigger bootstrap reconcile"
as_root_local systemctl restart sy-orchestrator

echo "Step 9/11: wait frontdesk relaunched by reconcile"
wait_admin_ready
frontdesk_lifecycle_after="$(wait_node_active "$FRONTDESK_NODE_NAME")"
wait_node_visible_in_router "$FRONTDESK_NODE_NAME"

echo "Step 10/11: spawn IO.test probe against relaunched frontdesk"
run_io_probe_until_success

echo "Step 11/11: summary"
config_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$FRONTDESK_NODE_NAME/config" "$config_body")"
if [[ "$config_http" != "200" ]]; then
  echo "FAIL: unable to fetch frontdesk config http=$config_http" >&2
  cat "$config_body" >&2 || true
  exit 1
fi
frontdesk_runtime_observed="$(json_get_file "payload.config._system.runtime" "$config_body")"
frontdesk_version_observed="$(json_get_file "payload.config._system.runtime_version" "$config_body")"
if [[ "$frontdesk_runtime_observed" != "$AI_RUNTIME_NAME" ]]; then
  echo "FAIL: unexpected frontdesk runtime in config '$frontdesk_runtime_observed' expected '$AI_RUNTIME_NAME'" >&2
  cat "$config_body" >&2 || true
  exit 1
fi
if [[ "$frontdesk_version_observed" != "$AI_RUNTIME_VERSION" ]]; then
  echo "FAIL: unexpected frontdesk runtime_version in config '$frontdesk_version_observed' expected '$AI_RUNTIME_VERSION'" >&2
  cat "$config_body" >&2 || true
  exit 1
fi

trace_id="$(as_root_local awk -F= '/^TRACE_ID=/{print $2; exit}' "$IO_LOG_FILE" 2>/dev/null || true)"
handled_by="$(as_root_local awk -F= '/^HANDLED_BY=/{print $2; exit}' "$IO_LOG_FILE" 2>/dev/null || true)"
echo "status=ok"
echo "frontdesk_node_name=$FRONTDESK_NODE_NAME"
echo "frontdesk_runtime=$AI_RUNTIME_NAME@$AI_RUNTIME_VERSION"
echo "frontdesk_lifecycle_before=$frontdesk_lifecycle_before"
echo "frontdesk_lifecycle_after=$frontdesk_lifecycle_after"
echo "io_runtime=$IO_RUNTIME_NAME@$IO_RUNTIME_VERSION"
echo "trace_id=$trace_id"
echo "handled_by=$handled_by"
echo "custom managed node reboot/reconcile E2E passed."
