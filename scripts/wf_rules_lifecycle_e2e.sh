#!/usr/bin/env bash
set -euo pipefail

# E2E lifecycle test for SY.wf-rules:
#   compile → apply (auto_spawn) → status check → update (compile_apply)
#   → rollback → delete
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="motherbee" \
#     bash scripts/wf_rules_lifecycle_e2e.sh
#
# Optional:
#   WF_NAME="test-wf-rules"   workflow name used for the test (auto-deleted on exit)
#   AUTO_SPAWN="true"         spawn WF node on first apply (default: true)
#   WF_NODE_WAIT_SECS="10"   seconds to wait for WF node to appear after apply
#   SKIP_ROLLBACK="0"         set to 1 to skip rollback step (e.g. debugging apply)

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
WF_NAME="${WF_NAME:-test-wf-rules}"
AUTO_SPAWN="${AUTO_SPAWN:-true}"
WF_NODE_WAIT_SECS="${WF_NODE_WAIT_SECS:-10}"
SKIP_ROLLBACK="${SKIP_ROLLBACK:-0}"

PASS=0
FAIL=0

# ── helpers ────────────────────────────────────────────────────────────────

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  fi
}

# json_get DOC KEY  →  value (string) or empty
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
    if isinstance(v, dict): v = v.get(part)
    else: v = None; break
if v is None: print("")
elif isinstance(v, bool): print("true" if v else "false")
elif isinstance(v, (dict, list)): print(json.dumps(v))
else: print(str(v))
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

wf_post() {
  local path="$1"
  local body="$2"
  curl -sS -X POST "$BASE/hives/$HIVE_ID/wf-rules$path" \
    -H "Content-Type: application/json" \
    -d "$body"
}

wf_get() {
  local path="${1:-}"
  curl -sS "$BASE/hives/$HIVE_ID/wf-rules${path}"
}

# ── workflow definition v1 ─────────────────────────────────────────────────

definition_v1() {
  cat <<EOF
{
  "wf_schema_version": "1",
  "workflow_type": "$WF_NAME",
  "description": "e2e test workflow v1",
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
      "entry_actions": [
        {
          "type": "send_message",
          "target": "IO.test@$HIVE_ID",
          "meta": {"msg": "WF_TEST_REQUEST"},
          "payload": {"request_id": {"\$ref": "input.request_id"}}
        }
      ],
      "exit_actions": [],
      "transitions": [
        {
          "event_match": {"msg": "WF_TEST_CONFIRM"},
          "guard": "event.payload.ok == true",
          "target_state": "done",
          "actions": [
            {"type": "set_variable", "name": "confirmed_at", "value": "now()"}
          ]
        },
        {
          "event_match": {"msg": "WF_TEST_CONFIRM"},
          "guard": "event.payload.ok == false",
          "target_state": "failed",
          "actions": []
        }
      ]
    },
    {
      "name": "done",
      "description": "completed",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "failed",
      "description": "failed",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}
EOF
}

definition_v2() {
  cat <<EOF
{
  "wf_schema_version": "1",
  "workflow_type": "$WF_NAME",
  "description": "e2e test workflow v2 - updated",
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
      "description": "waiting for confirmation (v2)",
      "entry_actions": [
        {
          "type": "send_message",
          "target": "IO.test@$HIVE_ID",
          "meta": {"msg": "WF_TEST_REQUEST_V2"},
          "payload": {"request_id": {"\$ref": "input.request_id"}}
        }
      ],
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
    {
      "name": "done",
      "description": "completed v2",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    },
    {
      "name": "failed",
      "description": "failed v2",
      "entry_actions": [],
      "exit_actions": [],
      "transitions": []
    }
  ]
}
EOF
}

# ── cleanup on exit ────────────────────────────────────────────────────────

cleanup() {
  echo
  echo "── cleanup: deleting $WF_NAME (force=true) ──"
  resp="$(wf_post "/delete" "{\"workflow_name\":\"$WF_NAME\",\"force\":true}" 2>/dev/null || true)"
  ok="$(json_get "$resp" "payload.ok")"
  deleted="$(json_get "$resp" "payload.deleted")"
  if [[ "$deleted" == "true" || "$ok" == "true" ]]; then
    echo "  cleaned up $WF_NAME"
  else
    echo "  warning: cleanup may have failed: $resp" >&2
  fi
}
trap cleanup EXIT

require_cmd curl
require_cmd python3

echo "SY.wf-rules lifecycle e2e"
echo "  BASE=$BASE  HIVE=$HIVE_ID  WF=$WF_NAME"

# ── STEP 1: compile only (validate + stage, no apply) ─────────────────────

step "1. compile (validate + stage)"

body="$(python3 -c "
import json, sys
print(json.dumps({
    'workflow_name': '$WF_NAME',
    'definition': json.loads(sys.stdin.read()),
}))" < <(definition_v1))"

resp="$(wf_post "/compile" "$body")"
echo "  response: $resp"

ok="$(json_get "$resp" "payload.ok")"
state="$(json_get "$resp" "payload.state")"
staged_version="$(json_get "$resp" "payload.effective_config.staged.version")"
staged_hash="$(json_get "$resp" "payload.effective_config.staged.hash")"
guard_count="$(json_get "$resp" "payload.effective_config.staged.guard_count")"

assert_eq  "ok"            "$ok"    "true"
assert_eq  "state"         "$state" "configured"
assert_nonempty "staged.version" "$staged_version"
assert_nonempty "staged.hash"    "$staged_hash"
assert_nonempty "staged.guard_count" "$guard_count"

# ── STEP 2: apply (promote staged → current, spawn node) ──────────────────

step "2. apply (auto_spawn=$AUTO_SPAWN)"

body="{\"workflow_name\":\"$WF_NAME\",\"auto_spawn\":$AUTO_SPAWN}"
resp="$(wf_post "/apply" "$body")"
echo "  response: $resp"

ok="$(json_get "$resp" "payload.ok")"
current_version="$(json_get "$resp" "payload.effective_config.current.version")"
wf_node_action="$(json_get "$resp" "payload.wf_node.action")"

assert_eq "ok"              "$ok"          "true"
assert_nonempty "current.version" "$current_version"

if [[ "$AUTO_SPAWN" == "true" ]]; then
  assert_eq "wf_node.action" "$wf_node_action" "restarted"
else
  assert_eq "wf_node.action" "$wf_node_action" "none"
fi

V1="$current_version"

# ── STEP 3: get_workflow (verify definition is stored) ─────────────────────

step "3. get_workflow"

resp="$(wf_get "?workflow_name=$WF_NAME")"
echo "  response: $(echo "$resp" | python3 -c "import json,sys; d=json.load(sys.stdin); d.pop('definition',None); print(json.dumps(d))" 2>/dev/null || echo "$resp")"

status="$(json_get "$resp" "status")"
wf_name="$(json_get "$resp" "payload.workflow_name")"
version="$(json_get "$resp" "payload.version")"

assert_eq "status"          "$status"  "ok"
assert_eq "workflow_name"   "$wf_name" "$WF_NAME"
assert_eq "version"         "$version" "$V1"

# ── STEP 4: get_status (WF node info) ─────────────────────────────────────

step "4. get_status"

# give the node a moment to start if auto_spawn was used
if [[ "$AUTO_SPAWN" == "true" && "$WF_NODE_WAIT_SECS" -gt 0 ]]; then
  echo "  waiting up to ${WF_NODE_WAIT_SECS}s for WF node..."
  deadline=$((SECONDS + WF_NODE_WAIT_SECS))
  while [[ $SECONDS -lt $deadline ]]; do
    resp="$(wf_get "/status?workflow_name=$WF_NAME")"
    running="$(json_get "$resp" "payload.wf_node.running")"
    if [[ "$running" == "true" ]]; then break; fi
    sleep 2
  done
fi

resp="$(wf_get "/status?workflow_name=$WF_NAME")"
echo "  response: $resp"

status="$(json_get "$resp" "status")"
current_v="$(json_get "$resp" "payload.current_version")"
current_hash="$(json_get "$resp" "payload.current_hash")"

assert_eq "status"          "$status"    "ok"
assert_eq "current_version" "$current_v" "$V1"
assert_nonempty "current_hash" "$current_hash"

if [[ "$AUTO_SPAWN" == "true" ]]; then
  running="$(json_get "$resp" "payload.wf_node.running")"
  node_name="$(json_get "$resp" "payload.wf_node.node_name")"
  assert_eq "wf_node.running" "$running" "true"
  assert_contains "wf_node.node_name" "$node_name" "WF.$WF_NAME@"
fi

# ── STEP 5: compile_apply with v2 definition (update) ─────────────────────

step "5. compile_apply v2 (update definition)"

body="$(python3 -c "
import json, sys
auto_spawn = '$AUTO_SPAWN' == 'true'
print(json.dumps({
    'workflow_name': '$WF_NAME',
    'definition': json.loads(sys.stdin.read()),
    'auto_spawn': auto_spawn,
}))" < <(definition_v2))"

resp="$(wf_post "" "$body")"
echo "  response: $resp"

ok="$(json_get "$resp" "payload.ok")"
current_version="$(json_get "$resp" "payload.effective_config.current.version")"
wf_node_action="$(json_get "$resp" "payload.wf_node.action")"

assert_eq "ok"             "$ok"   "true"
assert_nonempty "current.version" "$current_version"

V2="$current_version"
if [[ "$V2" -le "$V1" ]] 2>/dev/null; then
  fail "v2 version ($V2) should be > v1 ($V1)"
else
  pass "version incremented: v1=$V1 → v2=$V2"
fi

if [[ "$AUTO_SPAWN" == "true" ]]; then
  assert_eq "wf_node.action (update)" "$wf_node_action" "restarted"
fi

# ── STEP 6: rollback (optional) ────────────────────────────────────────────

if [[ "$SKIP_ROLLBACK" != "1" ]]; then
  step "6. rollback (restore v1)"

  resp="$(wf_post "/rollback" "{\"workflow_name\":\"$WF_NAME\"}")"
  echo "  response: $resp"

  ok="$(json_get "$resp" "payload.ok")"
  rolled_version="$(json_get "$resp" "payload.effective_config.current.version")"

  assert_eq "ok" "$ok" "true"
  assert_eq "rolled back to v1" "$rolled_version" "$V1"

  if [[ "$AUTO_SPAWN" == "true" ]]; then
    wf_node_action="$(json_get "$resp" "payload.wf_node.action")"
    assert_eq "wf_node.action (rollback)" "$wf_node_action" "restarted"
  fi
fi

# ── STEP 7: delete (force=false, no running instances expected) ─────────────

step "7. delete (force=false)"

resp="$(wf_post "/delete" "{\"workflow_name\":\"$WF_NAME\",\"force\":false}")"
echo "  response: $resp"

ok="$(json_get "$resp" "payload.ok")"
deleted="$(json_get "$resp" "payload.deleted")"

assert_eq "ok"      "$ok"      "true"
assert_eq "deleted" "$deleted" "true"

# verify it's gone
resp="$(wf_get "?workflow_name=$WF_NAME")"
status="$(json_get "$resp" "status")"
if [[ "$status" == "ok" ]]; then
  fail "workflow still returned after delete"
else
  pass "workflow not found after delete (expected)"
fi

# ── summary ────────────────────────────────────────────────────────────────

echo
echo "══════════════════════════════════"
echo "Results: $PASS passed, $FAIL failed"
echo "══════════════════════════════════"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
