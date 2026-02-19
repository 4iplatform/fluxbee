#!/usr/bin/env bash
set -euo pipefail

# Storage ingestion suite:
# 1) storage.turns smoke (write/read/delete)
# 2) storage.events/items/reactivation E2E (write/read/update/delete)
#
# Usage:
#   sudo bash scripts/storage_ingestion_suite.sh
#
# Optional env:
#   HIVE_CONFIG=/etc/fluxbee/hive.yaml
#   NATS_URL=nats://127.0.0.1:4222
#   DB_URL=postgresql://...
#   FLUXBEE_DATABASE_URL=postgresql://...
#   SMOKE_TIMEOUT_SECS=30
#   SMOKE_KEEP_ROW=1
#   SMOKE_KEEP_ROWS=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

run_step() {
  local label="$1"
  shift
  echo "[SUITE] running: $label"
  "$@"
  echo "[SUITE] ok: $label"
}

export HIVE_CONFIG="${HIVE_CONFIG:-/etc/fluxbee/hive.yaml}"
if [[ -n "${NATS_URL:-}" ]]; then export NATS_URL; fi
if [[ -n "${DB_URL:-}" ]]; then export DB_URL; fi
if [[ -n "${FLUXBEE_DATABASE_URL:-}" ]]; then export FLUXBEE_DATABASE_URL; fi
if [[ -n "${SMOKE_TIMEOUT_SECS:-}" ]]; then export SMOKE_TIMEOUT_SECS; fi
if [[ -n "${SMOKE_KEEP_ROW:-}" ]]; then export SMOKE_KEEP_ROW; fi
if [[ -n "${SMOKE_KEEP_ROWS:-}" ]]; then export SMOKE_KEEP_ROWS; fi

echo "Running storage ingestion suite with HIVE_CONFIG=$HIVE_CONFIG"

run_step "storage.turns smoke" bash "$SCRIPT_DIR/storage_smoke.sh"
run_step "storage subjects E2E" bash "$SCRIPT_DIR/storage_subjects_e2e.sh"

echo "storage ingestion suite passed."
