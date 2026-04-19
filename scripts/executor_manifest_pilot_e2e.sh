#!/usr/bin/env bash
set -euo pipefail

ARCHI_BASE="${ARCHI_BASE:-http://127.0.0.1:3000}"
HIVE_ID="${HIVE_ID:-motherbee}"
SESSION_ID="${SESSION_ID:-}"
ROUTE_PREFIX="${ROUTE_PREFIX:-tenant.executor-pilot.$(date +%s).}"
NEXT_HOP_HIVE="${NEXT_HOP_HIVE:-southbee}"

PASS=0
FAIL=0
DELETE_SENT=0

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
value = doc
for part in path.split("."):
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

assert_contains() {
  local label="$1" haystack="$2" needle="$3"
  if echo "$haystack" | grep -qF "$needle"; then
    pass "$label contains '$needle'"
  else
    fail "$label: expected to contain '$needle', got: $haystack"
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

executor_post() {
  local body="$1"
  curl -sS -X POST "$ARCHI_BASE/api/executor/plan" \
    -H "Content-Type: application/json" \
    -d "$body"
}

plan_read_only() {
  cat <<EOF
{
  "title": "executor pilot read only",
  "plan": {
    "plan_version": "0.1",
    "kind": "executor_plan",
    "metadata": {
      "name": "pilot_read_only",
      "target_hive": "$HIVE_ID"
    },
    "execution": {
      "strict": true,
      "stop_on_error": true,
      "allow_help_lookup": true,
      "steps": [
        {
          "id": "s1",
          "action": "get_runtime",
          "args": {
            "hive": "$HIVE_ID",
            "runtime": "AI.chat"
          }
        },
        {
          "id": "s2",
          "action": "list_nodes",
          "args": {
            "hive": "$HIVE_ID"
          }
        }
      ]
    }
  }
}
EOF
}

plan_add_route() {
  cat <<EOF
{
  "title": "executor pilot add route",
  "plan": {
    "plan_version": "0.1",
    "kind": "executor_plan",
    "metadata": {
      "name": "pilot_add_route",
      "target_hive": "$HIVE_ID"
    },
    "execution": {
      "strict": true,
      "stop_on_error": true,
      "allow_help_lookup": true,
      "steps": [
        {
          "id": "s1",
          "action": "add_route",
          "args": {
            "hive": "$HIVE_ID",
            "prefix": "$ROUTE_PREFIX",
            "action": "FORWARD",
            "next_hop_hive": "$NEXT_HOP_HIVE"
          }
        }
      ]
    }
  }
}
EOF
}

plan_delete_route() {
  cat <<EOF
{
  "title": "executor pilot delete route",
  "plan": {
    "plan_version": "0.1",
    "kind": "executor_plan",
    "metadata": {
      "name": "pilot_delete_route",
      "target_hive": "$HIVE_ID"
    },
    "execution": {
      "strict": true,
      "stop_on_error": true,
      "allow_help_lookup": true,
      "steps": [
        {
          "id": "s1",
          "action": "delete_route",
          "args": {
            "hive": "$HIVE_ID",
            "prefix": "$ROUTE_PREFIX"
          }
        }
      ]
    }
  }
}
EOF
}

plan_invalid() {
  cat <<EOF
{
  "title": "executor pilot invalid",
  "plan": {
    "plan_version": "0.1",
    "kind": "executor_plan",
    "metadata": {},
    "execution": {
      "strict": true,
      "stop_on_error": true,
      "allow_help_lookup": true,
      "steps": [
        {
          "id": "s1",
          "action": "get_runtime",
          "args": {
            "target": "$HIVE_ID",
            "runtime_name": "AI.chat"
          }
        }
      ]
    }
  }
}
EOF
}

cleanup() {
  if [[ "$DELETE_SENT" == "1" ]]; then
    return
  fi
  echo
  echo "── cleanup: delete test route (best effort) ──"
  local resp
  resp="$(executor_post "$(plan_delete_route)" 2>/dev/null || true)"
  echo "  response: $resp"
}
trap cleanup EXIT

require_cmd curl
require_cmd python3

echo "Executor manifest pilot E2E"
echo "  ARCHI_BASE=$ARCHI_BASE  HIVE=$HIVE_ID"
echo "  ROUTE_PREFIX=$ROUTE_PREFIX  NEXT_HOP_HIVE=$NEXT_HOP_HIVE"

step "1. read-only plan succeeds"
resp="$(executor_post "$(plan_read_only)")"
echo "  response: $resp"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "mode" "$(json_get "$resp" "mode")" "executor"
assert_eq "output.status" "$(json_get "$resp" "output.status")" "ok"
assert_eq "execution summary status" "$(json_get "$resp" "output.payload.summary.status")" "done"
assert_nonempty "execution id" "$(json_get "$resp" "output.payload.execution_id")"
assert_contains "events contain get_runtime" "$(json_get "$resp" "output.payload.events")" "\"step_action\": \"get_runtime\""

step "2. mutating add_route plan succeeds"
resp="$(executor_post "$(plan_add_route)")"
echo "  response: $resp"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "output.status" "$(json_get "$resp" "output.status")" "ok"
assert_eq "execution summary status" "$(json_get "$resp" "output.payload.summary.status")" "done"
assert_contains "events contain add_route" "$(json_get "$resp" "output.payload.events")" "\"step_action\": \"add_route\""

step "3. cleanup delete_route plan succeeds"
resp="$(executor_post "$(plan_delete_route)")"
DELETE_SENT=1
echo "  response: $resp"
assert_eq "status" "$(json_get "$resp" "status")" "ok"
assert_eq "output.status" "$(json_get "$resp" "output.status")" "ok"
assert_eq "execution summary status" "$(json_get "$resp" "output.payload.summary.status")" "done"

step "4. invalid plan fails before execution"
resp="$(executor_post "$(plan_invalid)")"
echo "  response: $resp"
assert_eq "status" "$(json_get "$resp" "status")" "error"
assert_eq "mode" "$(json_get "$resp" "mode")" "executor"
assert_eq "phase" "$(json_get "$resp" "output.phase")" "admin_validate"
assert_contains "error mentions missing hive" "$(json_get "$resp" "output.error")" "missing required field 'hive'"

echo
echo "══════════════════════════════════"
echo "Results: $PASS passed, $FAIL failed"
echo "══════════════════════════════════"

if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi
