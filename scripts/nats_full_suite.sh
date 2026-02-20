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

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
FULL_SUITE_SMOKE_CHECK_STOP_START="${FULL_SUITE_SMOKE_CHECK_STOP_START:-0}"
METRICS_TIMEOUT_SECS="${METRICS_TIMEOUT_SECS:-120}"

echo "Running NATS full suite against BASE=$BASE HIVE_ID=$HIVE_ID"

echo "==> [1/3] Embedded NATS lifecycle smoke"
CHECK_STOP_START="$FULL_SUITE_SMOKE_CHECK_STOP_START" bash scripts/nats_embedded_lifecycle_smoke.sh

echo "==> [2/3] NATS transport E2E"
BASE="$BASE" METRICS_TIMEOUT_SECS="$METRICS_TIMEOUT_SECS" bash scripts/nats_client_transport_e2e.sh

echo "==> [3/3] Admin nodes/routers/storage E2E"
BASE="$BASE" HIVE_ID="$HIVE_ID" bash scripts/admin_nodes_routers_storage_e2e.sh

echo "NATS full suite passed."
