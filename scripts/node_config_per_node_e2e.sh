#!/usr/bin/env bash
set -euo pipefail

# FR-05/06 E2E:
# - spawn con config inicial
# - GET config por nodo
# - PUT config patch por nodo (merge)
# - GET final para verificar persistencia y versionado
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="worker-220" \
#   RUNTIME="wf.orch.diag" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/node_config_per_node_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
TEST_ID="${TEST_ID:-cfgpn-$(date +%s)-$RANDOM}"
NODE_NAME="${NODE_NAME:-WF.config.pernode.${TEST_ID}}"

if [[ -z "$TENANT_ID" ]]; then
  echo "FAIL: TENANT_ID is required (format tnt:<uuid>)" >&2
  exit 1
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local key="$1"
  local file="$2"
  python3 - "$key" "$file" <<'PY'
import json
import sys

key = sys.argv[1]
path = sys.argv[2]
try:
    data = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)

value = data
for part in key.split("."):
    if isinstance(value, dict):
        value = value.get(part, "")
    else:
        value = ""
        break

if isinstance(value, (dict, list)):
    print(json.dumps(value))
else:
    print("" if value is None else value)
PY
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

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$body_file" -w "%{http_code}" \
      -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$body_file" -w "%{http_code}" \
      -X "$method" "$url"
  fi
}

runtime_exists() {
  local file="$1"
  local runtime="$2"
  python3 - "$file" "$runtime" <<'PY'
import json
import sys

path, runtime = sys.argv[1], sys.argv[2]
try:
    data = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("0")
    raise SystemExit(0)

payload = data.get("payload") if isinstance(data, dict) else None
versions = payload.get("versions") if isinstance(payload, dict) else None
if not isinstance(versions, list):
    print("0")
    raise SystemExit(0)

for item in versions:
    if isinstance(item, dict) and item.get("runtime") == runtime:
        print("1")
        raise SystemExit(0)
print("0")
PY
}

require_cmd curl
require_cmd python3

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "NODE CONFIG FR-05/06 E2E: BASE=$BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME"

echo "Step 1/7: validate runtime exists in /hives/$HIVE_ID/versions"
versions_body="$tmpdir/versions.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
assert_eq "$status" "200" "versions-http"
assert_eq "$(json_get "status" "$versions_body")" "ok" "versions-status"
if [[ "$(runtime_exists "$versions_body" "$RUNTIME")" != "1" ]]; then
  echo "FAIL: runtime '$RUNTIME' not present on hive '$HIVE_ID'" >&2
  cat "$versions_body" >&2
  exit 1
fi

echo "Step 2/7: cleanup baseline node (ignore errors)"
cleanup_body="$tmpdir/cleanup.json"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$cleanup_body" >/dev/null || true

echo "Step 3/7: spawn node with initial config"
spawn_payload="$(cat <<JSON
{"node_name":"$NODE_NAME","runtime":"$RUNTIME","runtime_version":"$RUNTIME_VERSION","config":{"tenant_id":"$TENANT_ID","model":"diag-v1","temperature":0.7}}
JSON
)"
spawn_body="$tmpdir/spawn.json"
status="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
if [[ "$status" != "200" || "$(json_get "status" "$spawn_body")" != "ok" ]]; then
  echo "FAIL: spawn failed http=$status" >&2
  cat "$spawn_body" >&2
  exit 1
fi
config_path="$(json_get "payload.config.path" "$spawn_body")"
if [[ -z "$config_path" ]]; then
  echo "FAIL: spawn response missing payload.config.path" >&2
  cat "$spawn_body" >&2
  exit 1
fi

echo "Step 4/7: read node config via API"
get1_body="$tmpdir/get1.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" "$get1_body")"
if [[ "$status" != "200" || "$(json_get "status" "$get1_body")" != "ok" ]]; then
  echo "FAIL: get config (initial) failed http=$status" >&2
  cat "$get1_body" >&2
  exit 1
fi
assert_eq "$(json_get "payload.config.model" "$get1_body")" "diag-v1" "get1-model"
assert_eq "$(json_get "payload.config.tenant_id" "$get1_body")" "$TENANT_ID" "get1-tenant"
v1="$(json_get "payload.config_version" "$get1_body")"

echo "Step 5/7: patch node config via API"
patch_payload='{"temperature":0.2,"system_prompt":"hello from FR-05/06"}'
set_body="$tmpdir/set.json"
status="$(http_call "PUT" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" "$set_body" "$patch_payload")"
if [[ "$status" != "200" || "$(json_get "status" "$set_body")" != "ok" ]]; then
  echo "FAIL: put config failed http=$status" >&2
  cat "$set_body" >&2
  exit 1
fi

echo "Step 6/7: read node config again and verify merge + version bump"
get2_body="$tmpdir/get2.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" "$get2_body")"
if [[ "$status" != "200" || "$(json_get "status" "$get2_body")" != "ok" ]]; then
  echo "FAIL: get config (after patch) failed http=$status" >&2
  cat "$get2_body" >&2
  exit 1
fi
assert_eq "$(json_get "payload.config.model" "$get2_body")" "diag-v1" "get2-model-preserved"
assert_eq "$(json_get "payload.config.temperature" "$get2_body")" "0.2" "get2-temperature"
assert_eq "$(json_get "payload.config.system_prompt" "$get2_body")" "hello from FR-05/06" "get2-system-prompt"
v2="$(json_get "payload.config_version" "$get2_body")"
if [[ -z "$v1" || -z "$v2" ]]; then
  echo "FAIL: missing config_version in GET responses" >&2
  cat "$get1_body" >&2
  cat "$get2_body" >&2
  exit 1
fi
if ! [[ "$v2" =~ ^[0-9]+$ && "$v1" =~ ^[0-9]+$ ]]; then
  echo "FAIL: non-numeric config_version (v1='$v1' v2='$v2')" >&2
  exit 1
fi
if (( v2 <= v1 )); then
  echo "FAIL: config_version did not increase (v1=$v1 v2=$v2)" >&2
  exit 1
fi

echo "Step 7/7: final kill + summary"
kill_body="$tmpdir/kill.json"
status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body")"
if [[ "$status" != "200" || "$(json_get "status" "$kill_body")" != "ok" ]]; then
  echo "WARN: final kill returned http=$status" >&2
  cat "$kill_body" >&2
fi

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "config_path=$config_path"
echo "config_version_before=$v1"
echo "config_version_after=$v2"
echo "node config FR-05/06 E2E passed."
