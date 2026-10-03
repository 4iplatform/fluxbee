#!/usr/bin/env bash
# Lab E2E (not CI): a person's identity from first contact to registered, live on a hive.
#
# Run it on the motherbee (the identity primary), from a repo checkout, as a user that can reach
# the router socket (/var/run/fluxbee/routers) and write /var/lib/fluxbee/state/nodes, e.g.:
#
#   sudo BUILD_BIN=0 bash scripts/identity_provision_complete_e2e.sh
#
# (BUILD_BIN=0 uses a target/release/identity_provision_complete_diag built elsewhere and copied
# here.)
#
# Steps:
#   1. build or reuse target/release/identity_provision_complete_diag;
#   2. create two active test tenants through SY.admin (POST $BASE/hives/$HIVE_ID/identity/tenants):
#      the frontdesk no longer creates tenants, so the diag cannot either;
#   3. run the diag under the frontdesk's name, with sy-frontdesk-gov stopped meanwhile (see
#      scripts/lib/identity_e2e.sh): in the first tenant an IO node provisions a temporary ILK, a
#      message from it is routed to the frontdesk, the frontdesk registers it, and the channel
#      then resolves to the same ILK as complete; registering it into the second tenant fails
#      with INVALID_TENANT_TRANSITION;
#   4. delete and purge both test tenants through SY.admin (CLEANUP=0 keeps them).
#
# Environment: BASE (SY.admin HTTP, default http://127.0.0.1:8080), HIVE_ID (default motherbee),
# IDENTITY_PROVISION_COMPLETE_TEST_ID, IDENTITY_PROVISION_COMPLETE_TIMEOUT_MS,
# IDENTITY_PROVISION_COMPLETE_TARGET, IDENTITY_PROVISION_COMPLETE_FALLBACK_TARGET,
# IDENTITY_PROVISION_COMPLETE_FRONTDESK_NODE_NAME, CLEANUP (default 1).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
CLEANUP="${CLEANUP:-1}"
TEST_ID="${IDENTITY_PROVISION_COMPLETE_TEST_ID:-idprov-$(date +%s)-${RANDOM}}"
TIMEOUT_MS="${IDENTITY_PROVISION_COMPLETE_TIMEOUT_MS:-12000}"
FALLBACK_TARGET="${IDENTITY_PROVISION_COMPLETE_FALLBACK_TARGET:-}"

WORK_DIR="$(mktemp -d)"
TMP_OUT="$WORK_DIR/diag.out"

# shellcheck source=scripts/lib/identity_e2e.sh
source "$ROOT_DIR/scripts/lib/identity_e2e.sh"

out_value() {
  grep -E "^$1=" "$TMP_OUT" | tail -n1 | cut -d= -f2- || true
}

cleanup() {
  local ec=$?
  start_frontdesk
  if [[ "$CLEANUP" == "1" ]]; then
    purge_created_tenants
  fi
  rm -rf "$WORK_DIR"
  return "$ec"
}
trap cleanup EXIT

command -v curl >/dev/null 2>&1 || fail "missing required command 'curl'"
command -v python3 >/dev/null 2>&1 || fail "missing required command 'python3'"
command -v timeout >/dev/null 2>&1 || fail "missing required command 'timeout'"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Step 1/4: build identity_provision_complete_diag"
  cargo build --release --bin identity_provision_complete_diag >/dev/null
else
  echo "Step 1/4: using existing identity_provision_complete_diag"
fi
if [[ ! -x "$ROOT_DIR/target/release/identity_provision_complete_diag" ]]; then
  fail "missing $ROOT_DIR/target/release/identity_provision_complete_diag (set BUILD_BIN=1)"
fi

echo "Step 2/4: create two active test tenants through SY.admin"
create_test_tenant "idprov-$TEST_ID" TENANT_ID
create_test_tenant "idprov-other-$TEST_ID" OTHER_TENANT_ID

echo "Step 3/4: run identity provision+complete diag (test id $TEST_ID)"
stop_frontdesk
set +e
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
IDENTITY_PROVISION_COMPLETE_TEST_ID="$TEST_ID" \
IDENTITY_PROVISION_COMPLETE_TENANT_ID="$TENANT_ID" \
IDENTITY_PROVISION_COMPLETE_OTHER_TENANT_ID="$OTHER_TENANT_ID" \
IDENTITY_PROVISION_COMPLETE_TIMEOUT_MS="$TIMEOUT_MS" \
IDENTITY_PROVISION_COMPLETE_FALLBACK_TARGET="$FALLBACK_TARGET" \
timeout "$DIAG_TIMEOUT_SECS" ./target/release/identity_provision_complete_diag 2>&1 | tee "$TMP_OUT"
rc=${PIPESTATUS[0]}
set -e
start_frontdesk
check_diag_rc identity_provision_complete_diag "$rc"

[[ "$(out_value STATUS)" == "ok" ]] || fail "identity provision+complete diag did not return STATUS=ok"

echo "Step 4/4: summary"
echo "identity provision+complete E2E passed."
