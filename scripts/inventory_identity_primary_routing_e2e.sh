#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
PRIMARY_HIVE_ID="${PRIMARY_HIVE_ID:-motherbee}"
PRIMARY_IDENTITY_SERVICE="${PRIMARY_IDENTITY_SERVICE:-sy-identity}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
TEST_ID="${TEST_ID:-invd45-$(date +%s)-${RANDOM}}"
POS_NODE_NAME="${POS_NODE_NAME:-WF.inventory.identity.d45.pos.${TEST_ID}}"
NEG_NODE_NAME="${NEG_NODE_NAME:-WF.inventory.identity.d45.neg.${TEST_ID}}"
SERVICE_WAIT_SECS="${SERVICE_WAIT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-1}"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_pos_body="$tmpdir/spawn_pos.json"
spawn_neg_body="$tmpdir/spawn_neg.json"
kill_body="$tmpdir/kill.json"

stopped_primary_identity="0"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$POS_NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NEG_NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  if [[ "$stopped_primary_identity" == "1" ]]; then
    as_root_local systemctl start "$PRIMARY_IDENTITY_SERVICE" >/dev/null 2>&1 || true
  fi
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

validate_tenant_id() {
  local tenant_id="$1"
  if [[ -z "$tenant_id" ]]; then
    echo "FAIL: TENANT_ID is required (format: tnt:<uuid-v4>)" >&2
    exit 1
  fi
  if [[ "$tenant_id" == *"<"* || "$tenant_id" == *">"* ]]; then
    echo "FAIL: TENANT_ID looks like placeholder ('$tenant_id'). Use a real tnt:<uuid-v4>." >&2
    exit 1
  fi
  if [[ ! "$tenant_id" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$tenant_id' (expected tnt:<uuid-v4>)." >&2
    exit 1
  fi
}

spawn_node() {
  local node_name="$1"
  local out_file="$2"
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
    "$node_name" "$RUNTIME" "$RUNTIME_VERSION" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

wait_service_state() {
  local expected="$1" # active|inactive
  local deadline=$(( $(date +%s) + SERVICE_WAIT_SECS ))
  while (( $(date +%s) <= deadline )); do
    if [[ "$expected" == "active" ]]; then
      if as_root_local systemctl is-active --quiet "$PRIMARY_IDENTITY_SERVICE"; then
        return 0
      fi
    else
      if ! as_root_local systemctl is-active --quiet "$PRIMARY_IDENTITY_SERVICE"; then
        return 0
      fi
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
  return 1
}

primary_identity_target() {
  echo "SY.identity@${PRIMARY_HIVE_ID}"
}

local_identity_target() {
  echo "SY.identity@${HIVE_ID}"
}

echo "INVENTORY D4+D5 E2E: BASE=$BASE HIVE_ID=$HIVE_ID PRIMARY_HIVE_ID=$PRIMARY_HIVE_ID RUNTIME=$RUNTIME"

require_cmd curl
require_cmd python3
require_cmd systemctl
validate_tenant_id "$TENANT_ID"

echo "Step 1/8: validate runtime exists in /hives/$HIVE_ID/versions"
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

echo "Step 2/8: cleanup baseline nodes (ignore errors)"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$POS_NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NEG_NODE_NAME" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true

echo "Step 3/8: D4 positive spawn (must route identity write to primary)"
spawn_pos_http="$(spawn_node "$POS_NODE_NAME" "$spawn_pos_body")"
spawn_pos_status="$(json_get_file "status" "$spawn_pos_body")"
if [[ "$spawn_pos_http" != "200" || "$spawn_pos_status" != "ok" ]]; then
  echo "FAIL: positive spawn failed http=$spawn_pos_http status=$spawn_pos_status" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi
id_primary_hive="$(json_get_file "payload.identity.identity_primary_hive_id" "$spawn_pos_body")"
id_target="$(json_get_file "payload.identity.identity_target" "$spawn_pos_body")"
reg_status="$(json_get_file "payload.identity.register.status" "$spawn_pos_body")"
reg_target="$(json_get_file "payload.identity.register.target" "$spawn_pos_body")"
reg_ilk="$(json_get_file "payload.identity.register.ilk_id" "$spawn_pos_body")"
expected_primary_target="$(primary_identity_target)"
expected_local_target="$(local_identity_target)"
if [[ "$id_primary_hive" != "$PRIMARY_HIVE_ID" ]]; then
  echo "FAIL[D4]: identity_primary_hive_id='$id_primary_hive' expected='$PRIMARY_HIVE_ID'" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi
if [[ "$id_target" != "$expected_primary_target" ]]; then
  echo "FAIL[D4]: identity_target='$id_target' expected='$expected_primary_target'" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi
if [[ "$reg_status" != "ok" ]]; then
  echo "FAIL[D4]: identity.register.status='$reg_status' (expected ok)" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi
if [[ "$reg_target" != "$expected_primary_target" ]]; then
  echo "FAIL[D4]: identity.register.target='$reg_target' expected='$expected_primary_target'" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi
if [[ "$id_target" == "$expected_local_target" || "$reg_target" == "$expected_local_target" ]]; then
  echo "FAIL[D4]: detected local identity fallback target='$expected_local_target'" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi
if [[ ! "$reg_ilk" =~ ^ilk:[0-9a-fA-F-]{36}$ ]]; then
  echo "FAIL[D4]: invalid ilk_id from register='$reg_ilk'" >&2
  cat "$spawn_pos_body" >&2 || true
  exit 1
fi

echo "Step 4/8: cleanup positive node"
kill_pos_http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$POS_NODE_NAME" "$kill_body" '{"force":true}')"
kill_pos_status="$(json_get_file "status" "$kill_body")"
if [[ "$kill_pos_http" != "200" || "$kill_pos_status" != "ok" ]]; then
  echo "FAIL: kill positive node failed http=$kill_pos_http status=$kill_pos_status" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi

echo "Step 5/8: stop primary identity service ($PRIMARY_IDENTITY_SERVICE) to force D5 failure path"
if ! as_root_local systemctl is-active --quiet "$PRIMARY_IDENTITY_SERVICE"; then
  echo "FAIL: primary identity service '$PRIMARY_IDENTITY_SERVICE' is not active before D5" >&2
  exit 1
fi
as_root_local systemctl stop "$PRIMARY_IDENTITY_SERVICE"
stopped_primary_identity="1"
if ! wait_service_state "inactive"; then
  echo "FAIL: timeout waiting service '$PRIMARY_IDENTITY_SERVICE' to stop" >&2
  exit 1
fi

echo "Step 6/8: D5 negative spawn (must fail explicitly, no local fallback)"
spawn_neg_http="$(spawn_node "$NEG_NODE_NAME" "$spawn_neg_body")"
spawn_neg_status="$(json_get_file "status" "$spawn_neg_body")"
spawn_neg_code="$(json_get_file "error_code" "$spawn_neg_body")"
spawn_neg_payload_code="$(json_get_file "payload.error_code" "$spawn_neg_body")"
spawn_neg_msg="$(json_get_file "payload.message" "$spawn_neg_body")"
spawn_neg_detail="$(json_get_file "error_detail" "$spawn_neg_body")"
effective_code="$spawn_neg_payload_code"
if [[ -z "$effective_code" ]]; then
  effective_code="$spawn_neg_code"
fi
if [[ "$spawn_neg_status" == "ok" ]]; then
  echo "FAIL[D5]: negative spawn unexpectedly succeeded" >&2
  cat "$spawn_neg_body" >&2 || true
  exit 1
fi
if [[ "$effective_code" != "IDENTITY_REGISTER_FAILED" ]]; then
  echo "FAIL[D5]: expected IDENTITY_REGISTER_FAILED, got '$effective_code' (http=$spawn_neg_http)" >&2
  cat "$spawn_neg_body" >&2 || true
  exit 1
fi
if [[ "$spawn_neg_msg" == *"$expected_local_target"* || "$spawn_neg_detail" == *"$expected_local_target"* ]]; then
  echo "FAIL[D5]: failure references local identity target '$expected_local_target' (fallback detected)" >&2
  cat "$spawn_neg_body" >&2 || true
  exit 1
fi

echo "Step 7/8: restart primary identity service and wait healthy"
as_root_local systemctl start "$PRIMARY_IDENTITY_SERVICE"
if ! wait_service_state "active"; then
  echo "FAIL: timeout waiting service '$PRIMARY_IDENTITY_SERVICE' to become active after D5" >&2
  exit 1
fi
stopped_primary_identity="0"

echo "Step 8/8: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "primary_hive_id=$PRIMARY_HIVE_ID"
echo "tenant_id=$TENANT_ID"
echo "positive_node_name=$POS_NODE_NAME@$HIVE_ID"
echo "negative_node_name=$NEG_NODE_NAME@$HIVE_ID"
echo "positive_identity_target=$id_target"
echo "negative_error_code=$effective_code"
echo "inventory D4+D5 identity primary routing E2E passed."
