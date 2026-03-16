#!/usr/bin/env bash
set -euo pipefail

# FR-03 / INV-D6 E2E:
# Verifica precedencia de node_name@hive sobre hive del endpoint:
# - POST /hives/{request_hive}/nodes con node_name "...@{target_hive}"
# - spawn/identity deben resolverse en {target_hive}
# - kill via request_hive también debe operar sobre {target_hive}
# - chequeo estricto de presencia en inventory requiere runtime de larga vida
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   REQUEST_HIVE_ID="motherbee" \
#   TARGET_HIVE_ID="worker-220" \
#   RUNTIME="wf.orch.diag" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/inventory_node_name_cross_hive_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
REQUEST_HIVE_ID="${REQUEST_HIVE_ID:-motherbee}"
TARGET_HIVE_ID="${TARGET_HIVE_ID:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
TEST_ID="${TEST_ID:-fr3d6-$(date +%s)-${RANDOM}}"
NODE_NAME_BASE="${NODE_NAME_BASE:-WF.inventory.crosshive.${TEST_ID}}"
NODE_FQN="${NODE_NAME_BASE}@${TARGET_HIVE_ID}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-1}"
APPEAR_TIMEOUT_SECS="${APPEAR_TIMEOUT_SECS:-45}"
DISAPPEAR_TIMEOUT_SECS="${DISAPPEAR_TIMEOUT_SECS:-45}"
REQUIRE_INVENTORY_PRESENT="${REQUIRE_INVENTORY_PRESENT:-0}"
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
WAIT_UPDATE_SECS="${WAIT_UPDATE_SECS:-120}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
AUTO_SEED_RUNTIME_IF_MISSING="${AUTO_SEED_RUNTIME_IF_MISSING:-1}"
RUNTIME_SEED_BUILD_BIN="${RUNTIME_SEED_BUILD_BIN:-1}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNTIME_SEED_SCRIPT="$ROOT_DIR/scripts/inventory_spawn_kill_e2e.sh"
STRICT_RUNTIME_VERSION="${STRICT_RUNTIME_VERSION:-diag-fr3-${TEST_ID}}"
EFFECTIVE_RUNTIME_VERSION="$RUNTIME_VERSION"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_body="$tmpdir/spawn.json"
kill_body="$tmpdir/kill.json"
inventory_target_body="$tmpdir/inventory_target.json"
inventory_request_body="$tmpdir/inventory_request.json"
sync_hint_body="$tmpdir/sync_hint.json"
update_body="$tmpdir/update.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$REQUEST_HIVE_ID/nodes/$NODE_FQN" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$TARGET_HIVE_ID/nodes/$NODE_FQN" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
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

validate_tenant_id() {
  local tenant_id="$1"
  if [[ -z "$tenant_id" ]]; then
    echo "FAIL: TENANT_ID is required (format: tnt:<uuid-v4>)" >&2
    exit 1
  fi
  if [[ "$tenant_id" == *"<"* || "$tenant_id" == *">"* ]]; then
    echo "FAIL: TENANT_ID looks like placeholder ('$tenant_id'). Use real tnt:<uuid-v4>." >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected tnt:<uuid-v4>)." >&2
    exit 1
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

inventory_contains_node() {
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
    print("0")
    raise SystemExit(0)

nodes = doc.get("payload", {}).get("nodes", [])
if not isinstance(nodes, list):
    print("0")
    raise SystemExit(0)

for node in nodes:
    if isinstance(node, dict) and node.get("node_name") == node_l2:
        print("1")
        raise SystemExit(0)

print("0")
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
  http_call "POST" "$BASE/hives/$TARGET_HIVE_ID/sync-hint" "$out_file" "$payload"
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
  http_call "POST" "$BASE/hives/$TARGET_HIVE_ID/update" "$out_file" "$payload"
}

wait_update_ok() {
  local manifest_version="$1"
  local manifest_hash="$2"
  local out_file="$3"
  local deadline=$(( $(date +%s) + WAIT_UPDATE_SECS ))
  while (( $(date +%s) <= deadline )); do
    local status error_code error_detail
    post_runtime_update "$manifest_version" "$manifest_hash" "$out_file" >/dev/null
    status="$(json_get_file "payload.status" "$out_file")"
    if [[ "$status" == "ok" ]]; then
      return 0
    fi
    if [[ "$status" == "sync_pending" ]]; then
      sleep 2
      continue
    fi
    error_code="$(json_get_file "error_code" "$out_file")"
    error_detail="$(json_get_file "error_detail" "$out_file")"
    if [[ "$error_code" == "TRANSPORT_ERROR" && "$error_detail" == *"UNREACHABLE"* ]]; then
      sleep 2
      continue
    fi
    echo "FAIL: update runtime status='$status'" >&2
    cat "$out_file" >&2 || true
    return 1
  done
  echo "FAIL: update runtime timeout waiting ok" >&2
  cat "$out_file" >&2 || true
  return 1
}

is_numeric() {
  [[ "$1" =~ ^[0-9]+$ ]]
}

materialize_runtime_on_target() {
  local manifest_version="$1"
  local manifest_hash="$2"
  if [[ -z "$manifest_version" || -z "$manifest_hash" ]]; then
    echo "FAIL: cannot materialize runtime (missing manifest_version/hash)" >&2
    return 1
  fi
  if ! is_numeric "$manifest_version"; then
    echo "FAIL: cannot materialize runtime (manifest_version is non-numeric: '$manifest_version')" >&2
    return 1
  fi
  echo "INFO: runtime missing on target, trying sync-hint + update (hive=$TARGET_HIVE_ID runtime=$RUNTIME)" >&2
  wait_sync_hint_ok "$sync_hint_body" || return 1
  wait_update_ok "$manifest_version" "$manifest_hash" "$update_body" || return 1
  return 0
}

seed_runtime_fixture_if_needed() {
  if [[ "$AUTO_SEED_RUNTIME_IF_MISSING" != "1" ]]; then
    return 1
  fi
  if [[ "$RUNTIME" != "wf.inventory.hold.diag" ]]; then
    return 1
  fi
  if [[ ! -x "$RUNTIME_SEED_SCRIPT" ]]; then
    echo "FAIL: runtime seed script missing or not executable: $RUNTIME_SEED_SCRIPT" >&2
    return 1
  fi
  echo "INFO: runtime source still missing after update; seeding '$RUNTIME' automatically via inventory_spawn_kill_e2e.sh." >&2
  if [[ "$RUNTIME_VERSION" == "current" ]]; then
    BASE="$BASE" \
    HIVE_ID="$TARGET_HIVE_ID" \
    BUILD_BIN="$RUNTIME_SEED_BUILD_BIN" \
    PREPARE_ONLY=1 \
    RUNTIME="$RUNTIME" \
    TENANT_ID="$TENANT_ID" \
    TEST_ID="fr3seed-${TEST_ID}" \
    NODE_NAME="$NODE_NAME_BASE" \
    INVENTORY_APPEAR_TIMEOUT_SECS=45 \
    INVENTORY_DISAPPEAR_TIMEOUT_SECS=45 \
    "$RUNTIME_SEED_SCRIPT"
  else
    BASE="$BASE" \
    HIVE_ID="$TARGET_HIVE_ID" \
    BUILD_BIN="$RUNTIME_SEED_BUILD_BIN" \
    PREPARE_ONLY=1 \
    RUNTIME="$RUNTIME" \
    RUNTIME_VERSION="$RUNTIME_VERSION" \
    TENANT_ID="$TENANT_ID" \
    TEST_ID="fr3seed-${TEST_ID}" \
    NODE_NAME="$NODE_NAME_BASE" \
    INVENTORY_APPEAR_TIMEOUT_SECS=45 \
    INVENTORY_DISAPPEAR_TIMEOUT_SECS=45 \
    "$RUNTIME_SEED_SCRIPT"
  fi
}

prepare_strict_runtime_fixture() {
  if [[ "$REQUIRE_INVENTORY_PRESENT" != "1" ]]; then
    return 0
  fi
  if [[ "$RUNTIME" != "wf.inventory.hold.diag" ]]; then
    return 0
  fi
  if [[ ! -x "$RUNTIME_SEED_SCRIPT" ]]; then
    echo "FAIL: runtime seed script missing or not executable: $RUNTIME_SEED_SCRIPT" >&2
    return 1
  fi
  echo "INFO: strict inventory mode: preparing long-lived runtime fixture bound to node '$NODE_NAME_BASE'." >&2
  BASE="$BASE" \
  HIVE_ID="$TARGET_HIVE_ID" \
  BUILD_BIN="$RUNTIME_SEED_BUILD_BIN" \
  PREPARE_ONLY=1 \
  RUNTIME="$RUNTIME" \
  RUNTIME_VERSION="$STRICT_RUNTIME_VERSION" \
  TENANT_ID="$TENANT_ID" \
  TEST_ID="fr3strict-${TEST_ID}" \
  NODE_NAME="$NODE_NAME_BASE" \
  INVENTORY_APPEAR_TIMEOUT_SECS=45 \
  INVENTORY_DISAPPEAR_TIMEOUT_SECS=45 \
  "$RUNTIME_SEED_SCRIPT"
}

wait_inventory_state() {
  local hive_id="$1"
  local node_l2="$2"
  local expected="$3" # present|absent
  local timeout_secs="$4"
  local out_file="$5"
  local started now elapsed http ok payload_ok present
  started="$(date +%s)"

  while true; do
    now="$(date +%s)"
    elapsed=$((now - started))
    if (( elapsed > timeout_secs )); then
      echo "FAIL: timeout waiting inventory hive='$hive_id' node='$node_l2' state='$expected'" >&2
      cat "$out_file" >&2 || true
      return 1
    fi

    http="$(http_call "GET" "$BASE/inventory/$hive_id" "$out_file")"
    if [[ "$http" != "200" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    ok="$(json_get_file "status" "$out_file")"
    payload_ok="$(json_get_file "payload.status" "$out_file")"
    if [[ "$ok" != "ok" || "$payload_ok" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi

    present="$(inventory_contains_node "$out_file" "$node_l2")"
    if [[ "$expected" == "present" && "$present" == "1" ]]; then
      return 0
    fi
    if [[ "$expected" == "absent" && "$present" == "0" ]]; then
      return 0
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

assert_eq() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL[$label]: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

is_long_lived_runtime() {
  case "$1" in
    wf.inventory.hold.diag) return 0 ;;
    *) return 1 ;;
  esac
}

echo "INVENTORY FR-03 D6 E2E: BASE=$BASE REQUEST_HIVE_ID=$REQUEST_HIVE_ID TARGET_HIVE_ID=$TARGET_HIVE_ID RUNTIME=$RUNTIME"

require_cmd curl
require_cmd python3
validate_tenant_id "$TENANT_ID"

if [[ "$REQUEST_HIVE_ID" == "$TARGET_HIVE_ID" ]]; then
  echo "FAIL: REQUEST_HIVE_ID and TARGET_HIVE_ID must be different for FR-03 cross-hive regression." >&2
  exit 1
fi
if [[ "$REQUIRE_INVENTORY_PRESENT" == "1" ]] && ! is_long_lived_runtime "$RUNTIME"; then
  echo "FAIL: REQUIRE_INVENTORY_PRESENT=1 requires a long-lived runtime (recommended: RUNTIME=wf.inventory.hold.diag)." >&2
  echo "Got RUNTIME='$RUNTIME'." >&2
  exit 1
fi

echo "Step 1/8: validate runtime exists in /hives/$TARGET_HIVE_ID/versions"
versions_http="$(http_call "GET" "$BASE/hives/$TARGET_HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http for target hive '$TARGET_HIVE_ID'" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME' missing in target hive '$TARGET_HIVE_ID' versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 2/8: cleanup baseline node (ignore errors)"
http_call "DELETE" "$BASE/hives/$REQUEST_HIVE_ID/nodes/$NODE_FQN" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
http_call "DELETE" "$BASE/hives/$TARGET_HIVE_ID/nodes/$NODE_FQN" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

if ! prepare_strict_runtime_fixture; then
  echo "FAIL: strict runtime fixture preparation failed" >&2
  exit 1
fi
if [[ "$REQUIRE_INVENTORY_PRESENT" == "1" && "$RUNTIME" == "wf.inventory.hold.diag" ]]; then
  EFFECTIVE_RUNTIME_VERSION="$STRICT_RUNTIME_VERSION"
fi

echo "Step 3/8: baseline inventory absent on both request/target hives"
wait_inventory_state "$TARGET_HIVE_ID" "$NODE_FQN" "absent" 10 "$inventory_target_body"
wait_inventory_state "$REQUEST_HIVE_ID" "$NODE_FQN" "absent" 10 "$inventory_request_body"

echo "Step 4/8: cross-hive spawn via request hive endpoint"
spawn_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
  "$NODE_FQN" "$RUNTIME" "$EFFECTIVE_RUNTIME_VERSION" "$TENANT_ID")"
spawn_http="$(http_call "POST" "$BASE/hives/$REQUEST_HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  spawn_code="$(json_get_file "error_code" "$spawn_body")"
  if [[ -z "$spawn_code" ]]; then
    spawn_code="$(json_get_file "payload.error_code" "$spawn_body")"
  fi
  if [[ "$spawn_code" == "RUNTIME_NOT_PRESENT" ]]; then
    manifest_version="$(json_get_file "payload.hive.runtimes.manifest_version" "$versions_body")"
    manifest_hash="$(json_get_file "payload.hive.runtimes.manifest_hash" "$versions_body")"
    if ! materialize_runtime_on_target "$manifest_version" "$manifest_hash"; then
      echo "FAIL: runtime materialization attempt failed; spawn cannot proceed." >&2
      cat "$spawn_body" >&2 || true
      exit 1
    fi
    spawn_http="$(http_call "POST" "$BASE/hives/$REQUEST_HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
    spawn_status="$(json_get_file "status" "$spawn_body")"
    if [[ "$spawn_http" == "200" && "$spawn_status" == "ok" ]]; then
      echo "INFO: spawn succeeded after runtime materialization retry." >&2
    fi
  fi
fi
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  spawn_code="$(json_get_file "error_code" "$spawn_body")"
  if [[ -z "$spawn_code" ]]; then
    spawn_code="$(json_get_file "payload.error_code" "$spawn_body")"
  fi
  if [[ "$spawn_code" == "RUNTIME_NOT_PRESENT" ]]; then
    if seed_runtime_fixture_if_needed; then
      spawn_http="$(http_call "POST" "$BASE/hives/$REQUEST_HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
      spawn_status="$(json_get_file "status" "$spawn_body")"
      if [[ "$spawn_http" == "200" && "$spawn_status" == "ok" ]]; then
        echo "INFO: spawn succeeded after runtime seed fallback." >&2
      fi
      spawn_code="$(json_get_file "error_code" "$spawn_body")"
      if [[ -z "$spawn_code" ]]; then
        spawn_code="$(json_get_file "payload.error_code" "$spawn_body")"
      fi
    fi
  fi
fi
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  spawn_code="$(json_get_file "error_code" "$spawn_body")"
  if [[ -z "$spawn_code" ]]; then
    spawn_code="$(json_get_file "payload.error_code" "$spawn_body")"
  fi
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status code=$spawn_code" >&2
  cat "$spawn_body" >&2 || true
  if [[ "$spawn_code" == "RUNTIME_NOT_PRESENT" ]]; then
    echo "Hint: runtime is still missing on target hive '$TARGET_HIVE_ID' after auto-remediation attempts." >&2
  fi
  exit 1
fi

echo "Step 5/8: assert response routed by node_name@hive (not endpoint hive)"
assert_eq "$(json_get_file "payload.node_name" "$spawn_body")" "$NODE_FQN" "spawn-node-name"
assert_eq "$(json_get_file "payload.target" "$spawn_body")" "$TARGET_HIVE_ID" "spawn-target"
assert_eq "$(json_get_file "payload.hive" "$spawn_body")" "$TARGET_HIVE_ID" "spawn-hive"
assert_eq "$(json_get_file "payload.identity.requested_hive" "$spawn_body")" "$TARGET_HIVE_ID" "identity-requested-hive"
assert_eq "$(json_get_file "payload.identity.identity_primary_hive_id" "$spawn_body")" "motherbee" "identity-primary-hive"

echo "Step 6/8: inventory check (target present if observable, request hive absent)"
observed_present="0"
if wait_inventory_state "$TARGET_HIVE_ID" "$NODE_FQN" "present" "$APPEAR_TIMEOUT_SECS" "$inventory_target_body"; then
  observed_present="1"
else
  if [[ "$REQUIRE_INVENTORY_PRESENT" == "1" ]]; then
    echo "FAIL: node never observed in target inventory and REQUIRE_INVENTORY_PRESENT=1" >&2
    cat "$inventory_target_body" >&2 || true
    exit 1
  fi
  echo "WARN: node not observed in target inventory within timeout (runtime may be short-lived); continuing with routing assertions." >&2
fi
wait_inventory_state "$REQUEST_HIVE_ID" "$NODE_FQN" "absent" 10 "$inventory_request_body"

echo "Step 7/8: cross-hive kill via request hive endpoint"
kill_http="$(http_call "DELETE" "$BASE/hives/$REQUEST_HIVE_ID/nodes/$NODE_FQN" "$kill_body" '{"force":true}')"
kill_status="$(json_get_file "status" "$kill_body")"
if [[ "$kill_http" != "200" || "$kill_status" != "ok" ]]; then
  echo "FAIL: kill failed http=$kill_http status=$kill_status" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi
assert_eq "$(json_get_file "payload.target" "$kill_body")" "$TARGET_HIVE_ID" "kill-target"

echo "Step 8/8: target inventory must remove node"
wait_inventory_state "$TARGET_HIVE_ID" "$NODE_FQN" "absent" "$DISAPPEAR_TIMEOUT_SECS" "$inventory_target_body"

echo "status=ok"
echo "request_hive_id=$REQUEST_HIVE_ID"
echo "target_hive_id=$TARGET_HIVE_ID"
echo "node_name=$NODE_FQN"
echo "runtime=$RUNTIME@$EFFECTIVE_RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "observed_in_target_inventory=$observed_present"
echo "inventory FR-03 D6 cross-hive node_name precedence E2E passed."
