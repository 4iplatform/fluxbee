#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-io-api-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  deploy-io-api.sh --base <url> --hive-id <id> --version <ver> [options]

Required:
  --base <url>                 Admin API base URL (example: http://127.0.0.1:8080)
  --hive-id <id>               Target hive ID (example: motherbee)
  --version <ver>              Runtime version to publish (example: 0.1.0)

Options:
  --node-name <name@hive>      If provided, script will also spawn/restart node
  --runtime <name>             Runtime key (default: io.api)
  --runtime-version <ver>      Runtime version for spawn payload (default: current)
  --update-scope <targeted|global>
                               SYSTEM_UPDATE scope (default: targeted)
  --tenant-id <id>             Tenant ID for spawn identity registration (required in some hives)
  --config-json <file>         JSON file with spawn config object
  --listen-address <addr>      Convenience: set listen.address in spawn config
  --listen-port <port>         Convenience: set listen.port in spawn config
  --dst-node <name>            Convenience: set io.dst_node in spawn config
  --blob-root <path>           Convenience: set blob.path in spawn config
  --identity-target <name>     Convenience: add identity_target to spawn config
  --identity-timeout-ms <ms>   Convenience: add identity_timeout_ms to spawn config
  --api-key <token>            Apply business config via CONFIG_SET with this bearer token
                               (default: $API_KEY from environment if set)
  --api-key-id <id>            key_id for auth.api_keys[] (default: support-main)
  --relay-window-ms <ms>       io.relay.window_ms for CONFIG_SET (default: 300000)
  --relay-max-open-sessions <n>
                               io.relay.max_open_sessions (default: 10000)
  --relay-max-fragments <n>    io.relay.max_fragments_per_session (default: 8)
  --relay-max-bytes <n>        io.relay.max_bytes_per_session (default: 262144)
  --skip-config-set            Skip CONFIG_SET/restart/schema validation step
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
  --log-file <path>            Log file path (default: /tmp/deploy-io-api-<ts>.log)
  -h, --help                   Show help

Notes:
  - The script parses manifest_version/manifest_hash from publish output automatically.
  - On update status=sync_pending, it retries according to --update-retries.
  - Default spawn config may be empty and leaves business config to CONFIG_SET.
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
RUNTIME="io.api"
RUNTIME_VERSION="current"
UPDATE_SCOPE="targeted"
TENANT_ID=""
NODE_NAME=""
CONFIG_JSON=""
LISTEN_ADDRESS=""
LISTEN_PORT=""
DST_NODE=""
BLOB_ROOT=""
IDENTITY_TARGET=""
IDENTITY_TIMEOUT_MS=""
API_KEY_VALUE="${API_KEY:-}"
API_KEY_ID="support-main"
RELAY_WINDOW_MS="300000"
RELAY_MAX_OPEN_SESSIONS="10000"
RELAY_MAX_FRAGMENTS="8"
RELAY_MAX_BYTES="262144"
SKIP_CONFIG_SET=0
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
LOG_FILE="/tmp/deploy-io-api-$(date +%Y%m%d-%H%M%S).log"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="${2:-}"; shift 2 ;;
    --hive-id) HIVE_ID="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --runtime) RUNTIME="${2:-}"; shift 2 ;;
    --runtime-version) RUNTIME_VERSION="${2:-}"; shift 2 ;;
    --update-scope) UPDATE_SCOPE="${2:-}"; shift 2 ;;
    --tenant-id) TENANT_ID="${2:-}"; shift 2 ;;
    --node-name) NODE_NAME="${2:-}"; shift 2 ;;
    --config-json) CONFIG_JSON="${2:-}"; shift 2 ;;
    --listen-address) LISTEN_ADDRESS="${2:-}"; shift 2 ;;
    --listen-port) LISTEN_PORT="${2:-}"; shift 2 ;;
    --dst-node) DST_NODE="${2:-}"; shift 2 ;;
    --blob-root) BLOB_ROOT="${2:-}"; shift 2 ;;
    --identity-target) IDENTITY_TARGET="${2:-}"; shift 2 ;;
    --identity-timeout-ms) IDENTITY_TIMEOUT_MS="${2:-}"; shift 2 ;;
    --api-key) API_KEY_VALUE="${2:-}"; shift 2 ;;
    --api-key-id) API_KEY_ID="${2:-}"; shift 2 ;;
    --relay-window-ms) RELAY_WINDOW_MS="${2:-}"; shift 2 ;;
    --relay-max-open-sessions) RELAY_MAX_OPEN_SESSIONS="${2:-}"; shift 2 ;;
    --relay-max-fragments) RELAY_MAX_FRAGMENTS="${2:-}"; shift 2 ;;
    --relay-max-bytes) RELAY_MAX_BYTES="${2:-}"; shift 2 ;;
    --skip-config-set) SKIP_CONFIG_SET=1; shift ;;
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

case "$UPDATE_SCOPE" in
  targeted|global) ;;
  *)
    echo "Error: --update-scope must be targeted or global (got '$UPDATE_SCOPE')" >&2
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

cfg.pop("_system", None)
cfg.pop("identity_fallback_target", None)
ident = cfg.get("identity")
if isinstance(ident, dict):
    ident.pop("fallback_target", None)

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

  LISTEN_ADDRESS="$LISTEN_ADDRESS" \
  LISTEN_PORT="$LISTEN_PORT" \
  DST_NODE="$DST_NODE" \
  BLOB_ROOT="$BLOB_ROOT" \
  IDENTITY_TARGET="$IDENTITY_TARGET" \
  IDENTITY_TIMEOUT_MS="$IDENTITY_TIMEOUT_MS" \
  python3 - <<'PY'
import json
import os

listen_address = os.environ.get("LISTEN_ADDRESS", "")
listen_port = os.environ.get("LISTEN_PORT", "")
dst_node = os.environ.get("DST_NODE", "")
blob_root = os.environ.get("BLOB_ROOT", "")
identity_target = os.environ.get("IDENTITY_TARGET", "")
identity_timeout_ms = os.environ.get("IDENTITY_TIMEOUT_MS", "")

cfg = {
}

if listen_address and listen_port:
    cfg["listen"] = {
        "address": listen_address,
        "port": int(listen_port)
    }
elif listen_address or listen_port:
    raise SystemExit("listen.address and listen.port must be provided together")

if dst_node:
    cfg.setdefault("io", {})["dst_node"] = dst_node
if blob_root:
    cfg.setdefault("blob", {})["path"] = blob_root
if identity_target:
    cfg["identity_target"] = identity_target
if identity_timeout_ms:
    cfg["identity_timeout_ms"] = int(identity_timeout_ms)

print(json.dumps(cfg, separators=(",", ":")))
PY
}

fetch_existing_config_json() {
  if [[ -z "$NODE_NAME" ]]; then
    echo "Error: --reuse-existing-config requires --node-name" >&2
    exit 1
  fi
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

next_config_version() {
  python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
}

build_runtime_config_json() {
  local effective_dst_node="${DST_NODE:-resolve}"
  CONFIG_LISTEN_ADDRESS="$LISTEN_ADDRESS" \
  CONFIG_LISTEN_PORT="$LISTEN_PORT" \
  CONFIG_API_KEY="$API_KEY_VALUE" \
  CONFIG_API_KEY_ID="$API_KEY_ID" \
  CONFIG_DST_NODE="$effective_dst_node" \
  CONFIG_RELAY_WINDOW_MS="$RELAY_WINDOW_MS" \
  CONFIG_RELAY_MAX_OPEN_SESSIONS="$RELAY_MAX_OPEN_SESSIONS" \
  CONFIG_RELAY_MAX_FRAGMENTS="$RELAY_MAX_FRAGMENTS" \
  CONFIG_RELAY_MAX_BYTES="$RELAY_MAX_BYTES" \
  python3 - <<'PY'
import json
import os

payload = {
    "listen": {
        "address": os.environ["CONFIG_LISTEN_ADDRESS"],
        "port": int(os.environ["CONFIG_LISTEN_PORT"]),
    },
    "auth": {
        "mode": "api_key",
        "api_keys": [
            {
                "key_id": os.environ["CONFIG_API_KEY_ID"],
                "token": os.environ["CONFIG_API_KEY"],
            }
        ],
    },
    "ingress": {
        "subject_mode": "explicit_subject",
        "accepted_content_types": [
            "application/json",
            "multipart/form-data",
        ],
        "max_request_bytes": 262144,
        "max_attachments_per_request": 4,
        "max_attachment_size_bytes": 10485760,
        "max_total_attachment_bytes": 20971520,
        "allowed_mime_types": [
            "image/png",
            "image/jpeg",
            "application/pdf",
            "text/plain",
            "application/json",
        ],
    },
    "io": {
        "dst_node": os.environ["CONFIG_DST_NODE"],
        "relay": {
            "window_ms": int(os.environ["CONFIG_RELAY_WINDOW_MS"]),
            "max_open_sessions": int(os.environ["CONFIG_RELAY_MAX_OPEN_SESSIONS"]),
            "max_fragments_per_session": int(os.environ["CONFIG_RELAY_MAX_FRAGMENTS"]),
            "max_bytes_per_session": int(os.environ["CONFIG_RELAY_MAX_BYTES"]),
        },
    },
}

print(json.dumps(payload, separators=(",", ":")))
PY
}

apply_runtime_config() {
  if [[ "$SKIP_CONFIG_SET" == "1" ]]; then
    log "config_set_skipped explicit_flag=1"
    return 0
  fi
  if [[ -z "$NODE_NAME" ]]; then
    log "config_set_skipped reason=node_name_missing"
    return 0
  fi
  if [[ -z "$LISTEN_ADDRESS" || -z "$LISTEN_PORT" ]]; then
    log "config_set_skipped reason=listen_missing"
    return 0
  fi
  if [[ -z "$API_KEY_VALUE" ]]; then
    log "config_set_skipped reason=api_key_missing set=--api-key_or_env_API_KEY"
    return 0
  fi

  local config_version runtime_config payload resp status error_code
  config_version="$(next_config_version)"
  runtime_config="$(build_runtime_config_json)"
  log "step=config_set config_version=$config_version relay_window_ms=$RELAY_WINDOW_MS dst_node=${DST_NODE:-resolve}"

  payload="$(
    NODE_NAME="$NODE_NAME" \
    CONFIG_VERSION="$config_version" \
    RUNTIME_CONFIG_JSON="$runtime_config" \
    python3 - <<'PY'
import json
import os

print(json.dumps({
    "requested_by": "deploy-io-api.sh",
    "schema_version": 1,
    "config_version": int(os.environ["CONFIG_VERSION"]),
    "apply_mode": "replace",
    "config": json.loads(os.environ["RUNTIME_CONFIG_JSON"]),
}, separators=(",", ":")))
PY
  )"

  resp="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" \
    -H "Content-Type: application/json" \
    -d "$payload" | tee -a "$LOG_FILE")"

  status="$(RAW_JSON="$resp" python3 - <<'PY'
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
try:
    data = json.loads(raw)
except Exception:
    print("invalid_json")
    sys.exit(0)
print(data.get("status", ""))
PY
)"

  error_code="$(RAW_JSON="$resp" python3 - <<'PY'
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
try:
    data = json.loads(raw)
except Exception:
    print("")
    sys.exit(0)
print(data.get("error_code") or "")
PY
)"

  if [[ "$status" == "ok" ]]; then
    log "config_set_ok config_version=$config_version"
    return 0
  fi
  if [[ "$error_code" == "TIMEOUT" ]]; then
    log "config_set_timeout config_version=$config_version continuing_with_restart_and_probe=1"
    return 0
  fi

  log "config_set_failed status=${status:-unknown} error_code=${error_code:-none}"
  return 1
}

probe_schema_ready() {
  if [[ -z "$LISTEN_ADDRESS" || -z "$LISTEN_PORT" ]]; then
    log "schema_probe_skipped reason=listen_missing"
    return 0
  fi

  local url="http://$LISTEN_ADDRESS:$LISTEN_PORT/schema"
  local attempt body status
  for attempt in $(seq 1 20); do
    body="$(curl -sS "$url" 2>/dev/null || true)"
    status="$(RAW_JSON="$body" python3 - <<'PY'
import json
import os
import sys
raw = (os.environ.get("RAW_JSON") or "").strip()
if not raw:
    print("")
    sys.exit(0)
try:
    data = json.loads(raw)
except Exception:
    print("")
    sys.exit(0)
print(data.get("status") or "")
PY
)"
    if [[ "$status" == "configured" ]]; then
      log "schema_probe_ok url=$url attempt=$attempt"
      return 0
    fi
    sleep 1
  done

  log "schema_probe_failed url=$url"
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

publish_cmd=(bash "$PUBLISH_SCRIPT" --version "$VERSION" --runtime "$RUNTIME" --set-current)
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
log "update_scope=$UPDATE_SCOPE runtime=$RUNTIME target_version=$VERSION"

if [[ "$UPDATE_SCOPE" == "targeted" ]]; then
  update_payload="$(python3 - <<PY
import json
print(json.dumps({
  "category": "runtime",
  "manifest_version": int("${MANIFEST_VERSION}"),
  "manifest_hash": "${MANIFEST_HASH}",
  "runtime": "${RUNTIME}",
  "runtime_version": "${VERSION}",
}, separators=(",", ":")))
PY
)"
else
  update_payload="$(python3 - <<PY
import json
print(json.dumps({
  "category": "runtime",
  "manifest_version": int("${MANIFEST_VERSION}"),
  "manifest_hash": "${MANIFEST_HASH}",
}, separators=(",", ":")))
PY
)"
fi

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
      log "update_existing: existing unit found; proceeding to config_set/restart node_name=$NODE_NAME"
    else
      DO_SPAWN=1
      REUSE_EXISTING_CONFIG=1
      log "update_existing: unit missing, falling back to spawn node_name=$NODE_NAME"
    fi
  else
    log "deploy completed (publish+update). spawn/config skipped."
    exit 0
  fi
fi

if [[ -z "$NODE_NAME" ]]; then
  log "error: spawn requested but --node-name missing"
  exit 1
fi

if [[ "$DO_SPAWN" == "1" ]]; then
  if [[ -z "$TENANT_ID" && -z "${ORCH_DEFAULT_TENANT_ID:-}" ]]; then
    log "warning: spawn may fail with IDENTITY_REGISTER_FAILED because tenant_id is missing; set --tenant-id or ORCH_DEFAULT_TENANT_ID"
  fi

  spawn_cfg_json="$(build_spawn_config_json)"
  log "step=spawn_config config=$spawn_cfg_json"

  spawn_payload="$(NODE_NAME="$NODE_NAME" RUNTIME="$RUNTIME" RUNTIME_VERSION="$RUNTIME_VERSION" TENANT_ID="$TENANT_ID" SPAWN_CFG_JSON="$spawn_cfg_json" python3 - <<'PY'
import json
import os
cfg = json.loads(os.environ["SPAWN_CFG_JSON"])
tenant_id = os.environ.get("TENANT_ID", "").strip()
if tenant_id:
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
      log "spawn_node_already_exists; continuing with config_set/restart for update flow"
    else
      log "spawn_failed status=${spawn_status:-unknown}"
      exit 1
    fi
  fi
fi

apply_runtime_config
restart_existing_unit_if_present "$NODE_NAME"
probe_schema_ready

log "deploy completed (publish+update+spawn+config_set+restart) node_name=$NODE_NAME"
