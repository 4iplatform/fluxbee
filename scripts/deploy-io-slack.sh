#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-io-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  deploy-io-slack.sh --base <url> --hive-id <id> --version <ver> [options]

Required:
  --base <url>                 Admin API base URL (example: http://127.0.0.1:8080)
  --hive-id <id>               Target hive ID (example: motherbee)
  --version <ver>              Runtime version to publish (example: 0.1.0)

Options:
  --node-name <name@hive>      If provided, script will also spawn/restart node
  --runtime <name>             Runtime key (default: IO.slack)
  --runtime-version <ver>      Runtime version for spawn payload (default: current)
  --tenant-id <id>             Tenant ID for spawn identity registration (required in some hives)
  --config-json <file>         JSON file with spawn config object
  --app-token <xapp-...>       Convenience: build spawn config with inline Slack app token
  --bot-token <xoxb-...>       Convenience: build spawn config with inline Slack bot token
  --identity-target <name>     Convenience: add identity_target to spawn config
  --identity-timeout-ms <ms>   Convenience: add identity_timeout_ms to spawn config
  --spawn                      Force spawn step (requires --node-name and config source)
  --update-existing            Existing node code update: reuse current config + restart unit (spawn only if unit missing)
  --skip-spawn                 Skip spawn step (default unless --node-name is set)
  --kill-first                 If spawning, call DELETE node before POST node
  --reuse-existing-config      When spawning, fetch config from GET /hives/{id}/nodes/{name}/config
  --sync-hint                  Run sync-hint before each update attempt
  --update-retries <n>         Retries when update returns sync_pending (default: 8)
  --retry-delay-s <seconds>    Delay between retries (default: 2)
  --allow-sync-pending         Continue deploy even if update stays in sync_pending after retries
  --sudo                       Pass --sudo to publish script
  --skip-build                 Pass --skip-build to publish script
  --dist-root <path>           Pass --dist-root to publish script
  --log-file <path>            Log file path (default: /tmp/deploy-io-slack-<ts>.log)
  -h, --help                   Show help

Notes:
  - The script parses manifest_version/manifest_hash from publish output automatically.
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
VERSION=""
RUNTIME="IO.slack"
RUNTIME_VERSION="current"
TENANT_ID=""
NODE_NAME=""
CONFIG_JSON=""
APP_TOKEN=""
BOT_TOKEN=""
IDENTITY_TARGET=""
IDENTITY_TIMEOUT_MS=""
DO_SPAWN=0
FORCE_SKIP_SPAWN=0
KILL_FIRST=0
REUSE_EXISTING_CONFIG=0
UPDATE_EXISTING=0
USE_SYNC_HINT=0
UPDATE_RETRIES=8
RETRY_DELAY_S=2
ALLOW_SYNC_PENDING=0
USE_SUDO=0
SKIP_BUILD=0
DIST_ROOT=""
LOG_FILE="/tmp/deploy-io-slack-$(date +%Y%m%d-%H%M%S).log"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="${2:-}"; shift 2 ;;
    --hive-id) HIVE_ID="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --runtime) RUNTIME="${2:-}"; shift 2 ;;
    --runtime-version) RUNTIME_VERSION="${2:-}"; shift 2 ;;
    --tenant-id) TENANT_ID="${2:-}"; shift 2 ;;
    --node-name) NODE_NAME="${2:-}"; shift 2 ;;
    --config-json) CONFIG_JSON="${2:-}"; shift 2 ;;
    --app-token) APP_TOKEN="${2:-}"; shift 2 ;;
    --bot-token) BOT_TOKEN="${2:-}"; shift 2 ;;
    --identity-target) IDENTITY_TARGET="${2:-}"; shift 2 ;;
    --identity-timeout-ms) IDENTITY_TIMEOUT_MS="${2:-}"; shift 2 ;;
    --spawn) DO_SPAWN=1; shift ;;
    --update-existing) UPDATE_EXISTING=1; shift ;;
    --skip-spawn) FORCE_SKIP_SPAWN=1; shift ;;
    --kill-first) KILL_FIRST=1; shift ;;
    --reuse-existing-config) REUSE_EXISTING_CONFIG=1; shift ;;
    --sync-hint) USE_SYNC_HINT=1; shift ;;
    --update-retries) UPDATE_RETRIES="${2:-}"; shift 2 ;;
    --retry-delay-s) RETRY_DELAY_S="${2:-}"; shift 2 ;;
    --allow-sync-pending) ALLOW_SYNC_PENDING=1; shift ;;
    --sudo) USE_SUDO=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --dist-root) DIST_ROOT="${2:-}"; shift 2 ;;
    --log-file) LOG_FILE="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Error: unknown option: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "$BASE" || -z "$HIVE_ID" || -z "$VERSION" ]]; then
  echo "Error: --base, --hive-id and --version are required" >&2
  usage
  exit 1
fi

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
  DO_SPAWN=0
  KILL_FIRST=0
  REUSE_EXISTING_CONFIG=1
fi

mkdir -p "$(dirname "$LOG_FILE")"
touch "$LOG_FILE"

log() {
  local msg="$1"
  echo "[$(date -Iseconds)] $msg" | tee -a "$LOG_FILE"
}

sanitize_config_json() {
  local source_path="${1:-}"
  SANITIZE_CONFIG_SOURCE="$source_path" python3 - <<'PY'
import json
import os
import sys

source = (os.environ.get("SANITIZE_CONFIG_SOURCE") or "").strip()
if source:
    with open(source, "r", encoding="utf-8-sig") as f:
        raw = f.read()
else:
    raw = sys.stdin.read()

if not raw.strip():
    print(f"Warning: empty config payload for sanitize (source={source or 'stdin'}); using {{}}", file=sys.stderr)
    print("{}")
    raise SystemExit(0)

try:
    cfg = json.loads(raw)
except Exception as e:
    print(f"Warning: invalid config JSON for sanitize (source={source or 'stdin'}): {e}; using {{}}", file=sys.stderr)
    print("{}")
    raise SystemExit(0)

if not isinstance(cfg, dict):
    print(f"Warning: config JSON root must be object (source={source or 'stdin'}); using {{}}", file=sys.stderr)
    print("{}")
    raise SystemExit(0)

# Reserved by orchestrator.
cfg.pop("_system", None)

# IO policy: no fallback identity target in active config.
cfg.pop("identity_fallback_target", None)
ident = cfg.get("identity")
if isinstance(ident, dict):
    ident.pop("fallback_target", None)

slack = cfg.get("slack")
if not isinstance(slack, dict) or not slack.get("app_token") or not slack.get("bot_token"):
    print(
        "Warning: sanitized config has no slack.app_token/bot_token; spawn may succeed but runtime will fail missing token",
        file=sys.stderr,
    )

print(json.dumps(cfg, separators=(",", ":")))
PY
}

build_spawn_config_json() {
  if [[ "$REUSE_EXISTING_CONFIG" == "1" ]]; then
    fetch_existing_config_json
    return
  fi

  if [[ -n "$CONFIG_JSON" ]]; then
    if [[ ! -f "$CONFIG_JSON" ]]; then
      echo "Error: --config-json not found: $CONFIG_JSON" >&2
      exit 1
    fi
    sanitize_config_json "$CONFIG_JSON"
    return
  fi

  if [[ -z "$APP_TOKEN" || -z "$BOT_TOKEN" ]]; then
    echo "Error: spawn requires either --config-json or both --app-token and --bot-token" >&2
    exit 1
  fi

  python3 - <<PY | sanitize_config_json
import json

cfg = {
    "slack": {
        "app_token": "${APP_TOKEN}",
        "bot_token": "${BOT_TOKEN}",
    }
}

if "${IDENTITY_TARGET}":
    cfg["identity_target"] = "${IDENTITY_TARGET}"
if "${IDENTITY_TIMEOUT_MS}":
    cfg["identity_timeout_ms"] = int("${IDENTITY_TIMEOUT_MS}")

print(json.dumps(cfg, separators=(",", ":")))
PY
}

fetch_existing_config_json() {
  if [[ -z "$NODE_NAME" ]]; then
    echo "Error: --reuse-existing-config requires --node-name" >&2
    exit 1
  fi
  # Important: this function may run inside command substitution.
  # Keep diagnostics out of stdout so captured JSON stays clean.
  log "step=get_existing_config node_name=$NODE_NAME" >&2
  local raw
  raw="$(curl -sS -X GET "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/config" | tee -a "$LOG_FILE" >&2)"
  RAW_JSON="$raw" python3 - <<'PY' | sanitize_config_json
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
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

derive_unit_name() {
  local node="$1"
  local base="${node%@*}"
  local hive="${node##*@}"
  if [[ "$base" == "$node" ]]; then
    hive="$HIVE_ID"
  fi
  echo "fluxbee-node-${base}-${hive}.service"
}

restart_existing_unit_if_present() {
  local node="$1"
  local unit
  unit="$(derive_unit_name "$node")"
  local systemctl_cmd=("systemctl")
  if [[ "$USE_SUDO" == "1" ]]; then
    systemctl_cmd=("sudo" "systemctl")
  fi

  if "${systemctl_cmd[@]}" cat "$unit" >/dev/null 2>&1; then
    log "step=restart_existing unit=$unit"
    "${systemctl_cmd[@]}" restart "$unit"
    log "restart_ok unit=$unit"
    return 0
  fi

  log "restart_skipped unit_not_found=$unit"
  return 1
}

require_cmd bash
require_cmd curl
require_cmd python3
require_cmd awk
require_cmd sed
require_cmd grep

log "starting deploy; log_file=$LOG_FILE"
log "step=publish runtime=$RUNTIME version=$VERSION"

publish_cmd=(bash "$PUBLISH_SCRIPT" --kind slack --version "$VERSION" --runtime "$RUNTIME" --set-current)
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

  UPDATE_STATUS="$(RAW_JSON="$update_resp" python3 - <<'PY'
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
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
  if [[ "$UPDATE_EXISTING" == "1" && -n "$NODE_NAME" ]]; then
    if restart_existing_unit_if_present "$NODE_NAME"; then
      log "deploy completed (publish+update+restart) node_name=$NODE_NAME"
      exit 0
    fi
    DO_SPAWN=1
    REUSE_EXISTING_CONFIG=1
    log "update_existing: unit missing, falling back to spawn node_name=$NODE_NAME"
  else
    log "deploy completed (publish+update). spawn skipped."
    exit 0
  fi
fi

if [[ -z "$NODE_NAME" ]]; then
  log "error: spawn requested but --node-name missing"
  exit 1
fi

if [[ -z "$TENANT_ID" && -z "${ORCH_DEFAULT_TENANT_ID:-}" ]]; then
  log "warning: spawn may fail with IDENTITY_REGISTER_FAILED because tenant_id is missing; set --tenant-id or ORCH_DEFAULT_TENANT_ID"
fi

spawn_cfg_json="$(build_spawn_config_json)"

spawn_payload="$(NODE_NAME="$NODE_NAME" RUNTIME="$RUNTIME" RUNTIME_VERSION="$RUNTIME_VERSION" TENANT_ID="$TENANT_ID" SPAWN_CFG_JSON="$spawn_cfg_json" python3 - <<'PY'
import json
import os
cfg = json.loads(os.environ["SPAWN_CFG_JSON"])
tenant_id = os.environ.get("TENANT_ID", "").strip()
if tenant_id:
    # Keep both paths to satisfy orchestrator variants.
    cfg.setdefault("tenant_id", tenant_id)

payload = {
  "node_name": os.environ["NODE_NAME"],
  "runtime": os.environ["RUNTIME"],
  "runtime_version": os.environ["RUNTIME_VERSION"],
  "config": cfg,
}
if tenant_id:
    payload["tenant_id"] = tenant_id

print(json.dumps(payload, separators=(",", ":")))
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

spawn_status="$(RAW_JSON="$spawn_resp" python3 - <<'PY'
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
try:
    d = json.loads(raw)
except Exception:
    print("invalid_json")
    sys.exit(0)
print(d.get("status",""))
PY
)"

spawn_error_code="$(RAW_JSON="$spawn_resp" python3 - <<'PY'
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
try:
    d = json.loads(raw)
except Exception:
    print("")
    sys.exit(0)
print(d.get("error_code") or (d.get("payload") or {}).get("error_code") or "")
PY
)"

if [[ "$spawn_status" != "ok" ]]; then
  if [[ "$spawn_error_code" == "NODE_ALREADY_EXISTS" && "$UPDATE_EXISTING" == "1" ]]; then
    log "spawn_node_already_exists; trying restart_existing for update flow"
    if restart_existing_unit_if_present "$NODE_NAME"; then
      log "deploy completed (publish+update+restart) node_name=$NODE_NAME"
      exit 0
    fi
  fi
  log "spawn_failed status=${spawn_status:-unknown}"
  exit 1
fi

log "deploy completed (publish+update+spawn) node_name=$NODE_NAME"
