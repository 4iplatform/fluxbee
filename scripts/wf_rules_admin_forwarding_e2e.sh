#!/usr/bin/env bash
set -euo pipefail

# Admin forwarding E2E for SY.wf-rules HTTP surface.
# Covers every wf-rules admin endpoint over HTTP and verifies
# response envelope/error mapping at the admin layer.

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
WF_NAME="${WF_NAME:-test-wf-rules-admin}"
TENANT_ID="${TENANT_ID:-}"
WF_ENGINE_VERSION="${WF_ENGINE_VERSION:-0.1.0}"
SKIP_PUBLISH_WF_ENGINE="${SKIP_PUBLISH_WF_ENGINE:-0}"

PASS=0
FAIL=0
DELETED_IN_MAIN_FLOW=0

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local doc="$1"
  local key="$2"
  JSON_DOC="$doc" python3 - "$key" <<'PY'
import json, os, sys
path = sys.argv[1]
try:
    doc = json.loads(os.environ["JSON_DOC"])
except Exception:
    print(""); sys.exit(0)
v = doc
for part in path.split("."):
    if isinstance(v, dict):
        v = v.get(part)
    else:
        v = None
        break
if v is None:
    print("")
elif isinstance(v, bool):
    print("true" if v else "false")
elif isinstance(v, (dict, list)):
    print(json.dumps(v))
else:
    print(str(v))
PY
}

step() { echo; echo "── $* ──"; }
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*" >&2; FAIL=$((FAIL + 1)); }

assert_eq() {
  local label="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then
    pass "$label = '$actual'"
  else
    fail "$label: expected '$expected', got '$actual'"
  fi
}

assert_nonempty() {
  local label="$1" value="$2"
  if [[ -n "$value" ]]; then
    pass "$label is set ('$value')"
  else
    fail "$label is empty"
  fi
}

assert_contains() {
  local label="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -qF "$needle"; then
    pass "$label contains '$needle'"
  else
    fail "$label: expected to contain '$needle', got: $haystack"
  fi
}

admin_post() {
  local path="$1"
  local body="$2"
  local body_file
  body_file="$(mktemp)"
  local code
  code="$(curl -sS -o "$body_file" -w '%{http_code}' -X POST "$BASE/hives/$HIVE_ID/wf-rules$path" \
    -H "Content-Type: application/json" \
    -d "$body")"
  local body_out
  body_out="$(cat "$body_file")"
  rm -f "$body_file"
  printf '%s\n%s\n' "$code" "$body_out"
}

admin_get() {
  local path="${1:-}"
  local body_file
  body_file="$(mktemp)"
  local code
  code="$(curl -sS -o "$body_file" -w '%{http_code}' "$BASE/hives/$HIVE_ID/wf-rules${path}")"
  local body_out
  body_out="$(cat "$body_file")"
  rm -f "$body_file"
  printf '%s\n%s\n' "$code" "$body_out"
}

definition_v1() {
  cat <<EOF
{
  "wf_schema_version": "1",
  "workflow_type": "$WF_NAME",
  "description": "admin forwarding e2e workflow v1",
  "input_schema": {
    "type": "object",
    "required": ["request_id"],
    "properties": {
      "request_id": {"type": "string"}
    }
  },
  "initial_state": "pending",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "pending",
      "description": "waiting for confirmation",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": {"msg": "WF_TEST_CONFIRM"},
          "guard": "event.payload.ok == true",
          "target_state": "done",
          "actions": []
        },
        {
          "event_match": {"msg": "WF_TEST_CONFIRM"},
          "guard": "event.payload.ok == false",
          "target_state": "failed",
          "actions": []
        }
      ]
    },
    { "name": "done", "description": "done", "entry_actions": [], "exit_actions": [], "transitions": [] },
    { "name": "failed", "description": "failed", "entry_actions": [], "exit_actions": [], "transitions": [] }
  ]
}
EOF
}

definition_v2() {
  cat <<EOF
{
  "wf_schema_version": "1",
  "workflow_type": "$WF_NAME",
  "description": "admin forwarding e2e workflow v2",
  "input_schema": {
    "type": "object",
    "required": ["request_id"],
    "properties": {
      "request_id": {"type": "string"}
    }
  },
  "initial_state": "pending",
  "terminal_states": ["done", "failed"],
  "states": [
    {
      "name": "pending",
      "description": "waiting for confirmation v2",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": {"msg": "WF_TEST_CONFIRM"},
          "guard": "event.payload.ok == true",
          "target_state": "done",
          "actions": []
        },
        {
          "event_match": {"msg": "WF_TEST_CONFIRM"},
          "guard": "event.payload.ok == false",
          "target_state": "failed",
          "actions": []
        }
      ]
    },
    { "name": "done", "description": "done v2", "entry_actions": [], "exit_actions": [], "transitions": [] },
    { "name": "failed", "description": "failed v2", "entry_actions": [], "exit_actions": [], "transitions": [] }
  ]
}
EOF
}

cleanup() {
  if [[ "$DELETED_IN_MAIN_FLOW" == "1" ]]; then
    return
  fi
  echo
  echo "── cleanup: deleting $WF_NAME (force=true) ──"
  local out code body
  out="$(admin_post "/delete" "{\"workflow_name\":\"$WF_NAME\",\"force\":true}" 2>/dev/null || true)"
  code="$(printf '%s\n' "$out" | sed -n '1p')"
  body="$(printf '%s\n' "$out" | sed -n '2,$p')"
  if [[ "$code" == "200" ]]; then
    echo "  cleaned up $WF_NAME"
  else
    echo "  warning: cleanup may have failed: $body" >&2
  fi
}
trap cleanup EXIT

require_cmd curl
require_cmd python3

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ "$SKIP_PUBLISH_WF_ENGINE" != "1" ]]; then
  echo "Publishing wf.engine@$WF_ENGINE_VERSION runtime..."
  bash "$SCRIPT_DIR/publish-wf-runtime.sh" --version "$WF_ENGINE_VERSION" --set-current --skip-build
  echo "  wf.engine runtime published."
fi

echo "SY.admin -> SY.wf-rules forwarding e2e"
echo "  BASE=$BASE  HIVE=$HIVE_ID  WF=$WF_NAME"
if [[ -n "$TENANT_ID" ]]; then
  echo "  TENANT_ID=$TENANT_ID"
fi

step "1. compile via admin /wf-rules/compile"
compile_body="$(python3 -c "
import json, sys
print(json.dumps({
    'workflow_name': '$WF_NAME',
    'definition': json.loads(sys.stdin.read()),
}))" < <(definition_v1))"
out="$(admin_post "/compile" "$compile_body")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "compile"
assert_eq "payload.ok" "$(json_get "$resp" "payload.ok")" "true"

step "2. apply via admin /wf-rules/apply"
apply_body="$(python3 -c "
import json
payload = {'workflow_name': '$WF_NAME', 'auto_spawn': True}
if '$TENANT_ID':
    payload['tenant_id'] = '$TENANT_ID'
print(json.dumps(payload))
")"
out="$(admin_post "/apply" "$apply_body")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "apply"
assert_eq "wf_node.action" "$(json_get "$resp" "payload.wf_node.action")" "restarted"

step "3. get_workflow via admin GET /wf-rules"
out="$(admin_get "?workflow_name=$WF_NAME")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "get_workflow"
assert_eq "workflow_name" "$(json_get "$resp" "payload.workflow_name")" "$WF_NAME"

step "4. get_status via admin GET /wf-rules/status"
out="$(admin_get "/status?workflow_name=$WF_NAME")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "get_status"
assert_eq "wf_node.running" "$(json_get "$resp" "payload.wf_node.running")" "true"

step "5. list_workflows via admin GET /wf-rules"
out="$(admin_get)"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "list_workflows"
assert_contains "payload.workflows" "$(json_get "$resp" "payload.workflows")" "$WF_NAME"

step "6. compile_apply via admin POST /wf-rules"
compile_apply_body="$(python3 -c "
import json, sys
payload = {
    'workflow_name': '$WF_NAME',
    'definition': json.loads(sys.stdin.read()),
    'auto_spawn': True,
}
if '$TENANT_ID':
    payload['tenant_id'] = '$TENANT_ID'
print(json.dumps(payload))
" < <(definition_v2))"
out="$(admin_post "" "$compile_apply_body")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "compile_apply"
assert_eq "wf_node.action" "$(json_get "$resp" "payload.wf_node.action")" "restarted"

step "7. rollback via admin /wf-rules/rollback"
rollback_body="$(python3 -c "
import json
payload = {'workflow_name': '$WF_NAME', 'auto_spawn': True}
if '$TENANT_ID':
    payload['tenant_id'] = '$TENANT_ID'
print(json.dumps(payload))
")"
out="$(admin_post "/rollback" "$rollback_body")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "rollback"
assert_eq "wf_node.action" "$(json_get "$resp" "payload.wf_node.action")" "restarted"

step "8. delete via admin /wf-rules/delete"
out="$(admin_post "/delete" "{\"workflow_name\":\"$WF_NAME\",\"force\":false}")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "200"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "action" "$(json_get "$resp" "action")" "delete_workflow"
assert_eq "payload.deleted" "$(json_get "$resp" "payload.deleted")" "true"
DELETED_IN_MAIN_FLOW=1

step "9. get_workflow after delete maps error envelope"
out="$(admin_get "?workflow_name=$WF_NAME")"
code="$(printf '%s\n' "$out" | sed -n '1p')"
resp="$(printf '%s\n' "$out" | sed -n '2,$p')"
echo "  response: $resp"
assert_eq "http" "$code" "404"
assert_eq "status" "$(json_get "$resp" "status")" "error"
assert_eq "action" "$(json_get "$resp" "action")" "get_workflow"
assert_eq "error_code" "$(json_get "$resp" "error_code")" "WORKFLOW_NOT_FOUND"
assert_nonempty "error_detail" "$(json_get "$resp" "error_detail")"

echo
echo "══════════════════════════════════"
echo "Results: $PASS passed, $FAIL failed"
echo "══════════════════════════════════"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
