#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
CATEGORY="${CATEGORY:-runtime}" # runtime|core|vendor
WAIT_SYNC_HINT_SECS="${WAIT_SYNC_HINT_SECS:-120}"
SYNC_HINT_TIMEOUT_MS="${SYNC_HINT_TIMEOUT_MS:-30000}"
SYNC_HINT_WAIT_FOR_IDLE="${SYNC_HINT_WAIT_FOR_IDLE:-1}"

if [[ "$CATEGORY" != "runtime" && "$CATEGORY" != "core" && "$CATEGORY" != "vendor" ]]; then
  echo "FAIL: CATEGORY must be runtime|core|vendor (got '$CATEGORY')" >&2
  exit 1
fi

post_update() {
  local version="$1"
  local hash="$2"
  curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
    -H "Content-Type: application/json" \
    -d "{\"category\":\"$CATEGORY\",\"manifest_version\":$version,\"manifest_hash\":\"$hash\"}"
}

post_sync_hint() {
  local channel="$1"
  local wait_for_idle="$2"
  local timeout_ms="$3"
  curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
    -H "Content-Type: application/json" \
    -d "{\"channel\":\"$channel\",\"folder_id\":\"fluxbee-$channel\",\"wait_for_idle\":$wait_for_idle,\"timeout_ms\":$timeout_ms}"
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
except Exception as exc:
    print(f"INVALID_JSON: {exc}", file=sys.stderr)
    print(raw[:500], file=sys.stderr)
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
    print(raw[:500], file=sys.stderr)
    sys.exit(2)

print(version)
print(h.strip())
PY
}

extract_payload_status() {
  local json_doc="$1"
  JSON_DOC="$json_doc" python3 - <<'PY'
import json
import os
raw = os.environ.get("JSON_DOC", "")
try:
    doc = json.loads(raw)
except Exception:
    print("")
    raise SystemExit(0)
print(doc.get("payload", {}).get("status", ""))
PY
}

echo "SYSTEM_UPDATE API E2E: BASE=$BASE HIVE_ID=$HIVE_ID CATEGORY=$CATEGORY"
echo "Step 1/4: fetch current versions"
versions="$(curl -sS "$BASE/hives/$HIVE_ID/versions")"
mapfile -t meta < <(extract_versions_meta "$CATEGORY" "$versions")
if [[ "${#meta[@]}" -lt 2 ]]; then
  echo "FAIL: could not resolve manifest version/hash from /hives/$HIVE_ID/versions" >&2
  echo "$versions" >&2
  exit 1
fi
manifest_version="${meta[0]}"
manifest_hash="${meta[1]}"
echo "resolved manifest_version=$manifest_version manifest_hash=$manifest_hash"

echo "Step 2/4: run sync-hint dist and wait ok"
deadline=$(( $(date +%s) + WAIT_SYNC_HINT_SECS ))
while (( $(date +%s) <= deadline )); do
  sync_hint_resp="$(post_sync_hint "dist" "$SYNC_HINT_WAIT_FOR_IDLE" "$SYNC_HINT_TIMEOUT_MS")"
  sync_hint_status="$(extract_payload_status "$sync_hint_resp")"
  if [[ "$sync_hint_status" == "ok" ]]; then
    echo "$sync_hint_resp"
    break
  fi
  if [[ "$sync_hint_status" == "sync_pending" ]]; then
    sleep 2
    continue
  fi
  echo "FAIL: expected sync-hint payload.status in {ok,sync_pending}, got '$sync_hint_status'" >&2
  echo "$sync_hint_resp" >&2
  exit 1
done
if [[ "${sync_hint_status:-}" != "ok" ]]; then
  echo "FAIL: sync-hint did not converge to ok within ${WAIT_SYNC_HINT_SECS}s" >&2
  echo "$sync_hint_resp" >&2
  exit 1
fi

echo "Step 3/4: send wrong hash (expect sync_pending)"
bad_hash="0000000000000000000000000000000000000000000000000000000000000000"
resp_bad="$(post_update "$manifest_version" "$bad_hash")"
status_bad="$(extract_payload_status "$resp_bad")"
echo "$resp_bad"
if [[ "$status_bad" != "sync_pending" ]]; then
  echo "FAIL: expected payload.status=sync_pending, got '$status_bad'" >&2
  exit 1
fi

echo "Step 4/4: send exact hash (expect ok)"
resp_ok="$(post_update "$manifest_version" "$manifest_hash")"
status_ok="$(extract_payload_status "$resp_ok")"
echo "$resp_ok"
if [[ "$CATEGORY" == "core" ]]; then
  if [[ "$status_ok" != "ok" && "$status_ok" != "rollback" ]]; then
    echo "FAIL: expected payload.status in {ok,rollback}, got '$status_ok'" >&2
    exit 1
  fi
else
  if [[ "$status_ok" != "ok" ]]; then
    echo "FAIL: expected payload.status=ok, got '$status_ok'" >&2
    exit 1
  fi
fi

echo "orchestrator SYSTEM_UPDATE API E2E passed."
