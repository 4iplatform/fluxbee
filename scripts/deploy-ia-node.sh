#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-ia-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  deploy-ia-node.sh --base <url> --hive-id <id> --runtime <AI.runtime> --version <ver> [options]

Required:
  --base <url>                 Admin API base URL (example: http://127.0.0.1:8080)
  --hive-id <id>               Target hive ID (example: motherbee)
  --runtime <AI.runtime>       Runtime key (example: AI.frontdesk.gov)
  --version <ver>              Runtime version to publish (example: 0.1.0)

Options:
  --mode <default|gov>         Default AI_NODE_MODE embedded in runtime start.sh (default: default)
  --forced-node-name <name>    Pass-through to publish-ia-runtime.sh (forces bootstrap node name in start.sh)
  --forced-dynamic-config-dir <path>
                               Pass-through to publish-ia-runtime.sh (forces bootstrap dynamic config dir)
  --node-name <name@hive>      If provided, script also spawns/restarts node
  --runtime-version <ver>      Runtime version for spawn payload (default: current)
  --config-json <file>         JSON file with spawn config object
  --spawn                      Force spawn step (requires --node-name and --config-json or --reuse-existing-config)
  --skip-spawn                 Skip spawn step (default unless --node-name is set)
  --kill-first                 If spawning, call DELETE node before POST node
  --reuse-existing-config      Fetch config from GET /hives/{id}/nodes/{name}/config and reuse it for spawn
  --update-existing            Shortcut: reuse existing config + kill-first + spawn (requires --node-name)
  --sync-hint                  Run sync-hint before each update attempt
  --allow-sync-pending         Continue to spawn even if update stays sync_pending
  --update-retries <n>         Retries when update returns sync_pending (default: 8)
  --retry-delay-s <seconds>    Delay between retries (default: 2)
  --sudo                       Pass --sudo to publish script
  --skip-build                 Pass --skip-build to publish script
  --dist-root <path>           Pass --dist-root to publish script
  --log-file <path>            Log file path (default: /tmp/deploy-ia-node-<ts>.log)
  -h, --help                   Show help

Notes:
  - Script parses manifest_version/manifest_hash from publish output.
  - On update status=sync_pending, it retries according to --update-retries.
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command: $1" >&2
    exit 1
  fi
}

BASE=""
HIVE_ID=""
RUNTIME=""
VERSION=""
MODE="default"
FORCED_NODE_NAME=""
FORCED_DYNAMIC_CONFIG_DIR=""
RUNTIME_VERSION="current"
NODE_NAME=""
CONFIG_JSON=""
DO_SPAWN=0
FORCE_SKIP_SPAWN=0
KILL_FIRST=0
REUSE_EXISTING_CONFIG=0
UPDATE_EXISTING=0
USE_SYNC_HINT=0
ALLOW_SYNC_PENDING=0
UPDATE_RETRIES=8
RETRY_DELAY_S=2
USE_SUDO=0
SKIP_BUILD=0
DIST_ROOT=""
LOG_FILE="/tmp/deploy-ia-node-$(date +%Y%m%d-%H%M%S).log"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="${2:-}"; shift 2 ;;
    --hive-id) HIVE_ID="${2:-}"; shift 2 ;;
    --runtime) RUNTIME="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --mode) MODE="${2:-}"; shift 2 ;;
    --forced-node-name) FORCED_NODE_NAME="${2:-}"; shift 2 ;;
    --forced-dynamic-config-dir) FORCED_DYNAMIC_CONFIG_DIR="${2:-}"; shift 2 ;;
    --runtime-version) RUNTIME_VERSION="${2:-}"; shift 2 ;;
    --node-name) NODE_NAME="${2:-}"; shift 2 ;;
    --config-json) CONFIG_JSON="${2:-}"; shift 2 ;;
    --spawn) DO_SPAWN=1; shift ;;
    --skip-spawn) FORCE_SKIP_SPAWN=1; shift ;;
    --kill-first) KILL_FIRST=1; shift ;;
    --reuse-existing-config) REUSE_EXISTING_CONFIG=1; shift ;;
    --update-existing) UPDATE_EXISTING=1; shift ;;
    --sync-hint) USE_SYNC_HINT=1; shift ;;
    --allow-sync-pending) ALLOW_SYNC_PENDING=1; shift ;;
    --update-retries) UPDATE_RETRIES="${2:-}"; shift 2 ;;
    --retry-delay-s) RETRY_DELAY_S="${2:-}"; shift 2 ;;
    --sudo) USE_SUDO=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --dist-root) DIST_ROOT="${2:-}"; shift 2 ;;
    --log-file) LOG_FILE="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Error: unknown option: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "$BASE" || -z "$HIVE_ID" || -z "$RUNTIME" || -z "$VERSION" ]]; then
  echo "Error: --base, --hive-id, --runtime and --version are required" >&2
  usage
  exit 1
fi

case "$MODE" in
  default|gov)
    ;;
  *)
    echo "Error: --mode must be default or gov (got '$MODE')" >&2
    exit 1
    ;;
esac

if [[ "$FORCE_SKIP_SPAWN" == "1" ]]; then
  DO_SPAWN=0
elif [[ -n "$NODE_NAME" ]]; then
  DO_SPAWN=1
fi

if [[ "$UPDATE_EXISTING" == "1" ]]; then
  if [[ -z "$NODE_NAME" ]]; then
    echo "Error: --update-existing requires --node-name" >&2
    exit 1
  fi
  DO_SPAWN=1
  KILL_FIRST=1
  REUSE_EXISTING_CONFIG=1
fi

mkdir -p "$(dirname "$LOG_FILE")"
touch "$LOG_FILE"

log() {
  local msg="$1"
  echo "[$(date -Iseconds)] $msg" | tee -a "$LOG_FILE"
}

fetch_existing_config_json() {
  if [[ -z "$NODE_NAME" ]]; then
    echo "Error: --reuse-existing-config requires --node-name" >&2
    exit 1
  fi
  log "step=get_existing_config node_name=$NODE_NAME"
  local raw
  raw="$(curl -sS -X GET "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" | tee -a "$LOG_FILE")"
  RAW_CONFIG_RESPONSE="$raw" python3 - <<'PY'
import json, os, sys
raw = (os.environ.get("RAW_CONFIG_RESPONSE") or "").strip()
if not raw:
    print("Error: empty response from get config", file=sys.stderr)
    sys.exit(1)
try:
    d = json.loads(raw)
except Exception as e:
    print(f"Error: invalid JSON from get config: {e}", file=sys.stderr)
    sys.exit(1)

status = d.get("status")
if status and status != "ok":
    print(f"Error: get config returned status={status}", file=sys.stderr)
    sys.exit(1)

payload = d.get("payload")
cfg = None
if isinstance(payload, dict):
    if isinstance(payload.get("config"), dict):
        cfg = payload["config"]
    elif isinstance(payload.get("node"), dict) and isinstance(payload["node"].get("config"), dict):
        cfg = payload["node"]["config"]
    elif isinstance(payload.get("data"), dict) and isinstance(payload["data"].get("config"), dict):
        cfg = payload["data"]["config"]

if cfg is None and isinstance(d.get("config"), dict):
    cfg = d["config"]

if cfg is None:
    print("Error: could not locate config object in get config response", file=sys.stderr)
    sys.exit(1)

print(json.dumps(cfg, separators=(",", ":")))
PY
}

build_spawn_config_json() {
  if [[ "$REUSE_EXISTING_CONFIG" == "1" ]]; then
    fetch_existing_config_json
    return
  fi
  if [[ -z "$CONFIG_JSON" ]]; then
    echo "Error: spawn requires --config-json or --reuse-existing-config" >&2
    exit 1
  fi
  if [[ ! -f "$CONFIG_JSON" ]]; then
    echo "Error: --config-json not found: $CONFIG_JSON" >&2
    exit 1
  fi
  cat "$CONFIG_JSON"
}

require_cmd bash
require_cmd curl
require_cmd python3
require_cmd awk
require_cmd sed
require_cmd grep

log "starting deploy; log_file=$LOG_FILE"
log "step=publish runtime=$RUNTIME version=$VERSION mode=$MODE"

publish_cmd=(bash "$PUBLISH_SCRIPT" --runtime "$RUNTIME" --version "$VERSION" --mode "$MODE" --set-current)
if [[ -n "$FORCED_NODE_NAME" ]]; then
  publish_cmd+=(--forced-node-name "$FORCED_NODE_NAME")
fi
if [[ -n "$FORCED_DYNAMIC_CONFIG_DIR" ]]; then
  publish_cmd+=(--forced-dynamic-config-dir "$FORCED_DYNAMIC_CONFIG_DIR")
fi
if [[ "$USE_SUDO" == "1" ]]; then
  publish_cmd+=(--sudo)
fi
if [[ "$SKIP_BUILD" == "1" ]]; then
  publish_cmd+=(--skip-build)
fi
if [[ -n "$DIST_ROOT" ]]; then
  publish_cmd+=(--dist-root "$DIST_ROOT")
fi

publish_out="$("${publish_cmd[@]}" 2>&1 | tee -a "$LOG_FILE")"

MANIFEST_VERSION="$(echo "$publish_out" | awk -F= '/^manifest_version=/{print $2}' | tail -n1 | tr -d '[:space:]')"
MANIFEST_HASH="$(echo "$publish_out" | awk -F= '/^manifest_hash=/{print $2}' | tail -n1 | tr -d '[:space:]')"

if [[ -z "$MANIFEST_VERSION" || -z "$MANIFEST_HASH" ]]; then
  log "error: failed parsing manifest_version/hash from publish output"
  exit 1
fi

log "publish_ok manifest_version=$MANIFEST_VERSION manifest_hash=$MANIFEST_HASH"

update_payload="$(python3 - <<PY
import json
print(json.dumps({
  "category": "runtime",
  "manifest_version": int("${MANIFEST_VERSION}"),
  "manifest_hash": "${MANIFEST_HASH}",
}, separators=(",", ":")))
PY
)"

attempt=1
UPDATE_STATUS=""
while [[ "$attempt" -le "$UPDATE_RETRIES" ]]; do
  if [[ "$USE_SYNC_HINT" == "1" ]]; then
    log "step=sync_hint attempt=$attempt"
    curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" \
      -H "Content-Type: application/json" \
      -d '{"channel":"dist","wait_for_idle":true,"timeout_ms":30000}' | tee -a "$LOG_FILE" >/dev/null
  fi

  log "step=update attempt=$attempt"
  update_resp="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/update" \
    -H "Content-Type: application/json" \
    -d "$update_payload" | tee -a "$LOG_FILE")"

  UPDATE_STATUS="$(UPDATE_RESP="$update_resp" python3 - <<'PY'
import json, os
raw = (os.environ.get("UPDATE_RESP") or "").strip()
try:
    d = json.loads(raw)
except Exception:
    print("invalid_json")
    sys.exit(0)
status = d.get("status")
if not status:
    status = (d.get("payload") or {}).get("status", "")
print(status or "")
PY
)"

  if [[ "$UPDATE_STATUS" == "ok" ]]; then
    log "update_ok"
    break
  fi

  if [[ "$UPDATE_STATUS" == "sync_pending" ]]; then
    log "update_sync_pending attempt=$attempt sleeping=${RETRY_DELAY_S}s"
    sleep "$RETRY_DELAY_S"
    attempt=$((attempt + 1))
    continue
  fi

  log "update_failed status=${UPDATE_STATUS:-unknown}"
  exit 1
done

if [[ "$UPDATE_STATUS" != "ok" ]]; then
  if [[ "$UPDATE_STATUS" == "sync_pending" && "$ALLOW_SYNC_PENDING" == "1" ]]; then
    log "update_not_ready status=$UPDATE_STATUS after $UPDATE_RETRIES attempts; continuing because --allow-sync-pending is set"
  else
    log "update_not_ready status=$UPDATE_STATUS after $UPDATE_RETRIES attempts"
    exit 1
  fi
fi

if [[ "$DO_SPAWN" != "1" ]]; then
  log "deploy completed (publish+update). spawn skipped."
  exit 0
fi

if [[ -z "$NODE_NAME" ]]; then
  log "error: spawn requested but --node-name missing"
  exit 1
fi

spawn_cfg_json="$(build_spawn_config_json)"
spawn_payload="$(NODE_NAME="$NODE_NAME" RUNTIME="$RUNTIME" RUNTIME_VERSION="$RUNTIME_VERSION" SPAWN_CFG_JSON="$spawn_cfg_json" python3 - <<'PY'
import json
import os
print(json.dumps({
  "node_name": os.environ["NODE_NAME"],
  "runtime": os.environ["RUNTIME"],
  "runtime_version": os.environ["RUNTIME_VERSION"],
  "config": json.loads(os.environ["SPAWN_CFG_JSON"]),
}, separators=(",", ":")))
PY
)"

if [[ "$KILL_FIRST" == "1" ]]; then
  log "step=kill_first node_name=$NODE_NAME"
  curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" | tee -a "$LOG_FILE" >/dev/null || true
fi

log "step=spawn node_name=$NODE_NAME"
spawn_resp="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" \
  -H "Content-Type: application/json" \
  -d "$spawn_payload" | tee -a "$LOG_FILE")"

spawn_status="$(SPAWN_RESP="$spawn_resp" python3 - <<'PY'
import json, os
raw = (os.environ.get("SPAWN_RESP") or "").strip()
try:
    d = json.loads(raw)
except Exception:
    print("invalid_json")
    sys.exit(0)
print(d.get("status",""))
PY
)"

if [[ "$spawn_status" != "ok" ]]; then
  log "spawn_failed status=${spawn_status:-unknown}"
  exit 1
fi

log "deploy completed (publish+update+spawn) node_name=$NODE_NAME"
