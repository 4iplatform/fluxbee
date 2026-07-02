#!/usr/bin/env bash
# Local end-to-end test of the LinkedHelper adapter self-update mechanics on the
# current host (Unix). No Proxmox required: exercises the real adapter binary
# against a mock Cloud (scripts/adapter-update-mock-cloud.py) through the full
# download -> verify -> atomic swap -> re-exec -> finalize/rollback path.
#
# Scenarios:
#   good  : required update with correct sha256  -> adapter upgrades v1 -> v2
#   bad   : required update with wrong sha256     -> adapter rejects, stays v1
#   avail : non-required offer                    -> adapter logs, stays v1
#
# The VM variant (test-adapter-update-loop-vm.sh) wraps this same logic on a
# pristine Proxmox VM so install/update residue is validated from a clean slate.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ADAPTER_CRATE="${ADAPTER_CRATE:-$HOME/repos/fluxbee_cloud/adapters/linked-helper/adapter-rs}"
PORT="${PORT:-8799}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/lh-update-XXXXXX")"
MOCK_PID=""
PASS=0; FAIL=0

log()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  PASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad()  { printf '\033[1;31m  FAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }

cleanup() {
  [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
  pkill -f "adapter-update-mock-cloud.py --port $PORT" 2>/dev/null || true
  [ -f "$WORK/Cargo.toml.bak" ] && cp "$WORK/Cargo.toml.bak" "$ADAPTER_CRATE/Cargo.toml" || true
  rm -rf "$WORK"
}
trap cleanup EXIT
pkill -f "adapter-update-mock-cloud.py --port $PORT" 2>/dev/null || true

# Reads a dotted JSON path from `adapter-rs status` output.
status_get() { # $1=state-file  $2=python-expression on `d`
  "$INSTALL/adapter-rs" --state-file "$1" status 2>/dev/null \
    | python3 -c "import sys,json; d=json.load(sys.stdin); print($2)"
}

start_mock() { # $1=directive
  [ -n "$MOCK_PID" ] && kill "$MOCK_PID" 2>/dev/null || true
  python3 "$HERE/adapter-update-mock-cloud.py" --port "$PORT" --bind 127.0.0.1 \
    --artifact "$WORK/adapter-rs.v2" --version 0.2.0 --directive "$1" \
    >"$WORK/mock-$1.log" 2>&1 &
  MOCK_PID=$!
  disown "$MOCK_PID" 2>/dev/null || true   # silence job-control 'Terminated' noise
  sleep 1
}

# Runs one scenario end-to-end and sets the global STATE_FILE. Must run in the
# parent shell (not a subshell) so MOCK_PID tracking kills the prior mock.
run_scenario() { # $1=name  $2=directive
  local name="$1" directive="$2"
  STATE_FILE="$WORK/$name/state.json"
  rm -rf "$WORK/$name"; mkdir -p "$WORK/$name"
  start_mock "$directive"
  cp "$WORK/adapter-rs.v1" "$INSTALL/adapter-rs"
  rm -f "$INSTALL/adapter-rs.prev"
  "$INSTALL/adapter-rs" --state-file "$STATE_FILE" enroll --cloud "http://127.0.0.1:$PORT" --token dummy >/dev/null
  # One cycle: a required update applies + re-execs into v2, which finalizes on
  # its own single cycle; a rejected/offered update just completes the cycle.
  "$INSTALL/adapter-rs" --state-file "$STATE_FILE" run --once >"$WORK/$name/run.log" 2>&1 || true
}

log "workdir: $WORK"
log "adapter crate: $ADAPTER_CRATE"

# --- build v1 (current version) and v2 (bumped) -----------------------------
log "building adapter v1 (current version)"
cargo build --quiet --manifest-path "$ADAPTER_CRATE/Cargo.toml"
cp "$ADAPTER_CRATE/target/debug/adapter-rs" "$WORK/adapter-rs.v1"

log "building adapter v2 (version 0.2.0)"
cp "$ADAPTER_CRATE/Cargo.toml" "$WORK/Cargo.toml.bak"
sed 's/^version = "0.1.0"/version = "0.2.0"/' "$ADAPTER_CRATE/Cargo.toml" > "$WORK/Cargo.toml.new"
mv "$WORK/Cargo.toml.new" "$ADAPTER_CRATE/Cargo.toml"
cargo build --quiet --manifest-path "$ADAPTER_CRATE/Cargo.toml"
cp "$ADAPTER_CRATE/target/debug/adapter-rs" "$WORK/adapter-rs.v2"
cp "$WORK/Cargo.toml.bak" "$ADAPTER_CRATE/Cargo.toml"   # restore immediately
log "v1 and v2 built"

INSTALL="$WORK/install"; mkdir -p "$INSTALL"

# --- scenario: good (required, correct sha) ---------------------------------
log "scenario GOOD: required update with correct sha256"
run_scenario good required
V="$(status_get "$STATE_FILE" 'd["state"]["adapterVersion"]')"
R="$(status_get "$STATE_FILE" 'd["state"]["runtime"].get("lastUpdate",{}).get("result")')"
[ "$V" = "0.2.0" ] && ok "adapter upgraded to $V" || bad "expected 0.2.0, got '$V'"
[ "$R" = "success" ] && ok "lastUpdate.result=success" || bad "expected success, got '$R'"
[ -f "$INSTALL/adapter-rs.prev" ] && bad "retained prev binary not cleaned up" || ok "prev binary cleaned up after finalize"

# --- scenario: bad (required, wrong sha) ------------------------------------
log "scenario BAD: required update with wrong sha256 (must be rejected)"
run_scenario bad required-badsha
V="$(status_get "$STATE_FILE" 'd["state"]["adapterVersion"]')"
R="$(status_get "$STATE_FILE" 'd["state"]["runtime"].get("lastUpdate",{}).get("result")')"
[ "$V" = "0.1.0" ] && ok "adapter stayed on $V" || bad "expected 0.1.0, got '$V'"
[ "$R" = "failed" ] && ok "lastUpdate.result=failed" || bad "expected failed, got '$R'"

# --- scenario: avail (non-required offer) -----------------------------------
log "scenario AVAIL: non-required offer (must not apply)"
run_scenario avail available
V="$(status_get "$STATE_FILE" 'd["state"]["adapterVersion"]')"
[ "$V" = "0.1.0" ] && ok "adapter stayed on $V (offer not applied)" || bad "expected 0.1.0, got '$V'"
grep -q '"action": "update_available"' "$WORK/avail/run.log" && ok "logged update_available" || bad "did not log update_available"

echo
log "RESULT: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
