#!/usr/bin/env bash
set -euo pipefail

# FR9-T13/T17 helper E2E (socket):
# validates hive-scoped actions require payload.target in ADMIN_COMMAND.
#
# Covered:
# - list_nodes without target -> INVALID_REQUEST
# - get_versions without target -> INVALID_REQUEST
# - run_node without target (with hive_id in params) -> INVALID_REQUEST
#
# Usage:
#   ADMIN_TARGET="SY.admin@motherbee" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/admin_internal_target_required_e2e.sh

ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
TENANT_ID="${TENANT_ID:-}"
ADMIN_TIMEOUT_SECS="${ADMIN_TIMEOUT_SECS:-20}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIAG_BIN="${ADMIN_DIAG_BIN:-$ROOT_DIR/target/debug/admin_internal_command_diag}"

if [[ -z "$TENANT_ID" ]]; then
  echo "FAIL: TENANT_ID is required (format tnt:<uuid>)" >&2
  exit 1
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

require_cmd python3
if [[ ! -x "$DIAG_BIN" ]]; then
  require_cmd cargo
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

if [[ ! -x "$DIAG_BIN" ]]; then
  (cd "$ROOT_DIR" && cargo build --quiet --bin admin_internal_command_diag)
fi
if [[ ! -x "$DIAG_BIN" ]]; then
  echo "FAIL: admin_internal_command_diag binary missing at '$DIAG_BIN'" >&2
  exit 1
fi

run_case() {
  local case_id="$1"
  local action="$2"
  local params_json="$3"
  local expected_substr="$4"
  local out_file="$tmpdir/${case_id}.json"

  ADMIN_ACTION="$action" \
  ADMIN_TARGET="$ADMIN_TARGET" \
  ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
  ADMIN_EXPECT_STATUS="error" \
  ADMIN_PARAMS_JSON="$params_json" \
  "$DIAG_BIN" >"$out_file"

  python3 - "$out_file" "$case_id" "$expected_substr" <<'PY'
import json
import sys

path, case_id, expected_substr = sys.argv[1:]
doc = json.load(open(path, "r", encoding="utf-8"))
status = doc.get("status")
code = doc.get("error_code")
detail = doc.get("error_detail")

if status != "error":
    raise SystemExit(f"FAIL[{case_id}]: expected status=error, got {status!r}")
if code != "INVALID_REQUEST":
    raise SystemExit(f"FAIL[{case_id}]: expected error_code=INVALID_REQUEST, got {code!r}")
detail_str = detail if isinstance(detail, str) else json.dumps(detail, ensure_ascii=False)
if expected_substr not in detail_str:
    raise SystemExit(
        f"FAIL[{case_id}]: error_detail missing substring {expected_substr!r}; got {detail_str!r}"
    )
print(f"OK[{case_id}]")
PY
}

echo "ADMIN internal target-required E2E: ADMIN_TARGET=$ADMIN_TARGET"

run_case \
  "list_nodes_missing_target" \
  "list_nodes" \
  '{}' \
  "missing target"

run_case \
  "get_versions_missing_target" \
  "get_versions" \
  '{}' \
  "missing target"

run_case \
  "run_node_hive_id_without_target" \
  "run_node" \
  "{\"node_name\":\"WF.admin.target.required.$RANDOM\",\"runtime\":\"wf.orch.diag\",\"runtime_version\":\"current\",\"tenant_id\":\"$TENANT_ID\",\"hive_id\":\"worker-220\"}" \
  "missing target"

echo "admin internal target-required E2E passed."
