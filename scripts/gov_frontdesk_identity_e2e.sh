#!/usr/bin/env bash
# Lab E2E (not CI): the frontdesk.gov identity flows on a hive, one after the other: from first
# contact to registered (scripts/identity_provision_complete_e2e.sh) and the merges
# (scripts/identity_merge_alias_e2e.sh). Run it on the motherbee like those, e.g.:
#
#   sudo BUILD_BIN=0 bash scripts/gov_frontdesk_identity_e2e.sh
#
# Each script reads its own environment (BASE, HIVE_ID, ...). To exercise the NOT_PRIMARY
# fallback of the merges, point MERGE_TARGET at a replica (SY.identity@worker1) and
# MERGE_FALLBACK_TARGET at the primary (SY.identity@motherbee).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-0}"
MERGE_TARGET="${MERGE_TARGET:-}"
MERGE_FALLBACK_TARGET="${MERGE_FALLBACK_TARGET:-}"

echo "Running frontdesk.gov identity E2E wrappers"
echo "  merge target:   ${MERGE_TARGET:-<the local SY.identity>}"
echo "  merge fallback: ${MERGE_FALLBACK_TARGET:-<none>}"

BUILD_BIN="$BUILD_BIN" bash scripts/identity_provision_complete_e2e.sh

BUILD_BIN="$BUILD_BIN" \
IDENTITY_MERGE_TARGET="$MERGE_TARGET" \
IDENTITY_MERGE_FALLBACK_TARGET="$MERGE_FALLBACK_TARGET" \
bash scripts/identity_merge_alias_e2e.sh

echo "frontdesk.gov wrapper E2E passed"
