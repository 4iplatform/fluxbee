#!/usr/bin/env bash
set -euo pipefail

# FR9-T23 E2E:
# validates admin action catalog introspection over HTTP and socket.
#
# Usage:
#   BASE="http://127.0.0.1:8080" \
#   ADMIN_TARGET="SY.admin@motherbee" \
#   bash scripts/admin_list_actions_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"

tmpdir="$(mktemp -d)"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local key="$1"
  local file="$2"
  python3 - "$key" "$file" <<'PY'
import json
import sys

key = sys.argv[1]
path = sys.argv[2]
try:
    data = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)

value = data
for part in key.split("."):
    if isinstance(value, dict):
        value = value.get(part, "")
    else:
        value = ""
        break

if isinstance(value, (dict, list)):
    print(json.dumps(value))
else:
    print("" if value is None else value)
PY
}

action_in_catalog() {
  local file="$1"
  local action="$2"
  python3 - "$file" "$action" <<'PY'
import json
import sys

path, action = sys.argv[1], sys.argv[2]
try:
    doc = json.load(open(path, "r", encoding="utf-8"))
except Exception:
    print("0")
    raise SystemExit(0)

actions = (
    doc.get("payload", {})
       .get("actions", [])
)
if isinstance(actions, list) and any(isinstance(x, dict) and x.get("action") == action for x in actions):
    print("1")
else:
    print("0")
PY
}

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url"
}

run_socket() {
  local out_file="$1"
  ADMIN_ACTION="list_admin_actions" \
  ADMIN_TARGET="$ADMIN_TARGET" \
  ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
  ADMIN_PARAMS_JSON='{}' \
  "$DIAG_BIN" >"$out_file"
}

require_cmd curl
require_cmd python3
if [[ ! -x "$DIAG_BIN" ]]; then
  require_cmd cargo
fi

echo "ADMIN list_admin_actions E2E: ADMIN_TARGET=$ADMIN_TARGET"

echo "Step 1/5: build admin_internal_command_diag once"
if [[ ! -x "$DIAG_BIN" ]]; then
  (cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: admin_internal_command_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

echo "Step 2/5: HTTP get admin actions catalog"
http_out="$tmpdir/http.json"
http_code="$(http_call "GET" "$BASE/admin/actions" "$http_out")"
if [[ "$http_code" != "200" ]]; then
  echo "FAIL[http]: expected 200 got $http_code" >&2
  cat "$http_out" >&2
  exit 1
fi
if [[ "$(json_get "status" "$http_out")" != "ok" || "$(json_get "action" "$http_out")" != "list_admin_actions" ]]; then
  echo "FAIL[http]: invalid status/action" >&2
  cat "$http_out" >&2
  exit 1
fi

echo "Step 3/5: socket list_admin_actions"
socket_out="$tmpdir/socket.json"
run_socket "$socket_out"
if [[ "$(json_get "status" "$socket_out")" != "ok" || "$(json_get "action" "$socket_out")" != "list_admin_actions" ]]; then
  echo "FAIL[socket]: invalid status/action" >&2
  cat "$socket_out" >&2
  exit 1
fi

echo "Step 4/5: validate registry version + key actions"
http_registry_version="$(json_get "payload.registry_version" "$http_out")"
socket_registry_version="$(json_get "payload.registry_version" "$socket_out")"
if [[ -z "$http_registry_version" || -z "$socket_registry_version" || "$http_registry_version" != "$socket_registry_version" ]]; then
  echo "FAIL[registry_version]: http='$http_registry_version' socket='$socket_registry_version'" >&2
  cat "$http_out" >&2
  cat "$socket_out" >&2
  exit 1
fi

http_payload="$tmpdir/http_payload.json"
socket_payload="$tmpdir/socket_payload.json"
json_get "payload" "$http_out" >"$http_payload"
json_get "payload" "$socket_out" >"$socket_payload"

for a in list_admin_actions run_node inventory update sync_hint; do
  if [[ "$(action_in_catalog "$http_payload" "$a")" != "1" ]]; then
    echo "FAIL[http-catalog]: missing action '$a'" >&2
    cat "$http_out" >&2
    exit 1
  fi
  if [[ "$(action_in_catalog "$socket_payload" "$a")" != "1" ]]; then
    echo "FAIL[socket-catalog]: missing action '$a'" >&2
    cat "$socket_out" >&2
    exit 1
  fi
done

echo "Step 5/5: summary"
echo "status=ok"
echo "admin_target=$ADMIN_TARGET"
echo "registry_version=$http_registry_version"
echo "admin list_admin_actions FR9-T23 E2E passed."
