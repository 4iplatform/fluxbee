#!/usr/bin/env bash
set -euo pipefail

# E2E for remove_hive cleanup contract (v2):
# - Case A (online): remove_hive should cleanup via socket (remote_cleanup=socket_ok)
# - Case B (offline): stop worker orchestrator, then remove_hive should degrade to local-only cleanup
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" HIVE_ADDR="192.168.8.220" \
#   bash scripts/orchestrator_remove_hive_socket_e2e.sh
#
# Optional:
#   HARDEN_SSH="true|false"                 # default: true
#   REQUIRE_SOCKET_ONLINE="1|0"             # default: 1
#   REQUIRE_OFFLINE_LOCAL_ONLY="1|0"        # default: 1
#   SSH_USER="administrator"                # default: administrator
#   SSH_KEY="/var/lib/fluxbee/ssh/motherbee.key"

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-192.168.8.220}"
HARDEN_SSH="${HARDEN_SSH:-true}"
REQUIRE_SOCKET_ONLINE="${REQUIRE_SOCKET_ONLINE:-1}"
REQUIRE_OFFLINE_LOCAL_ONLY="${REQUIRE_OFFLINE_LOCAL_ONLY:-${REQUIRE_OFFLINE_FALLBACK:-1}}"
SSH_USER="${SSH_USER:-administrator}"
SSH_KEY="${SSH_KEY:-/var/lib/fluxbee/ssh/motherbee.key}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

json_get() {
  local path="$1"
  local file="$2"
  python3 - "$path" "$file" <<'PY'
import json
import sys

path = sys.argv[1]
file = sys.argv[2]
try:
    with open(file, "r", encoding="utf-8") as f:
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

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$body_file" -w "%{http_code}" -X "$method" "$url"
  fi
}

print_http() {
  local status="$1"
  local body_file="$2"
  local compact
  compact="$(tr '\n' ' ' < "$body_file" | sed 's/[[:space:]]\+/ /g')"
  echo "[HTTP] status=${status} body=${compact}"
}

delete_hive_allow_not_found() {
  local body_file="$1"
  local status
  status="$(http_call "DELETE" "$BASE/hives/$HIVE_ID" "$body_file")"
  print_http "$status" "$body_file"
  local code
  code="$(json_get "error_code" "$body_file")"
  local top_status
  top_status="$(json_get "status" "$body_file")"
  if [[ "$status" == "200" && "$top_status" == "ok" ]]; then
    return 0
  fi
  if [[ "$status" == "404" && "$code" == "NOT_FOUND" ]]; then
    echo "INFO: hive '$HIVE_ID' already absent."
    return 0
  fi
  echo "FAIL: unexpected DELETE /hives/$HIVE_ID result" >&2
  exit 1
}

add_hive_expect_ok() {
  local body_file="$1"
  local payload
  payload="$(printf '{"hive_id":"%s","address":"%s","harden_ssh":%s}' "$HIVE_ID" "$HIVE_ADDR" "$HARDEN_SSH")"
  local status
  status="$(http_call "POST" "$BASE/hives" "$body_file" "$payload")"
  print_http "$status" "$body_file"
  local top_status
  top_status="$(json_get "status" "$body_file")"
  if [[ "$status" != "200" || "$top_status" != "ok" ]]; then
    echo "FAIL: add_hive did not return ok" >&2
    exit 1
  fi
}

stop_remote_orchestrator() {
  ssh -i "$SSH_KEY" \
    -o IdentitiesOnly=yes \
    -o PreferredAuthentications=publickey \
    -o PasswordAuthentication=no \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    "$SSH_USER@$HIVE_ADDR" \
    "sudo -n /bin/bash -lc 'systemctl stop sy-orchestrator || systemctl kill -s KILL sy-orchestrator || true'"
}

require_cmd curl
require_cmd python3
require_cmd ssh

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "REMOVE_HIVE socket/fallback E2E: BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR HARDEN_SSH=$HARDEN_SSH"

echo "Step 1/6: cleanup baseline (allow NOT_FOUND)"
delete_hive_allow_not_found "$tmpdir/delete-baseline.json"

echo "Step 2/6: add hive"
add_hive_expect_ok "$tmpdir/add-online.json"

echo "Step 3/6: remove hive while worker online (expect socket_ok)"
status_online="$(http_call "DELETE" "$BASE/hives/$HIVE_ID" "$tmpdir/delete-online.json")"
print_http "$status_online" "$tmpdir/delete-online.json"
cleanup_online="$(json_get "payload.remote_cleanup" "$tmpdir/delete-online.json")"
if [[ "$status_online" != "200" ]]; then
  echo "FAIL: online remove_hive http=$status_online" >&2
  exit 1
fi
if [[ "$REQUIRE_SOCKET_ONLINE" == "1" && "$cleanup_online" != "socket_ok" ]]; then
  echo "FAIL: expected payload.remote_cleanup=socket_ok for online case, got '$cleanup_online'" >&2
  exit 1
fi
echo "INFO: online remote_cleanup=$cleanup_online"

echo "Step 4/6: add hive again for offline scenario"
add_hive_expect_ok "$tmpdir/add-offline.json"

echo "Step 5/6: stop remote worker orchestrator (force offline socket path)"
if ! stop_remote_orchestrator; then
  if [[ "$REQUIRE_OFFLINE_LOCAL_ONLY" == "1" ]]; then
    echo "FAIL: could not stop remote sy-orchestrator via SSH key" >&2
    exit 1
  fi
  echo "WARN: could not stop remote sy-orchestrator; skipping strict offline assertion."
fi

echo "Step 6/6: remove hive after remote orchestrator stop (expect local-only cleanup)"
status_offline="$(http_call "DELETE" "$BASE/hives/$HIVE_ID" "$tmpdir/delete-offline.json")"
print_http "$status_offline" "$tmpdir/delete-offline.json"
cleanup_offline="$(json_get "payload.remote_cleanup" "$tmpdir/delete-offline.json")"
cleanup_via_offline="$(json_get "payload.remote_cleanup_via" "$tmpdir/delete-offline.json")"
if [[ "$status_offline" != "200" ]]; then
  echo "FAIL: offline remove_hive http=$status_offline" >&2
  exit 1
fi
if [[ "$REQUIRE_OFFLINE_LOCAL_ONLY" == "1" ]]; then
  case "$cleanup_offline" in
    socket_timeout|local_only) ;;
    *)
      echo "FAIL: offline remote_cleanup unexpected '$cleanup_offline'" >&2
      exit 1
      ;;
  esac
  if [[ "$cleanup_offline" == "socket_ok" ]]; then
    echo "FAIL: offline case should not report socket_ok" >&2
    exit 1
  fi
  if [[ "$cleanup_via_offline" != "local_only" ]]; then
    echo "FAIL: expected payload.remote_cleanup_via=local_only, got '$cleanup_via_offline'" >&2
    exit 1
  fi
fi
echo "INFO: offline remote_cleanup=$cleanup_offline remote_cleanup_via=$cleanup_via_offline"

echo "orchestrator remove_hive socket/fallback E2E passed."
