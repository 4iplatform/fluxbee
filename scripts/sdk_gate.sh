#!/usr/bin/env bash
set -euo pipefail

# Fluxbee SDK migration gate (local/CI): cargo check + fluxbee_sdk unit tests + blob diag
# (copy mode).
#
# Usage:
#   bash scripts/sdk_gate.sh
#
# Optional:
#   SDK_GATE_BUILD_BIN=0|1               # default: 0

SDK_GATE_BUILD_BIN="${SDK_GATE_BUILD_BIN:-0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

run_step() {
  local label="$1"
  shift
  echo "==> ${label}"
  "$@"
  echo "OK: ${label}"
}

require_cmd bash
require_cmd cargo
require_cmd rg

cd "$ROOT_DIR"

run_step "cargo check" cargo check -q
run_step "no direct jsr_client usage (M8)" \
  bash scripts/check_no_jsr_client_usage.sh
run_step "fluxbee_sdk unit tests (blob)" \
  cargo test -q -p fluxbee-sdk blob::tests
run_step "fluxbee_sdk unit tests (payload)" \
  cargo test -q -p fluxbee-sdk payload::tests

run_step "blob sync diag (copy mode, no syncthing required)" \
  env BUILD_BIN="$SDK_GATE_BUILD_BIN" \
      BLOB_DIAG_SYNC_MODE=copy \
      BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE=0 \
      SHOW_FULL_LOGS=0 \
      bash scripts/blob_sync_e2e.sh

echo "SDK gate passed."
