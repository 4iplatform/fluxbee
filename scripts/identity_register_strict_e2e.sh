#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
PRIMARY_HIVE_ID="${PRIMARY_HIVE_ID:-motherbee}"
PRIMARY_IDENTITY_SERVICE="${PRIMARY_IDENTITY_SERVICE:-sy-identity}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TENANT_ID="${TENANT_ID:-}"
TEST_ID="${TEST_ID:-fr2neg-$(date +%s)-${RANDOM}}"
NODE_MISSING_TENANT="${NODE_MISSING_TENANT:-WF.identity.fr2.missing.${TEST_ID}}"
NODE_IDENTITY_DOWN="${NODE_IDENTITY_DOWN:-WF.identity.fr2.down.${TEST_ID}}"
SERVICE_WAIT_SECS="${SERVICE_WAIT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-1}"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn_missing_body="$tmpdir/spawn_missing.json"
spawn_down_body="$tmpdir/spawn_down.json"
kill_body="$tmpdir/kill.json"

stopped_primary_identity="0"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_MISSING_TENANT" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_IDENTITY_DOWN" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
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
    echo "FAIL: TENANT_ID is required for FR2-T7 (identity_unavailable scenario)." >&2
    echo "Hint: pass TENANT_ID=tnt:<uuid-valid-in-primary>." >&2
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

spawn_without_tenant() {
  local node_name="$1"
  local out_file="$2"
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s"}' \
    "$node_name" "$RUNTIME" "$RUNTIME_VERSION")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

spawn_with_tenant() {
  local node_name="$1"
  local out_file="$2"
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
    "$node_name" "$RUNTIME" "$RUNTIME_VERSION" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

effective_error_code() {
  local file="$1"
  local payload_code top_code
  payload_code="$(json_get_file "payload.error_code" "$file")"
  top_code="$(json_get_file "error_code" "$file")"
  if [[ -n "$payload_code" ]]; then
    echo "$payload_code"
  else
    echo "$top_code"
  fi
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

assert_failed_with_identity_register() {
  local file="$1"
  local http_code="$2"
  local label="$3"
  local expected_message_fragment="${4:-}"
  local status code message detail reg_status reg_reason

  status="$(json_get_file "status" "$file")"
  code="$(effective_error_code "$file")"
  message="$(json_get_file "payload.message" "$file")"
  detail="$(json_get_file "error_detail" "$file")"
  reg_status="$(json_get_file "payload.identity.register.status" "$file")"
  reg_reason="$(json_get_file "payload.identity.register.reason" "$file")"

  if [[ "$status" == "ok" ]]; then
    echo "FAIL[$label]: unexpected success http=$http_code" >&2
    if [[ "$reg_status" == "skipped" && "$reg_reason" == "missing_tenant_id" ]]; then
      echo "Hint[$label]: worker sy-orchestrator still runs old logic (soft-fail register). Deploy/restart latest sy-orchestrator on worker and retry." >&2
    fi
    cat "$file" >&2 || true
    exit 1
  fi
  if [[ "$code" != "IDENTITY_REGISTER_FAILED" ]]; then
    echo "FAIL[$label]: expected code=IDENTITY_REGISTER_FAILED got code='$code' http=$http_code" >&2
    cat "$file" >&2 || true
    exit 1
  fi
  if [[ -n "$expected_message_fragment" ]]; then
    if [[ "$message" != *"$expected_message_fragment"* && "$detail" != *"$expected_message_fragment"* ]]; then
      echo "FAIL[$label]: expected message fragment '$expected_message_fragment' not found" >&2
      cat "$file" >&2 || true
      exit 1
    fi
  fi
}

echo "IDENTITY FR-02 strict E2E: BASE=$BASE HIVE_ID=$HIVE_ID PRIMARY_HIVE_ID=$PRIMARY_HIVE_ID RUNTIME=$RUNTIME"

require_cmd curl
require_cmd python3
require_cmd systemctl
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

echo "Step 2/6: FR2-T6 missing tenant_id must fail"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_MISSING_TENANT" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
spawn_missing_http="$(spawn_without_tenant "$NODE_MISSING_TENANT" "$spawn_missing_body")"
assert_failed_with_identity_register "$spawn_missing_body" "$spawn_missing_http" "missing_tenant" "tenant_id is missing"

echo "Step 3/6: stop primary identity service to simulate identity_unavailable"
if ! as_root_local systemctl is-active --quiet "$PRIMARY_IDENTITY_SERVICE"; then
  echo "FAIL: primary identity service '$PRIMARY_IDENTITY_SERVICE' is not active before FR2-T7" >&2
  exit 1
fi
as_root_local systemctl stop "$PRIMARY_IDENTITY_SERVICE"
stopped_primary_identity="1"
if ! wait_service_state "inactive"; then
  echo "FAIL: timeout waiting service '$PRIMARY_IDENTITY_SERVICE' to stop" >&2
  exit 1
fi

echo "Step 4/6: FR2-T7 identity unavailable must fail"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_IDENTITY_DOWN" "$kill_body" '{"force":true}' >/dev/null 2>&1 || true
spawn_down_http="$(spawn_with_tenant "$NODE_IDENTITY_DOWN" "$spawn_down_body")"
assert_failed_with_identity_register "$spawn_down_body" "$spawn_down_http" "identity_unavailable"

echo "Step 5/6: restart primary identity service and wait healthy"
as_root_local systemctl start "$PRIMARY_IDENTITY_SERVICE"
if ! wait_service_state "active"; then
  echo "FAIL: timeout waiting service '$PRIMARY_IDENTITY_SERVICE' to become active" >&2
  exit 1
fi
stopped_primary_identity="0"

echo "Step 6/6: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "tenant_id=$TENANT_ID"
echo "missing_tenant_node=$NODE_MISSING_TENANT@$HIVE_ID"
echo "identity_down_node=$NODE_IDENTITY_DOWN@$HIVE_ID"
echo "identity strict FR-02 E2E passed."
