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

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_body="$tmpdir/spawn.json"
kill_body="$tmpdir/kill.json"
inventory_target_body="$tmpdir/inventory_target.json"
inventory_request_body="$tmpdir/inventory_request.json"

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

echo "Step 3/8: baseline inventory absent on both request/target hives"
wait_inventory_state "$TARGET_HIVE_ID" "$NODE_FQN" "absent" 10 "$inventory_target_body"
wait_inventory_state "$REQUEST_HIVE_ID" "$NODE_FQN" "absent" 10 "$inventory_request_body"

echo "Step 4/8: cross-hive spawn via request hive endpoint"
spawn_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
  "$NODE_FQN" "$RUNTIME" "$RUNTIME_VERSION" "$TENANT_ID")"
spawn_http="$(http_call "POST" "$BASE/hives/$REQUEST_HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
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
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "observed_in_target_inventory=$observed_present"
echo "inventory FR-03 D6 cross-hive node_name precedence E2E passed."
