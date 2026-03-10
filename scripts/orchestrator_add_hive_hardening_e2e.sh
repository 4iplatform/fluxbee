#!/usr/bin/env bash
set -euo pipefail

# E2E-10:
# - add_hive con harden_ssh=true
# - validar que login por password queda rechazado
# - validar continuidad operativa via orchestrator/socket (run_node + kill_node)
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" HIVE_ADDR="192.168.8.220" \
#   bash scripts/orchestrator_add_hive_hardening_e2e.sh
#
# Optional:
#   HARDEN_SSH="true|false"                # default: true
#   RESTRICT_SSH="true|false|auto"         # default: auto (no envía campo)
#   SSH_USER="administrator"               # default: administrator
#   SSH_PASSWORD="magicAI"                 # default: magicAI
#   REQUIRE_PASSWORD_BLOCKED="1|0"         # default: 1
#   REQUIRE_RESTRICT_SSH_APPLIED="1|0"     # default: 0
#   REQUIRE_NODE_CYCLE="1|0"               # default: 1
#   RUNTIME_RESOLVE_TIMEOUT_SECS="180"     # default: 180
#   RUNTIME_RESOLVE_POLL_SECS="3"          # default: 3
#   NODE_NAME="WF.hardening.e2e"           # default: random suffix
#   NODE_FORCE_KILL="false|true"           # default: false

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-192.168.8.220}"
HARDEN_SSH="${HARDEN_SSH:-true}"
RESTRICT_SSH="${RESTRICT_SSH:-auto}"
SSH_USER="${SSH_USER:-administrator}"
SSH_PASSWORD="${SSH_PASSWORD:-magicAI}"
REQUIRE_PASSWORD_BLOCKED="${REQUIRE_PASSWORD_BLOCKED:-1}"
REQUIRE_RESTRICT_SSH_APPLIED="${REQUIRE_RESTRICT_SSH_APPLIED:-0}"
REQUIRE_NODE_CYCLE="${REQUIRE_NODE_CYCLE:-1}"
RUNTIME_RESOLVE_TIMEOUT_SECS="${RUNTIME_RESOLVE_TIMEOUT_SECS:-180}"
RUNTIME_RESOLVE_POLL_SECS="${RUNTIME_RESOLVE_POLL_SECS:-3}"
NODE_NAME="${NODE_NAME:-WF.hardening.e2e.$RANDOM}"
NODE_FORCE_KILL="${NODE_FORCE_KILL:-false}"

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

select_runtime() {
  local versions_file="$1"
  python3 - "$versions_file" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as f:
    doc = json.load(f)

runtimes = (
    doc.get("payload", {})
       .get("hive", {})
       .get("runtimes", {})
       .get("runtimes", {})
)

if not isinstance(runtimes, dict) or not runtimes:
    raise SystemExit(1)

for runtime_name in sorted(runtimes.keys()):
    entry = runtimes.get(runtime_name, {})
    if not isinstance(entry, dict):
        continue
    current = (entry.get("current") or "").strip()
    if current:
        print(runtime_name)
        print(current)
        raise SystemExit(0)

raise SystemExit(1)
PY
}

check_password_auth_blocked() {
  local askpass="$1"
  local out_file="$2"
  set +e
  setsid ssh \
    -o PreferredAuthentications=password \
    -o PubkeyAuthentication=no \
    -o PasswordAuthentication=yes \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 \
    "$SSH_USER@$HIVE_ADDR" "true" \
    >"$out_file" 2>&1 \
    </dev/null \
    >/dev/null \
    2>&1 \
    SSH_ASKPASS="$askpass" \
    SSH_ASKPASS_REQUIRE=force \
    DISPLAY=jsr
  local rc=$?
  set -e
  echo "$rc"
}

require_cmd curl
require_cmd python3
require_cmd ssh
require_cmd setsid

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "ADD_HIVE hardening E2E: BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR HARDEN_SSH=$HARDEN_SSH"

echo "Step 1/5: cleanup baseline (allow NOT_FOUND)"
delete_hive_allow_not_found "$tmpdir/delete-baseline.json"

echo "Step 2/5: add_hive harden_ssh=$HARDEN_SSH"
if [[ "$RESTRICT_SSH" == "auto" ]]; then
  add_payload="$(printf '{"hive_id":"%s","address":"%s","harden_ssh":%s}' "$HIVE_ID" "$HIVE_ADDR" "$HARDEN_SSH")"
else
  add_payload="$(printf '{"hive_id":"%s","address":"%s","harden_ssh":%s,"restrict_ssh":%s}' "$HIVE_ID" "$HIVE_ADDR" "$HARDEN_SSH" "$RESTRICT_SSH")"
fi
status_add="$(http_call "POST" "$BASE/hives" "$tmpdir/add.json" "$add_payload")"
print_http "$status_add" "$tmpdir/add.json"
if [[ "$status_add" != "200" || "$(json_get "status" "$tmpdir/add.json")" != "ok" ]]; then
  echo "FAIL: add_hive hardening call failed" >&2
  exit 1
fi
if [[ "$(json_get "payload.harden_ssh" "$tmpdir/add.json")" != "true" ]]; then
  echo "FAIL: payload.harden_ssh is not true" >&2
  exit 1
fi
if [[ "$REQUIRE_RESTRICT_SSH_APPLIED" == "1" ]]; then
  requested="$(json_get "payload.restrict_ssh_requested" "$tmpdir/add.json")"
  applied="$(json_get "payload.restrict_ssh" "$tmpdir/add.json")"
  if [[ "$requested" != "true" ]]; then
    echo "FAIL: payload.restrict_ssh_requested is not true" >&2
    exit 1
  fi
  if [[ "$applied" != "true" ]]; then
    echo "FAIL: payload.restrict_ssh is not true (requested=true)" >&2
    exit 1
  fi
fi
if [[ "$(json_get "payload.dist_sync_ready" "$tmpdir/add.json")" != "true" ]]; then
  echo "INFO: add_hive returned dist_sync_ready=false; waiting for runtime manifest convergence in Step 4"
fi

echo "Step 3/5: verify password login is blocked"
askpass="$tmpdir/askpass.sh"
cat > "$askpass" <<EOF
#!/bin/sh
echo "$SSH_PASSWORD"
EOF
chmod 700 "$askpass"

pw_out="$tmpdir/pw-check.log"
set +e
SSH_ASKPASS="$askpass" \
SSH_ASKPASS_REQUIRE=force \
DISPLAY=jsr \
setsid ssh \
  -o PreferredAuthentications=password \
  -o PubkeyAuthentication=no \
  -o PasswordAuthentication=yes \
  -o StrictHostKeyChecking=no \
  -o UserKnownHostsFile=/dev/null \
  -o ConnectTimeout=10 \
  "$SSH_USER@$HIVE_ADDR" "true" \
  < /dev/null >"$pw_out" 2>&1
pw_rc=$?
set -e

if [[ "$REQUIRE_PASSWORD_BLOCKED" == "1" ]]; then
  if [[ "$pw_rc" -eq 0 ]]; then
    echo "FAIL: password login still accepted after hardening" >&2
    cat "$pw_out" >&2 || true
    exit 1
  fi
  echo "INFO: password login rejected (rc=$pw_rc)"
else
  echo "INFO: password-block validation skipped (REQUIRE_PASSWORD_BLOCKED=0, rc=$pw_rc)"
fi

echo "Step 4/5: resolve runtime from /hives/$HIVE_ID/versions"
runtime_deadline=$((SECONDS + RUNTIME_RESOLVE_TIMEOUT_SECS))
runtime_attempt=0
runtime_meta=()
while true; do
  runtime_attempt=$((runtime_attempt + 1))
  status_versions="$(http_call "GET" "$BASE/hives/$HIVE_ID/versions" "$tmpdir/versions.json")"
  print_http "$status_versions" "$tmpdir/versions.json"
  if [[ "$status_versions" == "200" ]]; then
    mapfile -t runtime_meta < <(select_runtime "$tmpdir/versions.json" || true)
    if [[ "${#runtime_meta[@]}" -ge 2 ]]; then
      break
    fi
  fi

  if [[ "$REQUIRE_NODE_CYCLE" != "1" ]]; then
    break
  fi
  if (( SECONDS >= runtime_deadline )); then
    break
  fi
  echo "INFO: runtime manifest not ready yet (attempt=$runtime_attempt); retrying in ${RUNTIME_RESOLVE_POLL_SECS}s..."
  sleep "$RUNTIME_RESOLVE_POLL_SECS"
done

if [[ "${#runtime_meta[@]}" -lt 2 ]]; then
  if [[ "$REQUIRE_NODE_CYCLE" == "1" ]]; then
    echo "FAIL: no runtime with current version found for node cycle after ${RUNTIME_RESOLVE_TIMEOUT_SECS}s" >&2
    exit 1
  fi
  echo "WARN: no runtime found, skipping node cycle."
  echo "orchestrator add_hive hardening E2E passed (partial)."
  exit 0
fi
runtime="${runtime_meta[0]}"
runtime_version="${runtime_meta[1]}"
echo "INFO: runtime selected: $runtime (current=$runtime_version)"

echo "Step 5/5: continuity check via run_node + kill_node"
run_payload="$(printf '{"node_name":"%s","runtime":"%s","runtime_version":"current"}' "$NODE_NAME" "$runtime")"
status_run="$(http_call "POST" "$BASE/hives/$HIVE_ID/nodes" "$tmpdir/run-node.json" "$run_payload")"
print_http "$status_run" "$tmpdir/run-node.json"
if [[ "$status_run" != "200" || "$(json_get "status" "$tmpdir/run-node.json")" != "ok" ]]; then
  echo "FAIL: run_node failed after hardening" >&2
  exit 1
fi

kill_payload="$(printf '{"force":%s}' "$NODE_FORCE_KILL")"
status_kill="$(http_call "DELETE" "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" "$tmpdir/kill-node.json" "$kill_payload")"
print_http "$status_kill" "$tmpdir/kill-node.json"
if [[ "$status_kill" != "200" || "$(json_get "status" "$tmpdir/kill-node.json")" != "ok" ]]; then
  echo "FAIL: kill_node failed after hardening" >&2
  exit 1
fi

echo "orchestrator add_hive hardening E2E passed."
