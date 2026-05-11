#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
ARCHI_BASE="${ARCHI_BASE:-http://127.0.0.1:3000}"
HIVE_ID="${HIVE_ID:-motherbee}"
RUNTIME="${RUNTIME:-ai.generic}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TEST_ID="${TEST_ID:-cogdef-$(date +%s)-${RANDOM}}"
NODE_NAME="${NODE_NAME:-AI.cognitive.${TEST_ID}@${HIVE_ID}}"
TENANT_ID="${TENANT_ID:-}"
CONFIG_WAIT_SECS="${CONFIG_WAIT_SECS:-90}"
POLL_SECS="${POLL_SECS:-2}"
RUN_OPA_HASH_CHECK="${RUN_OPA_HASH_CHECK:-1}"
CLEANUP="${CLEANUP:-1}"
DIST_RUNTIMES_ROOT="${DIST_RUNTIMES_ROOT:-/var/lib/fluxbee/dist/runtimes}"

tmpdir="$(mktemp -d)"
cleanup_files=()

cleanup() {
  local ec=$?
  for pair in "${cleanup_files[@]:-}"; do
    local src="${pair%%::*}"
    local dst="${pair#*::}"
    if [[ -f "$src" && ! -f "$dst" ]]; then
      move_path "$src" "$dst" >/dev/null 2>&1 || true
    fi
  done
  if [[ "$CLEANUP" == "1" && -n "${ILK_ID:-}" ]]; then
    curl -sS -X POST "$BASE/hives/$HIVE_ID/identity/ilks/$ILK_ID/definition" \
      -H "Content-Type: application/json" \
      -d '{"definition":{}}' >/dev/null 2>&1 || true
  fi
  if [[ "$CLEANUP" == "1" ]]; then
    curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" \
      -H "Content-Type: application/json" \
      -d '{"force":true,"purge_instance":true}' >/dev/null 2>&1 || true
  fi
  rm -rf "$tmpdir"
  return "$ec"
}
trap cleanup EXIT

move_path() {
  local src="$1"
  local dst="$2"
  if mv "$src" "$dst" 2>/dev/null; then
    return 0
  fi
  if command -v sudo >/dev/null 2>&1; then
    sudo mv "$src" "$dst"
    return $?
  fi
  return 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "FAIL: missing required command '$1'" >&2
    exit 1
  }
}

validate_tenant_id() {
  if [[ -n "$TENANT_ID" && ! "$TENANT_ID" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$TENANT_ID' (expected tnt:<uuid>)." >&2
    exit 1
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

json_get() {
  local file="$1"
  local path="$2"
  python3 - "$file" "$path" <<'PY'
import json, sys
file_path, path = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(file_path, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)

roots = [doc]
if isinstance(doc, dict):
    roots.append(doc.get("payload", {}))
    payload = doc.get("payload", {})
    if isinstance(payload, dict):
        roots.append(payload.get("response", {}))
        roots.append(payload.get("payload", {}))

for root in roots:
    value = root
    ok = True
    for part in [p for p in path.split(".") if p]:
        if isinstance(value, dict):
            value = value.get(part)
        elif isinstance(value, list) and part.isdigit():
            idx = int(part)
            value = value[idx] if idx < len(value) else None
        else:
            value = None
        if value is None:
            ok = False
            break
    if ok:
        if isinstance(value, bool):
            print("true" if value else "false")
        elif isinstance(value, (dict, list)):
            print(json.dumps(value, separators=(",", ":")))
        else:
            print(str(value))
        raise SystemExit(0)
print("")
PY
}

find_ilk_for_node() {
  local file="$1"
  local node_name="$2"
  python3 - "$file" "$node_name" <<'PY'
import json, sys
file_path, node_name = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(file_path, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)
payload = doc.get("payload", doc)
for row in payload.get("ilks", []):
    if row.get("node_name") == node_name:
        print(row.get("ilk_id", ""))
        raise SystemExit(0)
print("")
PY
}

runtime_available_in_versions() {
  local file="$1"
  local runtime="$2"
  python3 - "$file" "$runtime" <<'PY'
import json, sys
file_path, runtime = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(file_path, "r", encoding="utf-8"))
except Exception:
    print("0")
    raise SystemExit(0)

def iter_dicts(value):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from iter_dicts(child)
    elif isinstance(value, list):
        for child in value:
            yield from iter_dicts(child)

for obj in iter_dicts(doc):
    runtimes = obj.get("runtimes")
    if isinstance(runtimes, dict) and runtime in runtimes:
        print("1")
        raise SystemExit(0)
print("0")
PY
}

fail_missing_runtime_preflight() {
  echo "FAIL: runtime '$RUNTIME' is not ready for spawn on hive '$HIVE_ID'." >&2
  echo "The core installer should publish ai.generic automatically. If this is a fresh host, re-run install.sh and check its ai.generic publish step." >&2
  echo "Manual repair command:" >&2
  echo "  PATH=\"\$PATH\" scripts/publish-ia-runtime.sh --runtime $RUNTIME --version 1.0.0 --set-current --sudo" >&2
  echo "If the runtime was just published, verify $DIST_RUNTIMES_ROOT/manifest.json and /hives/$HIVE_ID/versions." >&2
  echo "Last versions response:" >&2
  cat "$versions_body" >&2 || true
  exit 1
}

resolve_default_tenant_id() {
  local file="$1"
  python3 - "$file" <<'PY'
import json, sys
file_path = sys.argv[1]
try:
    doc = json.load(open(file_path, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)

payload = doc.get("payload", doc)
tenants = payload.get("tenants", [])
if not isinstance(tenants, list):
    print("")
    raise SystemExit(0)

def tenant_id(row):
    value = row.get("tenant_id") if isinstance(row, dict) else None
    return value if isinstance(value, str) else ""

for row in tenants:
    if not isinstance(row, dict):
        continue
    if row.get("status") == "active" and row.get("is_root") is True:
        print(tenant_id(row))
        raise SystemExit(0)
for row in tenants:
    if not isinstance(row, dict):
        continue
    if row.get("is_root") is True:
        print(tenant_id(row))
        raise SystemExit(0)
for row in tenants:
    if not isinstance(row, dict):
        continue
    if row.get("status") == "active":
        print(tenant_id(row))
        raise SystemExit(0)
if tenants:
    print(tenant_id(tenants[0]))
else:
    print("")
PY
}

tenant_exists_in_list() {
  local file="$1"
  local tenant_id="$2"
  python3 - "$file" "$tenant_id" <<'PY'
import json, sys
file_path, target = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(file_path, "r", encoding="utf-8"))
except Exception:
    print("0")
    raise SystemExit(0)
payload = doc.get("payload", doc)
for row in payload.get("tenants", []):
    if isinstance(row, dict) and row.get("tenant_id") == target:
        print("1")
        raise SystemExit(0)
print("0")
PY
}

create_agent_asset() {
  local label="$1"
  local payload="$2"
  local out_file="$tmpdir/asset_${label}.json"
  local http
  http="$(http_call "POST" "$ARCHI_BASE/api/agent-assets" "$out_file" "$payload")"
  local status
  status="$(json_get "$out_file" "status")"
  if [[ "$http" != "200" || "$status" != "ok" ]]; then
    echo "FAIL[$label]: Archi asset create failed http=$http status=$status" >&2
    cat "$out_file" >&2 || true
    exit 1
  fi
  echo "$out_file"
}

control_config_get() {
  local out_file="$1"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" "$out_file" \
    '{"requested_by":"agent_cognitive_definition_e2e"}'
}

wait_definition_state() {
  local expected="$1"
  local out_file="$2"
  local deadline=$((SECONDS + CONFIG_WAIT_SECS))
  local http state
  while :; do
    http="$(control_config_get "$out_file" || true)"
    state="$(json_get "$out_file" "definition_state")"
    if [[ "$http" == "200" && "$state" == "$expected" ]]; then
      return 0
    fi
    if (( SECONDS >= deadline )); then
      echo "FAIL: timed out waiting definition_state=$expected (last_http=$http last_state=$state)" >&2
      cat "$out_file" >&2 || true
      return 1
    fi
    sleep "$POLL_SECS"
  done
}

set_definition() {
  local role_hash="$1"
  local skill_hashes_json="$2"
  local handbook_hashes_json="$3"
  local out_file="$4"
  local personality_hash="${5:-}"
  local body
  body="$(python3 - "$role_hash" "$skill_hashes_json" "$handbook_hashes_json" "$personality_hash" <<'PY'
import json, sys
role_hash, skills, handbooks, personality_hash = (
    sys.argv[1],
    json.loads(sys.argv[2]),
    json.loads(sys.argv[3]),
    sys.argv[4],
)
definition = {}
if role_hash:
    definition["role_hash"] = role_hash
definition["skill_hashes"] = skills
definition["handbook_hashes"] = handbooks
if personality_hash:
    definition["personality_hash"] = personality_hash
print(json.dumps({"definition": definition}, separators=(",", ":")))
PY
)"
  local http
  http="$(http_call "POST" "$BASE/hives/$HIVE_ID/identity/ilks/$ILK_ID/definition" "$out_file" "$body")"
  local status
  status="$(json_get "$out_file" "status")"
  if [[ "$http" != "200" || "$status" != "ok" ]]; then
    echo "FAIL: set_ilk_definition failed http=$http status=$status" >&2
    cat "$out_file" >&2 || true
    exit 1
  fi
}

require_cmd curl
require_cmd python3
validate_tenant_id

versions_body="$tmpdir/versions.json"
spawn_body="$tmpdir/spawn.json"
ilks_body="$tmpdir/ilks.json"
config_body="$tmpdir/config.json"
set_body="$tmpdir/set_definition.json"
restart_body="$tmpdir/restart.json"
opa_body="$tmpdir/opa.json"
tenants_body="$tmpdir/tenants.json"

echo "Step 1/12: validate admin + archi are reachable"
admin_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
if [[ "$admin_http" != "200" ]]; then
  echo "FAIL: admin versions endpoint HTTP $admin_http" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi
if [[ ! -f "$DIST_RUNTIMES_ROOT/manifest.json" ]]; then
  fail_missing_runtime_preflight
fi
if [[ "$(runtime_available_in_versions "$versions_body" "$RUNTIME")" != "1" ]]; then
  fail_missing_runtime_preflight
fi
tenants_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/identity/tenants" "$tenants_body")"
if [[ "$tenants_http" != "200" ]]; then
  echo "FAIL: identity tenants endpoint HTTP $tenants_http" >&2
  cat "$tenants_body" >&2 || true
  exit 1
fi
if [[ -z "$TENANT_ID" ]]; then
  TENANT_ID="$(resolve_default_tenant_id "$tenants_body")"
  if [[ -z "$TENANT_ID" ]]; then
    echo "FAIL: could not resolve a tenant for AI spawn E2E." >&2
    cat "$tenants_body" >&2 || true
    exit 1
  fi
  echo "Resolved TENANT_ID=$TENANT_ID"
elif [[ "$(tenant_exists_in_list "$tenants_body" "$TENANT_ID")" != "1" ]]; then
  echo "FAIL: TENANT_ID '$TENANT_ID' does not exist in hive '$HIVE_ID'." >&2
  echo "Available tenants:" >&2
  cat "$tenants_body" >&2 || true
  exit 1
fi
echo "Agent cognitive definition E2E: BASE=$BASE ARCHI_BASE=$ARCHI_BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME TENANT_ID=$TENANT_ID"
archi_http="$(http_call "GET" "$ARCHI_BASE/api/agent-assets?refresh=true" "$tmpdir/assets_list_initial.json")"
if [[ "$archi_http" != "200" ]]; then
  echo "FAIL: Archi agent-assets endpoint HTTP $archi_http" >&2
  cat "$tmpdir/assets_list_initial.json" >&2 || true
  exit 1
fi

echo "Step 2/12: cleanup baseline node (ignore errors)"
curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" \
  -H "Content-Type: application/json" \
  -d '{"force":true,"purge_instance":true}' >/dev/null 2>&1 || true

echo "Step 3/12: run AI node on ai.generic"
spawn_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
  "$NODE_NAME" "$RUNTIME" "$RUNTIME_VERSION" "$TENANT_ID")"
spawn_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$spawn_body" "$spawn_payload")"
spawn_status="$(json_get "$spawn_body" "status")"
if [[ "$spawn_http" != "200" || "$spawn_status" != "ok" ]]; then
  echo "FAIL: run_node failed http=$spawn_http status=$spawn_status" >&2
  cat "$spawn_body" >&2 || true
  exit 1
fi
ILK_ID="$(json_get "$spawn_body" "identity.register.ilk_id")"

echo "Step 4/12: resolve agent ILK"
if [[ -z "$ILK_ID" ]]; then
  deadline=$((SECONDS + CONFIG_WAIT_SECS))
  while [[ -z "$ILK_ID" ]]; do
    http_call "GET" "$BASE/hives/$HIVE_ID/identity/ilks" "$ilks_body" >/dev/null || true
    ILK_ID="$(find_ilk_for_node "$ilks_body" "$NODE_NAME")"
    if [[ -n "$ILK_ID" ]]; then
      break
    fi
    if (( SECONDS >= deadline )); then
      echo "FAIL: could not resolve ILK for node_name=$NODE_NAME" >&2
      cat "$ilks_body" >&2 || true
      exit 1
    fi
    sleep "$POLL_SECS"
  done
fi
echo "ilk_id=$ILK_ID"

echo "Step 5/12: wait default empty cognitive definition"
wait_definition_state "empty" "$config_body"
seq_before="$(json_get "$config_body" "definition.last_identity_seq")"
prompt_chars_before="$(json_get "$config_body" "active_prompt_chars")"
if [[ -z "$prompt_chars_before" || "$prompt_chars_before" == "0" ]]; then
  echo "FAIL: expected non-zero default prompt chars, got '$prompt_chars_before'" >&2
  cat "$config_body" >&2 || true
  exit 1
fi

echo "Step 6/12: create role/skill/handbook assets through Archi"
role_file="$(create_agent_asset role "$(cat <<'JSON'
{"asset_type":"role","name":"E2E support role","description":"Answer as a concise Fluxbee test support agent.","tone":"direct","limits":["Do not invent runtime state."]}
JSON
)")"
skill_file="$(create_agent_asset skill "$(cat <<'JSON'
{"asset_type":"skill","name":"e2e-triage","description":"Triage Fluxbee test requests.","instructions":["Identify the requested Fluxbee operation.","State whether the next action is read-only or mutating."],"constraints":["Do not execute mutations directly."]}
JSON
)")"
handbook_file="$(create_agent_asset handbook "$(cat <<'JSON'
{"asset_type":"handbook","name":"E2E handbook","sections":[{"title":"Fluxbee rule","content":"Use workflows for deterministic orchestration and IO nodes only for external integration."}]}
JSON
)")"
role_hash="$(json_get "$role_file" "asset.hash")"
skill_hash="$(json_get "$skill_file" "asset.hash")"
handbook_hash="$(json_get "$handbook_file" "asset.hash")"

echo "Step 7/12: apply composed definition and verify seq changes"
set_definition "$role_hash" "[\"$skill_hash\"]" "[\"$handbook_hash\"]" "$set_body"
wait_definition_state "composed" "$config_body"
seq_after="$(json_get "$config_body" "definition.last_identity_seq")"
if [[ -n "$seq_before" && -n "$seq_after" && "$seq_before" == "$seq_after" ]]; then
  echo "FAIL: expected identity seq to change after ILK_SET_DEFINITION seq_before=$seq_before seq_after=$seq_after" >&2
  cat "$config_body" >&2 || true
  exit 1
fi

echo "Step 8/12: simulate missing asset and verify partial"
missing_skill_file="$(create_agent_asset missing_skill "$(cat <<'JSON'
{"asset_type":"skill","name":"e2e-delayed-sync","instructions":["This skill file is temporarily removed to simulate delayed blob sync."]}
JSON
)")"
missing_skill_hash="$(json_get "$missing_skill_file" "asset.hash")"
missing_skill_path="$(json_get "$missing_skill_file" "asset.path")"
missing_skill_backup="$tmpdir/${missing_skill_hash}.json"
move_path "$missing_skill_path" "$missing_skill_backup"
cleanup_files+=("${missing_skill_backup}::${missing_skill_path}")
set_definition "$role_hash" "[\"$skill_hash\",\"$missing_skill_hash\"]" "[\"$handbook_hash\"]" "$set_body"
wait_definition_state "partial" "$config_body"
failed_hashes="$(json_get "$config_body" "definition.failed_hashes")"
if [[ "$failed_hashes" != *"$missing_skill_hash"* ]]; then
  echo "FAIL: partial state did not report missing skill hash $missing_skill_hash" >&2
  cat "$config_body" >&2 || true
  exit 1
fi

echo "Step 9/12: restore missing asset and verify composed"
move_path "$missing_skill_backup" "$missing_skill_path"
cleanup_files=()
wait_definition_state "composed" "$config_body"

echo "Step 9b/12: attach personality asset and verify composed prompt renders it first"
personality_file="$(create_agent_asset personality "$(cat <<'JSON'
{"asset_type":"personality","name":"E2E argentine engineer","system_fields":{"timezone":"America/Argentina/Mendoza","country_code":"AR","primary_language":"es-AR","additional_languages":[{"code":"en","level":"C1"}]},"biographical":{"nationality":"Argentinian","display_name":"Lucía","birth_year":1985},"narrative":{"summary":"Mid-career engineer; direct but friendly.","communication_style":"Direct."}}
JSON
)")"
personality_hash="$(json_get "$personality_file" "asset.hash")"
set_definition "$role_hash" "[\"$skill_hash\"]" "[\"$handbook_hash\"]" "$set_body" "$personality_hash"
wait_definition_state "composed" "$config_body"
personality_loaded="$(json_get "$config_body" "definition.personality_hash_loaded")"
if [[ "$personality_loaded" != "$personality_hash" ]]; then
  echo "FAIL: personality_hash_loaded mismatch loaded='$personality_loaded' expected='$personality_hash'" >&2
  cat "$config_body" >&2 || true
  exit 1
fi
active_prompt="$(json_get "$config_body" "definition.active_prompt")"
if [[ -n "$active_prompt" && "$active_prompt" != "null" ]]; then
  # Best-effort check: ensure PERSONALITY appears before ROLE in the composed prompt when CONFIG_GET returns it.
  if [[ "$active_prompt" == *"[PERSONALITY:"* && "$active_prompt" == *"[ROLE:"* ]]; then
    if ! python3 - "$active_prompt" <<'PY'
import sys
prompt = sys.argv[1]
p_idx = prompt.find("[PERSONALITY:")
r_idx = prompt.find("[ROLE:")
if p_idx == -1 or r_idx == -1 or p_idx >= r_idx:
    sys.exit(1)
PY
    then
      echo "FAIL: personality must render before role in composed prompt" >&2
      exit 1
    fi
  fi
fi

echo "Step 9c/12: clear personality (set to null) and verify composed without personality block"
set_definition "$role_hash" "[\"$skill_hash\"]" "[\"$handbook_hash\"]" "$set_body"
wait_definition_state "composed" "$config_body"
personality_after_clear="$(json_get "$config_body" "definition.personality_hash_loaded")"
if [[ "$personality_after_clear" != "" && "$personality_after_clear" != "null" ]]; then
  echo "FAIL: personality_hash_loaded should be empty after clear, got '$personality_after_clear'" >&2
  cat "$config_body" >&2 || true
  exit 1
fi

echo "Step 10/12: verify OPA policy can compile hash-based route condition"
if [[ "$RUN_OPA_HASH_CHECK" == "1" ]]; then
  opa_payload="$(python3 - "$NODE_NAME" "$skill_hash" <<'PY'
import json, sys
node, skill = sys.argv[1], sys.argv[2]
rego = f'''package router

default target = null

target = "{node}" {{
  dst := input.dst_ilk
  h := data.identity[dst].skill_hashes[_]
  h == "{skill}"
}}
'''
print(json.dumps({"rego": rego, "entrypoint": "router/target"}, separators=(",", ":")))
PY
)"
  opa_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/opa/policy/check" "$opa_body" "$opa_payload")"
  opa_status="$(json_get "$opa_body" "status")"
  if [[ "$opa_http" != "200" || "$opa_status" != "ok" ]]; then
    echo "FAIL: OPA hash route policy check failed http=$opa_http status=$opa_status" >&2
    cat "$opa_body" >&2 || true
    exit 1
  fi
fi

echo "Step 11/12: restart node and verify definition persists"
restart_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/restart" "$restart_body" "{\"node_name\":\"$NODE_NAME\"}")"
restart_status="$(json_get "$restart_body" "status")"
if [[ "$restart_http" != "200" || "$restart_status" != "ok" ]]; then
  echo "FAIL: restart_node failed http=$restart_http status=$restart_status" >&2
  cat "$restart_body" >&2 || true
  exit 1
fi
wait_definition_state "composed" "$config_body"

echo "Step 12/12: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "node_name=$NODE_NAME"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "ilk_id=$ILK_ID"
echo "seq_before=$seq_before"
echo "seq_after=$seq_after"
echo "role_hash=$role_hash"
echo "skill_hash=$skill_hash"
echo "handbook_hash=$handbook_hash"
echo "missing_skill_hash=$missing_skill_hash"
echo "agent cognitive definition E2E passed."
