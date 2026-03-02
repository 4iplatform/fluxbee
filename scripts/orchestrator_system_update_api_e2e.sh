#!/usr/bin/env bash
set -euo pipefail

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
CATEGORY="${CATEGORY:-runtime}" # runtime|core|vendor

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

extract_versions_meta() {
  python3 - "$CATEGORY" <<'PY'
import json
import sys

category = sys.argv[1]
doc = json.load(sys.stdin)
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

extract_payload_status() {
  python3 - <<'PY'
import json
import sys
doc = json.load(sys.stdin)
print(doc.get("payload", {}).get("status", ""))
PY
}

echo "SYSTEM_UPDATE API E2E: BASE=$BASE HIVE_ID=$HIVE_ID CATEGORY=$CATEGORY"
echo "Step 1/3: fetch current versions"
versions="$(curl -sS "$BASE/hives/$HIVE_ID/versions")"
mapfile -t meta < <(printf '%s' "$versions" | extract_versions_meta)
manifest_version="${meta[0]}"
manifest_hash="${meta[1]}"
echo "resolved manifest_version=$manifest_version manifest_hash=$manifest_hash"

echo "Step 2/3: send wrong hash (expect sync_pending)"
bad_hash="0000000000000000000000000000000000000000000000000000000000000000"
resp_bad="$(post_update "$manifest_version" "$bad_hash")"
status_bad="$(printf '%s' "$resp_bad" | extract_payload_status)"
echo "$resp_bad"
if [[ "$status_bad" != "sync_pending" ]]; then
  echo "FAIL: expected payload.status=sync_pending, got '$status_bad'" >&2
  exit 1
fi

echo "Step 3/3: send exact hash (expect ok)"
resp_ok="$(post_update "$manifest_version" "$manifest_hash")"
status_ok="$(printf '%s' "$resp_ok" | extract_payload_status)"
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
