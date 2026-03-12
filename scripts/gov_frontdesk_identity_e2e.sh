#!/usr/bin/env bash
set -euo pipefail

BUILD_BIN="${BUILD_BIN:-0}"
IDENTITY_TARGET="${IDENTITY_TARGET:-SY.identity@sandbox}"
FRONTDESK_NODE_NAME="${FRONTDESK_NODE_NAME:-AI.frontdesk.gov@sandbox}"
MERGE_TARGET="${MERGE_TARGET:-SY.identity@worker-220}"
MERGE_FALLBACK_TARGET="${MERGE_FALLBACK_TARGET:-SY.identity@sandbox}"

echo "Running frontdesk.gov identity E2E wrappers"
echo "  provision target: $IDENTITY_TARGET"
echo "  frontdesk node:   $FRONTDESK_NODE_NAME"
echo "  merge target:     $MERGE_TARGET"
echo "  merge fallback:   $MERGE_FALLBACK_TARGET"

BUILD_BIN="$BUILD_BIN" \
IDENTITY_PROVISION_COMPLETE_TARGET="$IDENTITY_TARGET" \
IDENTITY_PROVISION_COMPLETE_FRONTDESK_NODE_NAME="$FRONTDESK_NODE_NAME" \
bash scripts/identity_provision_complete_e2e.sh

BUILD_BIN="$BUILD_BIN" \
IDENTITY_MERGE_TARGET="$MERGE_TARGET" \
IDENTITY_MERGE_FALLBACK_TARGET="$MERGE_FALLBACK_TARGET" \
IDENTITY_MERGE_FRONTDESK_NODE_NAME="$FRONTDESK_NODE_NAME" \
bash scripts/identity_merge_alias_e2e.sh

echo "frontdesk.gov wrapper E2E passed"
