#!/usr/bin/env bash
set -euo pipefail

# E2E negativo de versionado de RUNTIME_UPDATE:
# 1) enviar runtime manifest base (version alta) -> debe aplicar
# 2) enviar runtime manifest stale (version menor) -> debe rechazar con VERSION_MISMATCH
#
# Usage:
#   HIVE_ID="worker-220" BUILD_BIN=0 bash scripts/orchestrator_runtime_update_stale_e2e.sh

HIVE_ID="${HIVE_ID:-worker-220}"
BASE="${BASE:-http://127.0.0.1:8080}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
ORCH_RUNTIME_CURRENT="${ORCH_RUNTIME_CURRENT:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
BUILD_BIN="${BUILD_BIN:-1}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

http_status() {
  local url="$1"
  curl -sS -o /dev/null -w "%{http_code}" "$url"
}

require_cmd bash
require_cmd curl
require_cmd cargo

if [[ "$(http_status "$BASE/health")" != "200" ]]; then
  echo "FAIL: $BASE/health not ready" >&2
  exit 1
fi
if [[ "$(http_status "$BASE/hives/$HIVE_ID")" != "200" ]]; then
  echo "FAIL: hive '$HIVE_ID' not found in admin API" >&2
  exit 1
fi

if [[ "$BUILD_BIN" == "1" ]]; then
  (cd "$ROOT_DIR" && cargo build --release --bin orch_system_diag)
fi

base_manifest_version="$(date +%s%3N)"
stale_manifest_version="$((base_manifest_version - 1))"

echo "Step 1/2: applying base runtime update version=${base_manifest_version}"
(
  cd "$ROOT_DIR"
  ORCH_TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_RUNTIME_CURRENT="$ORCH_RUNTIME_CURRENT" \
  ORCH_RUNTIME_AVAILABLE="$ORCH_RUNTIME_CURRENT" \
  ORCH_RUNTIME_MANIFEST_VERSION="$base_manifest_version" \
  ORCH_SEND_RUNTIME_UPDATE=1 \
  ORCH_ONLY_RUNTIME_UPDATE=1 \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  ./target/release/orch_system_diag
)

echo "Step 2/2: sending stale runtime update version=${stale_manifest_version} (expect VERSION_MISMATCH)"
(
  cd "$ROOT_DIR"
  ORCH_TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_RUNTIME_CURRENT="$ORCH_RUNTIME_CURRENT" \
  ORCH_RUNTIME_AVAILABLE="$ORCH_RUNTIME_CURRENT" \
  ORCH_RUNTIME_MANIFEST_VERSION="$stale_manifest_version" \
  ORCH_SEND_RUNTIME_UPDATE=1 \
  ORCH_ONLY_RUNTIME_UPDATE=1 \
  ORCH_EXPECT_RUNTIME_UPDATE_ERROR_CODE="VERSION_MISMATCH" \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  ./target/release/orch_system_diag
)

echo "orchestrator runtime stale-update E2E passed."
