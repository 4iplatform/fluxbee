#!/usr/bin/env bash
# Lab E2E (not CI): merging a temporary ILK into a registered person, live on a hive.
#
# Run it on the motherbee (the identity primary), from a repo checkout, as a user that can reach
# the router socket (/var/run/fluxbee/routers) and write /var/lib/fluxbee/state/nodes, e.g.:
#
#   sudo BUILD_BIN=0 bash scripts/identity_merge_alias_e2e.sh
#
# (BUILD_BIN=0 uses a target/release/identity_merge_diag built elsewhere and copied here.)
#
# Steps:
#   1. build or reuse target/release/identity_merge_diag;
#   2. create an active test tenant through SY.admin (POST $BASE/hives/$HIVE_ID/identity/tenants):
#      the frontdesk no longer creates tenants, so the diag cannot either;
#   3. run the diag in that tenant under the frontdesk's name, with sy-frontdesk-gov stopped
#      meanwhile (see scripts/lib/identity_e2e.sh): it registers a person, merges one temporary
#      into them with ILK_ADD_CHANNEL merge_from_ilk_id (A) and another with ILK_REGISTER by the
#      person's email (B: merged=true, merged_from_ilk_id), and checks that both channels resolve
#      to the person;
#   4. check through SY.admin (get_ilk) that the merge kept the person's display_name, filled the
#      phone they lacked, and left both temporaries as aliases of the person;
#   5. delete and purge the test tenant through SY.admin (CLEANUP=0 keeps it).
#
# Environment: BASE (SY.admin HTTP, default http://127.0.0.1:8080), HIVE_ID (default motherbee),
# IDENTITY_MERGE_TENANT_ID (use this active tenant instead of creating one; it is never deleted),
# IDENTITY_MERGE_TEST_ID, IDENTITY_MERGE_TIMEOUT_MS, IDENTITY_MERGE_WAIT_GC_SECS,
# IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP, IDENTITY_MERGE_TARGET, IDENTITY_MERGE_FALLBACK_TARGET,
# CLEANUP (default 1).
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-motherbee}"
CLEANUP="${CLEANUP:-1}"
TEST_ID="${IDENTITY_MERGE_TEST_ID:-idmerge-$(date +%s)-${RANDOM}}"
TENANT_ID="${IDENTITY_MERGE_TENANT_ID:-}"
IDENTITY_MERGE_TIMEOUT_MS="${IDENTITY_MERGE_TIMEOUT_MS:-10000}"
IDENTITY_MERGE_WAIT_GC_SECS="${IDENTITY_MERGE_WAIT_GC_SECS:-0}"
IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP="${IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP:-0}"
IDENTITY_MERGE_FALLBACK_TARGET="${IDENTITY_MERGE_FALLBACK_TARGET:-}"

WORK_DIR="$(mktemp -d)"
TMP_OUT="$WORK_DIR/diag.out"

# shellcheck source=scripts/lib/identity_e2e.sh
source "$ROOT_DIR/scripts/lib/identity_e2e.sh"

# fill_only_problems <get_ilk reply> <kept display_name> <filled phone>: prints what the merge got
# wrong, without printing the values.
fill_only_problems() {
  python3 - "$1" "$2" "$3" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
ident = ((doc.get("payload") or {}).get("ilk") or {}).get("identification") or {}
problems = []
if ident.get("display_name") != sys.argv[2]:
    problems.append("display_name changed (a merge keeps a field that has a value)")
if ident.get("phone") != sys.argv[3]:
    problems.append("phone not filled (a merge fills the fields the person lacked)")
print("; ".join(problems))
PY
}

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
  echo "Step 1/5: build identity_merge_diag"
  cargo build --release --bin identity_merge_diag >/dev/null
else
  echo "Step 1/5: using existing identity_merge_diag"
fi
if [[ ! -x "$ROOT_DIR/target/release/identity_merge_diag" ]]; then
  fail "missing $ROOT_DIR/target/release/identity_merge_diag (set BUILD_BIN=1)"
fi

if [[ -n "$TENANT_ID" ]]; then
  echo "Step 2/5: using the given tenant $TENANT_ID (not deleted afterwards)"
else
  echo "Step 2/5: create an active test tenant through SY.admin"
  create_test_tenant "merge-diag-$TEST_ID" TENANT_ID
fi

echo "Step 3/5: run identity merge diag (test id $TEST_ID)"
stop_frontdesk
set +e
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
IDENTITY_MERGE_TEST_ID="$TEST_ID" \
IDENTITY_MERGE_TENANT_ID="$TENANT_ID" \
IDENTITY_MERGE_TIMEOUT_MS="$IDENTITY_MERGE_TIMEOUT_MS" \
IDENTITY_MERGE_WAIT_GC_SECS="$IDENTITY_MERGE_WAIT_GC_SECS" \
IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP="$IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP" \
IDENTITY_MERGE_FALLBACK_TARGET="$IDENTITY_MERGE_FALLBACK_TARGET" \
timeout "$DIAG_TIMEOUT_SECS" ./target/release/identity_merge_diag 2>&1 | tee "$TMP_OUT"
rc=${PIPESTATUS[0]}
set -e
start_frontdesk
check_diag_rc identity_merge_diag "$rc"

[[ "$(out_value STATUS)" == "ok" ]] || fail "identity merge diag did not return STATUS=ok"
canonical="$(out_value CANONICAL_ILK_ID)"
old_ilk="$(out_value OLD_ILK_ID)"
email_temp="$(out_value EMAIL_TEMP_ILK_ID)"
[[ -n "$canonical" ]] || fail "the diag printed no CANONICAL_ILK_ID"
[[ "$(out_value RESOLVED_OLD_CHANNEL_ILK_ID)" == "$canonical" ]] \
  || fail "(A) the channel merged by ILK_ADD_CHANNEL does not resolve to $canonical"
[[ "$(out_value EMAIL_MERGE_ILK_ID)" == "$canonical" ]] \
  || fail "(B) ILK_REGISTER by email did not end on $canonical"
[[ -n "$email_temp" && "$(out_value EMAIL_MERGED_FROM_ILK_ID)" == "$email_temp" ]] \
  || fail "(B) merged_from_ilk_id is not the temporary $email_temp"
[[ "$(out_value RESOLVED_EMAIL_CHANNEL_ILK_ID)" == "$canonical" ]] \
  || fail "(B) the channel merged by email does not resolve to $canonical"

echo "Step 4/5: check the merged person through SY.admin"
http="$(admin_call GET "/identity/ilks/$canonical" "$WORK_DIR/person.json")"
[[ "$http" == "200" ]] || fail "SY.admin get_ilk $canonical answered http=$http"
problems="$(fill_only_problems "$WORK_DIR/person.json" "$(out_value KEPT_DISPLAY_NAME)" \
  "$(out_value FILLED_PHONE)")"
[[ -z "$problems" ]] || fail "fill-only merge: $problems"
if [[ "$IDENTITY_MERGE_WAIT_GC_SECS" == "0" ]]; then
  for temp in "$old_ilk" "$email_temp"; do
    http="$(admin_call GET "/identity/ilks/$temp" "$WORK_DIR/alias.json")"
    if [[ "$http" != "200" \
      || "$(json_field "$WORK_DIR/alias.json" payload.alias_resolved)" != "true" \
      || "$(json_field "$WORK_DIR/alias.json" payload.canonical_ilk_id)" != "$canonical" ]]; then
      fail "the temporary $temp is not an alias of $canonical (http=$http)"
    fi
  done
else
  echo "  alias lookups skipped (IDENTITY_MERGE_WAIT_GC_SECS>0: the alias GC may have run)"
fi

echo "Step 5/5: summary"
echo "identity merge alias E2E passed."
