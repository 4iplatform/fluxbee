#!/usr/bin/env bash
set -euo pipefail

# PUB-T26 E2E (template layering on config_only):
# 1) publish base full_runtime fixture
# 2) publish config_only package with a config template that includes defaults
#    and a fake _system block
# 3) deploy + spawn config_only with request config overrides
# 4) verify template defaults remain, request config wins, and orchestrator
#    forces the final _system block
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/runtime_packaging_pub_t26_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
DEPLOY_STRICT="${DEPLOY_STRICT:-1}"
TEST_ID="${TEST_ID:-pubt26-$(date +%s)-$RANDOM}"
BASE_RUNTIME_NAME="${BASE_RUNTIME_NAME:-wf.publish.layer.base.diag.$(date +%s)}"
BASE_RUNTIME_VERSION="${BASE_RUNTIME_VERSION:-1.0.0-$TEST_ID}"
CONFIG_RUNTIME_NAME="${CONFIG_RUNTIME_NAME:-wf.publish.layer.config.diag.$(date +%s)}"
CONFIG_RUNTIME_VERSION="${CONFIG_RUNTIME_VERSION:-2.0.0-$TEST_ID}"
NODE_NAME="${NODE_NAME:-WF.publish.layering.$TEST_ID}"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_BASE_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$BASE_RUNTIME_NAME"
DIST_CONFIG_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$CONFIG_RUNTIME_NAME"
PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"
DIAG_BIN="$ROOT_DIR/target/release/inventory_hold_diag"

TEMPLATE_MODEL="template-model-v1"
REQUEST_MODEL="request-model-v2"
TEMPLATE_TEMPERATURE="0.2"
REQUEST_TEMPERATURE="0.9"
TEMPLATE_PROMPT_ASSET="assets/prompts/system.txt"
TEMPLATE_ONLY_MARKER="from-template"
REQUEST_ONLY_MARKER="from-request"
FAKE_TEMPLATE_SYSTEM_PACKAGE_PATH="/tmp/template-package"

tmpdir="$(mktemp -d)"
base_pkg_dir="$tmpdir/base_pkg"
cfg_pkg_dir="$tmpdir/cfg_pkg"
manifest_backup="$tmpdir/manifest.backup.json"
publish_base_log="$tmpdir/publish_base.log"
publish_cfg_log="$tmpdir/publish_cfg.log"
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
  as_root_local rm -rf "$DIST_BASE_RUNTIME_DIR" >/dev/null 2>&1 || true
  as_root_local rm -rf "$DIST_CONFIG_RUNTIME_DIR" >/dev/null 2>&1 || true
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

wait_runtime_readiness_full() {
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
  echo "FAIL: full_runtime readiness not ready runtime='$runtime_name' version='$runtime_version'" >&2
  cat "$versions_body" >&2 || true
  return 1
}

wait_runtime_readiness_config() {
  local runtime_name="$1"
  local runtime_version="$2"
  local expected_manifest_version="${3:-0}"
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
    runtime_exists="$(jq -r --arg rt "$runtime_name" \
      '(.payload.hive.runtimes.runtimes | has($rt)) // false | tostring' \
      "$versions_body")"
    if [[ "$runtime_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    readiness_exists="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness | has($ver)) // false | tostring' \
      "$versions_body")"
    if [[ "$readiness_exists" != "true" ]]; then
      sleep 2
      continue
    fi
    present="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].runtime_present // false) | tostring' \
      "$versions_body")"
    exec="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].start_sh_executable // false) | tostring' \
      "$versions_body")"
    has_base_ready="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver] | has("base_runtime_ready")) // false | tostring' \
      "$versions_body")"
    if [[ "$has_base_ready" != "true" ]]; then
      echo "FAIL: target hive '$HIVE_ID' runtime '$runtime_name' readiness for version '$runtime_version' is present but missing base_runtime_ready." >&2
      cat "$versions_body" >&2 || true
      return 1
    fi
    base_ready="$(jq -r --arg rt "$runtime_name" --arg ver "$runtime_version" \
      '(.payload.hive.runtimes.runtimes[$rt].readiness[$ver].base_runtime_ready // false) | tostring' \
      "$versions_body")"
    if [[ "$present" == "true" && "$exec" == "true" && "$base_ready" == "true" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: config_only readiness timeout runtime='$runtime_name' version='$runtime_version'" >&2
  cat "$versions_body" >&2 || true
  return 1
}

spawn_node() {
  local payload
  payload="$(cat <<EOF
{"node_name":"$NODE_NAME","runtime":"$CONFIG_RUNTIME_NAME","runtime_version":"current","tenant_id":"$TENANT_ID","config":{"tenant_id":"$TENANT_ID","model":"$REQUEST_MODEL","temperature":$REQUEST_TEMPERATURE,"request_only":"$REQUEST_ONLY_MARKER"}}
EOF
)"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$payload"
}

wait_node_active_with_runtime() {
  local expected_runtime="$1"
  local expected_version="$2"
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
    if [[ "$runtime_name" != "$expected_runtime" || "$runtime_version" != "$expected_version" ]]; then
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
  echo "FAIL: node did not reach active lifecycle with expected runtime node='$NODE_NAME'" >&2
  cat "$status_body" >&2 || true
  return 1
}

assert_layered_config() {
  local http api_status payload_status expected_package_path observed_model observed_temp observed_prompt observed_template_only observed_request_only observed_tenant observed_system_managed_by observed_runtime_base observed_package_path observed_system_runtime observed_system_version observed_system_tenant
  http="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" "$config_body")"
  api_status="$(json_get_file "status" "$config_body")"
  payload_status="$(json_get_file "payload.status" "$config_body")"
  if [[ "$http" != "200" || "$api_status" != "ok" || "$payload_status" != "ok" ]]; then
    echo "FAIL: get config failed http=$http status=$api_status payload_status=$payload_status" >&2
    cat "$config_body" >&2 || true
    return 1
  fi

  observed_model="$(json_get_file "payload.config.model" "$config_body")"
  observed_temp="$(json_get_file "payload.config.temperature" "$config_body")"
  observed_prompt="$(json_get_file "payload.config.prompt_asset" "$config_body")"
  observed_template_only="$(json_get_file "payload.config.template_only" "$config_body")"
  observed_request_only="$(json_get_file "payload.config.request_only" "$config_body")"
  observed_tenant="$(json_get_file "payload.config.tenant_id" "$config_body")"
  observed_system_managed_by="$(json_get_file "payload.config._system.managed_by" "$config_body")"
  observed_runtime_base="$(json_get_file "payload.config._system.runtime_base" "$config_body")"
  observed_package_path="$(json_get_file "payload.config._system.package_path" "$config_body")"
  observed_system_runtime="$(json_get_file "payload.config._system.runtime" "$config_body")"
  observed_system_version="$(json_get_file "payload.config._system.runtime_version" "$config_body")"
  observed_system_tenant="$(json_get_file "payload.config._system.tenant_id" "$config_body")"
  expected_package_path="/var/lib/fluxbee/dist/runtimes/$CONFIG_RUNTIME_NAME/$CONFIG_RUNTIME_VERSION"

  [[ "$observed_model" == "$REQUEST_MODEL" ]] || {
    echo "FAIL: request override for model not applied expected='$REQUEST_MODEL' got='$observed_model'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_temp" == "$REQUEST_TEMPERATURE" ]] || {
    echo "FAIL: request override for temperature not applied expected='$REQUEST_TEMPERATURE' got='$observed_temp'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_prompt" == "$TEMPLATE_PROMPT_ASSET" ]] || {
    echo "FAIL: template default prompt_asset missing expected='$TEMPLATE_PROMPT_ASSET' got='$observed_prompt'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_template_only" == "$TEMPLATE_ONLY_MARKER" ]] || {
    echo "FAIL: template default field missing expected='$TEMPLATE_ONLY_MARKER' got='$observed_template_only'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_request_only" == "$REQUEST_ONLY_MARKER" ]] || {
    echo "FAIL: request-only field missing expected='$REQUEST_ONLY_MARKER' got='$observed_request_only'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_tenant" == "$TENANT_ID" ]] || {
    echo "FAIL: final tenant_id mismatch expected='$TENANT_ID' got='$observed_tenant'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_system_managed_by" == "SY.orchestrator" ]] || {
    echo "FAIL: _system.managed_by was not forced by orchestrator got='$observed_system_managed_by'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_runtime_base" == "$BASE_RUNTIME_NAME" ]] || {
    echo "FAIL: _system.runtime_base mismatch expected='$BASE_RUNTIME_NAME' got='$observed_runtime_base'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_package_path" == "$expected_package_path" ]] || {
    echo "FAIL: _system.package_path mismatch expected='$expected_package_path' got='$observed_package_path'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_system_runtime" == "$CONFIG_RUNTIME_NAME" ]] || {
    echo "FAIL: _system.runtime mismatch expected='$CONFIG_RUNTIME_NAME' got='$observed_system_runtime'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_system_version" == "$CONFIG_RUNTIME_VERSION" ]] || {
    echo "FAIL: _system.runtime_version mismatch expected='$CONFIG_RUNTIME_VERSION' got='$observed_system_version'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  [[ "$observed_system_tenant" == "$TENANT_ID" ]] || {
    echo "FAIL: _system.tenant_id mismatch expected='$TENANT_ID' got='$observed_system_tenant'" >&2
    cat "$config_body" >&2 || true
    return 1
  }
  if grep -q "$FAKE_TEMPLATE_SYSTEM_PACKAGE_PATH" "$config_body"; then
    echo "FAIL: template _system leaked into final config" >&2
    cat "$config_body" >&2 || true
    return 1
  fi

  echo "$observed_package_path"
}

create_base_package_fixture() {
  mkdir -p "$base_pkg_dir/bin" "$base_pkg_dir/config"
  cat >"$base_pkg_dir/package.json" <<EOF
{
  "name": "$BASE_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "full_runtime",
  "description": "PUB-T26 base runtime fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$base_pkg_dir/bin/start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="${INVENTORY_HOLD_NODE_NAME:-${NODE_NAME:-WF.publish.layering}}"
export INVENTORY_HOLD_NODE_VERSION="${INVENTORY_HOLD_NODE_VERSION:-0.0.1}"
export INVENTORY_HOLD_SECS="${INVENTORY_HOLD_SECS:-0}"
export NODE_STATUS_DEFAULT_HANDLER_ENABLED="1"
export NODE_STATUS_DEFAULT_HEALTH_STATE="HEALTHY"
exec "$(dirname "${BASH_SOURCE[0]}")/inventory_hold_diag"
EOF
  chmod 0755 "$base_pkg_dir/bin/start.sh"
  cat >"$base_pkg_dir/config/default-config.json" <<'EOF'
{
  "diag": {
    "kind": "base"
  }
}
EOF
  as_root_local install -m 0755 "$DIAG_BIN" "$base_pkg_dir/bin/inventory_hold_diag"
}

create_config_package_fixture() {
  mkdir -p "$cfg_pkg_dir/config" "$cfg_pkg_dir/assets/prompts"
  cat >"$cfg_pkg_dir/package.json" <<EOF
{
  "name": "$CONFIG_RUNTIME_NAME",
  "version": "0.0.1",
  "type": "config_only",
  "description": "PUB-T26 layering fixture",
  "runtime_base": "$BASE_RUNTIME_NAME",
  "config_template": "config/default-config.json"
}
EOF
  cat >"$cfg_pkg_dir/config/default-config.json" <<EOF
{
  "tenant_id": "tnt:template-default",
  "model": "$TEMPLATE_MODEL",
  "temperature": $TEMPLATE_TEMPERATURE,
  "prompt_asset": "$TEMPLATE_PROMPT_ASSET",
  "template_only": "$TEMPLATE_ONLY_MARKER",
  "_system": {
    "managed_by": "template",
    "runtime_base": "template.runtime.base",
    "package_path": "$FAKE_TEMPLATE_SYSTEM_PACKAGE_PATH"
  }
}
EOF
  cat >"$cfg_pkg_dir/assets/prompts/system.txt" <<'EOF'
template prompt asset
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
  local label="$2"
  if ! grep -q "sync_hint_status=" "$out_log"; then
    echo "FAIL[$label]: publish output missing deploy summary" >&2
    cat "$out_log" >&2 || true
    exit 1
  fi
  local deploy_line sync_hint_status update_status update_error_code
  deploy_line="$(grep -E 'sync_hint_status=' "$out_log" | tail -n1)"
  sync_hint_status="$(echo "$deploy_line" | sed -n 's/.*sync_hint_status=\([^ ]*\).*/\1/p')"
  update_status="$(echo "$deploy_line" | sed -n 's/.*update_status=\([^ ]*\).*/\1/p')"
  update_error_code="$(echo "$deploy_line" | sed -n 's/.*update_error_code=\(.*\)$/\1/p')"
  if [[ "$DEPLOY_STRICT" == "1" ]]; then
    if [[ "$sync_hint_status" == "error" ]]; then
      echo "FAIL[$label]: strict deploy requires sync_hint_status != error" >&2
      cat "$out_log" >&2 || true
      exit 1
    fi
    if [[ "$update_status" == "error" ]]; then
      echo "FAIL[$label]: strict deploy requires update_status != error (code='$update_error_code')" >&2
      cat "$out_log" >&2 || true
      exit 1
    fi
  fi
  local manifest_version
  manifest_version="$(sed -n 's/.*Manifest: .* (version=\([0-9][0-9]*\)).*/\1/p' "$out_log" | tail -n1)"
  if [[ -z "$manifest_version" ]]; then
    echo "FAIL[$label]: unable to parse manifest version from publish output" >&2
    cat "$out_log" >&2 || true
    exit 1
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

echo "PUB-T26 template layering E2E: BASE=$BASE HIVE_ID=$HIVE_ID MOTHER_HIVE_ID=$MOTHER_HIVE_ID BASE_RUNTIME=$BASE_RUNTIME_NAME@$BASE_RUNTIME_VERSION CONFIG_RUNTIME=$CONFIG_RUNTIME_NAME@$CONFIG_RUNTIME_VERSION NODE=$NODE_NAME"

echo "Step 1/13: build binaries (fluxbee-publish + inventory_hold_diag)"
(cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish --bin inventory_hold_diag >/dev/null)
if [[ ! -x "$PUBLISH_BIN" ]]; then
  echo "FAIL: publish binary missing at '$PUBLISH_BIN'" >&2
  exit 1
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/13: backup manifest + create package fixtures (base + layering config_only)"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_base_package_fixture
create_config_package_fixture

echo "Step 3/13: publish base full_runtime with deploy"
run_publish "$base_pkg_dir" "$BASE_RUNTIME_VERSION" "$publish_base_log"

echo "Step 4/13: validate base deploy summary"
base_deploy="$(extract_and_validate_deploy_summary "$publish_base_log" "base")"
base_sync_hint_status="${base_deploy%%|*}"
base_rest="${base_deploy#*|}"
base_update_status="${base_rest%%|*}"
base_rest="${base_rest#*|}"
base_update_error_code="${base_rest%%|*}"
base_manifest_version="${base_rest#*|}"

echo "Step 5/13: wait base runtime readiness on target"
wait_runtime_readiness_full "$BASE_RUNTIME_NAME" "$BASE_RUNTIME_VERSION"

echo "Step 6/13: publish layering config_only runtime with deploy"
run_publish "$cfg_pkg_dir" "$CONFIG_RUNTIME_VERSION" "$publish_cfg_log"

echo "Step 7/13: validate layering config_only deploy summary"
cfg_deploy="$(extract_and_validate_deploy_summary "$publish_cfg_log" "config")"
cfg_sync_hint_status="${cfg_deploy%%|*}"
cfg_rest="${cfg_deploy#*|}"
cfg_update_status="${cfg_rest%%|*}"
cfg_rest="${cfg_rest#*|}"
cfg_update_error_code="${cfg_rest%%|*}"
cfg_manifest_version="${cfg_rest#*|}"

echo "Step 8/13: wait layering config_only readiness (including base_runtime_ready)"
wait_runtime_readiness_config "$CONFIG_RUNTIME_NAME" "$CONFIG_RUNTIME_VERSION" "$cfg_manifest_version"

echo "Step 9/13: spawn node with template defaults + request overrides"
spawn_http="$(spawn_node)"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi

echo "Step 10/13: wait node lifecycle RUNNING (or STARTING active) and runtime resolution"
lifecycle_observed="$(wait_node_active_with_runtime "$CONFIG_RUNTIME_NAME" "$CONFIG_RUNTIME_VERSION")"

echo "Step 11/13: verify template defaults, request overrides, and forced _system"
package_path_observed="$(assert_layered_config)"

echo "Step 12/13: cleanup node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null || true

echo "Step 13/13: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "base_runtime=$BASE_RUNTIME_NAME@$BASE_RUNTIME_VERSION"
echo "config_runtime=$CONFIG_RUNTIME_NAME@$CONFIG_RUNTIME_VERSION"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "base_sync_hint_status=$base_sync_hint_status"
echo "base_update_status=$base_update_status"
echo "base_update_error_code=$base_update_error_code"
echo "base_manifest_version=$base_manifest_version"
echo "config_sync_hint_status=$cfg_sync_hint_status"
echo "config_update_status=$cfg_update_status"
echo "config_update_error_code=$cfg_update_error_code"
echo "config_manifest_version=$cfg_manifest_version"
echo "lifecycle_observed=$lifecycle_observed"
echo "package_path_observed=$package_path_observed"
echo "runtime packaging PUB-T26 template layering E2E passed."
