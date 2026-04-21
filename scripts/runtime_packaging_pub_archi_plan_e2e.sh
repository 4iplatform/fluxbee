#!/usr/bin/env bash
set -euo pipefail

# Archi/executor E2E for canonical runtime publication:
# plan 1 publish_runtime_package(bundle_upload) -> sync/update
# external readiness wait
# plan 2 run_node
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   ARCHI_BASE="http://127.0.0.1:3000" \
#   HIVE_ID="motherbee" \
#   MOTHER_HIVE_ID="motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/runtime_packaging_pub_archi_plan_e2e.sh

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BASE="${BASE:-http://127.0.0.1:8080}"
ARCHI_BASE="${ARCHI_BASE:-http://127.0.0.1:3000}"
HIVE_ID="${HIVE_ID:-motherbee}"
MOTHER_HIVE_ID="${MOTHER_HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-}"
WAIT_READY_SECS="${WAIT_READY_SECS:-120}"
WAIT_STATUS_SECS="${WAIT_STATUS_SECS:-90}"
TEST_ID="${TEST_ID:-archipub-$(date +%s)-$RANDOM}"
SESSION_ID="${SESSION_ID:-$(python3 -c 'import uuid; print(uuid.uuid4())')}"
RUNTIME_NAME="${RUNTIME_NAME:-wf.publish.archi.diag.$(date +%s)}"
RUNTIME_VERSION="${RUNTIME_VERSION:-1.0.0-$TEST_ID}"
NODE_NAME="${NODE_NAME:-WF.publish.archi.$TEST_ID}"
NODE_NAME_L2="$NODE_NAME@$HIVE_ID"

MANIFEST_PATH="/var/lib/fluxbee/dist/runtimes/manifest.json"
DIST_RUNTIME_DIR="/var/lib/fluxbee/dist/runtimes/$RUNTIME_NAME"
DIAG_BIN="$ROOT_DIR/target/release/inventory_hold_diag"
BLOB_ROOT="${BLOB_ROOT:-/var/lib/fluxbee/blob}"
BLOB_REL_BASE="${BLOB_REL_BASE:-packages/incoming}"

tmpdir="$(mktemp -d)"
pkg_dir="$tmpdir/pkg"
manifest_backup="$tmpdir/manifest.backup.json"
bundle_zip="$tmpdir/runtime_bundle.zip"
bundle_blob_rel="$BLOB_REL_BASE/$RUNTIME_NAME-$RUNTIME_VERSION.zip"
bundle_blob_abs="$BLOB_ROOT/$bundle_blob_rel"
versions_body="$tmpdir/versions.json"
status_body="$tmpdir/status.json"
kill_body="$tmpdir/kill.json"
publish_plan_body="$tmpdir/archi_publish_plan.json"
publish_plan_resp="$tmpdir/archi_publish_response.json"
spawn_plan_body="$tmpdir/archi_spawn_plan.json"
spawn_plan_resp="$tmpdir/archi_spawn_response.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ -f "$manifest_backup" ]]; then
    as_root_local install -m 0644 "$manifest_backup" "$MANIFEST_PATH" >/dev/null 2>&1 || true
  fi
  as_root_local rm -f "$bundle_blob_abs" >/dev/null 2>&1 || true
  as_root_local rm -rf "$DIST_RUNTIME_DIR" >/dev/null 2>&1 || true
  rm -rf "$tmpdir" >/dev/null 2>&1 || true
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
  echo "FAIL: node did not reach RUNNING/STARTING(active) node='$NODE_NAME_L2'" >&2
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
  "description": "Archi publish E2E full runtime fixture",
  "config_template": "config/default-config.json",
  "entry_point": "bin/start.sh"
}
EOF
  cat >"$pkg_dir/bin/start.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="${INVENTORY_HOLD_NODE_NAME:-${NODE_NAME:-WF.publish.archi}}"
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

write_publish_plan() {
  cat >"$publish_plan_body" <<EOF
{
  "title": "runtime package publish via archi executor",
  "session_id": "$SESSION_ID",
  "plan": {
    "plan_version": "0.1",
    "kind": "executor_plan",
    "metadata": {
      "name": "runtime_package_publish_archi",
      "target_hive": "$MOTHER_HIVE_ID"
    },
    "execution": {
      "strict": true,
      "stop_on_error": true,
      "allow_help_lookup": true,
      "steps": [
        {
          "id": "s1",
          "action": "publish_runtime_package",
          "args": {
            "source": {
              "kind": "bundle_upload",
              "blob_path": "$bundle_blob_rel"
            },
            "sync_to": ["$HIVE_ID"],
            "update_to": ["$HIVE_ID"]
          }
        }
      ]
    }
  }
}
EOF
}

write_spawn_plan() {
  cat >"$spawn_plan_body" <<EOF
{
  "title": "runtime package spawn via archi executor",
  "session_id": "$SESSION_ID",
  "plan": {
    "plan_version": "0.1",
    "kind": "executor_plan",
    "metadata": {
      "name": "runtime_package_spawn_archi",
      "target_hive": "$HIVE_ID"
    },
    "execution": {
      "strict": true,
      "stop_on_error": true,
      "allow_help_lookup": true,
      "steps": [
        {
          "id": "s1",
          "action": "run_node",
          "args": {
            "hive": "$HIVE_ID",
            "node_name": "$NODE_NAME_L2",
            "runtime": "$RUNTIME_NAME",
            "runtime_version": "current",
            "tenant_id": "$TENANT_ID"
          }
        }
      ]
    }
  }
}
EOF
}

executor_post_file() {
  local plan_file="$1"
  local out_file="$2"
  curl -sS -o "$out_file" -w "%{http_code}" -X POST "$ARCHI_BASE/api/executor/plan" \
    -H "Content-Type: application/json" \
    --data-binary "@$plan_file"
}

extract_executor_success() {
  local body_file="$1"
  local expected_action="$2"
  local top_status mode output_status summary_status execution_id events
  top_status="$(json_get_file "status" "$body_file")"
  mode="$(json_get_file "mode" "$body_file")"
  output_status="$(json_get_file "output.status" "$body_file")"
  summary_status="$(json_get_file "output.payload.summary.status" "$body_file")"
  execution_id="$(json_get_file "output.payload.execution_id" "$body_file")"
  events="$(json_get_file "output.payload.events" "$body_file")"

  if [[ "$top_status" != "ok" || "$mode" != "executor" || "$output_status" != "ok" || "$summary_status" != "done" ]]; then
    echo "FAIL: executor plan failed top_status='$top_status' mode='$mode' output_status='$output_status' summary_status='$summary_status'" >&2
    cat "$body_file" >&2 || true
    exit 1
  fi
  if [[ -z "$execution_id" ]]; then
    echo "FAIL: executor response missing execution_id" >&2
    cat "$body_file" >&2 || true
    exit 1
  fi
  if ! echo "$events" | grep -qF "\"step_action\": \"$expected_action\""; then
    echo "FAIL: executor events missing step_action='$expected_action'" >&2
    cat "$body_file" >&2 || true
    exit 1
  fi
  echo "$execution_id"
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

echo "Archi runtime package E2E: ARCHI_BASE=$ARCHI_BASE BASE=$BASE HIVE_ID=$HIVE_ID MOTHER_HIVE_ID=$MOTHER_HIVE_ID RUNTIME=$RUNTIME_NAME VERSION=$RUNTIME_VERSION NODE=$NODE_NAME_L2"

echo "Step 1/11: build binaries (inventory_hold_diag)"
(cd "$ROOT_DIR" && cargo build --release --bin inventory_hold_diag >/dev/null)
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: inventory_hold_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/11: backup manifest + create full_runtime package fixture"
as_root_local install -m 0644 "$MANIFEST_PATH" "$manifest_backup"
create_package_fixture

echo "Step 3/11: build bundle zip and stage it in blob root"
create_zip_bundle "$pkg_dir" "$bundle_zip" "$RUNTIME_NAME"
stage_bundle_in_blob

echo "Step 4/11: submit Archi executor publish plan"
write_publish_plan
publish_http="$(executor_post_file "$publish_plan_body" "$publish_plan_resp")"
if [[ "$publish_http" != "200" ]]; then
  echo "FAIL: unexpected executor publish http=$publish_http" >&2
  cat "$publish_plan_resp" >&2 || true
  exit 1
fi

echo "Step 5/11: validate Archi publish execution summary"
publish_execution_id="$(extract_executor_success "$publish_plan_resp" "publish_runtime_package")"

echo "Step 6/11: wait readiness=true on target hive versions"
wait_runtime_ready

echo "Step 7/11: submit Archi executor spawn plan"
write_spawn_plan
spawn_http="$(executor_post_file "$spawn_plan_body" "$spawn_plan_resp")"
if [[ "$spawn_http" != "200" ]]; then
  echo "FAIL: unexpected executor spawn http=$spawn_http" >&2
  cat "$spawn_plan_resp" >&2 || true
  exit 1
fi

echo "Step 8/11: validate Archi spawn execution summary"
spawn_execution_id="$(extract_executor_success "$spawn_plan_resp" "run_node")"

echo "Step 9/11: wait node lifecycle RUNNING (or STARTING active)"
lifecycle_observed="$(wait_node_active)"

echo "Step 10/11: cleanup node"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null || true

echo "Step 11/11: summary"
echo "status=ok"
echo "archi_base=$ARCHI_BASE"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME_NAME@$RUNTIME_VERSION"
echo "node_name=$NODE_NAME_L2"
echo "publish_execution_id=$publish_execution_id"
echo "spawn_execution_id=$spawn_execution_id"
echo "lifecycle_observed=$lifecycle_observed"
echo "runtime packaging Archi plan E2E passed."
