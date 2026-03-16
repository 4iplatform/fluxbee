#!/usr/bin/env bash
set -euo pipefail

# Validates no-legacy contract on SY.admin internal socket commands.
# Cases covered:
# - run_node with legacy `name` -> INVALID_REQUEST
# - run_node with legacy `version` -> INVALID_REQUEST
# - kill_node with legacy `name` -> INVALID_REQUEST
# - update with legacy `version` -> INVALID_REQUEST
# - update with legacy `hash` -> INVALID_REQUEST
#
# Usage:
#   ADMIN_TARGET="SY.admin@motherbee" TARGET_HIVE="worker-220" \
#   bash scripts/admin_internal_no_legacy_e2e.sh

ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
TARGET_HIVE="${TARGET_HIVE:-worker-220}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd python3

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

run_case() {
  local case_id="$1"
  local action="$2"
  local params_json="$3"
  local expected_code="$4"
  local expected_substr="$5"

  local out_file="$tmpdir/${case_id}.json"
  ADMIN_ACTION="$action" \
  ADMIN_TARGET="$ADMIN_TARGET" \
  ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
  ADMIN_EXPECT_STATUS="error" \
  ADMIN_PAYLOAD_TARGET="$TARGET_HIVE" \
  ADMIN_PARAMS_JSON="$params_json" \
  cargo run --quiet --bin admin_internal_command_diag >"$out_file"

  python3 - "$out_file" "$case_id" "$expected_code" "$expected_substr" <<'PY'
import json
import sys

path, case_id, expected_code, expected_substr = sys.argv[1:]
doc = json.load(open(path, "r", encoding="utf-8"))

status = doc.get("status")
code = doc.get("error_code")
detail = doc.get("error_detail")

if status != "error":
    raise SystemExit(f"FAIL[{case_id}]: expected status=error, got {status!r}")
if code != expected_code:
    raise SystemExit(f"FAIL[{case_id}]: expected error_code={expected_code!r}, got {code!r}")

detail_str = detail if isinstance(detail, str) else json.dumps(detail, ensure_ascii=False)
if expected_substr not in detail_str:
    raise SystemExit(
        f"FAIL[{case_id}]: error_detail missing substring {expected_substr!r}; got {detail_str!r}"
    )

print(f"OK[{case_id}]")
PY
}

echo "ADMIN internal no-legacy E2E: ADMIN_TARGET=$ADMIN_TARGET TARGET_HIVE=$TARGET_HIVE"

run_case \
  "run_node_legacy_name" \
  "run_node" \
  '{"name":"WF.legacy.internal","runtime":"wf.orch.diag","runtime_version":"current"}' \
  "INVALID_REQUEST" \
  "legacy field 'name' is not supported"

run_case \
  "run_node_legacy_version" \
  "run_node" \
  '{"node_name":"WF.legacy.internal","runtime":"wf.orch.diag","version":"current"}' \
  "INVALID_REQUEST" \
  "legacy field 'version' is not supported"

run_case \
  "kill_node_legacy_name" \
  "kill_node" \
  '{"name":"WF.legacy.internal"}' \
  "INVALID_REQUEST" \
  "legacy field 'name' is not supported"

run_case \
  "update_legacy_version" \
  "update" \
  '{"category":"runtime","version":1,"manifest_hash":"deadbeef"}' \
  "INVALID_REQUEST" \
  "legacy field 'version' is not supported"

run_case \
  "update_legacy_hash" \
  "update" \
  '{"category":"runtime","manifest_version":1,"hash":"deadbeef"}' \
  "INVALID_REQUEST" \
  "legacy field 'hash' is not supported"

echo "admin internal no-legacy E2E passed."

