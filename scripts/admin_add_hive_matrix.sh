#!/usr/bin/env bash
set -euo pipefail

# E2E matrix smoke for SY.admin add_hive error contracts.
# Focus: fast negative-path checks without provisioning remote hosts.
#
# Covered:
# - INVALID_ADDRESS -> HTTP 400
# - INVALID_HIVE_ID -> HTTP 400
# - HIVE_EXISTS -> HTTP 409 (if at least one hive already exists)
# - SSH_* -> HTTP 502/504 using a reachable/invalid SSH target
#
# Usage:
#   BASE="http://127.0.0.1:8080" bash scripts/admin_add_hive_matrix.sh
# Optional:
#   SSH_TEST_ADDRESS="127.0.0.1"

BASE="${BASE:-http://127.0.0.1:8080}"
SSH_TEST_ADDRESS="${SSH_TEST_ADDRESS:-127.0.0.1}"

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
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
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
    print(value if value is not None else "")
PY
}

log_http_request() {
  local method="$1"
  local url="$2"
  local payload="${3:-}"
  echo "[HTTP] -> ${method} ${url}" >&2
  if [[ -n "$payload" ]]; then
    echo "[HTTP]    payload: ${payload}" >&2
  fi
}

log_http_response() {
  local status="$1"
  local body_file="$2"
  local compact
  compact="$(tr '\n' ' ' < "$body_file" | sed 's/[[:space:]]\+/ /g')"
  echo "[HTTP] <- status=${status} body=${compact}" >&2
}

call_post_hives() {
  local payload="$1"
  local body_file="$2"
  log_http_request "POST" "$BASE/hives" "$payload"
  curl -sS -o "$body_file" -w "%{http_code}" \
    -X POST "$BASE/hives" \
    -H "Content-Type: application/json" \
    -d "$payload"
}

call_get_hives() {
  local body_file="$1"
  log_http_request "GET" "$BASE/hives"
  curl -sS -o "$body_file" -w "%{http_code}" "$BASE/hives"
}

assert_eq() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL [$label]: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

require_cmd curl
require_cmd python3

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

echo "Running add_hive matrix against BASE=$BASE"

# 1) INVALID_ADDRESS
body="$tmpdir/invalid_address.json"
hid="e2e-invalid-address-$RANDOM"
status="$(call_post_hives "{\"hive_id\":\"$hid\",\"address\":\"bad address\"}" "$body")"
log_http_response "$status" "$body"
code="$(json_get "error_code" "$body")"
if [[ -z "$code" ]]; then
  code="$(json_get "payload.error_code" "$body")"
fi
assert_eq "$status" "400" "INVALID_ADDRESS/http"
assert_eq "$code" "INVALID_ADDRESS" "INVALID_ADDRESS/code"
echo "OK: INVALID_ADDRESS -> HTTP 400"

# 2) INVALID_HIVE_ID
body="$tmpdir/invalid_hive_id.json"
status="$(call_post_hives "{\"hive_id\":\"bad id\",\"address\":\"127.0.0.1\"}" "$body")"
log_http_response "$status" "$body"
code="$(json_get "error_code" "$body")"
if [[ -z "$code" ]]; then
  code="$(json_get "payload.error_code" "$body")"
fi
assert_eq "$status" "400" "INVALID_HIVE_ID/http"
assert_eq "$code" "INVALID_HIVE_ID" "INVALID_HIVE_ID/code"
echo "OK: INVALID_HIVE_ID -> HTTP 400"

# 3) HIVE_EXISTS (best-effort if at least one hive exists)
hives_json="$tmpdir/hives.json"
status="$(call_get_hives "$hives_json")"
log_http_response "$status" "$hives_json"
assert_eq "$status" "200" "GET /hives/http"
existing_hive="$(python3 - "$hives_json" <<'PY'
import json, sys
p = sys.argv[1]
try:
    data = json.load(open(p, "r", encoding="utf-8"))
except Exception:
    print("")
    raise SystemExit(0)
items = data.get("payload", {}).get("hives", [])
if not items:
    print("")
    raise SystemExit(0)
for item in items:
    if isinstance(item, dict) and item.get("hive_id") and item.get("address"):
        print(item.get("hive_id", ""))
        raise SystemExit(0)
print("")
PY
)"

if [[ -n "$existing_hive" ]]; then
  body="$tmpdir/hive_exists.json"
  status="$(call_post_hives "{\"hive_id\":\"$existing_hive\",\"address\":\"127.0.0.1\"}" "$body")"
  log_http_response "$status" "$body"
  code="$(json_get "error_code" "$body")"
  if [[ -z "$code" ]]; then
    code="$(json_get "payload.error_code" "$body")"
  fi
  assert_eq "$status" "409" "HIVE_EXISTS/http"
  assert_eq "$code" "HIVE_EXISTS" "HIVE_EXISTS/code"
  echo "OK: HIVE_EXISTS -> HTTP 409"
else
  echo "SKIP: HIVE_EXISTS check (no existing hives from GET /hives)"
fi

# 4) SSH_* (address valid, no bootstrap credentials expected)
body="$tmpdir/ssh_error.json"
hid="e2e-ssh-$RANDOM"
status="$(call_post_hives "{\"hive_id\":\"$hid\",\"address\":\"$SSH_TEST_ADDRESS\"}" "$body")"
log_http_response "$status" "$body"
code="$(json_get "error_code" "$body")"
if [[ -z "$code" ]]; then
  code="$(json_get "payload.error_code" "$body")"
fi
if [[ "$status" != "502" && "$status" != "504" ]]; then
  echo "FAIL [SSH_*/http]: expected 502 or 504, got '$status'" >&2
  cat "$body" >&2
  exit 1
fi
if [[ "${code:-}" != SSH_* ]]; then
  echo "FAIL [SSH_*/code]: expected SSH_* code, got '$code'" >&2
  cat "$body" >&2
  exit 1
fi
echo "OK: SSH_* -> HTTP $status ($code)"

echo "add_hive matrix smoke passed."
