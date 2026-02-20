#!/usr/bin/env bash
set -euo pipefail

# Runs a minimal WF NATS diagnostic pair:
# - server: subscribes + replies using jsr_client NATS
# - client: sends request_with_session_inbox and validates response path
#
# Usage:
#   bash scripts/wf_nats_diag.sh
#
# Optional:
#   NATS_URL="nats://127.0.0.1:4222"
#   WF_DIAG_SUBJECT="wf.diag.echo"
#   WF_DIAG_TIMEOUT_SECS="8"
#   WF_DIAG_LOOPS="3"
#   WF_DIAG_INTERVAL_MS="500"
#   WF_DIAG_SID="51001"
#   JSR_LOG_LEVEL="debug"
#   BUILD_BIN="1"
#   INCLUDE_ROUTER_JOURNAL="1"
#   DIAG_ROUTER_LOG_LINES="200"
#   STREAM_CLIENT_LOG="1"
#   STREAM_SERVER_LOG="0"

NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
WF_DIAG_SUBJECT="${WF_DIAG_SUBJECT:-wf.diag.echo}"
WF_DIAG_TIMEOUT_SECS="${WF_DIAG_TIMEOUT_SECS:-8}"
WF_DIAG_LOOPS="${WF_DIAG_LOOPS:-3}"
WF_DIAG_INTERVAL_MS="${WF_DIAG_INTERVAL_MS:-500}"
WF_DIAG_SID="${WF_DIAG_SID:-51001}"
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-debug}"
BUILD_BIN="${BUILD_BIN:-1}"
INCLUDE_ROUTER_JOURNAL="${INCLUDE_ROUTER_JOURNAL:-1}"
DIAG_ROUTER_LOG_LINES="${DIAG_ROUTER_LOG_LINES:-200}"
STREAM_CLIENT_LOG="${STREAM_CLIENT_LOG:-1}"
STREAM_SERVER_LOG="${STREAM_SERVER_LOG:-0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${WF_DIAG_BIN_PATH:-$ROOT_DIR/target/release/wf_nats_diag}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

print_router_journal_if_available() {
  if [[ "$INCLUDE_ROUTER_JOURNAL" != "1" ]]; then
    return 0
  fi
  if ! command -v journalctl >/dev/null 2>&1; then
    return 0
  fi

  local -a jcmd
  if [[ "$(id -u)" -eq 0 ]]; then
    jcmd=(journalctl)
  elif sudo -n true >/dev/null 2>&1; then
    jcmd=(sudo -n journalctl)
  else
    echo "INFO: skipping rt-gateway journal dump (sudo password required)" >&2
    return 0
  fi

  echo "---- rt-gateway journal (last ${DIAG_ROUTER_LOG_LINES}) ----"
  "${jcmd[@]}" -u rt-gateway -n "$DIAG_ROUTER_LOG_LINES" --no-pager || true
}

require_cmd cargo

if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  echo "Building wf_nats_diag..."
  (cd "$ROOT_DIR" && cargo build --release --bin wf_nats_diag)
fi

tmpdir="$(mktemp -d)"
server_log="$tmpdir/wf_nats_diag_server.log"
client_log="$tmpdir/wf_nats_diag_client.log"
server_pid=""
tail_server_pid=""

cleanup() {
  if [[ -n "$tail_server_pid" ]] && kill -0 "$tail_server_pid" >/dev/null 2>&1; then
    kill "$tail_server_pid" >/dev/null 2>&1 || true
    wait "$tail_server_pid" >/dev/null 2>&1 || true
  fi
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

echo "Starting WF diag server: NATS_URL=$NATS_URL SUBJECT=$WF_DIAG_SUBJECT SID=$WF_DIAG_SID"
(
  cd "$ROOT_DIR"
  WF_DIAG_MODE=server \
  NATS_URL="$NATS_URL" \
  WF_DIAG_SUBJECT="$WF_DIAG_SUBJECT" \
  WF_DIAG_SID="$WF_DIAG_SID" \
  JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
  "$BIN_PATH"
) >"$server_log" 2>&1 &
server_pid="$!"

sleep 1

if ! kill -0 "$server_pid" >/dev/null 2>&1; then
  echo "FAIL: wf nats diag server exited during startup" >&2
  echo "---- WF diag server log ----" >&2
  cat "$server_log" >&2 || true
  exit 1
fi

if [[ "$STREAM_SERVER_LOG" == "1" ]]; then
  echo "Streaming WF diag server log in background..."
  tail -f "$server_log" &
  tail_server_pid="$!"
fi

echo "Running WF diag client: LOOPS=$WF_DIAG_LOOPS TIMEOUT=${WF_DIAG_TIMEOUT_SECS}s"
echo "Estimated max wait: ~${WF_DIAG_LOOPS}x${WF_DIAG_TIMEOUT_SECS}s (+interval/retries)"
set +e
if [[ "$STREAM_CLIENT_LOG" == "1" ]]; then
  (
    cd "$ROOT_DIR"
    WF_DIAG_MODE=client \
    NATS_URL="$NATS_URL" \
    WF_DIAG_SUBJECT="$WF_DIAG_SUBJECT" \
    WF_DIAG_TIMEOUT_SECS="$WF_DIAG_TIMEOUT_SECS" \
    WF_DIAG_LOOPS="$WF_DIAG_LOOPS" \
    WF_DIAG_INTERVAL_MS="$WF_DIAG_INTERVAL_MS" \
    JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
    "$BIN_PATH"
  ) 2>&1 | tee "$client_log"
  client_rc=${PIPESTATUS[0]}
else
  (
    cd "$ROOT_DIR"
    WF_DIAG_MODE=client \
    NATS_URL="$NATS_URL" \
    WF_DIAG_SUBJECT="$WF_DIAG_SUBJECT" \
    WF_DIAG_TIMEOUT_SECS="$WF_DIAG_TIMEOUT_SECS" \
    WF_DIAG_LOOPS="$WF_DIAG_LOOPS" \
    WF_DIAG_INTERVAL_MS="$WF_DIAG_INTERVAL_MS" \
    JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
    "$BIN_PATH"
  ) >"$client_log" 2>&1
  client_rc=$?
fi
set -e

if [[ -n "${tail_server_pid:-}" ]] && kill -0 "$tail_server_pid" >/dev/null 2>&1; then
  kill "$tail_server_pid" >/dev/null 2>&1 || true
fi

echo "---- WF diag client log ----"
cat "$client_log"
echo "---- WF diag server log ----"
cat "$server_log"
print_router_journal_if_available

if [[ "$client_rc" -ne 0 ]]; then
  echo "FAIL: wf nats diag client exited with rc=$client_rc" >&2
  exit "$client_rc"
fi

echo "wf nats diag completed."
