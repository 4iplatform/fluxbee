#!/usr/bin/env bash
set -euo pipefail

# Node teardown completeness E2E:
# - spawn an identity-managed AI node
# - set a cognitive definition field on its ILK
# - write a vault secret dedicated to that ILK
# - kill with purge_instance=true
# - verify node_ilk_map, identity, vault, and routing summary cleanup
# - recreate the same node_name and verify a fresh ILK is assigned
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   HIVE_ID="motherbee" \
#   TENANT_ID="tnt:00000000-0000-0000-0000-000000000001" \
#   bash scripts/node_teardown_completeness_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
TENANT_ID="${TENANT_ID:-tnt:00000000-0000-0000-0000-000000000001}"
RUNTIME="${RUNTIME:-ai.generic}"
RUNTIME_VERSION="${RUNTIME_VERSION:-current}"
TEST_ID="${TEST_ID:-ntc-$(date +%s)-${RANDOM}}"
NODE_LOCAL="${NODE_LOCAL:-AI.teardown.${TEST_ID}}"
NODE_NAME="${NODE_NAME:-${NODE_LOCAL}@${HIVE_ID}}"
SECRET_KEY="${SECRET_KEY:-ntc:teardown:${TEST_ID}}"
ORCH_NODE_ILK_MAP="${ORCH_NODE_ILK_MAP:-/var/lib/fluxbee/state/orchestrator/identity-node-ilk-map.json}"

tmpdir="$(mktemp -d)"
versions_body="$tmpdir/versions.json"
spawn1_body="$tmpdir/spawn1.json"
spawn2_body="$tmpdir/spawn2.json"
kill_body="$tmpdir/kill.json"
identity_body="$tmpdir/identity.json"
definition_body="$tmpdir/definition.json"
vault_body="$tmpdir/vault.json"
map_body="$tmpdir/node_ilk_map.json"

cleanup() {
  local _ec=$?
  http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true,"purge_instance":true}' >/dev/null 2>&1 || true
  http_call "DELETE" "$BASE/hives/$HIVE_ID/vault/secrets/$(url_encode "$SECRET_KEY")" "$vault_body" >/dev/null 2>&1 || true
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

url_encode() {
  python3 - "$1" <<'PY'
import sys
from urllib.parse import quote
print(quote(sys.argv[1], safe=""))
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
    print(json.dumps(value, separators=(",", ":")))
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
doc = json.load(open(sys.argv[2], "r", encoding="utf-8"))
runtimes = (
    doc.get("payload", {})
       .get("hive", {})
       .get("runtimes", {})
       .get("runtimes", {})
)
raise SystemExit(0 if isinstance(runtimes, dict) and runtime in runtimes else 1)
PY
}

find_ilk_for_node() {
  local file="$1"
  local node_name="$2"
  python3 - "$file" "$node_name" <<'PY'
import json
import sys

doc = json.load(open(sys.argv[1], "r", encoding="utf-8"))
node_name = sys.argv[2]
for row in doc.get("payload", {}).get("ilks", []):
    if row.get("node_name") == node_name and not row.get("deleted_at_ms"):
        print(row.get("ilk_id", ""))
        raise SystemExit(0)
print("")
PY
}

assert_ilk_absent_from_list() {
  local file="$1"
  local ilk_id="$2"
  python3 - "$file" "$ilk_id" <<'PY'
import json
import sys

doc = json.load(open(sys.argv[1], "r", encoding="utf-8"))
ilk_id = sys.argv[2]
for row in doc.get("payload", {}).get("ilks", []):
    if row.get("ilk_id") == ilk_id and not row.get("deleted_at_ms"):
        raise SystemExit(1)
raise SystemExit(0)
PY
}

assert_vault_list_empty() {
  local file="$1"
  python3 - "$file" <<'PY'
import json
import sys

doc = json.load(open(sys.argv[1], "r", encoding="utf-8"))
secrets = doc.get("payload", {}).get("secrets", [])
raise SystemExit(0 if isinstance(secrets, list) and len(secrets) == 0 else 1)
PY
}

assert_routing_summary_present() {
  local file="$1"
  python3 - "$file" <<'PY'
import json
import sys

doc = json.load(open(sys.argv[1], "r", encoding="utf-8"))
refs = doc.get("payload", {}).get("routing_references")
if not isinstance(refs, dict):
    raise SystemExit(1)
for key in ("routes", "vpns", "taps"):
    if not isinstance(refs.get(key), list):
        raise SystemExit(1)
raise SystemExit(0)
PY
}

assert_node_ilk_map_clean() {
  if ! as_root_local test -f "$ORCH_NODE_ILK_MAP"; then
    return 0
  fi
  as_root_local cat "$ORCH_NODE_ILK_MAP" > "$map_body"
  python3 - "$map_body" "$NODE_NAME" <<'PY'
import json
import sys

doc = json.load(open(sys.argv[1], "r", encoding="utf-8"))
node_name = sys.argv[2]
if node_name in doc.get("nodes", {}):
    raise SystemExit(1)
if node_name in doc.get("tenants", {}):
    raise SystemExit(1)
raise SystemExit(0)
PY
}

assert_http_ok_status_ok() {
  local label="$1"
  local http="$2"
  local file="$3"
  local status
  status="$(json_get_file "status" "$file")"
  if [[ "$http" != "200" || "$status" != "ok" ]]; then
    echo "FAIL[$label]: http=$http status=$status" >&2
    cat "$file" >&2 || true
    exit 1
  fi
}

validate_tenant_id() {
  if [[ ! "$TENANT_ID" =~ ^tnt:[0-9a-fA-F-]{36}$ ]]; then
    echo "FAIL: invalid TENANT_ID='$TENANT_ID' (expected tnt:<uuid-v4>)" >&2
    exit 1
  fi
}

spawn_node() {
  local out_file="$1"
  local payload
  payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"%s","tenant_id":"%s"}' \
    "$NODE_NAME" "$RUNTIME" "$RUNTIME_VERSION" "$TENANT_ID")"
  http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$out_file" "$payload"
}

require_cmd curl
require_cmd python3
validate_tenant_id

echo "Node teardown completeness E2E: BASE=$BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME TEST_ID=$TEST_ID"

echo "Step 1/10: validate runtime exists"
versions_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$versions_body")"
assert_http_ok_status_ok "versions" "$versions_http" "$versions_body"
if ! runtime_exists_in_manifest "$RUNTIME" "$versions_body"; then
  echo "FAIL: runtime '$RUNTIME' missing in /hives/$HIVE_ID/versions" >&2
  cat "$versions_body" >&2 || true
  exit 1
fi

echo "Step 2/10: cleanup baseline node and secret"
http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true,"purge_instance":true}' >/dev/null 2>&1 || true
http_call "DELETE" "$BASE/hives/$HIVE_ID/vault/secrets/$(url_encode "$SECRET_KEY")" "$vault_body" >/dev/null 2>&1 || true

echo "Step 3/10: spawn node"
spawn1_http="$(spawn_node "$spawn1_body")"
assert_http_ok_status_ok "spawn1" "$spawn1_http" "$spawn1_body"
ILK_1="$(json_get_file "payload.identity.register.ilk_id" "$spawn1_body")"
if [[ -z "$ILK_1" ]]; then
  ilks_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/identity/ilks" "$identity_body")"
  assert_http_ok_status_ok "list_ilks_after_spawn1" "$ilks_http" "$identity_body"
  ILK_1="$(find_ilk_for_node "$identity_body" "$NODE_NAME")"
fi
if [[ ! "$ILK_1" =~ ^ilk:[0-9a-fA-F-]{36}$ ]]; then
  echo "FAIL: could not resolve first ILK for NODE_NAME=$NODE_NAME (got '$ILK_1')" >&2
  cat "$spawn1_body" >&2 || true
  exit 1
fi
echo "ilk_id_first=$ILK_1"

echo "Step 4/10: set cognitive definition on first ILK"
PERSONALITY_HASH="$(python3 - "$TEST_ID" <<'PY'
import hashlib
import sys
print(hashlib.sha256(("node-teardown-completeness:" + sys.argv[1]).encode()).hexdigest())
PY
)"
definition_payload="$(printf '{"definition":{"personality_hash":"%s"}}' "$PERSONALITY_HASH")"
definition_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/identity/ilks/$ILK_1/definition" "$definition_body" "$definition_payload")"
assert_http_ok_status_ok "set_ilk_definition" "$definition_http" "$definition_body"

echo "Step 5/10: put vault secret dedicated to first ILK"
vault_payload="$(printf '{"key":"%s","value":{"token":"node-teardown-completeness"},"metadata":{"tenant_id":"%s","resource_type":"api_key","ilk":"%s","description":"node teardown completeness e2e"}}' \
  "$SECRET_KEY" "$TENANT_ID" "$ILK_1")"
vault_put_http="$(http_call "POST" "$BASE/hives/$HIVE_ID/vault/secrets" "$vault_body" "$vault_payload")"
assert_http_ok_status_ok "vault_put" "$vault_put_http" "$vault_body"

echo "Step 6/10: kill node with purge_instance=true"
kill_http="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$kill_body" '{"force":true,"purge_instance":true}')"
assert_http_ok_status_ok "kill_purge" "$kill_http" "$kill_body"
if [[ "$(json_get_file "payload.ilk_mapping_removed" "$kill_body")" != "true" ]]; then
  echo "FAIL: expected payload.ilk_mapping_removed=true" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi
if [[ "$(json_get_file "payload.ilk_deleted" "$kill_body")" != "true" ]]; then
  echo "FAIL: expected payload.ilk_deleted=true" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi
if ! assert_routing_summary_present "$kill_body"; then
  echo "FAIL: purge response missing routing_references summary" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi
vault_deleted="$(json_get_file "payload.vault_secrets_purged.deleted" "$kill_body")"
if [[ ! "$vault_deleted" =~ ^[0-9]+$ || "$vault_deleted" -lt 1 ]]; then
  echo "FAIL: expected at least one dedicated vault secret to be purged" >&2
  cat "$kill_body" >&2 || true
  exit 1
fi

echo "Step 7/10: assert orchestrator node_ilk_map is clean"
if ! assert_node_ilk_map_clean; then
  echo "FAIL: node_ilk_map still contains '$NODE_NAME'" >&2
  cat "$map_body" >&2 || true
  exit 1
fi

echo "Step 8/10: assert first ILK is gone from identity"
ilks_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/identity/ilks" "$identity_body")"
assert_http_ok_status_ok "list_ilks_after_purge" "$ilks_http" "$identity_body"
if ! assert_ilk_absent_from_list "$identity_body" "$ILK_1"; then
  echo "FAIL: first ILK still appears active after purge: $ILK_1" >&2
  cat "$identity_body" >&2 || true
  exit 1
fi

echo "Step 9/10: assert dedicated vault secret is gone"
encoded_key="$(url_encode "$SECRET_KEY")"
encoded_ilk="$(url_encode "$ILK_1")"
vault_list_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/vault/secrets?prefix=$encoded_key&ilk=$encoded_ilk&limit=20" "$vault_body")"
assert_http_ok_status_ok "vault_list_after_purge" "$vault_list_http" "$vault_body"
if ! assert_vault_list_empty "$vault_body"; then
  echo "FAIL: dedicated vault secret still exists after purge" >&2
  cat "$vault_body" >&2 || true
  exit 1
fi

echo "Step 10/10: recreate same node_name and verify fresh ILK"
spawn2_http="$(spawn_node "$spawn2_body")"
assert_http_ok_status_ok "spawn2" "$spawn2_http" "$spawn2_body"
ILK_2="$(json_get_file "payload.identity.register.ilk_id" "$spawn2_body")"
if [[ -z "$ILK_2" ]]; then
  ilks_http="$(http_call "GET" "$BASE/hives/$HIVE_ID/identity/ilks" "$identity_body")"
  assert_http_ok_status_ok "list_ilks_after_spawn2" "$ilks_http" "$identity_body"
  ILK_2="$(find_ilk_for_node "$identity_body" "$NODE_NAME")"
fi
if [[ ! "$ILK_2" =~ ^ilk:[0-9a-fA-F-]{36}$ ]]; then
  echo "FAIL: could not resolve recreated ILK for NODE_NAME=$NODE_NAME (got '$ILK_2')" >&2
  cat "$spawn2_body" >&2 || true
  exit 1
fi
if [[ "$ILK_1" == "$ILK_2" ]]; then
  echo "FAIL: recreated node reused stale ILK ($ILK_1)" >&2
  cat "$spawn1_body" >&2 || true
  cat "$spawn2_body" >&2 || true
  exit 1
fi

echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "node_name=$NODE_NAME"
echo "runtime=$RUNTIME@$RUNTIME_VERSION"
echo "ilk_id_first=$ILK_1"
echo "ilk_id_second=$ILK_2"
echo "secret_key=$SECRET_KEY"
echo "node teardown completeness E2E passed."
