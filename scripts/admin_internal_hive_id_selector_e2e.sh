#!/usr/bin/env bash
set -euo pipefail

# FR9-T14/T15/T16 E2E (socket):
# - reject `hive_id` selector for non-hive-domain actions
# - keep `hive_id` allowed for add_hive/get_hive/remove_hive
#
# Usage:
#   ADMIN_TARGET="SY.admin@motherbee" \
#   HIVE_ID="worker-220" \
#   TENANT_ID="tnt:<uuid>" \
#   bash scripts/admin_internal_hive_id_selector_e2e.sh

ADMIN_TARGET="${ADMIN_TARGET:-SY.admin@motherbee}"
HIVE_ID="${HIVE_ID:-worker-220}"
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

run_error_case() {
  local case_id="$1"
  local action="$2"
  local params_json="$3"
  local out_file="$tmpdir/${case_id}.json"

  ADMIN_ACTION="$action" \
  ADMIN_TARGET="$ADMIN_TARGET" \
  ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
  ADMIN_EXPECT_STATUS="error" \
  ADMIN_PARAMS_JSON="$params_json" \
  "$DIAG_BIN" >"$out_file"

  python3 - "$out_file" "$case_id" <<'PY'
import json
import sys

path, case_id = sys.argv[1:]
doc = json.load(open(path, "r", encoding="utf-8"))
status = doc.get("status")
code = doc.get("error_code")
detail = doc.get("error_detail")

if status != "error":
    raise SystemExit(f"FAIL[{case_id}]: expected status=error, got {status!r}")
if code != "INVALID_REQUEST":
    raise SystemExit(f"FAIL[{case_id}]: expected error_code=INVALID_REQUEST, got {code!r}")
detail_str = detail if isinstance(detail, str) else json.dumps(detail, ensure_ascii=False)
needle = "legacy field 'hive_id' is not supported"
if needle not in detail_str:
    raise SystemExit(f"FAIL[{case_id}]: missing expected detail substring {needle!r}; got {detail_str!r}")
print(f"OK[{case_id}]")
PY
}

run_ok_case() {
  local case_id="$1"
  local action="$2"
  local params_json="$3"
  local out_file="$tmpdir/${case_id}.json"

  ADMIN_ACTION="$action" \
  ADMIN_TARGET="$ADMIN_TARGET" \
  ADMIN_TIMEOUT_SECS="$ADMIN_TIMEOUT_SECS" \
  ADMIN_PARAMS_JSON="$params_json" \
  "$DIAG_BIN" >"$out_file"

  python3 - "$out_file" "$case_id" <<'PY'
import json
import sys

path, case_id = sys.argv[1:]
doc = json.load(open(path, "r", encoding="utf-8"))
code = doc.get("error_code")
if code == "INVALID_REQUEST":
    raise SystemExit(f"FAIL[{case_id}]: unexpected INVALID_REQUEST for allowed hive action")
print(f"OK[{case_id}]")
PY
}

echo "ADMIN internal hive_id selector E2E: ADMIN_TARGET=$ADMIN_TARGET HIVE_ID=$HIVE_ID"

run_error_case \
  "run_node_hive_id_rejected" \
  "run_node" \
  "{\"node_name\":\"WF.admin.hiveid.$RANDOM\",\"runtime\":\"wf.orch.diag\",\"runtime_version\":\"current\",\"tenant_id\":\"$TENANT_ID\",\"hive_id\":\"$HIVE_ID\",\"target\":\"$HIVE_ID\"}"

run_error_case \
  "update_hive_id_rejected" \
  "update" \
  "{\"category\":\"runtime\",\"manifest_version\":1,\"manifest_hash\":\"deadbeef\",\"hive_id\":\"$HIVE_ID\",\"target\":\"$HIVE_ID\"}"

run_ok_case \
  "get_hive_hive_id_allowed" \
  "get_hive" \
  "{\"hive_id\":\"$HIVE_ID\"}"

echo "admin internal hive_id selector E2E passed."
