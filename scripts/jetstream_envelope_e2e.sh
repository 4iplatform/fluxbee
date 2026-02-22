#!/usr/bin/env bash
set -euo pipefail

# JetStream envelope diagnostics (contract-light):
# - payload is opaque JSON
# - validates publish/consume/ack and redelivery when ack is skipped intentionally
#
# Usage:
#   bash scripts/jetstream_envelope_e2e.sh
#
# Optional:
#   NATS_URL="nats://127.0.0.1:4222"
#   JETSTREAM_DIAG_RUN_ID="manual-001"
#   JETSTREAM_DIAG_SUBJECT="jetstream.diag.envelope.<run_id>"
#   JETSTREAM_DIAG_QUEUE="durable.jetstream.diag.envelope.<run_id>"
#   JETSTREAM_DIAG_SID="52001"
#   JETSTREAM_DIAG_LOOPS="10"
#   JETSTREAM_DIAG_INTERVAL_MS="100"
#   JETSTREAM_DIAG_FAIL_FIRST_N="1"
#   JETSTREAM_DIAG_WAIT_SECS="0"   # auto when 0
#   JSR_LOG_LEVEL="info"
#   BUILD_BIN="1"
#   SHOW_FULL_LOGS="0"

NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
JETSTREAM_DIAG_RUN_ID="${JETSTREAM_DIAG_RUN_ID:-$(date +%s)-$RANDOM}"
JETSTREAM_DIAG_SUBJECT="${JETSTREAM_DIAG_SUBJECT:-jetstream.diag.envelope.${JETSTREAM_DIAG_RUN_ID}}"
JETSTREAM_DIAG_QUEUE="${JETSTREAM_DIAG_QUEUE:-durable.jetstream.diag.envelope.${JETSTREAM_DIAG_RUN_ID}}"
JETSTREAM_DIAG_SID="${JETSTREAM_DIAG_SID:-52001}"
JETSTREAM_DIAG_LOOPS="${JETSTREAM_DIAG_LOOPS:-10}"
JETSTREAM_DIAG_INTERVAL_MS="${JETSTREAM_DIAG_INTERVAL_MS:-100}"
JETSTREAM_DIAG_FAIL_FIRST_N="${JETSTREAM_DIAG_FAIL_FIRST_N:-1}"
JETSTREAM_DIAG_WAIT_SECS="${JETSTREAM_DIAG_WAIT_SECS:-0}"
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}"
BUILD_BIN="${BUILD_BIN:-1}"
SHOW_FULL_LOGS="${SHOW_FULL_LOGS:-0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${JETSTREAM_DIAG_BIN_PATH:-$ROOT_DIR/target/release/jetstream_envelope_diag}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd rg

if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  echo "Building jetstream_envelope_diag..."
  (cd "$ROOT_DIR" && cargo build --release --bin jetstream_envelope_diag)
fi

tmpdir="$(mktemp -d)"
server_log="$tmpdir/jetstream_envelope_server.log"
client_log="$tmpdir/jetstream_envelope_client.log"
server_pid=""

cleanup() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo "Starting JetStream envelope diag server: NATS_URL=$NATS_URL RUN_ID=$JETSTREAM_DIAG_RUN_ID SUBJECT=$JETSTREAM_DIAG_SUBJECT QUEUE=$JETSTREAM_DIAG_QUEUE SID=$JETSTREAM_DIAG_SID FAIL_FIRST_N=$JETSTREAM_DIAG_FAIL_FIRST_N"
(
  cd "$ROOT_DIR"
  JETSTREAM_DIAG_MODE=server \
  NATS_URL="$NATS_URL" \
  JETSTREAM_DIAG_SUBJECT="$JETSTREAM_DIAG_SUBJECT" \
  JETSTREAM_DIAG_QUEUE="$JETSTREAM_DIAG_QUEUE" \
  JETSTREAM_DIAG_SID="$JETSTREAM_DIAG_SID" \
  JETSTREAM_DIAG_FAIL_FIRST_N="$JETSTREAM_DIAG_FAIL_FIRST_N" \
  JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
  "$BIN_PATH"
) >"$server_log" 2>&1 &
server_pid="$!"

sleep 1
if ! kill -0 "$server_pid" >/dev/null 2>&1; then
  echo "FAIL: jetstream envelope server exited during startup" >&2
  cat "$server_log" >&2 || true
  exit 1
fi

echo "Running JetStream envelope diag client: LOOPS=$JETSTREAM_DIAG_LOOPS INTERVAL_MS=$JETSTREAM_DIAG_INTERVAL_MS"
set +e
(
  cd "$ROOT_DIR"
  JETSTREAM_DIAG_MODE=client \
  NATS_URL="$NATS_URL" \
  JETSTREAM_DIAG_SUBJECT="$JETSTREAM_DIAG_SUBJECT" \
  JETSTREAM_DIAG_LOOPS="$JETSTREAM_DIAG_LOOPS" \
  JETSTREAM_DIAG_INTERVAL_MS="$JETSTREAM_DIAG_INTERVAL_MS" \
  JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
  "$BIN_PATH"
) >"$client_log" 2>&1
client_rc=$?
set -e

if [[ "$client_rc" -ne 0 ]]; then
  echo "FAIL: client exited with rc=$client_rc" >&2
  cat "$client_log" >&2 || true
  exit "$client_rc"
fi

wait_secs="$JETSTREAM_DIAG_WAIT_SECS"
if [[ "$wait_secs" == "0" ]]; then
  wait_secs=$(( (JETSTREAM_DIAG_FAIL_FIRST_N * 3) + 2 ))
fi

echo "Waiting ${wait_secs}s for redelivery/acks to settle..."
sleep "$wait_secs"

if kill -0 "$server_pid" >/dev/null 2>&1; then
  kill "$server_pid" >/dev/null 2>&1 || true
  wait "$server_pid" >/dev/null 2>&1 || true
fi

client_published="$(rg -c "jetstream diag client published" "$client_log" || true)"
server_received="$(rg -c "jetstream diag server received" "$server_log" || true)"
server_acked="$(rg -c "jetstream diag server acked" "$server_log" || true)"
server_noack="$(rg -c "jetstream diag server intentionally not acking" "$server_log" || true)"

echo "---- JetStream envelope compact timeline ----"
{
  rg -n "jetstream diag client published|jetstream diag server received|jetstream diag server intentionally not acking|jetstream diag server acked" "$client_log" "$server_log" || true
} | sed -E 's/[0-9a-f]{8}-[0-9a-f-]{27,}/<uuid>/g'

echo "---- JetStream envelope summary ----"
echo "client_published=${client_published}"
echo "server_received=${server_received}"
echo "server_acked=${server_acked}"
echo "server_intentional_noack=${server_noack}"

if [[ "$SHOW_FULL_LOGS" == "1" ]]; then
  echo "---- client log ----"
  cat "$client_log"
  echo "---- server log ----"
  cat "$server_log"
fi

if [[ "$client_published" -ne "$JETSTREAM_DIAG_LOOPS" ]]; then
  echo "FAIL: published count mismatch (expected=$JETSTREAM_DIAG_LOOPS got=$client_published)" >&2
  exit 1
fi

if [[ "$server_acked" -ne "$JETSTREAM_DIAG_LOOPS" ]]; then
  echo "FAIL: acked count mismatch (expected=$JETSTREAM_DIAG_LOOPS got=$server_acked)" >&2
  exit 1
fi

if (( server_received < JETSTREAM_DIAG_LOOPS )); then
  echo "FAIL: received count too low (expected >=$JETSTREAM_DIAG_LOOPS got=$server_received)" >&2
  exit 1
fi

if (( JETSTREAM_DIAG_FAIL_FIRST_N > 0 )) && (( server_noack < JETSTREAM_DIAG_FAIL_FIRST_N )); then
  echo "FAIL: expected at least $JETSTREAM_DIAG_FAIL_FIRST_N intentional no-ack events, got $server_noack" >&2
  exit 1
fi

echo "jetstream envelope E2E passed."
