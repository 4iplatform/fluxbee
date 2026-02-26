#!/usr/bin/env bash
set -euo pipefail

# Orchestrator vendor rollback E2E:
# 1) Baseline vendor convergence (local == remote, service active)
# 2) Corrupt local vendor source + update vendor manifest hash/size (forces bad deploy artifact)
# 3) Trigger runtime_update cycle to force vendor sync; expect worker deployment error with rollback applied
# 4) Validate rollback evidence:
#    - deployment history contains vendor worker error with reason including "rollback applied"
#    - remote syncthing service is active
#    - remote syncthing hash restored to baseline hash
# 5) Restore local vendor source/manifest and re-run runtime_update cycle for clean state
#
# Usage:
#   BASE="http://127.0.0.1:8080" HIVE_ID="worker-220" BUILD_BIN=0 \
#     bash scripts/orchestrator_vendor_rollback_e2e.sh
#
# Optional:
#   HIVE_ADDR="192.168.8.220"
#   ORCH_RUNTIME="wf.orch.diag"
#   ORCH_VERSION="0.0.1"
#   ORCH_TIMEOUT_SECS="45"
#   WAIT_TIMEOUT_SECS="60"
#   POLL_INTERVAL_SECS="2"
#   REMOTE_SUDO_PASS="magicAI"
#   VENDOR_COMPONENT_NAME="syncthing"

BASE="${BASE:-http://127.0.0.1:8080}"
HIVE_ID="${HIVE_ID:-worker-220}"
HIVE_ADDR="${HIVE_ADDR:-}"
ORCH_RUNTIME="${ORCH_RUNTIME:-wf.orch.diag}"
ORCH_VERSION="${ORCH_VERSION:-0.0.1}"
ORCH_TIMEOUT_SECS="${ORCH_TIMEOUT_SECS:-45}"
WAIT_TIMEOUT_SECS="${WAIT_TIMEOUT_SECS:-60}"
POLL_INTERVAL_SECS="${POLL_INTERVAL_SECS:-2}"
BUILD_BIN="${BUILD_BIN:-0}"
REMOTE_SUDO_PASS="${REMOTE_SUDO_PASS:-magicAI}"
VENDOR_COMPONENT_NAME="${VENDOR_COMPONENT_NAME:-syncthing}"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INFO_FILE="/var/lib/fluxbee/hives/${HIVE_ID}/info.yaml"
KEY_PATH="/var/lib/fluxbee/hives/${HIVE_ID}/ssh.key"
VENDOR_MANIFEST="/var/lib/fluxbee/vendor/manifest.json"
LEGACY_VENDOR_BIN="/var/lib/fluxbee/vendor/syncthing/syncthing"
REMOTE_VENDOR_BIN="${REMOTE_VENDOR_BIN:-/var/lib/fluxbee/vendor/bin/syncthing}"
REMOTE_SYNCTHING_SERVICE="fluxbee-syncthing"

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
    -o LogLevel=ERROR \
    -o ConnectTimeout=10 \
    "administrator@${HIVE_ADDR}" \
    "$remote_cmd"
}

remote_root() {
  local root_cmd="$1"
  local escaped
  escaped="$(printf "%s" "$root_cmd" | sed "s/'/'\"'\"'/g")"
  if remote_ssh "sudo -n true" >/dev/null 2>&1; then
    remote_ssh "sudo bash -lc '$escaped'"
    return
  fi
  if [[ -z "$REMOTE_SUDO_PASS" ]]; then
    echo "FAIL: remote sudo requires password; set REMOTE_SUDO_PASS" >&2
    return 1
  fi
  remote_ssh "printf '%s\n' '$REMOTE_SUDO_PASS' | sudo -S -p '' bash -lc '$escaped'"
}

local_vendor_path() {
  ${SUDO} python3 - "$VENDOR_MANIFEST" "$VENDOR_COMPONENT_NAME" "$LEGACY_VENDOR_BIN" <<'PY'
import json
import os
import sys

manifest = sys.argv[1]
component = sys.argv[2]
legacy = sys.argv[3]

if os.path.exists(manifest):
    try:
        with open(manifest, "r", encoding="utf-8") as f:
            data = json.load(f)
        path = (((data or {}).get("components") or {}).get(component) or {}).get("path")
        if isinstance(path, str) and path.strip():
            root = os.path.dirname(manifest)
            full = os.path.join(root, path)
            print(full)
            raise SystemExit(0)
    except Exception:
        pass

print(legacy)
PY
}

local_vendor_hash() {
  local vendor_path
  vendor_path="$(local_vendor_path)"
  ${SUDO} bash -lc "if [ -f '${vendor_path}' ]; then sha256sum '${vendor_path}' | awk '{print \$1}'; fi"
}

remote_vendor_hash() {
  remote_root "if [ -f '${REMOTE_VENDOR_BIN}' ]; then sha256sum '${REMOTE_VENDOR_BIN}' | awk '{print \$1}'; fi"
}

remote_syncthing_service_active() {
  remote_root "systemctl is-active --quiet '${REMOTE_SYNCTHING_SERVICE}'"
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

has_recent_vendor_rollback_deployment() {
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
    if item.get("category") != "vendor":
        continue
    started = int(item.get("started_at") or 0)
    if started < since_ms:
        continue
    workers = item.get("workers") or []
    for worker in workers:
        if worker.get("hive_id") != hive_id:
            continue
        if worker.get("status") != "error":
            continue
        reason = str(worker.get("reason") or "")
        if "rollback applied" in reason:
            print("1")
            raise SystemExit(0)
print("0")
PY
}

diagnose_remote_vendor_missing() {
  local versions_body="$tmpdir/versions_worker.json"
  local status_versions
  status_versions="$(http_call "GET" "$BASE/versions?hive=$HIVE_ID" "$versions_body")"
  log_http_response "$status_versions" "$versions_body"

  local vendor_status vendor_path
  vendor_status="$(json_get "payload.hive.vendor.status" "$versions_body")"
  vendor_path="$(json_get "payload.hive.vendor.syncthing_installed_path" "$versions_body")"

  echo "Diagnostic: worker vendor status='${vendor_status:-<empty>}' path='${vendor_path:-<empty>}'" >&2
  if [[ -f "config/hive.yaml" ]]; then
    local blob_sync_line
    blob_sync_line="$(awk '/^[[:space:]]*sync:[[:space:]]*$/,/^[[:space:]]*[a-zA-Z0-9_-]+:[[:space:]]*$/' config/hive.yaml | awk '/^[[:space:]]*enabled:[[:space:]]*/{print; exit}')"
    if [[ -n "$blob_sync_line" ]]; then
      echo "Diagnostic: config/hive.yaml blob.sync ${blob_sync_line}" >&2
    fi
  fi
}

tmpdir="$(mktemp -d)"
vendor_path=""
vendor_backup="$tmpdir/vendor.bin.bak"
manifest_backup="$tmpdir/vendor.manifest.bak"
manifest_present=0
restore_pending=0

restore_local_vendor() {
  if [[ "$restore_pending" != "1" || -z "$vendor_path" ]]; then
    return 0
  fi
  if [[ -f "$vendor_backup" ]]; then
    ${SUDO} install -m 0755 "$vendor_backup" "$vendor_path"
  fi
  if [[ "$manifest_present" == "1" && -f "$manifest_backup" ]]; then
    ${SUDO} install -m 0644 "$manifest_backup" "$VENDOR_MANIFEST"
  fi
  restore_pending=0
}

cleanup() {
  local code=$?
  if [[ "$restore_pending" == "1" ]]; then
    echo "Cleanup: restoring local vendor source/manifest..." >&2
    if ! restore_local_vendor; then
      echo "WARN: failed to restore local vendor source/manifest during cleanup" >&2
    fi
  fi
  rm -rf "$tmpdir"
  return "$code"
}
trap cleanup EXIT

require_cmd curl
require_cmd python3
require_cmd ssh
require_cmd sha256sum
require_cmd awk
require_cmd sed
require_cmd stat

HIVE_ADDR="$(extract_hive_addr)"
if [[ -z "$HIVE_ADDR" ]]; then
  echo "FAIL: cannot resolve HIVE_ADDR (set HIVE_ADDR or ensure $INFO_FILE exists)" >&2
  exit 1
fi
if [[ ! -f "$KEY_PATH" ]]; then
  echo "FAIL: missing key file $KEY_PATH" >&2
  exit 1
fi

echo "Running vendor rollback E2E: BASE=$BASE HIVE_ID=$HIVE_ID HIVE_ADDR=$HIVE_ADDR"

health_body="$tmpdir/health.json"
status="$(http_call "GET" "$BASE/health" "$health_body")"
log_http_response "$status" "$health_body"
assert_eq "$status" "200" "GET /health/http"

hive_body="$tmpdir/hive.json"
status="$(http_call "GET" "$BASE/hives/$HIVE_ID" "$hive_body")"
log_http_response "$status" "$hive_body"
assert_eq "$status" "200" "GET /hives/{id}/http"
assert_eq "$(json_get "status" "$hive_body")" "ok" "GET /hives/{id}/status"

echo "Step 1/6: baseline runtime update cycle (drives vendor check)"
trigger_runtime_update_cycle

baseline_local_hash="$(local_vendor_hash)"
if [[ -z "$baseline_local_hash" ]]; then
  echo "FAIL: local vendor hash is empty (missing vendor binary?)" >&2
  exit 1
fi
baseline_remote_hash="$(remote_vendor_hash || true)"
if [[ -z "$baseline_remote_hash" ]]; then
  diagnose_remote_vendor_missing
  echo "FAIL: remote vendor hash is empty before rollback scenario" >&2
  echo "Hint: this E2E needs blob sync enabled and Syncthing installed on worker (try blob.sync.enabled=true and restart sy-orchestrator)." >&2
  exit 1
fi
if [[ "$baseline_remote_hash" != "$baseline_local_hash" ]]; then
  echo "FAIL: baseline is not converged (local != remote) before rollback scenario" >&2
  echo "local=$baseline_local_hash remote=$baseline_remote_hash" >&2
  exit 1
fi
if ! remote_syncthing_service_active; then
  echo "FAIL: ${REMOTE_SYNCTHING_SERVICE} is not active before rollback scenario" >&2
  exit 1
fi
echo "Baseline hashes: local=$baseline_local_hash remote=$baseline_remote_hash"

echo "Step 2/6: corrupt local vendor source + manifest (forced bad artifact)"
vendor_path="$(local_vendor_path)"
if [[ -z "$vendor_path" ]]; then
  echo "FAIL: cannot resolve local vendor path" >&2
  exit 1
fi
if [[ ! -f "$vendor_path" ]]; then
  echo "FAIL: local vendor path does not exist: $vendor_path" >&2
  exit 1
fi

${SUDO} cp "$vendor_path" "$vendor_backup"
if [[ -f "$VENDOR_MANIFEST" ]]; then
  manifest_present=1
  ${SUDO} cp "$VENDOR_MANIFEST" "$manifest_backup"
fi
restore_pending=1

${SUDO} bash -lc "printf '%s\n' 'fluxbee-e2e-invalid-syncthing-binary' > '${vendor_path}' && chmod 0755 '${vendor_path}'"
bad_hash="$(local_vendor_hash)"
if [[ -z "$bad_hash" ]]; then
  echo "FAIL: failed to compute hash for corrupted local vendor binary" >&2
  exit 1
fi
bad_size="$(${SUDO} stat -c %s "$vendor_path")"
if [[ "$manifest_present" == "1" ]]; then
  ${SUDO} python3 - "$VENDOR_MANIFEST" "$VENDOR_COMPONENT_NAME" "$bad_hash" "$bad_size" <<'PY'
import json
import sys

manifest_path = sys.argv[1]
component_name = sys.argv[2]
bad_hash = sys.argv[3]
bad_size = int(sys.argv[4])

with open(manifest_path, "r", encoding="utf-8") as f:
    data = json.load(f)
components = (data or {}).get("components") or {}
if component_name not in components:
    raise SystemExit(f"component '{component_name}' missing in vendor manifest")
component = components[component_name]
component["hash"] = bad_hash
component["size"] = bad_size
with open(manifest_path, "w", encoding="utf-8") as f:
    json.dump(data, f, indent=2, sort_keys=True)
    f.write("\n")
PY
fi
echo "Corrupted local vendor hash: $bad_hash"

echo "Step 3/6: trigger runtime_update again to force vendor sync with bad artifact"
rollback_start_ms="$(epoch_ms)"
trigger_runtime_update_cycle

echo "Step 4/6: validate rollback evidence (deployment + service + hash restore)"
start_secs="$(date +%s)"
while true; do
  now_secs="$(date +%s)"
  elapsed=$((now_secs - start_secs))
  if (( elapsed > WAIT_TIMEOUT_SECS )); then
    echo "FAIL: timeout waiting rollback evidence (${WAIT_TIMEOUT_SECS}s)" >&2
    break
  fi

  current_remote_hash="$(remote_vendor_hash || true)"
  health_ok=0
  if remote_syncthing_service_active; then
    health_ok=1
  fi

  deployments_body="$tmpdir/deployments.json"
  status_deploy="$(http_call "GET" "$BASE/deployments?hive=$HIVE_ID&category=vendor&limit=50" "$deployments_body")"
  log_http_response "$status_deploy" "$deployments_body"
  rollback_ok=0
  if [[ "$status_deploy" == "200" && "$(has_recent_vendor_rollback_deployment "$deployments_body" "$rollback_start_ms")" == "1" ]]; then
    rollback_ok=1
  fi

  if [[ "$current_remote_hash" == "$baseline_local_hash" && "$health_ok" == "1" && "$rollback_ok" == "1" ]]; then
    echo "OK: vendor rollback applied and evidenced via API"
    break
  fi

  echo "[WAIT] elapsed=${elapsed}s remote_hash=${current_remote_hash:-<none>} health_ok=${health_ok} rollback_ok=${rollback_ok}" >&2
  sleep "$POLL_INTERVAL_SECS"
done

if [[ "${current_remote_hash:-}" != "$baseline_local_hash" ]]; then
  echo "---- deployments body ----" >&2
  cat "$tmpdir/deployments.json" >&2 || true
  echo "FAIL: remote vendor hash was not restored to baseline by rollback" >&2
  exit 1
fi
if ! remote_syncthing_service_active; then
  echo "FAIL: ${REMOTE_SYNCTHING_SERVICE} is not active after rollback attempt" >&2
  exit 1
fi
if [[ "${rollback_ok:-0}" != "1" ]]; then
  echo "---- deployments body ----" >&2
  cat "$tmpdir/deployments.json" >&2 || true
  echo "FAIL: did not find vendor deployment evidence with rollback applied" >&2
  exit 1
fi

echo "Step 5/6: restore local vendor source/manifest"
restore_local_vendor
restored_local_hash="$(local_vendor_hash)"
if [[ -z "$restored_local_hash" ]]; then
  echo "FAIL: local vendor hash empty after restore" >&2
  exit 1
fi

echo "Step 6/6: trigger runtime_update cycle to leave cluster converged"
trigger_runtime_update_cycle
final_remote_hash="$(remote_vendor_hash || true)"
if [[ -z "$final_remote_hash" || "$final_remote_hash" != "$restored_local_hash" ]]; then
  echo "FAIL: final vendor convergence failed (remote != restored local)" >&2
  echo "local=$restored_local_hash remote=${final_remote_hash:-<none>}" >&2
  exit 1
fi
if ! remote_syncthing_service_active; then
  echo "FAIL: ${REMOTE_SYNCTHING_SERVICE} is not active at end of E2E" >&2
  exit 1
fi

echo "vendor rollback E2E passed."
