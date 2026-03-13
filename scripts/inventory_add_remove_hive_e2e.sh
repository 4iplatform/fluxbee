#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
INVENTORY_WAIT_SECS="${INVENTORY_WAIT_SECS:-120}"
INVENTORY_READY_WAIT_SECS="${INVENTORY_READY_WAIT_SECS:-60}"

tmpdir="$(mktemp -d)"
inventory_body="$tmpdir/inventory.json"
hive_get_body="$tmpdir/hive_get.json"
add_body="$tmpdir/add.json"
remove_body="$tmpdir/remove.json"

cleanup() {
  local _ec=$?
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
    print(json.dumps(value))
else:
    print(str(value))
PY
}

inventory_has_hive() {
  local file="$1"
  local hive_id="$2"
  python3 - "$file" "$hive_id" <<'PY'
import json
import sys

file_path = sys.argv[1]
hive_id = sys.argv[2]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    raise SystemExit(2)

payload = doc.get("payload", {})
if isinstance(payload, dict) and "hives" not in payload and isinstance(payload.get("payload"), dict):
    payload = payload["payload"]
hives = payload.get("hives", [])
if not isinstance(hives, list):
    raise SystemExit(2)

for hive in hives:
    if isinstance(hive, dict) and hive.get("hive_id") == hive_id:
        raise SystemExit(0)

raise SystemExit(1)
PY
}

wait_inventory_hive_state() {
  local hive_id="$1"
  local expected="$2" # present|absent
  local timeout_secs="$3"
  local started_at
  started_at="$(date +%s)"

  while true; do
    local now elapsed http
    now="$(date +%s)"
    elapsed=$((now - started_at))
    if (( elapsed > timeout_secs )); then
      echo "FAIL: timeout waiting inventory hive '$hive_id' state='$expected'" >&2
      cat "$inventory_body" >&2 || true
      exit 1
    fi

    http="$(http_call "GET" "$BASE/inventory" "$inventory_body")"
    if [[ "$http" != "200" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    if [[ "$(json_get_file "status" "$inventory_body")" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi
    if [[ "$(json_get_file "payload.status" "$inventory_body")" != "ok" ]]; then
      sleep "$POLL_INTERVAL_SECS"
      continue
    fi

    if inventory_has_hive "$inventory_body" "$hive_id"; then
      if [[ "$expected" == "present" ]]; then
        return 0
      fi
    else
      if [[ "$expected" == "absent" ]]; then
        return 0
      fi
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

wait_inventory_ready() {
  local timeout_secs="$1"
  local started_at
  started_at="$(date +%s)"

  while true; do
    local now elapsed http
    now="$(date +%s)"
    elapsed=$((now - started_at))
    if (( elapsed > timeout_secs )); then
      echo "FAIL: timeout waiting /inventory readiness" >&2
      cat "$inventory_body" >&2 || true
      exit 1
    fi

    http="$(http_call "GET" "$BASE/inventory" "$inventory_body")"
    if [[ "$http" == "200" && "$(json_get_file "status" "$inventory_body")" == "ok" && "$(json_get_file "payload.status" "$inventory_body")" == "ok" ]]; then
      return 0
    fi
    sleep "$POLL_INTERVAL_SECS"
  done
}

inventory_state_now() {
  local hive_id="$1"
  local http
  http="$(http_call "GET" "$BASE/inventory" "$inventory_body")"
  if [[ "$http" != "200" ]]; then
    echo "unknown"
    return 0
  fi
  if [[ "$(json_get_file "status" "$inventory_body")" != "ok" || "$(json_get_file "payload.status" "$inventory_body")" != "ok" ]]; then
    echo "unknown"
    return 0
  fi
  if inventory_has_hive "$inventory_body" "$hive_id"; then
    echo "present"
  else
    echo "absent"
  fi
}

extract_address_from_json() {
  local file="$1"
  python3 - "$file" <<'PY'
import json
import sys

file_path = sys.argv[1]
try:
    with open(file_path, "r", encoding="utf-8") as f:
        doc = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

def walk(value):
    if isinstance(value, dict):
        if "address" in value:
            addr = value.get("address")
            if isinstance(addr, str) and addr.strip():
                return addr.strip()
        for child in value.values():
            out = walk(child)
            if out:
                return out
    elif isinstance(value, list):
        for child in value:
            out = walk(child)
            if out:
                return out
    return ""

print(walk(doc))
PY
}

resolve_hive_addr() {
  local resolved="${HIVE_ADDR}"
  if [[ -n "$resolved" ]]; then
    echo "$resolved"
    return 0
  fi

  local get_http
  get_http="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$hive_get_body")"
  if [[ "$get_http" == "200" && "$(json_get_file "status" "$hive_get_body")" == "ok" ]]; then
    resolved="$(extract_address_from_json "$hive_get_body")"
  fi

  if [[ -z "$resolved" && -f "/var/lib/fluxbee/state/hives/$HIVE_ID/info.yaml" ]]; then
    resolved="$(awk -F': *' '/^address:/ {gsub(/"/,"",$2); print $2; exit}' "/var/lib/fluxbee/state/hives/$HIVE_ID/info.yaml" || true)"
  fi

  if [[ -z "$resolved" ]]; then
    echo "FAIL: could not resolve HIVE_ADDR automatically for '$HIVE_ID'. Set HIVE_ADDR=..." >&2
    exit 1
  fi
  echo "$resolved"
}

remove_hive_expect_ok_or_not_found() {
  local hive_id="$1"
  local http
  http="$(http_call "DELETE" "$BASE/hives/$hive_id" "$remove_body")"
  if [[ "$http" == "200" && "$(json_get_file "status" "$remove_body")" == "ok" ]]; then
    return 0
  fi
  if [[ "$http" == "404" ]]; then
    return 0
  fi
  echo "FAIL: remove_hive failed http=$http" >&2
  cat "$remove_body" >&2 || true
  exit 1
}

add_hive_expect_ok() {
  local hive_id="$1"
  local hive_addr="$2"
  local payload
  payload="$(printf '{"hive_id":"%s","address":"%s"}' "$hive_id" "$hive_addr")"
  local http
  http="$(http_call "POST" "$BASE/hives" "$add_body" "$payload")"
  if [[ "$http" != "200" || "$(json_get_file "status" "$add_body")" != "ok" ]]; then
    echo "FAIL: add_hive failed http=$http" >&2
    cat "$add_body" >&2 || true
    exit 1
  fi
}

require_cmd curl
require_cmd python3

echo "INVENTORY D2 E2E: BASE=$BASE HIVE_ID=$HIVE_ID"

echo "Step 1/7: resolve hive address"
HIVE_ADDR="$(resolve_hive_addr)"
echo "resolved_hive_addr=$HIVE_ADDR"

echo "Step 2/7: wait inventory endpoint ready and detect initial state"
wait_inventory_ready "$INVENTORY_READY_WAIT_SECS"
initial_state="$(inventory_state_now "$HIVE_ID")"
if [[ "$initial_state" == "unknown" ]]; then
  echo "FAIL: could not determine initial inventory state for hive '$HIVE_ID'" >&2
  cat "$inventory_body" >&2 || true
  exit 1
fi
if [[ "$initial_state" == "present" ]]; then
  initial_present=1
else
  initial_present=0
fi
echo "initial_present=$initial_present"

if [[ "$initial_present" == "1" ]]; then
  echo "Step 3/7: remove hive and wait absent"
  remove_hive_expect_ok_or_not_found "$HIVE_ID"
  wait_inventory_hive_state "$HIVE_ID" "absent" "$INVENTORY_WAIT_SECS"

  echo "Step 4/7: add hive and wait present"
  add_hive_expect_ok "$HIVE_ID" "$HIVE_ADDR"
  wait_inventory_hive_state "$HIVE_ID" "present" "$INVENTORY_WAIT_SECS"
else
  echo "Step 3/7: add hive and wait present"
  add_hive_expect_ok "$HIVE_ID" "$HIVE_ADDR"
  wait_inventory_hive_state "$HIVE_ID" "present" "$INVENTORY_WAIT_SECS"

  echo "Step 4/7: remove hive and wait absent"
  remove_hive_expect_ok_or_not_found "$HIVE_ID"
  wait_inventory_hive_state "$HIVE_ID" "absent" "$INVENTORY_WAIT_SECS"

  echo "Step 5/7: add hive again and wait present (restore baseline)"
  add_hive_expect_ok "$HIVE_ID" "$HIVE_ADDR"
  wait_inventory_hive_state "$HIVE_ID" "present" "$INVENTORY_WAIT_SECS"
fi

echo "Step 6/7: verify global inventory endpoint healthy"
inventory_http="$(http_call "GET" "$BASE/inventory" "$inventory_body")"
if [[ "$inventory_http" != "200" || "$(json_get_file "status" "$inventory_body")" != "ok" || "$(json_get_file "payload.status" "$inventory_body")" != "ok" ]]; then
  echo "FAIL: inventory endpoint unhealthy at end" >&2
  cat "$inventory_body" >&2 || true
  exit 1
fi

echo "Step 7/7: summary"
echo "status=ok"
echo "hive_id=$HIVE_ID"
echo "hive_addr=$HIVE_ADDR"
echo "inventory add/remove hive E2E passed."
