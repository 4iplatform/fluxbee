#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TEST_ID="${TEST_ID:-invd1-$(date +%s)-${RANDOM}}"
NODE_NAME="${NODE_NAME:-WF.inventory.spawnkill.${TEST_ID}}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-1}"
INVENTORY_APPEAR_TIMEOUT_SECS="${INVENTORY_APPEAR_TIMEOUT_SECS:-30}"
INVENTORY_DISAPPEAR_TIMEOUT_SECS="${INVENTORY_DISAPPEAR_TIMEOUT_SECS:-30}"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
inventory_body="$tmpdir/inventory.json"
spawn_body="$tmpdir/spawn.json"
kill_body="$tmpdir/kill.json"

cleanup() {
  local _ec=$?
  curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" \
    -H "Content-Type: application/json" \
    -d '{"force":true}' >/dev/null 2>&1 || true
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

inventory_node_status() {
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
    print("")
    raise SystemExit(0)

payload = doc.get("payload", {})
nodes = payload.get("nodes", [])
if not isinstance(nodes, list):
    print("")
    raise SystemExit(0)

for node in nodes:
    if not isinstance(node, dict):
        continue
    if node.get("node_name") == node_l2:
        status = node.get("status")
        print("" if status is None else str(status))
        raise SystemExit(0)

print("")
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

wait_for_inventory_state() {
  local node_l2="$1"
  local expected="$2" # present|absent
  local timeout_secs="$3"
  local started_at
  started_at="$(date +%s)"

  while true; do
    local now elapsed http status
    now="$(date +%s)"
    elapsed=$((now - started_at))
    if (( elapsed > timeout_secs )); then
      echo "FAIL: timeout waiting inventory state='$expected' for node '$node_l2'" >&2
      cat "$inventory_body" >&2 || true
      exit 1
    fi

    http="$(http_call "GET" "$BASE/inventory/$HIVE_ID" "$inventory_body")"
    if [[ "$http" != "200" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    if [[ "$(json_get_file "status" "$inventory_body")" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    if [[ "$(json_get_file "payload.status" "$inventory_body")" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi

    status="$(inventory_node_status "$inventory_body" "$node_l2")"
    if [[ "$expected" == "present" && -n "$status" ]]; then
      return 0
    fi
    if [[ "$expected" == "absent" && -z "$status" ]]; then
      return 0
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

require_cmd curl
require_cmd python3

echo "INVENTORY D1 E2E: BASE=$BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME"

echo "Step 1/7: validate runtime exists in /hives/$HIVE_ID/versions"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$versions_http" != "200" ]]; then
  echo "FAIL: versions endpoint HTTP $versions_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if ! runtime_exists_in_manifest "$RUNTIME" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 2/7: cleanup baseline node (ignore errors)"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "Step 3/7: verify node is absent in inventory baseline"
wait_for_inventory_state "$NODE_NAME@$HIVE_ID" "absent" "$INVENTORY_DISAPPEAR_TIMEOUT_SECS"

echo "Step 4/7: spawn node"
spawn_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s"}' "$NODE_NAME" "$RUNTIME" "$RUNTIME_VERSION")")"
spawn_status="$(json_get_file "status" "$spawn_body")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: spawn failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi

echo "Step 5/7: wait until spawned node appears in inventory"
wait_for_inventory_state "$NODE_NAME@$HIVE_ID" "present" "$INVENTORY_APPEAR_TIMEOUT_SECS"

echo "Step 6/7: kill node"
kill_http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}')"
kill_status="$(json_get_file "status" "$kill_body")"
if [[ "$kill_http" != "200" || "$kill_status" != "ok" ]]; then
  echo "FAIL: kill failed http=$kill_http status=$kill_status" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi

echo "Step 7/7: wait until killed node disappears from inventory"
wait_for_inventory_state "$NODE_NAME@$HIVE_ID" "absent" "$INVENTORY_DISAPPEAR_TIMEOUT_SECS"

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "inventory spawn/kill E2E passed."
