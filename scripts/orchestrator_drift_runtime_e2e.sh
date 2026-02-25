#!/usr/bin/env bash
set -euo pipefail

# Orchestrator drift E2E (runtime manifest drift on worker):
# 1) Baseline runtime update (ensures manifests/sync path healthy)
# 2) Tamper worker runtime manifest to force hash drift
# 3) Trigger runtime_update again (auto-resync path)
# 4) Validate:
#    - worker runtime manifest hash == local hash after resync
#    - drift alert exists in API/log evidence
#    - deployment history contains runtime sync entry for target hive
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" \
#     bash scripts/orchestrator_drift_runtime_e2e.sh
#
# Optional:
#   HIVE_ADDR="192.168.8.220"                # auto-detected from /var/lib/fluxbee/hives/<id>/info.yaml if empty
#   ORCH_RUNTIME="wf.orch.diag"
#   ORCH_VERSION="0.0.1"
#   ORCH_TIMEOUT_SECS="45"
#   WAIT_TIMEOUT_SECS="60"
#   POLL_INTERVAL_SECS="2"
#   BUILD_BIN="0|1"

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
ORCH_VERSION="${ORCH_VERSION:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
WAIT_TIMEOUT_SECS="${WAIT_TIMEOUT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
BUILD_BIN="${BUILD_BIN:-0}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INFO_FILE="/var/lib/fluxbee/hives/${HIVE_ID}/info.yaml"
KEY_PATH="/var/lib/fluxbee/hives/${HIVE_ID}/ssh.key"
LOCAL_RUNTIME_MANIFEST="/var/lib/fluxbee/runtimes/manifest.json"

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

log_http_request() {
  local method="$1"
  local url="$2"
  local payload="${3:-}"
  echo "[HTTP] -> ${method} ${url}" >&2
  if [[ -n "$payload" ]]; then
    echo "[HTTP]    payload: ${payload}" >&2
  fi
}

log_http_response() {
  local status="$1"
  local body_file="$2"
  local compact
  compact="$(tr '\n' ' ' < "$body_file" | sed 's/[[:space:]]\+/ /g')"
  echo "[HTTP] <- status=${status} body=${compact}" >&2
}

http_call() {
  local method="$1"
  local url="$2"
  local body_file="$3"
  local payload="${4:-}"
  log_http_request "$method" "$url" "$payload"
  if [[ -n "$payload" ]]; then
    curl -sS -o "$body_file" -w "%{http_code}" \
      -X "$method" "$url" \
      -H "Content-Type: application/json" \
      -d "$payload"
  else
    curl -sS -o "$body_file" -w "%{http_code}" \
      -X "$method" "$url"
  fi
}

json_get() {
  local key="$1"
  local file="$2"
  python3 - "$key" "$file" <<'PY'
import json
import sys

key = sys.argv[1]
path = sys.argv[2]
try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("")
    raise SystemExit(0)

value = data
for part in key.split("."):
    if isinstance(value, dict):
        value = value.get(part, "")
    elif isinstance(value, list):
        if part.isdigit():
            idx = int(part)
            if 0 <= idx < len(value):
                value = value[idx]
            else:
                value = ""
                break
        else:
            value = ""
            break
    else:
        value = ""
        break

if isinstance(value, (dict, list)):
    print(json.dumps(value))
elif isinstance(value, bool):
    print("true" if value else "false")
else:
    print(value if value is not None else "")
PY
}

assert_eq() {
  local actual="$1"
  local expected="$2"
  local label="$3"
  if [[ "$actual" != "$expected" ]]; then
    echo "FAIL [$label]: expected '$expected', got '$actual'" >&2
    exit 1
  fi
}

epoch_ms() {
  python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

extract_hive_addr() {
  if [[ -n "$HIVE_ADDR" ]]; then
    echo "$HIVE_ADDR"
    return
  fi
  if [[ ! -f "$INFO_FILE" ]]; then
    echo ""
    return
  fi
  awk -F': ' '/^address:/ {gsub(/"/, "", $2); print $2; exit}' "$INFO_FILE"
}

remote_ssh() {
  local remote_cmd="$1"
  ssh -i "$KEY_PATH" \
    -o StrictHostKeyChecking=no \
    -o UserKnownHostsFile=/dev/null \
    -o ConnectTimeout=10 \
    "administrator@${HIVE_ADDR}" \
    "$remote_cmd"
}

remote_runtime_hash() {
  remote_ssh "sudo bash -lc \"if [ -f /var/lib/fluxbee/runtimes/manifest.json ]; then sha256sum /var/lib/fluxbee/runtimes/manifest.json | awk '{print \$1}'; fi\""
}

local_runtime_hash() {
  ${SUDO} bash -lc "if [ -f '${LOCAL_RUNTIME_MANIFEST}' ]; then sha256sum '${LOCAL_RUNTIME_MANIFEST}' | awk '{print \$1}'; fi"
}

trigger_runtime_update_cycle() {
  echo "Triggering runtime_update + spawn/kill cycle..." >&2
  (
    cd "$ROOT_DIR"
    TARGET_HIVE="$HIVE_ID" \
    ORCH_RUNTIME="$ORCH_RUNTIME" \
    ORCH_VERSION="$ORCH_VERSION" \
    ORCH_TIMEOUT_SECS="$ORCH_TIMEOUT_SECS" \
    ORCH_SEND_RUNTIME_UPDATE=1 \
    ORCH_SEND_KILL=1 \
    BUILD_BIN="$BUILD_BIN" \
    bash scripts/orchestrator_runtime_update_spawn_e2e.sh
  )
}

has_recent_runtime_drift_alert() {
  local file="$1"
  local since_ms="$2"
  python3 - "$file" "$since_ms" "$HIVE_ID" <<'PY'
import json
import sys

path = sys.argv[1]
since_ms = int(sys.argv[2])
hive_id = sys.argv[3]
try:
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
except Exception:
    print("0")
    raise SystemExit(0)

entries = (((data or {}).get("payload") or {}).get("entries")) or []
for item in entries:
    if item.get("hive_id") != hive_id:
        continue
    if item.get("category") != "runtime":
        continue
    detected = int(item.get("detected_at") or 0)
    if detected < since_ms:
        continue
    kind = item.get("kind", "")
    severity = item.get("severity", "")
    if kind in ("pre_sync_mismatch_resolved", "post_sync_mismatch", "sync_error") and severity in ("warning", "error"):
        print("1")
        raise SystemExit(0)

print("0")
PY
}

has_recent_runtime_deployment() {
  local file="$1"
  local since_ms="$2"
  python3 - "$file" "$since_ms" "$HIVE_ID" <<'PY'
import json
import sys

path = sys.argv[1]
since_ms = int(sys.argv[2])
hive_id = sys.argv[3]
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
    started = int(item.get("started_at") or 0)
    if started < since_ms:
        continue
    workers = item.get("workers") or []
    for worker in workers:
        if worker.get("hive_id") == hive_id and worker.get("status") in ("ok", "error"):
            print("1")
            raise SystemExit(0)
print("0")
PY
}

require_cmd curl
require_cmd python3
require_cmd ssh
require_cmd sha256sum
require_cmd awk
require_cmd sed

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

HIVE_ADDR="$(extract_hive_addr)"
if [[ -z "$HIVE_ADDR" ]]; then
  echo "FAIL: cannot resolve HIVE_ADDR (set HIVE_ADDR or ensure $INFO_FILE exists)" >&2
  exit 1
fi
if [[ ! -f "$KEY_PATH" ]]; then
  echo "FAIL: missing key file $KEY_PATH" >&2
  exit 1
fi

echo "Running runtime drift E2E: BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR"

health_body="$tmpdir/health.json"
status="$(http_call "GET" "$BASE/health" "$health_body")"
log_http_response "$status" "$health_body"
assert_eq "$status" "200" "GET /health/http"

hive_body="$tmpdir/hive.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$hive_body")"
log_http_response "$status" "$hive_body"
assert_eq "$status" "200" "GET /hives/{id}/http"
assert_eq "$(json_get "status" "$hive_body")" "ok" "GET /hives/{id}/status"

echo "Step 1/4: baseline runtime update cycle"
trigger_runtime_update_cycle

baseline_local_hash="$(local_runtime_hash)"
if [[ -z "$baseline_local_hash" ]]; then
  echo "FAIL: local runtime manifest hash is empty (missing $LOCAL_RUNTIME_MANIFEST?)" >&2
  exit 1
fi
baseline_remote_hash="$(remote_runtime_hash || true)"
if [[ -z "$baseline_remote_hash" ]]; then
  echo "FAIL: remote runtime manifest hash is empty before tamper" >&2
  exit 1
fi
if [[ "$baseline_remote_hash" != "$baseline_local_hash" ]]; then
  echo "FAIL: baseline is not converged (local != remote) before drift injection" >&2
  echo "local=$baseline_local_hash remote=$baseline_remote_hash" >&2
  exit 1
fi
echo "Baseline hashes: local=$baseline_local_hash remote=$baseline_remote_hash"

echo "Step 2/4: tamper remote runtime manifest to inject drift"
drift_start_ms="$(epoch_ms)"
remote_ssh "sudo bash -lc \"printf '\n# drift-e2e-$(date +%s)' >> /var/lib/fluxbee/runtimes/manifest.json\""
tampered_remote_hash="$(remote_runtime_hash || true)"
if [[ -z "$tampered_remote_hash" || "$tampered_remote_hash" == "$baseline_local_hash" ]]; then
  echo "FAIL: tamper did not change remote runtime manifest hash" >&2
  exit 1
fi
echo "Tampered remote hash: $tampered_remote_hash"

echo "Step 3/4: trigger runtime_update again to force auto-resync"
trigger_runtime_update_cycle

echo "Step 4/4: wait for convergence + API evidence"
start_secs="$(date +%s)"
while true; do
  now_secs="$(date +%s)"
  elapsed=$((now_secs - start_secs))
  if (( elapsed > WAIT_TIMEOUT_SECS )); then
    echo "FAIL: timeout waiting drift evidence (${WAIT_TIMEOUT_SECS}s)" >&2
    break
  fi

  current_remote_hash="$(remote_runtime_hash || true)"

  alerts_body="$tmpdir/drift_alerts.json"
  status_alerts="$(http_call "GET" "$BASE/drift-alerts?hive=$HIVE_ID&category=runtime&limit=50" "$alerts_body")"
  log_http_response "$status_alerts" "$alerts_body"
  alerts_ok=0
  if [[ "$status_alerts" == "200" && "$(has_recent_runtime_drift_alert "$alerts_body" "$drift_start_ms")" == "1" ]]; then
    alerts_ok=1
  fi

  deployments_body="$tmpdir/deployments.json"
  status_deploy="$(http_call "GET" "$BASE/deployments?hive=$HIVE_ID&category=runtime&limit=50" "$deployments_body")"
  log_http_response "$status_deploy" "$deployments_body"
  deploy_ok=0
  if [[ "$status_deploy" == "200" && "$(has_recent_runtime_deployment "$deployments_body" "$drift_start_ms")" == "1" ]]; then
    deploy_ok=1
  fi

  if [[ "$current_remote_hash" == "$baseline_local_hash" && "$alerts_ok" == "1" && "$deploy_ok" == "1" ]]; then
    echo "OK: drift reconciled and evidenced via API"
    echo "runtime drift E2E passed."
    exit 0
  fi

  echo "[WAIT] elapsed=${elapsed}s remote_hash=${current_remote_hash:-<none>} alerts_ok=${alerts_ok} deploy_ok=${deploy_ok}" >&2
  sleep "$POLL_INTERVAL_SECS"
done

echo "---- drift alerts body ----" >&2
cat "$tmpdir/drift_alerts.json" >&2 || true
echo "---- deployments body ----" >&2
cat "$tmpdir/deployments.json" >&2 || true
echo "---- remote hash ----" >&2
echo "${current_remote_hash:-<none>}" >&2
exit 1
