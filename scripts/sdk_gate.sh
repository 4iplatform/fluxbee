#!/usr/bin/env bash
set -euo pipefail

# Fluxbee SDK migration gate (local/CI):
# - mandatory core: cargo check + fluxbee_sdk unit tests + blob diag (copy mode)
# - full profile adds NATS diagnostics using fluxbee_sdk stack
#
# Usage:
#   bash scripts/sdk_gate.sh
#
# Optional:
#   SDK_GATE_PROFILE=quick|full          # default: full
#   SDK_GATE_BUILD_BIN=0|1               # default: 0
#   NATS_URL=nats://127.0.0.1:4222
#   SDK_GATE_WF_LOOPS=5
#   SDK_GATE_WF_TIMEOUT_SECS=3
#   SDK_GATE_WF_INTERVAL_MS=100
#   SDK_GATE_JETSTREAM_LOOPS=10
#   SDK_GATE_JETSTREAM_INTERVAL_MS=50
#   SDK_GATE_JETSTREAM_FAIL_FIRST_N=1
#   SDK_GATE_JETSTREAM_CHECK_RESTART=0|1 # default: 0 (CI/local estable)

SDK_GATE_PROFILE="${SDK_GATE_PROFILE:-full}"
SDK_GATE_BUILD_BIN="${SDK_GATE_BUILD_BIN:-0}"
NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"

SDK_GATE_WF_LOOPS="${SDK_GATE_WF_LOOPS:-5}"
SDK_GATE_WF_TIMEOUT_SECS="${SDK_GATE_WF_TIMEOUT_SECS:-3}"
SDK_GATE_WF_INTERVAL_MS="${SDK_GATE_WF_INTERVAL_MS:-100}"

SDK_GATE_JETSTREAM_LOOPS="${SDK_GATE_JETSTREAM_LOOPS:-10}"
SDK_GATE_JETSTREAM_INTERVAL_MS="${SDK_GATE_JETSTREAM_INTERVAL_MS:-50}"
SDK_GATE_JETSTREAM_FAIL_FIRST_N="${SDK_GATE_JETSTREAM_FAIL_FIRST_N:-1}"
SDK_GATE_JETSTREAM_CHECK_RESTART="${SDK_GATE_JETSTREAM_CHECK_RESTART:-0}"

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

case "$SDK_GATE_PROFILE" in
  quick|full) ;;
  *)
    echo "Error: SDK_GATE_PROFILE must be quick|full (got '$SDK_GATE_PROFILE')" >&2
    exit 1
    ;;
esac

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

if [[ "$SDK_GATE_PROFILE" == "full" ]]; then
  run_step "wf nats diag (fluxbee_sdk path)" \
    env BUILD_BIN="$SDK_GATE_BUILD_BIN" \
        NATS_URL="$NATS_URL" \
        WF_DIAG_LOOPS="$SDK_GATE_WF_LOOPS" \
        WF_DIAG_TIMEOUT_SECS="$SDK_GATE_WF_TIMEOUT_SECS" \
        WF_DIAG_INTERVAL_MS="$SDK_GATE_WF_INTERVAL_MS" \
        INCLUDE_ROUTER_JOURNAL=0 \
        STREAM_CLIENT_LOG=0 \
        SHOW_FULL_LOGS=0 \
        SHOW_TIMING_SUMMARY=1 \
        JSR_LOG_LEVEL=info \
        bash scripts/wf_nats_diag.sh

  run_step "jetstream envelope diag (stack=fluxbee_sdk)" \
    env BUILD_BIN="$SDK_GATE_BUILD_BIN" \
        NATS_URL="$NATS_URL" \
        JETSTREAM_DIAG_STACK=fluxbee_sdk \
        JETSTREAM_DIAG_LOOPS="$SDK_GATE_JETSTREAM_LOOPS" \
        JETSTREAM_DIAG_INTERVAL_MS="$SDK_GATE_JETSTREAM_INTERVAL_MS" \
        JETSTREAM_DIAG_FAIL_FIRST_N="$SDK_GATE_JETSTREAM_FAIL_FIRST_N" \
        JETSTREAM_DIAG_CHECK_RESTART="$SDK_GATE_JETSTREAM_CHECK_RESTART" \
        SHOW_FULL_LOGS=0 \
        JSR_LOG_LEVEL=info \
        bash scripts/jetstream_envelope_e2e.sh
fi

echo "SDK gate passed (profile=${SDK_GATE_PROFILE})."
