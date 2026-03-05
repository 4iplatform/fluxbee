#!/usr/bin/env bash
set -euo pipefail

# E2E-8: validar que run/kill/update NO dependen de SSH operativo.
# Estrategia:
# 1) inutilizar temporalmente la key local de motherbee (si existe)
# 2) ejecutar run_node + kill_node por API
# 3) ejecutar SYSTEM_UPDATE por API con hash/version actuales del hive
# 4) restaurar key y verificar que no hubo fallo por dependencia SSH
#
# Uso:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#   NODE_NAME="WF.orch.diag.no-ssh" RUNTIME="wf.orch.diag" \
#   bash scripts/orchestrator_no_ssh_run_kill_update_e2e.sh
#
# Opcionales:
#   UPDATE_CATEGORY="runtime|core|vendor"   (default: runtime)
#   SSH_KEY_PATH="/var/lib/fluxbee/ssh/motherbee.key"

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
NODE_NAME="${NODE_NAME:-WF.orch.diag.no-ssh}"
RUNTIME="${RUNTIME:-wf.orch.diag}"
UPDATE_CATEGORY="${UPDATE_CATEGORY:-runtime}"
SSH_KEY_PATH="${SSH_KEY_PATH:-/var/lib/fluxbee/ssh/motherbee.key}"

if [[ "$UPDATE_CATEGORY" != "runtime" && "$UPDATE_CATEGORY" != "core" && "$UPDATE_CATEGORY" != "vendor" ]]; then
  echo "FAIL: UPDATE_CATEGORY must be runtime|core|vendor (got '$UPDATE_CATEGORY')" >&2
  exit 1
fi

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "FAIL: missing command '$1'" >&2
    exit 1
  fi
}

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

extract_versions_meta() {
  local category="$1"
  local json_doc="$2"
  JSON_DOC="$json_doc" python3 - "$category" <<'PY'
import json
import os
import sys

category = sys.argv[1]
raw = os.environ.get("JSON_DOC", "")
try:
    doc = json.loads(raw)
except Exception:
    print("INVALID_JSON", file=sys.stderr)
    sys.exit(2)

hive = doc.get("payload", {}).get("hive", {})
if category == "runtime":
    node = hive.get("runtimes", {})
    version = int(node.get("manifest_version", 0) or 0)
    h = node.get("manifest_hash")
elif category == "core":
    node = hive.get("core", {})
    version = 0
    h = node.get("manifest_hash")
else:
    node = hive.get("vendor", {})
    version = int(node.get("manifest_version", 0) or 0)
    h = node.get("manifest_hash")

if not isinstance(h, str) or not h.strip():
    print("MISSING_HASH", file=sys.stderr)
    sys.exit(2)

print(version)
print(h.strip())
PY
}

post_node() {
  curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
    -H "Content-Type: application/json" \
    -d "{\"node_name\":\"$NODE_NAME\",\"runtime\":\"$RUNTIME\",\"runtime_version\":\"current\"}"
}

delete_node() {
  local force="$1"
  curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" \
    -H "Content-Type: application/json" \
    -d "{\"force\":$force}"
}

post_update() {
  local version="$1"
  local hash="$2"
  curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
    -H "Content-Type: application/json" \
    -d "{\"category\":\"$UPDATE_CATEGORY\",\"manifest_version\":$version,\"manifest_hash\":\"$hash\"}"
}

tmpdir="$(mktemp -d)"
key_backup="$tmpdir/motherbee.key.bak"
key_mutated="0"

restore_key() {
  if [[ "$key_mutated" == "1" && -f "$key_backup" ]]; then
    $SUDO cp "$key_backup" "$SSH_KEY_PATH"
    $SUDO chmod 600 "$SSH_KEY_PATH" || true
    echo "Restored SSH key at $SSH_KEY_PATH"
  fi
  rm -rf "$tmpdir"
}
trap restore_key EXIT

require_cmd curl
require_cmd python3

echo "E2E-8 no-SSH run/kill/update"
echo "BASE=$BASE HIVE_ID=$HIVE_ID NODE_NAME=$NODE_NAME RUNTIME=$RUNTIME UPDATE_CATEGORY=$UPDATE_CATEGORY"

echo "Step 1/5: sabotage local SSH key (if present)"
if [[ -f "$SSH_KEY_PATH" ]]; then
  $SUDO cp "$SSH_KEY_PATH" "$key_backup"
  printf '%s\n' "NOT_A_VALID_PRIVATE_KEY" | $SUDO tee "$SSH_KEY_PATH" >/dev/null
  $SUDO chmod 600 "$SSH_KEY_PATH" || true
  key_mutated="1"
  echo "Local key sabotaged: $SSH_KEY_PATH"
else
  echo "Key not present ($SSH_KEY_PATH), continuing"
fi

echo "Step 2/5: run_node should still work via socket/orchestrator"
run_resp="$(post_node)"
echo "$run_resp"
run_status="$(json_get "$run_resp" "status")"
if [[ "$run_status" != "ok" ]]; then
  echo "FAIL: run_node status='$run_status'" >&2
  exit 1
fi

echo "Step 3/5: kill_node should still work via socket/orchestrator"
kill_resp="$(delete_node "false")"
echo "$kill_resp"
kill_status="$(json_get "$kill_resp" "status")"
if [[ "$kill_status" != "ok" ]]; then
  echo "FAIL: kill_node status='$kill_status'" >&2
  exit 1
fi

echo "Step 4/5: resolve current manifest version/hash"
versions_resp="$(curl -sS "$BASE/hives/$HIVE_ID/versions")"
mapfile -t meta < <(extract_versions_meta "$UPDATE_CATEGORY" "$versions_resp")
if [[ "${#meta[@]}" -lt 2 ]]; then
  echo "FAIL: could not resolve manifest metadata for category '$UPDATE_CATEGORY'" >&2
  echo "$versions_resp" >&2
  exit 1
fi
manifest_version="${meta[0]}"
manifest_hash="${meta[1]}"
echo "resolved manifest_version=$manifest_version manifest_hash=$manifest_hash"

echo "Step 5/5: SYSTEM_UPDATE should not require SSH"
update_resp="$(post_update "$manifest_version" "$manifest_hash")"
echo "$update_resp"
top_status="$(json_get "$update_resp" "status")"
payload_status="$(json_get "$update_resp" "payload.status")"
if [[ "$top_status" == "error" ]]; then
  echo "FAIL: update top-level status=error" >&2
  exit 1
fi
if [[ -z "$payload_status" ]]; then
  echo "FAIL: update payload.status missing" >&2
  exit 1
fi
case "$payload_status" in
  ok|sync_pending|rollback) ;;
  *)
    echo "FAIL: unexpected payload.status='$payload_status'" >&2
    exit 1
    ;;
esac

echo "orchestrator no-SSH run/kill/update E2E passed."
