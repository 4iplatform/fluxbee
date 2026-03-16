#!/usr/bin/env bash
set -euo pipefail

# Verifies SY.admin internal socket command path for inventory:
# ADMIN_COMMAND(action=inventory) -> ADMIN_COMMAND_RESPONSE(status=ok)
#
# Usage:
#   ADMIN_TARGET="SY.admin@motherbee" bash scripts/admin_internal_inventory_socket_e2e.sh
#
# Optional:
#   INVENTORY_SCOPE="summary"         # global|hive|summary
#   INVENTORY_FILTER_HIVE="worker-220"
#   INVENTORY_FILTER_TYPE="WF"
#   ADMIN_TIMEOUT_SECS="20"

ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
INVENTORY_SCOPE="${INVENTORY_SCOPE:-summary}"
INVENTORY_FILTER_HIVE="${INVENTORY_FILTER_HIVE:-}"
INVENTORY_FILTER_TYPE="${INVENTORY_FILTER_TYPE:-}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

params_file="$tmpdir/params.json"
python3 - "$params_file" "$INVENTORY_SCOPE" "$INVENTORY_FILTER_HIVE" "$INVENTORY_FILTER_TYPE" <<'PY'
import json
import sys

path, scope, hive, kind = sys.argv[1:]
payload = {"scope": scope}
if hive:
    payload["filter_hive"] = hive
if kind:
    payload["filter_type"] = kind
with open(path, "w", encoding="utf-8") as f:
    json.dump(payload, f)
PY

out_file="$tmpdir/response.json"
ADMIN_ACTION="inventory" \
ADMIN_TARGET="$ADMIN_TARGET" \
ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
ADMIN_EXPECT_STATUS="ok" \
ADMIN_PARAMS_JSON="$(cat "$params_file")" \
cargo run --quiet --bin admin_internal_command_diag >"$out_file"

python3 - "$out_file" <<'PY'
import json
import sys

path = sys.argv[1]
doc = json.load(open(path, "r", encoding="utf-8"))
status = doc.get("status")
action = doc.get("action")
payload = doc.get("payload") if isinstance(doc.get("payload"), dict) else {}
payload_status = payload.get("status")

if status != "ok":
    raise SystemExit(f"FAIL: expected top-level status=ok, got {status!r}")
if action != "inventory":
    raise SystemExit(f"FAIL: expected action=inventory, got {action!r}")
if payload_status != "ok":
    raise SystemExit(f"FAIL: expected payload.status=ok, got {payload_status!r}")

print("status=ok")
print(f"action={action}")
print(f"inventory_status={payload_status}")
PY

echo "admin internal inventory socket E2E passed."

