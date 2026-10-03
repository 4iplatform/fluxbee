#!/usr/bin/env bash
# Lab E2E (not CI): identity writes on the primary reach a replica, live on a hive.
#
# Run it on the motherbee (the identity primary), from a repo checkout, as a user that can reach
# the router socket (/var/run/fluxbee/routers) and write /var/lib/fluxbee/state/nodes, naming a
# hive that runs a replica, e.g.:
#
#   sudo BUILD_BIN=0 REPLICA_HIVE=worker1 bash scripts/identity_replica_sync_e2e.sh
#
# (BUILD_BIN=0 uses a target/release/identity_replica_sync_diag built elsewhere and copied here.)
#
# Steps:
#   1. build or reuse target/release/identity_replica_sync_diag;
#   2. create an active test tenant through SY.admin on the primary (the frontdesk no longer
#      creates tenants, so the diag cannot either) and wait until the replica has it
#      (GET $BASE/hives/$REPLICA_HIVE/identity/tenants/<id>);
#   3. run the diag (DIAG_TIMEOUT_SECS, default 180): the replica's counts reach the primary's, an
#      IO node provisions an ILK in the tenant on the primary, and the replica's counts reach the
#      primary's again;
#   4. check through SY.admin that the replica has that ILK, in the test tenant;
#   5. delete and purge the test tenant, with its ILK, through SY.admin (CLEANUP=0 keeps it).
#
# Environment: REPLICA_HIVE (required), BASE (SY.admin HTTP, default http://127.0.0.1:8080),
# HIVE_ID (the primary, default motherbee), IDENTITY_REPLICA_TEST_ID, IDENTITY_REPLICA_TIMEOUT_MS,
# IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS, IDENTITY_REPLICA_POLL_MS,
# IDENTITY_REPLICA_STARTUP_WAIT_SECS and IDENTITY_REPLICA_RETRY_SLEEP_SECS (waiting for SY.admin),
# DIAG_TIMEOUT_SECS,
# IDENTITY_REPLICA_PRIMARY_TARGET, IDENTITY_REPLICA_TARGET (default SY.identity@$REPLICA_HIVE),
# IDENTITY_REPLICA_PRIMARY_FALLBACK_TARGET, IDENTITY_REPLICA_REQUIRE_BASELINE_SYNC, CLEANUP
# (default 1).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
REPLICA_HIVE="${REPLICA_HIVE:-}"
CLEANUP="${CLEANUP:-1}"
TEST_ID="${IDENTITY_REPLICA_TEST_ID:-idrep-$(date +%s)-${RANDOM}}"
IDENTITY_REPLICA_TIMEOUT_MS="${IDENTITY_REPLICA_TIMEOUT_MS:-10000}"
IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS="${IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS:-30000}"
IDENTITY_REPLICA_POLL_MS="${IDENTITY_REPLICA_POLL_MS:-250}"
STARTUP_WAIT_SECS="${IDENTITY_REPLICA_STARTUP_WAIT_SECS:-60}"
RETRY_SLEEP_SECS="${IDENTITY_REPLICA_RETRY_SLEEP_SECS:-2}"

WORK_DIR="$(mktemp -d)"
TMP_OUT="$WORK_DIR/diag.out"

# shellcheck source=scripts/lib/identity_e2e.sh
source "$ROOT_DIR/scripts/lib/identity_e2e.sh"

out_value() {
  grep -E "^$1=" "$TMP_OUT" | tail -n1 | cut -d= -f2- || true
}

cleanup() {
  local ec=$?
  if [[ "$CLEANUP" == "1" ]]; then
    purge_created_tenants
  fi
  rm -rf "$WORK_DIR"
  return "$ec"
}
trap cleanup EXIT

[[ -n "$REPLICA_HIVE" ]] || fail "REPLICA_HIVE is required: a hive that runs an SY.identity replica"
[[ "$REPLICA_HIVE" != "$HIVE_ID" ]] || fail "REPLICA_HIVE must differ from the primary $HIVE_ID"
command -v curl >/dev/null 2>&1 || fail "missing required command 'curl'"
command -v python3 >/dev/null 2>&1 || fail "missing required command 'python3'"
command -v timeout >/dev/null 2>&1 || fail "missing required command 'timeout'"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Step 1/5: build identity_replica_sync_diag"
  cargo build --release --bin identity_replica_sync_diag >/dev/null
else
  echo "Step 1/5: using existing identity_replica_sync_diag"
fi
if [[ ! -x "$ROOT_DIR/target/release/identity_replica_sync_diag" ]]; then
  fail "missing $ROOT_DIR/target/release/identity_replica_sync_diag (set BUILD_BIN=1)"
fi

echo "Step 2/5: create an active test tenant on $HIVE_ID and wait for it on $REPLICA_HIVE"
create_test_tenant "identity-replica-sync-$TEST_ID" TENANT_ID
converge_deadline=$((SECONDS + (IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS + 999) / 1000))
while :; do
  http="$(HIVE_ID="$REPLICA_HIVE" admin_call GET "/identity/tenants/$TENANT_ID" "$WORK_DIR/replica-tenant.json")"
  if [[ "$http" == "200" && "$(json_field "$WORK_DIR/replica-tenant.json" status)" == "ok" ]]; then
    echo "  $REPLICA_HIVE has the tenant"
    break
  fi
  if ((SECONDS >= converge_deadline)); then
    fail "the tenant did not reach $REPLICA_HIVE within ${IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS}ms (last http=$http)"
  fi
  sleep 1
done

echo "Step 3/5: run identity replica sync diag (test id $TEST_ID)"
set +e
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
IDENTITY_REPLICA_TEST_ID="$TEST_ID" \
IDENTITY_REPLICA_TENANT_ID="$TENANT_ID" \
IDENTITY_REPLICA_PRIMARY_TARGET="${IDENTITY_REPLICA_PRIMARY_TARGET:-SY.identity@$HIVE_ID}" \
IDENTITY_REPLICA_TARGET="${IDENTITY_REPLICA_TARGET:-SY.identity@$REPLICA_HIVE}" \
IDENTITY_REPLICA_PRIMARY_FALLBACK_TARGET="${IDENTITY_REPLICA_PRIMARY_FALLBACK_TARGET:-}" \
IDENTITY_REPLICA_TIMEOUT_MS="$IDENTITY_REPLICA_TIMEOUT_MS" \
IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS="$IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS" \
IDENTITY_REPLICA_POLL_MS="$IDENTITY_REPLICA_POLL_MS" \
IDENTITY_REPLICA_REQUIRE_BASELINE_SYNC="${IDENTITY_REPLICA_REQUIRE_BASELINE_SYNC:-1}" \
timeout "$DIAG_TIMEOUT_SECS" ./target/release/identity_replica_sync_diag >"$TMP_OUT" 2>&1
rc=$?
set -e
cat "$TMP_OUT"
check_diag_rc identity_replica_sync_diag "$rc"

[[ "$(out_value STATUS)" == "ok" ]] || fail "identity replica sync diag did not return STATUS=ok"
[[ "$(out_value BASELINE_SYNC_OK)" == "1" ]] || fail "baseline sync check did not pass"
[[ "$(out_value DELTA_SYNC_OK)" == "1" ]] || fail "delta sync check did not pass"

echo "Step 4/5: check the provisioned ILK on $REPLICA_HIVE through SY.admin"
ilk="$(out_value PROVISIONED_ILK_ID)"
[[ "$ilk" =~ ^ilk:[0-9a-fA-F-]{36}$ ]] || fail "the diag printed no PROVISIONED_ILK_ID"
http="$(HIVE_ID="$REPLICA_HIVE" admin_call GET "/identity/ilks/$ilk" "$WORK_DIR/replica-ilk.json")"
[[ "$http" == "200" ]] || fail "SY.admin get_ilk $ilk on $REPLICA_HIVE answered http=$http"
[[ "$(json_field "$WORK_DIR/replica-ilk.json" payload.ilk.tenant_id)" == "$TENANT_ID" ]] \
  || fail "$REPLICA_HIVE has $ilk outside the test tenant $TENANT_ID"

echo "Step 5/5: summary"
echo "identity replica sync E2E passed."
