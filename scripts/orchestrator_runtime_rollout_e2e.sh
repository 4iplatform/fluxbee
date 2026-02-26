#!/usr/bin/env bash
set -euo pipefail

# E2E runtime rollout:
# 1) canary rollout (target_hives subset)
# 2) verify canary spawn on new runtime version
# 3) global rollout
# 4) rollback (manifest version mayor apuntando current anterior)
# 5) verify spawn con versión rollback
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" BUILD_BIN=0 \
#     bash scripts/orchestrator_runtime_rollout_e2e.sh

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
CANARY_VERSION="${CANARY_VERSION:-0.0.2}"
ROLLBACK_VERSION="${ROLLBACK_VERSION:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
WAIT_TIMEOUT_SECS="${WAIT_TIMEOUT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
BUILD_BIN="${BUILD_BIN:-1}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  SUDO=""
else
  SUDO="sudo"
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

epoch_ms() {
  python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

http_call() {
  local url="$1"
  local out="$2"
  curl -sS -o "$out" -w "%{http_code}" "$url"
}

check_canary_deployment() {
  local file="$1"
  local since_ms="$2"
  local hive_id="$3"
  python3 - "$file" "$since_ms" "$hive_id" <<'PY'
import json
import sys

path, since_ms, hive = sys.argv[1], int(sys.argv[2]), sys.argv[3]
try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("0")
    raise SystemExit(0)

entries = (((data or {}).get("payload") or {}).get("entries")) or []
for item in entries:
    if item.get("category") != "runtime":
        continue
    if int(item.get("started_at") or 0) < since_ms:
        continue
    targets = item.get("target_hives") or []
    if targets != [hive]:
        continue
    workers = item.get("workers") or []
    for w in workers:
        if w.get("hive_id") == hive and w.get("status") == "ok":
            print("1")
            raise SystemExit(0)
print("0")
PY
}

check_global_deployment() {
  local file="$1"
  local since_ms="$2"
  local hive_id="$3"
  python3 - "$file" "$since_ms" "$hive_id" <<'PY'
import json
import sys

path, since_ms, hive = sys.argv[1], int(sys.argv[2]), sys.argv[3]
try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("0")
    raise SystemExit(0)

entries = (((data or {}).get("payload") or {}).get("entries")) or []
for item in entries:
    if item.get("category") != "runtime":
        continue
    if int(item.get("started_at") or 0) < since_ms:
        continue
    targets = item.get("target_hives") or []
    if hive not in targets:
        continue
    workers = item.get("workers") or []
    for w in workers:
        if w.get("hive_id") == hive and w.get("status") == "ok":
            print("1")
            raise SystemExit(0)
print("0")
PY
}

wait_for_deployment() {
  local checker="$1"
  local since_ms="$2"
  local label="$3"
  local tmpdir="$4"
  local started
  started="$(date +%s)"
  while true; do
    local now elapsed body status
    now="$(date +%s)"
    elapsed=$((now - started))
    if (( elapsed > WAIT_TIMEOUT_SECS )); then
      echo "FAIL: timeout waiting deployment evidence (${label})" >&2
      cat "$tmpdir/deployments.json" >&2 || true
      exit 1
    fi
    body="$tmpdir/deployments.json"
    status="$(http_call "$BASE/deployments?hive=$HIVE_ID&category=runtime&limit=50" "$body")"
    if [[ "$status" == "200" ]] && [[ "$($checker "$body" "$since_ms" "$HIVE_ID")" == "1" ]]; then
      echo "OK: deployment evidence ready (${label})"
      return
    fi
    echo "[WAIT] ${label} elapsed=${elapsed}s" >&2
    sleep "$POLL_INTERVAL_SECS"
  done
}

prepare_runtime_fixture() {
  local version="$1"
  local runtime_dir="/var/lib/fluxbee/runtimes/${ORCH_RUNTIME}/${version}"
  local start_script="${runtime_dir}/bin/start.sh"
  ${SUDO} mkdir -p "${runtime_dir}/bin"
  cat <<'SCRIPT' | ${SUDO} tee "$start_script" >/dev/null
#!/usr/bin/env bash
set -euo pipefail
exec /bin/sleep 3600
SCRIPT
  ${SUDO} chmod +x "$start_script"
}

require_cmd bash
require_cmd curl
require_cmd python3
require_cmd cargo

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

if [[ "$(http_call "$BASE/health" "$tmpdir/health.json")" != "200" ]]; then
  echo "FAIL: $BASE/health not ready" >&2
  exit 1
fi
if [[ "$(http_call "$BASE/hives/$HIVE_ID" "$tmpdir/hive.json")" != "200" ]]; then
  echo "FAIL: hive '$HIVE_ID' not found in admin API" >&2
  exit 1
fi

prepare_runtime_fixture "$ROLLBACK_VERSION"
prepare_runtime_fixture "$CANARY_VERSION"

if [[ "$BUILD_BIN" == "1" ]]; then
  (cd "$ROOT_DIR" && cargo build --release --bin orch_system_diag)
fi

v1="$(epoch_ms)"
v2="$((v1 + 1))"
v3="$((v2 + 1))"

echo "Step 1/5: canary rollout -> hive=${HIVE_ID} version=${CANARY_VERSION} manifest=${v1}"
step1_ms="$(epoch_ms)"
(
  cd "$ROOT_DIR"
  ORCH_TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_RUNTIME_CURRENT="$CANARY_VERSION" \
  ORCH_RUNTIME_AVAILABLE="$ROLLBACK_VERSION,$CANARY_VERSION" \
  ORCH_RUNTIME_MANIFEST_VERSION="$v1" \
  ORCH_RUNTIME_UPDATE_TARGET_HIVES="$HIVE_ID" \
  ORCH_SEND_RUNTIME_UPDATE=1 \
  ORCH_ONLY_RUNTIME_UPDATE=1 \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  ./target/release/orch_system_diag
)
wait_for_deployment check_canary_deployment "$step1_ms" "canary" "$tmpdir"

echo "Step 2/5: verify canary spawn on version=${CANARY_VERSION}"
(
  cd "$ROOT_DIR"
  TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_VERSION="$CANARY_VERSION" \
  ORCH_SEND_RUNTIME_UPDATE=0 \
  ORCH_SEND_KILL=1 \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  BUILD_BIN=0 \
  bash scripts/orchestrator_runtime_update_spawn_e2e.sh
)

echo "Step 3/5: global rollout version=${CANARY_VERSION} manifest=${v2}"
step3_ms="$(epoch_ms)"
(
  cd "$ROOT_DIR"
  ORCH_TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_RUNTIME_CURRENT="$CANARY_VERSION" \
  ORCH_RUNTIME_AVAILABLE="$ROLLBACK_VERSION,$CANARY_VERSION" \
  ORCH_RUNTIME_MANIFEST_VERSION="$v2" \
  ORCH_SEND_RUNTIME_UPDATE=1 \
  ORCH_ONLY_RUNTIME_UPDATE=1 \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  ./target/release/orch_system_diag
)
wait_for_deployment check_global_deployment "$step3_ms" "global" "$tmpdir"

echo "Step 4/5: rollback rollout -> version=${ROLLBACK_VERSION} manifest=${v3}"
step4_ms="$(epoch_ms)"
(
  cd "$ROOT_DIR"
  ORCH_TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_RUNTIME_CURRENT="$ROLLBACK_VERSION" \
  ORCH_RUNTIME_AVAILABLE="$ROLLBACK_VERSION,$CANARY_VERSION" \
  ORCH_RUNTIME_MANIFEST_VERSION="$v3" \
  ORCH_SEND_RUNTIME_UPDATE=1 \
  ORCH_ONLY_RUNTIME_UPDATE=1 \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  ./target/release/orch_system_diag
)
wait_for_deployment check_global_deployment "$step4_ms" "rollback" "$tmpdir"

echo "Step 5/5: verify spawn after rollback on version=${ROLLBACK_VERSION}"
(
  cd "$ROOT_DIR"
  TARGET_HIVE="$HIVE_ID" \
  ORCH_RUNTIME="$ORCH_RUNTIME" \
  ORCH_VERSION="$ROLLBACK_VERSION" \
  ORCH_SEND_RUNTIME_UPDATE=0 \
  ORCH_SEND_KILL=1 \
  ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
  BUILD_BIN=0 \
  bash scripts/orchestrator_runtime_update_spawn_e2e.sh
)

echo "orchestrator runtime rollout canary/global/rollback E2E passed."
