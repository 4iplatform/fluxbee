#!/usr/bin/env bash
set -euo pipefail

# Blob sync E2E:
# - phase A: same-root resolve (sin sync)
# - phase B: cross-root resolve_with_retry (modo sync)
# - validates contract invariance (`BlobRef` + `text/v1`) across both modes
#
# Optional:
#   BUILD_BIN=1
#   BLOB_DIAG_BIN_PATH=./target/release/blob_sync_diag
#   BLOB_DIAG_SOURCE_ROOT=/tmp/fluxbee-blob-sync/source
#   BLOB_DIAG_TARGET_ROOT=/tmp/fluxbee-blob-sync/target
#   BLOB_DIAG_FILENAME=blob-sync-diag.txt
#   BLOB_DIAG_CONTENT='fluxbee-blob-sync-diag'
#   BLOB_DIAG_MIME=text/plain
#   BLOB_DIAG_PAYLOAD_TEXT='blob sync diag'
#   BLOB_DIAG_SYNC_MODE=copy                # copy|external
#   BLOB_DIAG_SYNC_DELAY_MS=800
#   BLOB_DIAG_RETRY_MAX_WAIT_MS=15000
#   BLOB_DIAG_RETRY_INITIAL_MS=100
#   BLOB_DIAG_RETRY_BACKOFF=2.0
#   BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE=1
#   BLOB_DIAG_SYNCTHING_SERVICE=fluxbee-syncthing
#   SHOW_FULL_LOGS=0
#   JSR_LOG_LEVEL=info

BUILD_BIN="${BUILD_BIN:-1}"
BLOB_DIAG_SOURCE_ROOT="${BLOB_DIAG_SOURCE_ROOT:-/tmp/fluxbee-blob-sync/source}"
BLOB_DIAG_TARGET_ROOT="${BLOB_DIAG_TARGET_ROOT:-/tmp/fluxbee-blob-sync/target}"
BLOB_DIAG_FILENAME="${BLOB_DIAG_FILENAME:-blob-sync-diag.txt}"
BLOB_DIAG_CONTENT="${BLOB_DIAG_CONTENT:-fluxbee-blob-sync-diag}"
BLOB_DIAG_MIME="${BLOB_DIAG_MIME:-text/plain}"
BLOB_DIAG_PAYLOAD_TEXT="${BLOB_DIAG_PAYLOAD_TEXT:-blob sync diag}"
BLOB_DIAG_SYNC_MODE="${BLOB_DIAG_SYNC_MODE:-copy}"
BLOB_DIAG_SYNC_DELAY_MS="${BLOB_DIAG_SYNC_DELAY_MS:-800}"
BLOB_DIAG_RETRY_MAX_WAIT_MS="${BLOB_DIAG_RETRY_MAX_WAIT_MS:-15000}"
BLOB_DIAG_RETRY_INITIAL_MS="${BLOB_DIAG_RETRY_INITIAL_MS:-100}"
BLOB_DIAG_RETRY_BACKOFF="${BLOB_DIAG_RETRY_BACKOFF:-2.0}"
BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE="${BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE:-1}"
BLOB_DIAG_SYNCTHING_SERVICE="${BLOB_DIAG_SYNCTHING_SERVICE:-fluxbee-syncthing}"
SHOW_FULL_LOGS="${SHOW_FULL_LOGS:-0}"
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${BLOB_DIAG_BIN_PATH:-$ROOT_DIR/target/release/blob_sync_diag}"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd sed
require_cmd grep
require_cmd awk

if [[ "${BLOB_DIAG_SYNC_MODE}" != "copy" && "${BLOB_DIAG_SYNC_MODE}" != "external" ]]; then
  echo "Error: BLOB_DIAG_SYNC_MODE must be copy|external (got '${BLOB_DIAG_SYNC_MODE}')" >&2
  exit 1
fi

if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  echo "Building blob_sync_diag..."
  (cd "$ROOT_DIR" && cargo build --release --bin blob_sync_diag)
fi

if [[ "${BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE}" == "1" ]]; then
  require_cmd systemctl
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    SUDO=""
  else
    SUDO="sudo"
  fi
  if ! ${SUDO} systemctl is-active --quiet "${BLOB_DIAG_SYNCTHING_SERVICE}"; then
    echo "FAIL: service '${BLOB_DIAG_SYNCTHING_SERVICE}' is not active" >&2
    ${SUDO} systemctl status "${BLOB_DIAG_SYNCTHING_SERVICE}" --no-pager -l || true
    exit 1
  fi
fi

tmpdir="$(mktemp -d)"
base_log="$tmpdir/blob_sync_base.log"
sync_log="$tmpdir/blob_sync_sync.log"
summary_log="$tmpdir/blob_sync_summary.log"

copy_pid=""
cleanup() {
  if [[ -n "$copy_pid" ]] && kill -0 "$copy_pid" >/dev/null 2>&1; then
    kill "$copy_pid" >/dev/null 2>&1 || true
    wait "$copy_pid" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

extract_key() {
  local key="$1"
  local file="$2"
  local line
  line="$(grep -E "^${key}=" "$file" | tail -n 1 || true)"
  if [[ -z "$line" ]]; then
    echo ""
    return 0
  fi
  printf '%s' "${line#*=}"
}

run_producer() {
  local root="$1"
  local log_file="$2"
  (
    cd "$ROOT_DIR"
    BLOB_SYNC_DIAG_MODE=produce \
    BLOB_ROOT="$root" \
    BLOB_DIAG_FILENAME="$BLOB_DIAG_FILENAME" \
    BLOB_DIAG_CONTENT="$BLOB_DIAG_CONTENT" \
    BLOB_DIAG_MIME="$BLOB_DIAG_MIME" \
    BLOB_DIAG_PAYLOAD_TEXT="$BLOB_DIAG_PAYLOAD_TEXT" \
    JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
    "$BIN_PATH"
  ) >"$log_file" 2>&1
  local status
  status="$(extract_key STATUS "$log_file")"
  if [[ "$status" != "ok" ]]; then
    echo "FAIL: producer failed (root=$root)" >&2
    cat "$log_file" >&2 || true
    exit 1
  fi
}

run_consumer() {
  local root="$1"
  local blob_ref_json="$2"
  local log_file="$3"
  (
    cd "$ROOT_DIR"
    BLOB_SYNC_DIAG_MODE=consume \
    BLOB_ROOT="$root" \
    BLOB_DIAG_BLOB_REF_JSON="$blob_ref_json" \
    BLOB_DIAG_EXPECT_CONTENT="$BLOB_DIAG_CONTENT" \
    BLOB_DIAG_RETRY_MAX_WAIT_MS="$BLOB_DIAG_RETRY_MAX_WAIT_MS" \
    BLOB_DIAG_RETRY_INITIAL_MS="$BLOB_DIAG_RETRY_INITIAL_MS" \
    BLOB_DIAG_RETRY_BACKOFF="$BLOB_DIAG_RETRY_BACKOFF" \
    JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
    "$BIN_PATH"
  ) >"$log_file" 2>&1
  local status
  status="$(extract_key STATUS "$log_file")"
  if [[ "$status" != "ok" ]]; then
    echo "FAIL: consumer failed (root=$root)" >&2
    cat "$log_file" >&2 || true
    exit 1
  fi
}

echo "Preparing roots..."
rm -rf "$BLOB_DIAG_SOURCE_ROOT" "$BLOB_DIAG_TARGET_ROOT"
mkdir -p "$BLOB_DIAG_SOURCE_ROOT" "$BLOB_DIAG_TARGET_ROOT"

echo "Phase A (no sync): produce + resolve in same root"
run_producer "$BLOB_DIAG_SOURCE_ROOT" "$base_log"
base_blob_ref_json="$(extract_key BLOB_REF_JSON "$base_log")"
base_contract_sig="$(extract_key CONTRACT_SIGNATURE "$base_log")"
if [[ -z "$base_blob_ref_json" || -z "$base_contract_sig" ]]; then
  echo "FAIL: missing producer outputs in base phase" >&2
  cat "$base_log" >&2 || true
  exit 1
fi
run_consumer "$BLOB_DIAG_SOURCE_ROOT" "$base_blob_ref_json" "$summary_log"

echo "Phase B (sync mode=${BLOB_DIAG_SYNC_MODE}): cross-root resolve_with_retry"
run_producer "$BLOB_DIAG_SOURCE_ROOT" "$sync_log"
sync_blob_ref_json="$(extract_key BLOB_REF_JSON "$sync_log")"
sync_contract_sig="$(extract_key CONTRACT_SIGNATURE "$sync_log")"
sync_active_path="$(extract_key ACTIVE_PATH "$sync_log")"

if [[ -z "$sync_blob_ref_json" || -z "$sync_contract_sig" || -z "$sync_active_path" ]]; then
  echo "FAIL: missing producer outputs in sync phase" >&2
  cat "$sync_log" >&2 || true
  exit 1
fi

target_active_path="${sync_active_path/#$BLOB_DIAG_SOURCE_ROOT/$BLOB_DIAG_TARGET_ROOT}"
if [[ "$target_active_path" == "$sync_active_path" ]]; then
  echo "FAIL: could not map source active path to target root" >&2
  echo "source_root=$BLOB_DIAG_SOURCE_ROOT active_path=$sync_active_path target_root=$BLOB_DIAG_TARGET_ROOT" >&2
  exit 1
fi

if [[ "$BLOB_DIAG_SYNC_MODE" == "copy" ]]; then
  delay_secs="$(awk "BEGIN { printf \"%.3f\", ${BLOB_DIAG_SYNC_DELAY_MS} / 1000.0 }")"
  (
    sleep "$delay_secs"
    mkdir -p "$(dirname "$target_active_path")"
    cp "$sync_active_path" "$target_active_path"
  ) &
  copy_pid="$!"
fi

run_consumer "$BLOB_DIAG_TARGET_ROOT" "$sync_blob_ref_json" "$summary_log"
if [[ -n "$copy_pid" ]]; then
  wait "$copy_pid"
  copy_pid=""
fi

sync_elapsed_ms="$(extract_key ELAPSED_MS "$summary_log")"
contract_invariant="false"
if [[ "$base_contract_sig" == "$sync_contract_sig" ]]; then
  contract_invariant="true"
fi

echo "---- Blob sync summary ----"
echo "phase_a_contract_signature=${base_contract_sig}"
echo "phase_b_contract_signature=${sync_contract_sig}"
echo "contract_invariant=${contract_invariant}"
echo "sync_mode=${BLOB_DIAG_SYNC_MODE}"
echo "consumer_retry_elapsed_ms=${sync_elapsed_ms}"
echo "source_root=${BLOB_DIAG_SOURCE_ROOT}"
echo "target_root=${BLOB_DIAG_TARGET_ROOT}"
echo "syncthing_service_checked=${BLOB_DIAG_REQUIRE_SYNCTHING_ACTIVE}"

if [[ "$contract_invariant" != "true" ]]; then
  echo "FAIL: blob contract signature changed across modes" >&2
  exit 1
fi

if [[ "$SHOW_FULL_LOGS" == "1" ]]; then
  echo "---- base log ----"
  cat "$base_log" || true
  echo "---- sync log ----"
  cat "$sync_log" || true
  echo "---- consumer log ----"
  cat "$summary_log" || true
fi

echo "blob sync E2E passed."
