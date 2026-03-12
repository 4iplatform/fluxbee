#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
TIMEOUT_MS="${IDENTITY_NEGATIVE_TIMEOUT_MS:-8000}"
STARTUP_WAIT_SECS="${IDENTITY_NEGATIVE_STARTUP_WAIT_SECS:-60}"
RETRY_SLEEP_SECS="${IDENTITY_NEGATIVE_RETRY_SLEEP_SECS:-2}"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Step 1/3: build identity_negative_diag"
  cargo build --release --bin identity_negative_diag >/dev/null
else
  echo "Step 1/3: using existing identity_negative_diag"
fi

if [[ ! -x "$ROOT_DIR/target/release/identity_negative_diag" ]]; then
  echo "FAIL: missing $ROOT_DIR/target/release/identity_negative_diag (set BUILD_BIN=1)" >&2
  exit 1
fi

echo "Step 2/3: run identity negative diag"
TMP_OUT="$(mktemp)"
trap 'rm -f "$TMP_OUT"' EXIT

deadline=$((SECONDS + STARTUP_WAIT_SECS))
attempt=0
while :; do
  attempt=$((attempt + 1))
  tmp_run="$(mktemp)"
  set +e
  JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
  IDENTITY_NEGATIVE_TIMEOUT_MS="$TIMEOUT_MS" \
  IDENTITY_NEGATIVE_TARGET="${IDENTITY_NEGATIVE_TARGET:-}" \
  IDENTITY_NEGATIVE_FALLBACK_TARGET="${IDENTITY_NEGATIVE_FALLBACK_TARGET:-}" \
  IDENTITY_NEGATIVE_REPLICA_TARGET="${IDENTITY_NEGATIVE_REPLICA_TARGET:-}" \
  ./target/release/identity_negative_diag >"$tmp_run" 2>&1
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
      echo "FAIL: identity_negative_diag did not become ready within ${STARTUP_WAIT_SECS}s" >&2
      exit $rc
    fi
    echo "WARN: control plane not ready yet (attempt=$attempt rc=$rc), retrying in ${RETRY_SLEEP_SECS}s..."
    rm -f "$tmp_run"
    sleep "$RETRY_SLEEP_SECS"
    continue
  fi

  cat "$tmp_run" | tee "$TMP_OUT"
  rm -f "$tmp_run"
  exit $rc
done

status="$(grep -E '^STATUS=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
if [[ "$status" != "ok" ]]; then
  echo "FAIL: identity negative diag did not return STATUS=ok" >&2
  exit 1
fi

unauth_code="$(grep -E '^UNAUTHORIZED_CODE=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
invalid_req_code="$(grep -E '^INVALID_REQUEST_CODE=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
invalid_tnt_code="$(grep -E '^INVALID_TENANT_CODE=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
dup_email_code="$(grep -E '^DUPLICATE_EMAIL_CODE=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
dup_ich_code="$(grep -E '^DUPLICATE_ICH_CODE=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"

if [[ "$unauth_code" != "UNAUTHORIZED_REGISTRAR" ]]; then
  echo "FAIL: unexpected UNAUTHORIZED_CODE='$unauth_code'" >&2
  exit 1
fi
if [[ "$invalid_req_code" != "INVALID_REQUEST" ]]; then
  echo "FAIL: unexpected INVALID_REQUEST_CODE='$invalid_req_code'" >&2
  exit 1
fi
if [[ "$invalid_tnt_code" != "INVALID_TENANT" ]]; then
  echo "FAIL: unexpected INVALID_TENANT_CODE='$invalid_tnt_code'" >&2
  exit 1
fi
if [[ "$dup_email_code" != "DUPLICATE_EMAIL" ]]; then
  echo "FAIL: unexpected DUPLICATE_EMAIL_CODE='$dup_email_code'" >&2
  exit 1
fi
if [[ "$dup_ich_code" != "DUPLICATE_ICH" ]]; then
  echo "FAIL: unexpected DUPLICATE_ICH_CODE='$dup_ich_code'" >&2
  exit 1
fi

echo "Step 3/3: summary"
echo "identity negative E2E passed."
