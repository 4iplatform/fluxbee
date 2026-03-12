#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
TIMEOUT_MS="${IDENTITY_PROVISION_COMPLETE_TIMEOUT_MS:-12000}"
FALLBACK_TARGET="${IDENTITY_PROVISION_COMPLETE_FALLBACK_TARGET:-}"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Step 1/3: build identity_provision_complete_diag"
  cargo build --release --bin identity_provision_complete_diag >/dev/null
else
  echo "Step 1/3: using existing identity_provision_complete_diag"
fi

if [[ ! -x "$ROOT_DIR/target/release/identity_provision_complete_diag" ]]; then
  echo "FAIL: missing $ROOT_DIR/target/release/identity_provision_complete_diag (set BUILD_BIN=1)" >&2
  exit 1
fi

echo "Step 2/3: run identity provision+complete diag"
TMP_OUT="$(mktemp)"
trap 'rm -f "$TMP_OUT"' EXIT

JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
IDENTITY_PROVISION_COMPLETE_TIMEOUT_MS="$TIMEOUT_MS" \
IDENTITY_PROVISION_COMPLETE_FALLBACK_TARGET="$FALLBACK_TARGET" \
./target/release/identity_provision_complete_diag | tee "$TMP_OUT"

status="$(grep -E '^STATUS=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
if [[ "$status" != "ok" ]]; then
  echo "FAIL: identity provision+complete diag did not return STATUS=ok" >&2
  exit 1
fi

echo "Step 3/3: summary"
echo "identity provision+complete E2E passed."
