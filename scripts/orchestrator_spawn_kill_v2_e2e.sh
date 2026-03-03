#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"

# Case 1 (E2E-5 intent): IO.* node with graceful kill
NODE_NAME_IO="${NODE_NAME_IO:-IO.e2e.smoke}"
RUNTIME_IO="${RUNTIME_IO:-io.e2e.smoke}"

# Case 2 (E2E-6 intent): AI.* node with force kill
NODE_NAME_AI="${NODE_NAME_AI:-AI.e2e.smoke}"
RUNTIME_AI="${RUNTIME_AI:-ai.e2e.smoke}"

# If 1, missing runtime in manifest fails the script.
# If 0, missing runtime is reported as SKIP.
REQUIRE_CASES="${REQUIRE_CASES:-0}"

json_get() {
  local json_doc="$1"
  local path="$2"
  JSON_DOC="$json_doc" python3 - "$path" <<'PY'
import json
import os
import sys

path = sys.argv[1]
raw = os.environ.get("JSON_DOC", "")
try:
    doc = json.loads(raw)
except Exception:
    print("")
    raise SystemExit(0)

value = doc
for part in path.split("."):
    if part == "":
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
  local versions
  versions="$(curl -sS "$BASE/hives/$HIVE_ID/versions")"
  JSON_DOC="$versions" python3 - "$runtime" <<'PY'
import json
import os
import sys

runtime = sys.argv[1]
raw = os.environ.get("JSON_DOC", "")
try:
    doc = json.loads(raw)
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

spawn_node() {
  local node_name="$1"
  local runtime="$2"
  curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
    -H "Content-Type: application/json" \
    -d "{\"node_name\":\"$node_name\",\"runtime\":\"$runtime\",\"runtime_version\":\"current\"}"
}

kill_node() {
  local node_name="$1"
  local force="$2"
  curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$node_name" \
    -H "Content-Type: application/json" \
    -d "{\"force\":$force}"
}

run_case() {
  local label="$1"
  local node_name="$2"
  local runtime="$3"
  local force="$4"
  local expected_signal="$5"

  if ! runtime_exists_in_manifest "$runtime"; then
    if [[ "$REQUIRE_CASES" == "1" ]]; then
      echo "FAIL[$label]: runtime '$runtime' missing in /hives/$HIVE_ID/versions" >&2
      exit 1
    fi
    echo "SKIP[$label]: runtime '$runtime' missing in manifest"
    return 0
  fi

  echo "CASE[$label] spawn node_name=$node_name runtime=$runtime"
  local spawn_resp
  spawn_resp="$(spawn_node "$node_name" "$runtime")"
  echo "$spawn_resp"
  local spawn_status
  spawn_status="$(json_get "$spawn_resp" "status")"
  if [[ "$spawn_status" != "ok" ]]; then
    echo "FAIL[$label]: spawn returned status='$spawn_status'" >&2
    exit 1
  fi

  local returned_node
  returned_node="$(json_get "$spawn_resp" "payload.node_name")"
  if [[ "$returned_node" != "$node_name@$HIVE_ID" ]]; then
    echo "FAIL[$label]: payload.node_name='$returned_node' expected '$node_name@$HIVE_ID'" >&2
    exit 1
  fi

  echo "CASE[$label] kill force=$force"
  local kill_resp
  kill_resp="$(kill_node "$node_name" "$force")"
  echo "$kill_resp"
  local kill_status
  kill_status="$(json_get "$kill_resp" "status")"
  if [[ "$kill_status" != "ok" ]]; then
    echo "FAIL[$label]: kill returned status='$kill_status'" >&2
    exit 1
  fi

  local kill_signal
  kill_signal="$(json_get "$kill_resp" "payload.signal")"
  if [[ "$kill_signal" != "$expected_signal" ]]; then
    echo "FAIL[$label]: payload.signal='$kill_signal' expected '$expected_signal'" >&2
    exit 1
  fi
}

echo "SPAWN/KILL v2 E2E: BASE=$BASE HIVE_ID=$HIVE_ID REQUIRE_CASES=$REQUIRE_CASES"
run_case "E2E-5-IO" "$NODE_NAME_IO" "$RUNTIME_IO" "false" "SIGTERM"
run_case "E2E-6-AI" "$NODE_NAME_AI" "$RUNTIME_AI" "true" "SIGKILL"
echo "orchestrator SPAWN/KILL v2 E2E passed."

