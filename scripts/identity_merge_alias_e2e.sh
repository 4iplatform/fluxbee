#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

BUILD_BIN="${BUILD_BIN:-1}"
IDENTITY_MERGE_TIMEOUT_MS="${IDENTITY_MERGE_TIMEOUT_MS:-10000}"
IDENTITY_MERGE_WAIT_GC_SECS="${IDENTITY_MERGE_WAIT_GC_SECS:-0}"
IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP="${IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP:-0}"
IDENTITY_MERGE_FALLBACK_TARGET="${IDENTITY_MERGE_FALLBACK_TARGET:-}"

if [[ "${BUILD_BIN}" == "1" ]]; then
  echo "Step 1/3: build identity_merge_diag"
  cargo build --release --bin identity_merge_diag >/dev/null
else
  echo "Step 1/3: using existing identity_merge_diag"
fi

if [[ ! -x "$ROOT_DIR/target/release/identity_merge_diag" ]]; then
  echo "FAIL: missing $ROOT_DIR/target/release/identity_merge_diag (set BUILD_BIN=1)" >&2
  exit 1
fi

echo "Step 2/3: run identity merge diag"
TMP_OUT="$(mktemp)"
trap 'rm -f "$TMP_OUT"' EXIT

JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}" \
IDENTITY_MERGE_TIMEOUT_MS="$IDENTITY_MERGE_TIMEOUT_MS" \
IDENTITY_MERGE_WAIT_GC_SECS="$IDENTITY_MERGE_WAIT_GC_SECS" \
IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP="$IDENTITY_MERGE_REQUIRE_ALIAS_CLEANUP" \
IDENTITY_MERGE_FALLBACK_TARGET="$IDENTITY_MERGE_FALLBACK_TARGET" \
./target/release/identity_merge_diag | tee "$TMP_OUT"

status="$(grep -E '^STATUS=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
if [[ "$status" != "ok" ]]; then
  echo "FAIL: identity merge diag did not return STATUS=ok" >&2
  exit 1
fi

canonical="$(grep -E '^CANONICAL_ILK_ID=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
resolved="$(grep -E '^RESOLVED_OLD_CHANNEL_ILK_ID=' "$TMP_OUT" | tail -n1 | cut -d= -f2- || true)"
if [[ -z "$canonical" || -z "$resolved" || "$canonical" != "$resolved" ]]; then
  echo "FAIL: old channel did not converge to canonical ILK" >&2
  echo "canonical='$canonical' resolved='$resolved'" >&2
  exit 1
fi

echo "Step 3/3: summary"
echo "identity merge alias E2E passed."
