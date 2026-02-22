#!/usr/bin/env bash
set -euo pipefail

# Full NATS-related suite:
# 1) Embedded broker lifecycle smoke (rt-gateway service)
# 2) Transport resilience E2E (SY.admin <-> NATS <-> SY.storage)
# 3) Admin functional E2E (nodes/routers/storage API checks)
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#     bash scripts/nats_full_suite.sh
#
# Optional pass-through:
#   SERVICE, NATS_URL, TIMEOUT_SECS, POLL_INTERVAL_SECS, CHECK_STOP_START
#   ROUTER_SERVICE, STORAGE_SERVICE, METRICS_TIMEOUT_SECS
#   RUN_ROUTER_RESTART, RUN_ROUTER_STOP_START, RUN_STORAGE_RESTART
#   RUN_ROUTER_CYCLE, RUN_NODE_CYCLE, RUN_STORAGE_METRICS_CHECK
#   STORAGE_METRICS_TIMEOUT_SECS, STORAGE_METRICS_POLL_SECS
#   FULL_SUITE_SMOKE_CHECK_STOP_START=0|1
#   FULL_SUITE_PROFILE=resilience|perf
#   FULL_SUITE_INCLUDE_LIFECYCLE=0|1
#   FULL_SUITE_INCLUDE_ADMIN=0|1
#   FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE=0|1

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
FULL_SUITE_SMOKE_CHECK_STOP_START="${FULL_SUITE_SMOKE_CHECK_STOP_START:-0}"
METRICS_TIMEOUT_SECS="${METRICS_TIMEOUT_SECS:-120}"
FULL_SUITE_PROFILE="${FULL_SUITE_PROFILE:-resilience}"
FULL_SUITE_INCLUDE_LIFECYCLE="${FULL_SUITE_INCLUDE_LIFECYCLE:-1}"
FULL_SUITE_INCLUDE_ADMIN="${FULL_SUITE_INCLUDE_ADMIN:-1}"
FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE="${FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE:-}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

run_timed_step() {
  local label="$1"
  shift
  local started ended elapsed
  started="$(date +%s)"
  "$@"
  ended="$(date +%s)"
  elapsed=$((ended - started))
  echo "OK: ${label} completed in ${elapsed}s"
}

require_cmd bash
require_cmd date

case "$FULL_SUITE_PROFILE" in
  resilience|perf) ;;
  *)
    echo "Error: FULL_SUITE_PROFILE must be 'resilience' or 'perf' (got '$FULL_SUITE_PROFILE')" >&2
    exit 1
    ;;
esac

if [[ "$FULL_SUITE_PROFILE" == "perf" ]]; then
  FULL_SUITE_SMOKE_CHECK_STOP_START=0
  if [[ -z "$FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE" ]]; then
    FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE=1
  fi
  RUN_ROUTER_RESTART="${RUN_ROUTER_RESTART:-0}"
  RUN_ROUTER_STOP_START="${RUN_ROUTER_STOP_START:-0}"
  RUN_STORAGE_RESTART="${RUN_STORAGE_RESTART:-0}"
  RUN_ROUTER_CYCLE="${RUN_ROUTER_CYCLE:-0}"
  RUN_NODE_CYCLE="${RUN_NODE_CYCLE:-0}"
  RUN_STORAGE_CONFIG_CYCLE="${RUN_STORAGE_CONFIG_CYCLE:-0}"
  RUN_STORAGE_METRICS_CHECK="${RUN_STORAGE_METRICS_CHECK:-1}"
  echo "Running NATS full suite (PERF profile) against BASE=$BASE HIVE_ID=$HIVE_ID"
else
  if [[ -z "$FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE" ]]; then
    FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE=0
  fi
  RUN_ROUTER_RESTART="${RUN_ROUTER_RESTART:-1}"
  RUN_ROUTER_STOP_START="${RUN_ROUTER_STOP_START:-1}"
  RUN_STORAGE_RESTART="${RUN_STORAGE_RESTART:-1}"
  RUN_ROUTER_CYCLE="${RUN_ROUTER_CYCLE:-1}"
  RUN_NODE_CYCLE="${RUN_NODE_CYCLE:-1}"
  RUN_STORAGE_CONFIG_CYCLE="${RUN_STORAGE_CONFIG_CYCLE:-1}"
  RUN_STORAGE_METRICS_CHECK="${RUN_STORAGE_METRICS_CHECK:-1}"
  echo "Running NATS full suite (RESILIENCE profile) against BASE=$BASE HIVE_ID=$HIVE_ID"
fi

suite_started="$(date +%s)"

if [[ "$FULL_SUITE_INCLUDE_LIFECYCLE" == "1" ]]; then
  echo "==> [1/4] Embedded NATS lifecycle smoke"
  run_timed_step "embedded lifecycle smoke" \
    env CHECK_STOP_START="$FULL_SUITE_SMOKE_CHECK_STOP_START" bash scripts/nats_embedded_lifecycle_smoke.sh
else
  echo "SKIP: [1/4] Embedded NATS lifecycle smoke disabled (FULL_SUITE_INCLUDE_LIFECYCLE=0)"
fi

echo "==> [2/4] NATS transport E2E"
run_timed_step "nats transport E2E" \
  env BASE="$BASE" \
      METRICS_TIMEOUT_SECS="$METRICS_TIMEOUT_SECS" \
      RUN_ROUTER_RESTART="$RUN_ROUTER_RESTART" \
      RUN_ROUTER_STOP_START="$RUN_ROUTER_STOP_START" \
      RUN_STORAGE_RESTART="$RUN_STORAGE_RESTART" \
      bash scripts/nats_client_transport_e2e.sh

if [[ "$FULL_SUITE_INCLUDE_ADMIN" == "1" ]]; then
  echo "==> [3/4] Admin nodes/routers/storage E2E"
  run_timed_step "admin nodes/routers/storage E2E" \
    env BASE="$BASE" \
        HIVE_ID="$HIVE_ID" \
        RUN_ROUTER_CYCLE="$RUN_ROUTER_CYCLE" \
        RUN_NODE_CYCLE="$RUN_NODE_CYCLE" \
        RUN_STORAGE_CONFIG_CYCLE="$RUN_STORAGE_CONFIG_CYCLE" \
        RUN_STORAGE_METRICS_CHECK="$RUN_STORAGE_METRICS_CHECK" \
        bash scripts/admin_nodes_routers_storage_e2e.sh
else
  echo "SKIP: [3/4] Admin nodes/routers/storage E2E disabled (FULL_SUITE_INCLUDE_ADMIN=0)"
fi

if [[ "$FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE" == "1" ]]; then
  echo "==> [4/4] JetStream envelope E2E"
  run_timed_step "jetstream envelope E2E" \
    env NATS_URL="${NATS_URL:-nats://127.0.0.1:4222}" \
        bash scripts/jetstream_envelope_e2e.sh
else
  echo "SKIP: [4/4] JetStream envelope E2E disabled (FULL_SUITE_INCLUDE_JETSTREAM_ENVELOPE=0)"
fi

suite_ended="$(date +%s)"
suite_elapsed=$((suite_ended - suite_started))
echo "NATS full suite passed in ${suite_elapsed}s."
