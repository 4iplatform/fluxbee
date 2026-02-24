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
#   JETSTREAM_DIAG_STACK="fluxbee_sdk"   # router_nats | fluxbee_sdk (legacy alias: jsr_client)
#   JETSTREAM_DIAG_TRACE_PREFIX="js-env"
#   JETSTREAM_DIAG_TRACE_PREFIX_REPLAY="js-rpl"
#   JETSTREAM_DIAG_LOOPS="10"
#   JETSTREAM_DIAG_INTERVAL_MS="100"
#   JETSTREAM_DIAG_FAIL_FIRST_N="1"
#   JETSTREAM_DIAG_CHECK_RESTART="1"
#   JETSTREAM_DIAG_REPLAY_LOOPS="5"
#   JETSTREAM_DIAG_ROUTER_SERVICE="rt-gateway"
#   JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS="3"
#   JETSTREAM_DIAG_SYSTEMCTL_RETRY_DELAY_SECS="1"
#   JETSTREAM_DIAG_WAIT_SECS="0"   # auto when 0
#   JETSTREAM_DIAG_MAX_WAIT_SECS="45"
#   JSR_LOG_LEVEL="info"
#   BUILD_BIN="1"
#   SHOW_FULL_LOGS="0"

NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}"
JETSTREAM_DIAG_RUN_ID="${JETSTREAM_DIAG_RUN_ID:-$(date +%s)-$RANDOM}"
JETSTREAM_DIAG_SUBJECT="${JETSTREAM_DIAG_SUBJECT:-jetstream.diag.envelope.${JETSTREAM_DIAG_RUN_ID}}"
JETSTREAM_DIAG_QUEUE="${JETSTREAM_DIAG_QUEUE:-durable.jetstream.diag.envelope.${JETSTREAM_DIAG_RUN_ID}}"
JETSTREAM_DIAG_SID="${JETSTREAM_DIAG_SID:-52001}"
JETSTREAM_DIAG_STACK="${JETSTREAM_DIAG_STACK:-fluxbee_sdk}"
JETSTREAM_DIAG_TRACE_PREFIX="${JETSTREAM_DIAG_TRACE_PREFIX:-js-env}"
JETSTREAM_DIAG_TRACE_PREFIX_REPLAY="${JETSTREAM_DIAG_TRACE_PREFIX_REPLAY:-js-rpl}"
JETSTREAM_DIAG_LOOPS="${JETSTREAM_DIAG_LOOPS:-10}"
JETSTREAM_DIAG_INTERVAL_MS="${JETSTREAM_DIAG_INTERVAL_MS:-100}"
JETSTREAM_DIAG_FAIL_FIRST_N="${JETSTREAM_DIAG_FAIL_FIRST_N:-1}"
JETSTREAM_DIAG_CHECK_RESTART="${JETSTREAM_DIAG_CHECK_RESTART:-1}"
JETSTREAM_DIAG_REPLAY_LOOPS="${JETSTREAM_DIAG_REPLAY_LOOPS:-5}"
JETSTREAM_DIAG_ROUTER_SERVICE="${JETSTREAM_DIAG_ROUTER_SERVICE:-rt-gateway}"
JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS="${JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS:-3}"
JETSTREAM_DIAG_SYSTEMCTL_RETRY_DELAY_SECS="${JETSTREAM_DIAG_SYSTEMCTL_RETRY_DELAY_SECS:-1}"
JETSTREAM_DIAG_WAIT_SECS="${JETSTREAM_DIAG_WAIT_SECS:-0}"
JETSTREAM_DIAG_MAX_WAIT_SECS="${JETSTREAM_DIAG_MAX_WAIT_SECS:-45}"
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

if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
else
  SUDO=""
fi

service_active() {
  local service="$1"
  ${SUDO} systemctl is-active --quiet "$service"
}

wait_for_service_state() {
  local service="$1"
  local expected="$2" # active|inactive
  local label="$3"
  local start now elapsed
  start="$(date +%s)"
  while true; do
    now="$(date +%s)"
    elapsed=$((now - start))
    if [[ "$expected" == "active" ]] && service_active "$service"; then
      echo "OK: service '$service' is active ($label)"
      return 0
    fi
    if [[ "$expected" == "inactive" ]] && ! service_active "$service"; then
      echo "OK: service '$service' is inactive ($label)"
      return 0
    fi
    if (( elapsed >= JETSTREAM_DIAG_MAX_WAIT_SECS )); then
      echo "FAIL [$label]: timeout waiting service '$service' state '$expected'" >&2
      ${SUDO} systemctl status "$service" --no-pager -l || true
      exit 1
    fi
    sleep 1
  done
}

run_systemctl_action() {
  local action="$1"
  local service="$2"
  local label="$3"
  local attempt output rc
  for ((attempt = 1; attempt <= JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS; attempt++)); do
    if output="$(${SUDO} systemctl "$action" "$service" 2>&1)"; then
      if [[ -n "$output" ]]; then
        echo "$output"
      fi
      return 0
    fi
    rc=$?
    echo "WARN [$label]: systemctl $action $service failed (attempt ${attempt}/${JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS}, rc=${rc})" >&2
    if [[ -n "$output" ]]; then
      echo "WARN [$label]: ${output}" >&2
    fi
    if [[ "$action" == "stop" ]] && ! service_active "$service"; then
      return 0
    fi
    if [[ "$action" == "start" || "$action" == "restart" ]] && service_active "$service"; then
      return 0
    fi
    if (( attempt < JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS )); then
      sleep "$JETSTREAM_DIAG_SYSTEMCTL_RETRY_DELAY_SECS"
    fi
  done
  echo "FAIL [$label]: systemctl $action $service failed after ${JETSTREAM_DIAG_SYSTEMCTL_RETRY_ATTEMPTS} attempts" >&2
  ${SUDO} systemctl status "$service" --no-pager -l || true
  exit 1
}

start_server() {
  local fail_first_n="$1"
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
    echo "FAIL: server already running (pid=$server_pid)" >&2
    exit 1
  fi
  echo "Starting JetStream envelope diag server: NATS_URL=$NATS_URL RUN_ID=$JETSTREAM_DIAG_RUN_ID STACK=$JETSTREAM_DIAG_STACK SUBJECT=$JETSTREAM_DIAG_SUBJECT QUEUE=$JETSTREAM_DIAG_QUEUE SID=$JETSTREAM_DIAG_SID FAIL_FIRST_N=$fail_first_n"
  (
    cd "$ROOT_DIR"
    JETSTREAM_DIAG_MODE=server \
    JETSTREAM_DIAG_STACK="$JETSTREAM_DIAG_STACK" \
    NATS_URL="$NATS_URL" \
    JETSTREAM_DIAG_SUBJECT="$JETSTREAM_DIAG_SUBJECT" \
    JETSTREAM_DIAG_QUEUE="$JETSTREAM_DIAG_QUEUE" \
    JETSTREAM_DIAG_SID="$JETSTREAM_DIAG_SID" \
    JETSTREAM_DIAG_FAIL_FIRST_N="$fail_first_n" \
    JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
    "$BIN_PATH"
  ) >>"$server_log" 2>&1 &
  server_pid="$!"
  sleep 1
  if ! kill -0 "$server_pid" >/dev/null 2>&1; then
    echo "FAIL: jetstream envelope server exited during startup" >&2
    cat "$server_log" >&2 || true
    exit 1
  fi
}

stop_server() {
  if [[ -n "$server_pid" ]] && kill -0 "$server_pid" >/dev/null 2>&1; then
    kill "$server_pid" >/dev/null 2>&1 || true
    wait "$server_pid" >/dev/null 2>&1 || true
  fi
  server_pid=""
}

run_client_phase() {
  local loops="$1"
  local trace_prefix="$2"
  local seq_start="$3"
  local phase="$4"
  echo "Running JetStream envelope diag client (${phase}): LOOPS=$loops INTERVAL_MS=$JETSTREAM_DIAG_INTERVAL_MS TRACE_PREFIX=$trace_prefix SEQ_START=$seq_start"
  set +e
  (
    cd "$ROOT_DIR"
    JETSTREAM_DIAG_MODE=client \
    JETSTREAM_DIAG_STACK="$JETSTREAM_DIAG_STACK" \
    NATS_URL="$NATS_URL" \
    JETSTREAM_DIAG_SUBJECT="$JETSTREAM_DIAG_SUBJECT" \
    JETSTREAM_DIAG_LOOPS="$loops" \
    JETSTREAM_DIAG_INTERVAL_MS="$JETSTREAM_DIAG_INTERVAL_MS" \
    JETSTREAM_DIAG_TRACE_PREFIX="$trace_prefix" \
    JETSTREAM_DIAG_SEQ_START="$seq_start" \
    JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
    "$BIN_PATH"
  ) >>"$client_log" 2>&1
  local client_rc=$?
  set -e
  if [[ "$client_rc" -ne 0 ]]; then
    echo "FAIL: client (${phase}) exited with rc=$client_rc" >&2
    cat "$client_log" >&2 || true
    exit "$client_rc"
  fi
}

wait_for_convergence() {
  local expected_acked="$1"
  local expected_noack="$2"
  local label="$3"
  local start_ts now_ts elapsed
  echo "Waiting for convergence (${label}): expected_acked=${expected_acked} expected_noack>=${expected_noack} max_wait=${JETSTREAM_DIAG_MAX_WAIT_SECS}s"
  start_ts="$(date +%s)"
  while true; do
    local server_acked_now server_noack_now
    server_acked_now="$(rg -c "jetstream diag server acked" "$server_log" || true)"
    server_noack_now="$(rg -c "jetstream diag server intentionally not acking" "$server_log" || true)"
    if [[ "$server_acked_now" -ge "$expected_acked" ]] && [[ "$server_noack_now" -ge "$expected_noack" ]]; then
      return 0
    fi
    if [[ -n "$server_pid" ]] && ! kill -0 "$server_pid" >/dev/null 2>&1; then
      echo "FAIL: server process exited before convergence (${label})" >&2
      echo "---- server tail ----" >&2
      tail -n 80 "$server_log" >&2 || true
      exit 1
    fi
    now_ts="$(date +%s)"
    elapsed=$(( now_ts - start_ts ))
    if [[ "$elapsed" -ge "$JETSTREAM_DIAG_MAX_WAIT_SECS" ]]; then
      return 1
    fi
    sleep 1
  done
}

cleanup() {
  stop_server
}
trap cleanup EXIT

if [[ "$JETSTREAM_DIAG_CHECK_RESTART" == "1" ]] && (( JETSTREAM_DIAG_REPLAY_LOOPS > 0 )); then
  require_cmd systemctl
fi

start_server "$JETSTREAM_DIAG_FAIL_FIRST_N"
run_client_phase "$JETSTREAM_DIAG_LOOPS" "$JETSTREAM_DIAG_TRACE_PREFIX" "0" "base"

wait_secs="$JETSTREAM_DIAG_WAIT_SECS"
if [[ "$wait_secs" != "0" ]]; then
  echo "Waiting fixed ${wait_secs}s before convergence checks..."
  sleep "$wait_secs"
fi

if ! wait_for_convergence "$JETSTREAM_DIAG_LOOPS" "$JETSTREAM_DIAG_FAIL_FIRST_N" "base_phase"; then
  echo "FAIL: base phase did not converge in ${JETSTREAM_DIAG_MAX_WAIT_SECS}s" >&2
  exit 1
fi

base_server_acked_before_replay="$(rg -c "jetstream diag server acked" "$server_log" || true)"

expected_total_published="$JETSTREAM_DIAG_LOOPS"
expected_total_acked="$JETSTREAM_DIAG_LOOPS"
if [[ "$JETSTREAM_DIAG_CHECK_RESTART" == "1" ]] && (( JETSTREAM_DIAG_REPLAY_LOOPS > 0 )); then
  echo "Step: replay durable post-restart scenario"
  echo "Step: stopping diag server to accumulate durable backlog..."
  stop_server
  run_client_phase "$JETSTREAM_DIAG_REPLAY_LOOPS" "$JETSTREAM_DIAG_TRACE_PREFIX_REPLAY" "$JETSTREAM_DIAG_LOOPS" "replay_publish"
  expected_total_published=$((expected_total_published + JETSTREAM_DIAG_REPLAY_LOOPS))
  expected_total_acked=$((expected_total_acked + JETSTREAM_DIAG_REPLAY_LOOPS))

  echo "Step: restarting router service '$JETSTREAM_DIAG_ROUTER_SERVICE'..."
  run_systemctl_action "restart" "$JETSTREAM_DIAG_ROUTER_SERVICE" "router_restart_replay"
  wait_for_service_state "$JETSTREAM_DIAG_ROUTER_SERVICE" "active" "after_router_restart_replay"

  echo "Step: restarting diag server and verifying replay..."
  start_server "0"
  if ! wait_for_convergence "$expected_total_acked" "$JETSTREAM_DIAG_FAIL_FIRST_N" "post_restart_replay"; then
    echo "FAIL: replay phase did not converge in ${JETSTREAM_DIAG_MAX_WAIT_SECS}s" >&2
    exit 1
  fi
fi

stop_server

client_published="$(rg -c "jetstream diag client published" "$client_log" || true)"
server_received="$(rg -c "jetstream diag server received" "$server_log" || true)"
server_acked="$(rg -c "jetstream diag server acked" "$server_log" || true)"
server_noack="$(rg -c "jetstream diag server intentionally not acking" "$server_log" || true)"
replay_seq_start="$JETSTREAM_DIAG_LOOPS"
replay_seq_end="$((JETSTREAM_DIAG_LOOPS + JETSTREAM_DIAG_REPLAY_LOOPS - 1))"
server_replay_acked=0
if [[ "$JETSTREAM_DIAG_CHECK_RESTART" == "1" ]] && (( JETSTREAM_DIAG_REPLAY_LOOPS > 0 )); then
  if [[ -z "${base_server_acked_before_replay:-}" ]]; then
    base_server_acked_before_replay=0
  fi
  if (( server_acked > base_server_acked_before_replay )); then
    server_replay_acked=$((server_acked - base_server_acked_before_replay))
  fi
fi

echo "---- JetStream envelope compact timeline ----"
{
  rg -n "jetstream diag client published|jetstream diag server received|jetstream diag server intentionally not acking|jetstream diag server acked" "$client_log" "$server_log" || true
} | sed -E 's/[0-9a-f]{8}-[0-9a-f-]{27,}/<uuid>/g'

echo "---- JetStream envelope summary ----"
echo "client_published=${client_published}"
echo "server_received=${server_received}"
echo "server_acked=${server_acked}"
echo "server_intentional_noack=${server_noack}"
if [[ "$JETSTREAM_DIAG_CHECK_RESTART" == "1" ]] && (( JETSTREAM_DIAG_REPLAY_LOOPS > 0 )); then
  echo "server_replayed_after_restart_acked=${server_replay_acked}"
  echo "server_replay_seq_window=${replay_seq_start}-${replay_seq_end}"
fi

if [[ "$SHOW_FULL_LOGS" == "1" ]]; then
  echo "---- client log ----"
  cat "$client_log"
  echo "---- server log ----"
  cat "$server_log"
fi

if [[ "$client_published" -ne "$expected_total_published" ]]; then
  echo "FAIL: published count mismatch (expected=$expected_total_published got=$client_published)" >&2
  exit 1
fi

if (( server_acked < expected_total_acked )); then
  echo "FAIL: acked count too low (expected >=$expected_total_acked got=$server_acked)" >&2
  exit 1
fi

if (( server_received < expected_total_acked )); then
  echo "FAIL: received count too low (expected >=$expected_total_acked got=$server_received)" >&2
  exit 1
fi

if (( JETSTREAM_DIAG_FAIL_FIRST_N > 0 )) && (( server_noack < JETSTREAM_DIAG_FAIL_FIRST_N )); then
  echo "FAIL: expected at least $JETSTREAM_DIAG_FAIL_FIRST_N intentional no-ack events, got $server_noack" >&2
  exit 1
fi

if [[ "$JETSTREAM_DIAG_CHECK_RESTART" == "1" ]] && (( JETSTREAM_DIAG_REPLAY_LOOPS > 0 )) && (( server_replay_acked < JETSTREAM_DIAG_REPLAY_LOOPS )); then
  echo "FAIL: replay post-restart acked too low (expected >=$JETSTREAM_DIAG_REPLAY_LOOPS got=$server_replay_acked)" >&2
  exit 1
fi

echo "jetstream envelope E2E passed."
