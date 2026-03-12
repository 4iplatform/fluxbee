#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-sandbox}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
TEST_ID="${TEST_ID:-idnreg-$(date +%s)-${RANDOM}}"
NODE_NAME="${NODE_NAME:-WF.identity.node.reg.${TEST_ID}}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
REQUIRE_IDENTITY_REGISTER_OK="${REQUIRE_IDENTITY_REGISTER_OK:-1}"

tmpdir="$(mktemp -d)"
cleanup() {
  local _ec=$?
  rm -rf "$tmpdir"
  return "$_ec"
}
trap cleanup EXIT

versions_body="$tmpdir/versions.json"
spawn1_body="$tmpdir/spawn1.json"
spawn2_body="$tmpdir/spawn2.json"
kill_body="$tmpdir/kill.json"

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  }
}

require_cmd curl
require_cmd python3

validate_tenant_id() {
  local tenant_id="$1"
  if [[ -z "$tenant_id" ]]; then
    return 0
  fi
  if [[ "$tenant_id" == *"<"* || "$tenant_id" == *">"* ]]; then
    echo "FAIL: TENANT_ID looks like a placeholder ('$tenant_id'). Use a real value: tnt:<uuid-v4>." >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected format: tnt:<uuid-v4>)." >&2
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
    doc = json.load(open(file_path, "r", encoding="utf-8"))
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

kill_node_ignore() {
  local node_name="$1"
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$node_name" "$kill_body" "{\"force\":true}" >/dev/null 2>&1 || true
}

spawn_node() {
  local node_name="$1"
  local out_file="$2"
  local payload
  if [[ -n "$TENANT_ID" ]]; then
    payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
      "$node_name" "$RUNTIME" "$RUNTIME_VERSION" "$TENANT_ID")"
  else
    payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s"}' \
      "$node_name" "$RUNTIME" "$RUNTIME_VERSION")"
  fi
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

check_register_contract() {
  local file="$1"
  local label="$2"
  local reg_status reg_reason reg_ilk reg_code

  reg_status="$(json_get_file "payload.identity.register.status" "$file")"
  reg_reason="$(json_get_file "payload.identity.register.reason" "$file")"
  reg_ilk="$(json_get_file "payload.identity.register.ilk_id" "$file")"
  reg_code="$(json_get_file "payload.identity.register.error_code" "$file")"

  if [[ "$REQUIRE_IDENTITY_REGISTER_OK" == "1" && "$reg_status" != "ok" ]]; then
    echo "FAIL[$label]: payload.identity.register.status='$reg_status' reason='$reg_reason'" >&2
    if [[ "$reg_code" == "INVALID_TENANT" ]]; then
      echo "Hint: TENANT_ID does not exist in identity primary. Use a fresh tenant_id from scripts/identity_provision_complete_e2e.sh output (TENANT_ID=...)." >&2
    elif [[ "$reg_code" == "INVALID_TENANT_TRANSITION" ]]; then
      echo "Hint: NODE_NAME already exists under another tenant in identity primary. Use a fresh NODE_NAME or keep the original tenant for this node_name." >&2
    else
      echo "Hint: provide TENANT_ID=tnt:<uuid> or set ORCH_DEFAULT_TENANT_ID in sy-orchestrator environment." >&2
    fi
    cat "$file" >&2 || true
    exit 1
  fi
  if [[ "$reg_status" == "ok" && ! "$reg_ilk" =~ ^ilk:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL[$label]: invalid payload.identity.register.ilk_id='$reg_ilk'" >&2
    cat "$file" >&2 || true
    exit 1
  fi
}

echo "IDENTITY node registration E2E: BASE=$BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME TEST_ID=$TEST_ID REQUIRE_IDENTITY_REGISTER_OK=$REQUIRE_IDENTITY_REGISTER_OK"
validate_tenant_id "$TENANT_ID"

echo "Step 1/6: validate runtime exists in /hives/$HIVE_ID/versions"
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

echo "Step 2/6: cleanup baseline node (ignore errors)"
kill_node_ignore "$NODE_NAME"

echo "Step 3/6: first spawn"
spawn1_http="$(spawn_node "$NODE_NAME" "$spawn1_body")"
spawn1_status="$(json_get_file "status" "$spawn1_body")"
if [[ "$spawn1_http" != "200" || "$spawn1_status" != "ok" ]]; then
  echo "FAIL: first spawn failed http=$spawn1_http status=$spawn1_status" >&2
  cat "$spawn1_body" >&2 || true
  exit 1
fi
check_register_contract "$spawn1_body" "spawn1"
ilk_1="$(json_get_file "payload.identity.register.ilk_id" "$spawn1_body")"

echo "Step 4/6: kill first node"
kill1_http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}')"
kill1_status="$(json_get_file "status" "$kill_body")"
if [[ "$kill1_http" != "200" || "$kill1_status" != "ok" ]]; then
  echo "FAIL: first kill failed http=$kill1_http status=$kill1_status" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi

echo "Step 5/6: second spawn with same node_name (expect same ILK)"
spawn2_http="$(spawn_node "$NODE_NAME" "$spawn2_body")"
spawn2_status="$(json_get_file "status" "$spawn2_body")"
if [[ "$spawn2_http" != "200" || "$spawn2_status" != "ok" ]]; then
  echo "FAIL: second spawn failed http=$spawn2_http status=$spawn2_status" >&2
  cat "$spawn2_body" >&2 || true
  exit 1
fi
check_register_contract "$spawn2_body" "spawn2"
ilk_2="$(json_get_file "payload.identity.register.ilk_id" "$spawn2_body")"
if [[ -n "$ilk_1" && -n "$ilk_2" && "$ilk_1" != "$ilk_2" ]]; then
  echo "FAIL: ILK changed across restart for same node_name ilk1=$ilk_1 ilk2=$ilk_2" >&2
  cat "$spawn1_body" >&2 || true
  cat "$spawn2_body" >&2 || true
  exit 1
fi

echo "Step 6/6: final kill + summary"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "node_name=$NODE_NAME@$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "ilk_id_first=$ilk_1"
echo "ilk_id_second=$ilk_2"
echo "identity node registration E2E passed."
