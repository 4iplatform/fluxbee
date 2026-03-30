#!/usr/bin/env bash
set -euo pipefail

# E2E positivo para runtime targeted update:
# 1) build de fixture runtime
# 2) publish local del runtime/version al dist manifest
# 3) sync-hint dist
# 4) SYSTEM_UPDATE category=runtime con scope runtime+runtime_version
# 5) verificación explícita de materialization
# 6) spawn y kill del nodo
#
# Uso:
#   BASE="http://127.0.0.1:8080" HIVE_ID="motherbee" \
#     bash scripts/orchestrator_runtime_targeted_e2e.sh
#
# Variables opcionales:
#   BUILD_BIN=1
#   RUNTIME=wf.inventory.hold.diag
#   RUNTIME_VERSION=diag-20260330123000
#   NODE_NAME=WF.runtime.targeted.<id>
#   TENANT_ID=tnt:<uuid>
#   WAIT_SYNC_HINT_SECS=120
#   WAIT_UPDATE_SECS=120
#   WAIT_RUNTIME_SECS=60
#   WAIT_INVENTORY_SECS=45
#   SYNC_HINT_TIMEOUT_MS=30000

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-}"
BUILD_BIN="${BUILD_BIN:-1}"
RUNTIME="${RUNTIME:-wf.inventory.hold.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-diag-$(date +%Y%m%d%H%M%S)}"
TEST_ID="${TEST_ID:-rt-targeted-$(date +%s)-$RANDOM}"
NODE_NAME="${NODE_NAME:-WF.runtime.targeted.${TEST_ID}}"
TENANT_ID="${TENANT_ID:-}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"
WAIT_RUNTIME_SECS="${WAIT_RUNTIME_SECS:-60}"
WAIT_INVENTORY_SECS="${WAIT_INVENTORY_SECS:-45}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${TARGETED_RUNTIME_BIN_PATH:-$ROOT_DIR/target/release/inventory_hold_diag}"

tmpdir="$(mktemp -d)"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"
runtime_body="$tmpdir/runtime.json"
spawn_body="$tmpdir/spawn.json"
kill_body="$tmpdir/kill.json"
inventory_body="$tmpdir/inventory.json"
node_status_body="$tmpdir/node_status.json"
publish_log="$tmpdir/publish.log"
wrapper_start="$tmpdir/start.sh"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  fi
}

load_local_hive_id() {
  local path=""
  if [[ -f /etc/fluxbee/hive.yaml ]]; then
    path="/etc/fluxbee/hive.yaml"
  elif [[ -f "$ROOT_DIR/config/hive.yaml" ]]; then
    path="$ROOT_DIR/config/hive.yaml"
  fi
  if [[ -n "$path" ]]; then
    awk -F': *' '/^hive_id:/ {print $2; exit}' "$path" | tr -d '"'
  fi
}

resolve_effective_tenant_id() {
  if [[ -n "${TENANT_ID:-}" ]]; then
    printf '%s\n' "$TENANT_ID"
    return 0
  fi
  if [[ -n "${ORCH_DEFAULT_TENANT_ID:-}" ]]; then
    printf '%s\n' "$ORCH_DEFAULT_TENANT_ID"
    return 0
  fi
}

validate_tenant_id() {
  local tenant_id="${1:-}"
  if [[ -z "$tenant_id" ]]; then
    echo "FAIL: TENANT_ID is required for this E2E because the selected runtime requires identity registration at spawn time." >&2
    echo "Hint: pass TENANT_ID=tnt:<uuid-v4> or set ORCH_DEFAULT_TENANT_ID in sy-orchestrator." >&2
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

json_pick() {
  local file="$1"
  shift
  python3 - "$file" "$@" <<'PY'
import json
import sys

file_path = sys.argv[1]
keys = sys.argv[2:]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        value = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

for key in keys:
    if isinstance(value, dict):
        value = value.get(key)
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

inventory_has_node() {
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
    print("false")
    raise SystemExit(0)

nodes = (((doc or {}).get("payload") or {}).get("nodes")) or []
found = False
for node in nodes:
    if isinstance(node, dict) and node.get("node_name") == node_l2:
        found = True
        break
print("true" if found else "false")
PY
}

post_sync_hint() {
  local out_file="$1"
  local payload
  payload="{\"channel\":\"dist\",\"folder_id\":\"fluxbee-dist\",\"wait_for_idle\":true,\"timeout_ms\":$SYNC_HINT_TIMEOUT_MS}"
  http_call "POST" "$BASE/hives/$HIVE_ID/sync-hint" "$out_file" "$payload"
}

wait_sync_hint_ok() {
  local deadline=$(( $(date +%s) + WAIT_SYNC_HINT_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status payload_status
    http_status="$(post_sync_hint "$sync_hint_body")"
    payload_status="$(json_pick "$sync_hint_body" payload status)"
    if [[ "$payload_status" == "ok" ]]; then
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: sync-hint status='$payload_status' http=$http_status" >&2
    cat "$sync_hint_body" >&2 || true
    return 1
  done
  echo "FAIL: sync-hint timeout waiting ok" >&2
  cat "$sync_hint_body" >&2 || true
  return 1
}

post_targeted_runtime_update() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local payload
  payload="$(printf '{"category":"runtime","manifest_version":%s,"manifest_hash":"%s","runtime":"%s","runtime_version":"%s"}' \
    "$manifest_version" "$manifest_hash" "$RUNTIME" "$RUNTIME_VERSION")"
  http_call "POST" "$BASE/hives/$HIVE_ID/update" "$update_body" "$payload"
}

wait_targeted_update_ok() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status payload_status error_code error_detail
    http_status="$(post_targeted_runtime_update "$manifest_version" "$manifest_hash")"
    payload_status="$(json_pick "$update_body" payload status)"
    if [[ "$payload_status" == "ok" ]]; then
      local readiness_scope targeted_status
      readiness_scope="$(json_pick "$update_body" payload readiness_scope)"
      targeted_status="$(json_pick "$update_body" payload targeted_runtime_health status)"
      if [[ "$readiness_scope" != "targeted" ]]; then
        echo "FAIL: expected payload.readiness_scope=targeted, got '$readiness_scope'" >&2
        cat "$update_body" >&2 || true
        return 1
      fi
      if [[ "$targeted_status" != "ok" ]]; then
        echo "FAIL: expected payload.targeted_runtime_health.status=ok, got '$targeted_status'" >&2
        cat "$update_body" >&2 || true
        return 1
      fi
      return 0
    fi
    if [[ "$payload_status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    error_code="$(json_pick "$update_body" error_code)"
    error_detail="$(json_pick "$update_body" error_detail)"
    if [[ "$error_code" == "TRANSPORT_ERROR" && "$error_detail" == *"UNREACHABLE"* ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: targeted runtime update status='$payload_status' http=$http_status" >&2
    cat "$update_body" >&2 || true
    return 1
  done
  echo "FAIL: targeted runtime update timeout waiting ok" >&2
  cat "$update_body" >&2 || true
  return 1
}

wait_runtime_materialized() {
  local deadline=$(( $(date +%s) + WAIT_RUNTIME_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status runtime_status materialized start_present start_exec blocking_reason
    http_status="$(http_call "GET" "$BASE/hives/$HIVE_ID/runtimes/$RUNTIME" "$runtime_body")"
    if [[ "$http_status" != "200" ]]; then
      sleep 2
      continue
    fi
    runtime_status="$(json_pick "$runtime_body" payload status)"
    materialized="$(json_pick "$runtime_body" payload materialization versions "$RUNTIME_VERSION" materialized)"
    start_present="$(json_pick "$runtime_body" payload materialization versions "$RUNTIME_VERSION" start_script_present)"
    start_exec="$(json_pick "$runtime_body" payload materialization versions "$RUNTIME_VERSION" start_script_executable)"
    if [[ "$runtime_status" == "ok" && "$materialized" == "true" && "$start_present" == "true" && "$start_exec" == "true" ]]; then
      return 0
    fi
    blocking_reason="$(json_pick "$runtime_body" payload materialization versions "$RUNTIME_VERSION" blocking_reason)"
    echo "WAIT: runtime materialization runtime=$RUNTIME version=$RUNTIME_VERSION status=${runtime_status:-unknown} materialized=${materialized:-} blocking_reason=${blocking_reason:-}" >&2
    sleep 2
  done
  echo "FAIL: runtime materialization not ready for $RUNTIME@$RUNTIME_VERSION" >&2
  cat "$runtime_body" >&2 || true
  return 1
}

wait_inventory_state() {
  local expected="$1"
  local node_l2="${NODE_NAME}@${HIVE_ID}"
  local deadline=$(( $(date +%s) + WAIT_INVENTORY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status present
    http_status="$(http_call "GET" "$BASE/inventory/$HIVE_ID" "$inventory_body")"
    if [[ "$http_status" != "200" ]]; then
      sleep 2
      continue
    fi
    present="$(inventory_has_node "$inventory_body" "$node_l2")"
    if [[ "$expected" == "present" && "$present" == "true" ]]; then
      return 0
    fi
    if [[ "$expected" == "absent" && "$present" == "false" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: timeout waiting inventory state '$expected' for ${NODE_NAME}@${HIVE_ID}" >&2
  cat "$inventory_body" >&2 || true
  return 1
}

wait_node_absent() {
  local deadline=$(( $(date +%s) + WAIT_INVENTORY_SECS ))
  while (( $(date +%s) <= deadline )); do
    local http_status error_code payload_error_code
    http_status="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/status" "$node_status_body")"
    error_code="$(json_pick "$node_status_body" error_code)"
    payload_error_code="$(json_pick "$node_status_body" payload error_code)"
    if [[ "$http_status" == "404" || "$error_code" == "NODE_NOT_FOUND" || "$payload_error_code" == "NODE_NOT_FOUND" ]]; then
      return 0
    fi
    sleep 2
  done
  echo "FAIL: timeout waiting node status absence for ${NODE_NAME}@${HIVE_ID}" >&2
  cat "$node_status_body" >&2 || true
  return 1
}

require_cmd bash
require_cmd curl
require_cmd python3
require_cmd sudo

LOCAL_HIVE_ID="$(load_local_hive_id || true)"
if [[ -z "$HIVE_ID" ]]; then
  HIVE_ID="${LOCAL_HIVE_ID:-motherbee}"
fi

if [[ -n "$LOCAL_HIVE_ID" && "$HIVE_ID" != "$LOCAL_HIVE_ID" ]]; then
  echo "FAIL: scripts/orchestrator_runtime_targeted_e2e.sh is local-hive only in this iteration." >&2
  echo "requested_hive=$HIVE_ID local_hive=$LOCAL_HIVE_ID" >&2
  echo "Reason: /hives/{hive}/sync-hint and /hives/{hive}/update relay to SY.orchestrator@{hive}, and worker hives do not expose a remote orchestrator node." >&2
  echo "Run this E2E against the local hive (typically motherbee), and leave remote-worker validation to dev." >&2
  exit 1
fi

EFFECTIVE_TENANT_ID="$(resolve_effective_tenant_id || true)"

echo "TARGETED RUNTIME E2E: BASE=$BASE HIVE_ID=$HIVE_ID RUNTIME=$RUNTIME VERSION=$RUNTIME_VERSION NODE_NAME=$NODE_NAME"

echo "Step 1/8: build inventory_hold_diag"
if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  require_cmd cargo
  (cd "$ROOT_DIR" && cargo build --release --bin inventory_hold_diag)
fi
if [[ ! -x "$BIN_PATH" ]]; then
  echo "FAIL: runtime binary missing at $BIN_PATH" >&2
  exit 1
fi

echo "Step 2/8: publish runtime locally into dist manifest"
(
  cd "$ROOT_DIR"
  bash scripts/publish-runtime.sh \
    --runtime "$RUNTIME" \
    --version "$RUNTIME_VERSION" \
    --binary "$BIN_PATH" \
    --set-current \
    --sudo
) | tee "$publish_log"

manifest_version="$(sed -n 's/^manifest_version=//p' "$publish_log" | tail -n1)"
manifest_hash="$(sed -n 's/^manifest_hash=//p' "$publish_log" | tail -n1)"
if [[ -z "$manifest_version" || -z "$manifest_hash" ]]; then
  echo "FAIL: publish-runtime.sh did not return manifest metadata" >&2
  cat "$publish_log" >&2 || true
  exit 1
fi

echo "Step 2b/8: rewrite runtime wrapper with explicit node identity for the fixture"
cat >"$wrapper_start" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export INVENTORY_HOLD_NODE_NAME="${NODE_NAME}"
export INVENTORY_HOLD_NODE_VERSION="0.0.1"
export INVENTORY_HOLD_SECS="0"
exec "\$(dirname "\${BASH_SOURCE[0]}")/inventory_hold_diag"
EOF
if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  install -m 0755 "$wrapper_start" "/var/lib/fluxbee/dist/runtimes/${RUNTIME}/${RUNTIME_VERSION}/bin/start.sh"
else
  sudo install -m 0755 "$wrapper_start" "/var/lib/fluxbee/dist/runtimes/${RUNTIME}/${RUNTIME_VERSION}/bin/start.sh"
fi

echo "Step 3/8: wait sync-hint dist"
wait_sync_hint_ok

echo "Step 4/8: SYSTEM_UPDATE runtime targeted to $RUNTIME@$RUNTIME_VERSION"
wait_targeted_update_ok "$manifest_version" "$manifest_hash"

echo "Step 5/8: verify runtime materialization on target hive"
wait_runtime_materialized

echo "Step 6/8: cleanup baseline node if present"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "Step 7/8: spawn exact runtime version"
validate_tenant_id "$EFFECTIVE_TENANT_ID"
if [[ -n "$EFFECTIVE_TENANT_ID" ]]; then
  spawn_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
    "$NODE_NAME" "$RUNTIME" "$RUNTIME_VERSION" "$EFFECTIVE_TENANT_ID")"
else
  spawn_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s"}' \
    "$NODE_NAME" "$RUNTIME" "$RUNTIME_VERSION")"
fi
spawn_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
spawn_status="$(json_pick "$spawn_body" status)"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi
wait_inventory_state "present"

echo "Step 8/8: kill node and verify cleanup"
kill_http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}')"
kill_status="$(json_pick "$kill_body" status)"
if [[ "$kill_http" != "200" || "$kill_status" != "ok" ]]; then
  echo "FAIL: kill failed http=$kill_http status=$kill_status" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi
wait_node_absent

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "manifest_version=$manifest_version"
echo "manifest_hash=$manifest_hash"
echo "tenant_id=$EFFECTIVE_TENANT_ID"
echo "orchestrator targeted runtime update E2E passed."
