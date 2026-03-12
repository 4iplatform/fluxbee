#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
IDENTITY_REPLICA_TIMEOUT_MS="${IDENTITY_REPLICA_TIMEOUT_MS:-10000}"
IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS="${IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS:-30000}"
IDENTITY_REPLICA_POLL_MS="${IDENTITY_REPLICA_POLL_MS:-250}"
IDENTITY_REPLICA_STARTUP_WAIT_SECS="${IDENTITY_REPLICA_STARTUP_WAIT_SECS:-60}"
IDENTITY_REPLICA_RETRY_SLEEP_SECS="${IDENTITY_REPLICA_RETRY_SLEEP_SECS:-2}"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Step 1/3: build identity_replica_sync_diag"
  cargo build --release --bin identity_replica_sync_diag >/dev/null
else
  echo "Step 1/3: using existing identity_replica_sync_diag"
fi

if [[ ! -x "$ROOT_DIR/target/release/identity_replica_sync_diag" ]]; then
  echo "FAIL: missing $ROOT_DIR/target/release/identity_replica_sync_diag (set BUILD_BIN=1)" >&2
  exit 1
fi

echo "Step 2/3: run identity replica sync diag"
TMP_OUT="$(mktemp)"
trap 'rm -f "$TMP_OUT"' EXIT

deadline=$((SECONDS + IDENTITY_REPLICA_STARTUP_WAIT_SECS))
attempt=0
while :; do
  attempt=$((attempt + 1))
  tmp_run="$(mktemp)"
  set +e
  JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
  IDENTITY_REPLICA_PRIMARY_TARGET="${IDENTITY_REPLICA_PRIMARY_TARGET:-}" \
  IDENTITY_REPLICA_TARGET="${IDENTITY_REPLICA_TARGET:-}" \
  IDENTITY_REPLICA_PRIMARY_FALLBACK_TARGET="${IDENTITY_REPLICA_PRIMARY_FALLBACK_TARGET:-}" \
  IDENTITY_REPLICA_TIMEOUT_MS="$IDENTITY_REPLICA_TIMEOUT_MS" \
  IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS="$IDENTITY_REPLICA_CONVERGENCE_TIMEOUT_MS" \
  IDENTITY_REPLICA_POLL_MS="$IDENTITY_REPLICA_POLL_MS" \
  IDENTITY_REPLICA_REQUIRE_BASELINE_SYNC="${IDENTITY_REPLICA_REQUIRE_BASELINE_SYNC:-1}" \
  ./target/release/identity_replica_sync_diag >"$tmp_run" 2>&1
  rc=$?
  set -e

  if [[ $rc -eq 0 ]]; then
    cat "$tmp_run" | tee "$TMP_OUT"
    rm -f "$tmp_run"
    break
  fi

  if grep -Eq "Connection refused|No such file or directory|timed out waiting ANNOUNCE|router socket" "$tmp_run"; then
    if (( SECONDS >= deadline )); then
      cat "$tmp_run" | tee "$TMP_OUT"
      rm -f "$tmp_run"
      echo "FAIL: identity_replica_sync_diag did not become ready within ${IDENTITY_REPLICA_STARTUP_WAIT_SECS}s" >&2
      exit $rc
    fi
    echo "WARN: control plane not ready yet (attempt=$attempt rc=$rc), retrying in ${IDENTITY_REPLICA_RETRY_SLEEP_SECS}s..."
    rm -f "$tmp_run"
    sleep "$IDENTITY_REPLICA_RETRY_SLEEP_SECS"
    continue
  fi

  cat "$tmp_run" | tee "$TMP_OUT"
  rm -f "$tmp_run"
  exit $rc
done

status="$(grep -E '^STATUS=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
if [[ "$status" != "ok" ]]; then
  echo "FAIL: identity replica sync diag did not return STATUS=ok" >&2
  exit 1
fi

baseline_sync="$(grep -E '^BASELINE_SYNC_OK=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
delta_sync="$(grep -E '^DELTA_SYNC_OK=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
if [[ "$baseline_sync" != "1" ]]; then
  echo "FAIL: baseline sync check did not pass (BASELINE_SYNC_OK='$baseline_sync')" >&2
  exit 1
fi
if [[ "$delta_sync" != "1" ]]; then
  echo "FAIL: delta sync check did not pass (DELTA_SYNC_OK='$delta_sync')" >&2
  exit 1
fi

echo "Step 3/3: summary"
echo "identity replica sync E2E passed."
