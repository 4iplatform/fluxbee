#!/usr/bin/env bash
set -euo pipefail

# # v2 behavior: strict SYSTEM_UPDATE runtime contract:
# - wrong hash -> payload.status=sync_pending
# - exact hash -> payload.status=ok
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#   bash scripts/orchestrator_system_update_stale_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"

echo "system stale-update e2e (v2 strict SYSTEM_UPDATE runtime): BASE=$BASE HIVE_ID=$HIVE_ID"
BASE="$BASE" HIVE_ID="$HIVE_ID" CATEGORY="runtime" \
  bash scripts/orchestrator_system_update_api_e2e.sh

echo "orchestrator system stale-update E2E passed."
