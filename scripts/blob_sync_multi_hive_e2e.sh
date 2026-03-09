#!/usr/bin/env bash
set -euo pipefail

# Blob sync real multi-hive E2E (Syncthing, no copy mode):
# - produce blob on motherbee (`BLOB_ROOT_LOCAL`)
# - consume same blob on worker (`BLOB_ROOT_REMOTE`) via resolve_with_retry
#
# Requirements:
# - worker hive already added and online
# - syncthing active on mother + worker
# - SSH key access from mother -> worker (bootstrap key)
#
# Optional:
#   BUILD_BIN=1
#   BLOB_DIAG_BIN_PATH=./target/release/blob_sync_diag
#   HIVE_ADDR=192.168.8.220
#   HIVE_USER=administrator
#   HIVE_KEY=/var/lib/fluxbee/ssh/motherbee.key
#   BLOB_ROOT_LOCAL=/var/lib/fluxbee/blob
#   BLOB_ROOT_REMOTE=/var/lib/fluxbee/blob
#   BLOB_DIAG_FILENAME=blob-sync-multi-hive.txt
#   BLOB_DIAG_CONTENT='fluxbee-blob-sync-multi-hive'
#   BLOB_DIAG_MIME=text/plain
#   BLOB_DIAG_PAYLOAD_TEXT='blob sync multi hive diag'
#   BLOB_DIAG_RETRY_MAX_WAIT_MS=180000
#   BLOB_DIAG_RETRY_INITIAL_MS=200
#   BLOB_DIAG_RETRY_BACKOFF=1.8
#   BLOB_DIAG_SYNCTHING_SERVICE=fluxbee-syncthing
#   SHOW_FULL_LOGS=0
#   JSR_LOG_LEVEL=info

BUILD_BIN="${BUILD_BIN:-1}"
HIVE_ADDR="${HIVE_ADDR:-}"
HIVE_USER="${HIVE_USER:-administrator}"
HIVE_KEY="${HIVE_KEY:-/var/lib/fluxbee/ssh/motherbee.key}"
BLOB_ROOT_LOCAL="${BLOB_ROOT_LOCAL:-/var/lib/fluxbee/blob}"
BLOB_ROOT_REMOTE="${BLOB_ROOT_REMOTE:-/var/lib/fluxbee/blob}"
BLOB_DIAG_FILENAME="${BLOB_DIAG_FILENAME:-blob-sync-multi-hive.txt}"
BLOB_DIAG_CONTENT="${BLOB_DIAG_CONTENT:-fluxbee-blob-sync-multi-hive}"
BLOB_DIAG_MIME="${BLOB_DIAG_MIME:-text/plain}"
BLOB_DIAG_PAYLOAD_TEXT="${BLOB_DIAG_PAYLOAD_TEXT:-blob sync multi hive diag}"
BLOB_DIAG_RETRY_MAX_WAIT_MS="${BLOB_DIAG_RETRY_MAX_WAIT_MS:-180000}"
BLOB_DIAG_RETRY_INITIAL_MS="${BLOB_DIAG_RETRY_INITIAL_MS:-200}"
BLOB_DIAG_RETRY_BACKOFF="${BLOB_DIAG_RETRY_BACKOFF:-1.8}"
BLOB_DIAG_SYNCTHING_SERVICE="${BLOB_DIAG_SYNCTHING_SERVICE:-fluxbee-syncthing}"
SHOW_FULL_LOGS="${SHOW_FULL_LOGS:-0}"
JSR_LOG_LEVEL="${JSR_LOG_LEVEL:-info}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_PATH="${BLOB_DIAG_BIN_PATH:-$ROOT_DIR/target/release/blob_sync_diag}"
REMOTE_BIN_PATH="/tmp/blob_sync_diag"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

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

as_root_local() {
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    "$@"
  else
    sudo -n "$@"
  fi
}

ssh_remote() {
  local cmd="$1"
  if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
    ssh -i "$HIVE_KEY" \
      -o IdentitiesOnly=yes \
      -o PreferredAuthentications=publickey \
      -o PasswordAuthentication=no \
      -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null \
      "$HIVE_USER@$HIVE_ADDR" "$cmd"
  else
    sudo -n ssh -i "$HIVE_KEY" \
      -o IdentitiesOnly=yes \
      -o PreferredAuthentications=publickey \
      -o PasswordAuthentication=no \
      -o StrictHostKeyChecking=no \
      -o UserKnownHostsFile=/dev/null \
      "$HIVE_USER@$HIVE_ADDR" "$cmd"
  fi
}

require_cmd cargo
require_cmd awk
require_cmd sed
require_cmd grep
require_cmd base64
require_cmd ssh
require_cmd systemctl
require_cmd sudo

if [[ -z "$HIVE_ADDR" ]]; then
  echo "Error: HIVE_ADDR is required" >&2
  exit 1
fi

if [[ "$BUILD_BIN" == "1" || ! -x "$BIN_PATH" ]]; then
  echo "Building blob_sync_diag..."
  (cd "$ROOT_DIR" && cargo build --release --bin blob_sync_diag)
fi

tmpdir="$(mktemp -d)"
producer_log="$tmpdir/blob_sync_multi_hive_producer.log"
consumer_log="$tmpdir/blob_sync_multi_hive_consumer.log"

cleanup() {
  ssh_remote "sudo -n rm -f '$REMOTE_BIN_PATH'" >/dev/null 2>&1 || true
}
trap cleanup EXIT

echo "Step 1/6: verify local syncthing service active"
as_root_local systemctl is-active --quiet "$BLOB_DIAG_SYNCTHING_SERVICE"

echo "Step 2/6: verify remote syncthing service active"
ssh_remote "sudo -n systemctl is-active --quiet '$BLOB_DIAG_SYNCTHING_SERVICE'"

echo "Step 3/6: run producer on motherbee (real blob path)"
as_root_local env \
  BLOB_SYNC_DIAG_MODE=produce \
  BLOB_ROOT="$BLOB_ROOT_LOCAL" \
  BLOB_DIAG_FILENAME="$BLOB_DIAG_FILENAME" \
  BLOB_DIAG_CONTENT="$BLOB_DIAG_CONTENT" \
  BLOB_DIAG_MIME="$BLOB_DIAG_MIME" \
  BLOB_DIAG_PAYLOAD_TEXT="$BLOB_DIAG_PAYLOAD_TEXT" \
  JSR_LOG_LEVEL="$JSR_LOG_LEVEL" \
  "$BIN_PATH" >"$producer_log" 2>&1

producer_status="$(extract_key STATUS "$producer_log")"
if [[ "$producer_status" != "ok" ]]; then
  echo "FAIL: producer failed" >&2
  cat "$producer_log" >&2 || true
  exit 1
fi
blob_ref_json="$(extract_key BLOB_REF_JSON "$producer_log")"
active_path="$(extract_key ACTIVE_PATH "$producer_log")"
contract_signature="$(extract_key CONTRACT_SIGNATURE "$producer_log")"
if [[ -z "$blob_ref_json" || -z "$active_path" ]]; then
  echo "FAIL: producer missing BLOB_REF_JSON or ACTIVE_PATH" >&2
  cat "$producer_log" >&2 || true
  exit 1
fi

echo "Step 4/6: stage diagnostic binary on worker"
if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  cat "$BIN_PATH" | ssh -i "$HIVE_KEY" \
    -o IdentitiesOnly=yes \
    -o PreferredAuthentications=publickey \
    -o PasswordAuthentication=no \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    "$HIVE_USER@$HIVE_ADDR" \
    "sudo -n tee '$REMOTE_BIN_PATH' >/dev/null && sudo -n chmod 755 '$REMOTE_BIN_PATH'"
else
  sudo -n bash -lc "cat '$BIN_PATH' | ssh -i '$HIVE_KEY' \
    -o IdentitiesOnly=yes \
    -o PreferredAuthentications=publickey \
    -o PasswordAuthentication=no \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    '$HIVE_USER@$HIVE_ADDR' \
    \"sudo -n tee '$REMOTE_BIN_PATH' >/dev/null && sudo -n chmod 755 '$REMOTE_BIN_PATH'\""
fi

echo "Step 5/6: run consumer on worker via resolve_with_retry (no copy mode)"
blob_ref_b64="$(printf '%s' "$blob_ref_json" | base64 | tr -d '\n')"
remote_consume_cmd="$(cat <<EOF
set -euo pipefail
BLOB_REF_JSON="\$(printf '%s' '$blob_ref_b64' | base64 -d)"
sudo -n env \
  BLOB_SYNC_DIAG_MODE=consume \
  BLOB_ROOT='$BLOB_ROOT_REMOTE' \
  BLOB_DIAG_BLOB_REF_JSON="\$BLOB_REF_JSON" \
  BLOB_DIAG_EXPECT_CONTENT='$BLOB_DIAG_CONTENT' \
  BLOB_DIAG_RETRY_MAX_WAIT_MS='$BLOB_DIAG_RETRY_MAX_WAIT_MS' \
  BLOB_DIAG_RETRY_INITIAL_MS='$BLOB_DIAG_RETRY_INITIAL_MS' \
  BLOB_DIAG_RETRY_BACKOFF='$BLOB_DIAG_RETRY_BACKOFF' \
  JSR_LOG_LEVEL='$JSR_LOG_LEVEL' \
  '$REMOTE_BIN_PATH'
EOF
)"
ssh_remote "$remote_consume_cmd" >"$consumer_log" 2>&1

consumer_status="$(extract_key STATUS "$consumer_log")"
if [[ "$consumer_status" != "ok" ]]; then
  echo "FAIL: remote consumer failed" >&2
  cat "$consumer_log" >&2 || true
  exit 1
fi

elapsed_ms="$(extract_key ELAPSED_MS "$consumer_log")"
resolved_path="$(extract_key RESOLVED_PATH "$consumer_log")"

echo "Step 6/6: summary"
echo "---- Blob multi-hive sync summary ----"
echo "status=ok"
echo "mode=real_syncthing_multi_hive"
echo "hive_addr=$HIVE_ADDR"
echo "blob_root_local=$BLOB_ROOT_LOCAL"
echo "blob_root_remote=$BLOB_ROOT_REMOTE"
echo "active_path_local=$active_path"
echo "resolved_path_remote=$resolved_path"
echo "contract_signature=$contract_signature"
echo "consumer_retry_elapsed_ms=$elapsed_ms"

if [[ "$SHOW_FULL_LOGS" == "1" ]]; then
  echo "---- producer log ----"
  cat "$producer_log" || true
  echo "---- remote consumer log ----"
  cat "$consumer_log" || true
fi

echo "blob sync multi-hive E2E passed."
