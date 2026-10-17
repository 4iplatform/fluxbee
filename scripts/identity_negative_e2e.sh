#!/usr/bin/env bash
# Lab E2E (not CI): the codes SY.identity refuses identity writes with, live on a hive.
#
# Run it on the motherbee (the identity primary), from a repo checkout, as a user that can reach
# the router socket (/var/run/fluxbee/routers) and write /var/lib/fluxbee/state/nodes, e.g.:
#
#   sudo BUILD_BIN=0 bash scripts/identity_negative_e2e.sh
#
# (BUILD_BIN=0 uses a target/release/identity_negative_diag built elsewhere and copied here.)
#
# Steps:
#   1. build or reuse target/release/identity_negative_diag;
#   2. create an active test tenant through SY.admin (POST $BASE/hives/$HIVE_ID/identity/tenants):
#      the frontdesk no longer creates tenants, so the diag cannot either;
#   3. run the diag under the frontdesk's name (DIAG_TIMEOUT_SECS, default 180), with
#      sy-frontdesk-gov stopped meanwhile (see scripts/lib/identity_e2e.sh), and check each code:
#        UNAUTHORIZED_REGISTRAR       ILK_REGISTER from a node that is not a registrar
#        INVALID_REQUEST              ILK_REGISTER with a malformed ilk_id
#        INVALID_TENANT               ILK_REGISTER into a tenant that does not exist
#        UNAUTHORIZED_REGISTRAR       TNT_CREATE from the frontdesk (it creates no tenants)
#        DUPLICATE_EMAIL              a new ILK with an email another ILK of the tenant has
#        DUPLICATE_ICH                a channel another ILK of the tenant holds
#        DUPLICATE_EMAIL              a complete ILK with another one's email (it does not merge)
#        TENANT_ROOT_NOT_REGISTRABLE  a person registered into the root tenant; the temporary a
#                                     root-tenant IO node provisioned stays temporary
#        NOT_PRIMARY                  only with IDENTITY_NEGATIVE_REPLICA_TARGET
#   4. delete and purge, through SY.admin, the test tenant and the root-tenant temporary the diag
#      provisioned (CLEANUP=0 keeps them).
#
# Environment: BASE (SY.admin HTTP, default http://127.0.0.1:8080), HIVE_ID (default motherbee),
# IDENTITY_NEGATIVE_TENANT_ID (use this active tenant instead of creating one; it is never
# deleted), IDENTITY_NEGATIVE_TEST_ID, IDENTITY_NEGATIVE_TIMEOUT_MS,
# IDENTITY_NEGATIVE_STARTUP_WAIT_SECS and IDENTITY_NEGATIVE_RETRY_SLEEP_SECS (waiting for SY.admin),
# DIAG_TIMEOUT_SECS, IDENTITY_NEGATIVE_TARGET,
# IDENTITY_NEGATIVE_FALLBACK_TARGET, IDENTITY_NEGATIVE_REPLICA_TARGET, CLEANUP (default 1).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
CLEANUP="${CLEANUP:-1}"
TEST_ID="${IDENTITY_NEGATIVE_TEST_ID:-idneg-$(date +%s)-${RANDOM}}"
TENANT_ID="${IDENTITY_NEGATIVE_TENANT_ID:-}"
TIMEOUT_MS="${IDENTITY_NEGATIVE_TIMEOUT_MS:-8000}"
STARTUP_WAIT_SECS="${IDENTITY_NEGATIVE_STARTUP_WAIT_SECS:-60}"
RETRY_SLEEP_SECS="${IDENTITY_NEGATIVE_RETRY_SLEEP_SECS:-2}"

WORK_DIR="$(mktemp -d)"
TMP_OUT="$WORK_DIR/diag.out"
: >"$TMP_OUT"

# shellcheck source=scripts/lib/identity_e2e.sh
source "$ROOT_DIR/scripts/lib/identity_e2e.sh"

out_value() {
  grep -E "^$1=" "$TMP_OUT" | tail -n1 | cut -d= -f2- || true
}

cleanup() {
  local ec=$? root_temp
  start_frontdesk
  if [[ "$CLEANUP" == "1" ]]; then
    root_temp="$(out_value ROOT_TEMP_ILK_ID)"
    if [[ "$root_temp" =~ ^ilk:[0-9a-fA-F-]{36}$ ]]; then
      delete_and_purge "/identity/ilks/$root_temp" "the root-tenant temporary $root_temp"
    fi
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
  echo "Step 1/4: build identity_negative_diag"
  cargo build --release --bin identity_negative_diag >/dev/null
else
  echo "Step 1/4: using existing identity_negative_diag"
fi
if [[ ! -x "$ROOT_DIR/target/release/identity_negative_diag" ]]; then
  fail "missing $ROOT_DIR/target/release/identity_negative_diag (set BUILD_BIN=1)"
fi

if [[ -n "$TENANT_ID" ]]; then
  echo "Step 2/4: using the given tenant $TENANT_ID (not deleted afterwards)"
else
  echo "Step 2/4: create an active test tenant through SY.admin"
  create_test_tenant "identity-negative-$TEST_ID" TENANT_ID
fi

echo "Step 3/4: run identity negative diag (test id $TEST_ID)"
stop_frontdesk
set +e
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
IDENTITY_NEGATIVE_TEST_ID="$TEST_ID" \
IDENTITY_NEGATIVE_TENANT_ID="$TENANT_ID" \
IDENTITY_NEGATIVE_TIMEOUT_MS="$TIMEOUT_MS" \
IDENTITY_NEGATIVE_TARGET="${IDENTITY_NEGATIVE_TARGET:-}" \
IDENTITY_NEGATIVE_FALLBACK_TARGET="${IDENTITY_NEGATIVE_FALLBACK_TARGET:-}" \
IDENTITY_NEGATIVE_REPLICA_TARGET="${IDENTITY_NEGATIVE_REPLICA_TARGET:-}" \
timeout "$DIAG_TIMEOUT_SECS" ./target/release/identity_negative_diag >"$TMP_OUT" 2>&1
rc=$?
set -e
cat "$TMP_OUT"
start_frontdesk
check_diag_rc identity_negative_diag "$rc"

expect_code() {
  local key="$1" expected="$2" got
  got="$(out_value "$key")"
  [[ "$got" == "$expected" ]] || fail "unexpected $key='$got' (expected $expected)"
}

[[ "$(out_value STATUS)" == "ok" ]] || fail "identity negative diag did not return STATUS=ok"
expect_code UNAUTHORIZED_CODE UNAUTHORIZED_REGISTRAR
expect_code INVALID_REQUEST_CODE INVALID_REQUEST
expect_code INVALID_TENANT_CODE INVALID_TENANT
expect_code FRONTDESK_TNT_CREATE_CODE UNAUTHORIZED_REGISTRAR
expect_code DUPLICATE_EMAIL_CODE DUPLICATE_EMAIL
expect_code DUPLICATE_ICH_CODE DUPLICATE_ICH
expect_code COMPLETE_DUPLICATE_EMAIL_CODE DUPLICATE_EMAIL
expect_code ROOT_TENANT_CODE TENANT_ROOT_NOT_REGISTRABLE
expect_code ROOT_TEMP_STATUS temporary
if [[ -n "${IDENTITY_NEGATIVE_REPLICA_TARGET:-}" ]]; then
  expect_code NOT_PRIMARY_CODE NOT_PRIMARY
fi

echo "Step 4/4: summary"
echo "identity negative E2E passed."
