#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-io-api-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  deploy-io-api.sh --base <url> --hive-id <id> --version <ver> [options]

Required:
  --base <url>                 SY.admin HTTP base URL
  --hive-id <id>               Hive where the IO.api instance runs
  --version <ver>              io.api runtime version to publish

Instance options:
  --node-name <name@hive>      Spawn or update one managed IO.api instance
  --tenant-id <tnt:uuid>       Tenant injected by Orchestrator (required for new spawn)
  --config-json <file>         Complete Edge-native IO.api config
  --edge-node <SY.edge@hive>   Convenience config builder: Edge owner of the public URL
  --api-channel-id <id>        Stable channel address used to create the instance ICH
  --dst-node <name>            Fixed internal destination; requests cannot override it
  --subject-mode <mode>        explicit_subject or caller_is_subject (default: explicit_subject)
  --caller-external-id <id>    Required when subject-mode=caller_is_subject
  --publish <true|false>       Desired Edge publication state (default: true)
  --relay-window-ms <ms>       Relay window (default: 0, immediate passthrough)

Runtime/update options:
  --runtime <name>             Runtime key (default: io.api)
  --runtime-version <ver>      Version selected by spawn (default: current)
  --update-scope <targeted|global>
  --spawn                      Force spawn (implied by --node-name unless --update-existing)
  --update-existing            Restart an existing unit on the new runtime before CONFIG_SET
  --kill-first                 Delete the managed node before spawning it
  --skip-spawn                 Publish/update only
  --skip-config-set            Do not send the typed config to the running node
  --sync-hint                  Wait for dist sync before update attempts
  --update-retries <n>         Default: 8
  --retry-delay-s <seconds>    Default: 2
  --allow-sync-pending         Continue when runtime sync remains pending
  --dist-root <path>           Dist root passed to publish helper
  --sudo                       Use sudo for local publish/restart operations
  --skip-build                 Reuse an existing io-api release binary
  --log-file <path>            Deployment log path
  -h, --help                   Show help

The runtime exposes no local HTTP port. Readiness is checked through
POST /hives/{hive}/nodes/{node}/control/config-get and runtime.publication.status.
SY.admin mints the Edge bearer. When newly issued, this helper prints it once without
writing it to the deployment log.
EOF
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Error: missing command: $1" >&2
    exit 1
  }
}

BASE=""
HIVE_ID=""
VERSION=""
RUNTIME="io.api"
RUNTIME_VERSION="current"
UPDATE_SCOPE="targeted"
NODE_NAME=""
TENANT_ID=""
CONFIG_JSON=""
EDGE_NODE=""
API_CHANNEL_ID=""
DST_NODE=""
SUBJECT_MODE="explicit_subject"
CALLER_EXTERNAL_ID=""
PUBLISH="true"
RELAY_WINDOW_MS="0"
DO_SPAWN=0
UPDATE_EXISTING=0
KILL_FIRST=0
SKIP_SPAWN=0
SKIP_CONFIG_SET=0
USE_SYNC_HINT=0
UPDATE_RETRIES=8
RETRY_DELAY_S=2
ALLOW_SYNC_PENDING=0
DIST_ROOT=""
USE_SUDO=0
SKIP_BUILD=0
LOG_FILE="/tmp/deploy-io-api-$(date +%Y%m%d-%H%M%S).log"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base) BASE="${2:-}"; shift 2 ;;
    --hive-id) HIVE_ID="${2:-}"; shift 2 ;;
    --version) VERSION="${2:-}"; shift 2 ;;
    --runtime) RUNTIME="${2:-}"; shift 2 ;;
    --runtime-version) RUNTIME_VERSION="${2:-}"; shift 2 ;;
    --update-scope) UPDATE_SCOPE="${2:-}"; shift 2 ;;
    --node-name) NODE_NAME="${2:-}"; shift 2 ;;
    --tenant-id) TENANT_ID="${2:-}"; shift 2 ;;
    --config-json) CONFIG_JSON="${2:-}"; shift 2 ;;
    --edge-node) EDGE_NODE="${2:-}"; shift 2 ;;
    --api-channel-id) API_CHANNEL_ID="${2:-}"; shift 2 ;;
    --dst-node) DST_NODE="${2:-}"; shift 2 ;;
    --subject-mode) SUBJECT_MODE="${2:-}"; shift 2 ;;
    --caller-external-id) CALLER_EXTERNAL_ID="${2:-}"; shift 2 ;;
    --publish) PUBLISH="${2:-}"; shift 2 ;;
    --relay-window-ms) RELAY_WINDOW_MS="${2:-}"; shift 2 ;;
    --spawn) DO_SPAWN=1; shift ;;
    --update-existing) UPDATE_EXISTING=1; shift ;;
    --kill-first) KILL_FIRST=1; shift ;;
    --skip-spawn) SKIP_SPAWN=1; shift ;;
    --skip-config-set) SKIP_CONFIG_SET=1; shift ;;
    --sync-hint) USE_SYNC_HINT=1; shift ;;
    --update-retries) UPDATE_RETRIES="${2:-}"; shift 2 ;;
    --retry-delay-s) RETRY_DELAY_S="${2:-}"; shift 2 ;;
    --allow-sync-pending) ALLOW_SYNC_PENDING=1; shift ;;
    --dist-root) DIST_ROOT="${2:-}"; shift 2 ;;
    --sudo) USE_SUDO=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
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
case "$UPDATE_SCOPE" in targeted|global) ;; *) echo "Error: invalid --update-scope" >&2; exit 1 ;; esac
case "$SUBJECT_MODE" in explicit_subject|caller_is_subject) ;; *) echo "Error: invalid --subject-mode" >&2; exit 1 ;; esac
case "$PUBLISH" in true|false) ;; *) echo "Error: --publish must be true or false" >&2; exit 1 ;; esac
if [[ "$SUBJECT_MODE" == "caller_is_subject" && -z "$CALLER_EXTERNAL_ID" && -z "$CONFIG_JSON" ]]; then
  echo "Error: caller_is_subject requires --caller-external-id" >&2
  exit 1
fi
if [[ -n "$NODE_NAME" && "$SKIP_SPAWN" != "1" && "$UPDATE_EXISTING" != "1" ]]; then
  DO_SPAWN=1
fi
if [[ "$UPDATE_EXISTING" == "1" && -z "$NODE_NAME" ]]; then
  echo "Error: --update-existing requires --node-name" >&2
  exit 1
fi

require_cmd bash
require_cmd curl
require_cmd python3
require_cmd awk
require_cmd sed
require_cmd grep

mkdir -p "$(dirname "$LOG_FILE")"
touch "$LOG_FILE"

log() {
  echo "[$(date -Iseconds)] $*" | tee -a "$LOG_FILE"
}

json_status() {
  RAW_JSON="$1" python3 - <<'PY'
import json, os
try:
    value = json.loads(os.environ.get("RAW_JSON", ""))
    print(value.get("status", ""))
except Exception:
    print("invalid_json")
PY
}

build_runtime_config() {
  if [[ -n "$CONFIG_JSON" ]]; then
    [[ -f "$CONFIG_JSON" ]] || { echo "Error: config file not found: $CONFIG_JSON" >&2; exit 1; }
    python3 - "$CONFIG_JSON" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8-sig"))
if not isinstance(value, dict):
    raise SystemExit("IO.api config root must be an object")
value.pop("_system", None)
print(json.dumps(value, separators=(",", ":")))
PY
    return
  fi
  for item in EDGE_NODE API_CHANNEL_ID DST_NODE; do
    if [[ -z "${!item}" ]]; then
      echo "Error: --edge-node, --api-channel-id and --dst-node are required without --config-json" >&2
      exit 1
    fi
  done
  EDGE_NODE="$EDGE_NODE" API_CHANNEL_ID="$API_CHANNEL_ID" \
  DST_NODE="$DST_NODE" SUBJECT_MODE="$SUBJECT_MODE" CALLER_EXTERNAL_ID="$CALLER_EXTERNAL_ID" \
  PUBLISH="$PUBLISH" RELAY_WINDOW_MS="$RELAY_WINDOW_MS" python3 - <<'PY'
import json, os
ingress = {"subject_mode": os.environ["SUBJECT_MODE"]}
if os.environ["SUBJECT_MODE"] == "caller_is_subject":
    ingress["caller_identity"] = {"external_user_id": os.environ["CALLER_EXTERNAL_ID"]}
value = {
    "edge": {
        "node": os.environ["EDGE_NODE"],
        "publish": os.environ["PUBLISH"] == "true",
    },
    "io": {
        "api_channel_id": os.environ["API_CHANNEL_ID"],
        "dst_node": os.environ["DST_NODE"],
        "relay": {
            "window_ms": int(os.environ["RELAY_WINDOW_MS"]),
            "max_open_sessions": 10000,
            "max_fragments_per_session": 8,
            "max_bytes_per_session": 262144,
        },
    },
    "ingress": ingress,
}
print(json.dumps(value, separators=(",", ":")))
PY
}

derive_publish() {
  RUNTIME_CONFIG_JSON="$1" python3 - <<'PY'
import json, os
value = json.loads(os.environ["RUNTIME_CONFIG_JSON"])
print("true" if value.get("edge", {}).get("publish", True) else "false")
PY
}

publication_field() {
  RAW_JSON="$1" FIELD="$2" python3 - <<'PY'
import json, os
try:
    value = json.loads(os.environ.get("RAW_JSON", ""))
except Exception:
    raise SystemExit
field = os.environ["FIELD"]
def find(v):
    if isinstance(v, dict):
        publication = v.get("publication")
        if isinstance(publication, dict) and field in publication:
            return publication[field]
        for child in v.values():
            result = find(child)
            if result is not None:
                return result
    elif isinstance(v, list):
        for child in v:
            result = find(child)
            if result is not None:
                return result
    return None
result = find(value)
if isinstance(result, bool):
    print("true" if result else "false")
elif result is not None:
    print(result)
PY
}

derive_unit_name() {
  local base="${1%@*}"
  local hive="${1##*@}"
  echo "fluxbee-node-${base}-${hive}.service"
}

restart_existing() {
  local unit
  unit="$(derive_unit_name "$NODE_NAME")"
  local cmd=(systemctl)
  [[ "$USE_SUDO" == "1" ]] && cmd=(sudo systemctl)
  if "${cmd[@]}" cat "$unit" >/dev/null 2>&1; then
    log "restart unit=$unit"
    "${cmd[@]}" restart "$unit"
    return 0
  fi
  log "restart skipped; unit not found: $unit"
  return 1
}

log "publish runtime=$RUNTIME version=$VERSION"
publish_cmd=(bash "$PUBLISH_SCRIPT" --version "$VERSION" --runtime "$RUNTIME" --set-current)
[[ "$USE_SUDO" == "1" ]] && publish_cmd+=(--sudo)
[[ "$SKIP_BUILD" == "1" ]] && publish_cmd+=(--skip-build)
[[ -n "$DIST_ROOT" ]] && publish_cmd+=(--dist-root "$DIST_ROOT")
publish_out="$("${publish_cmd[@]}" 2>&1 | tee -a "$LOG_FILE")"
manifest_version="$(echo "$publish_out" | awk -F= '/^manifest_version=/{print $2}' | tail -n1 | tr -d '[:space:]')"
manifest_hash="$(echo "$publish_out" | awk -F= '/^manifest_hash=/{print $2}' | tail -n1 | tr -d '[:space:]')"
[[ -n "$manifest_version" && -n "$manifest_hash" ]] || { log "publish manifest parse failed"; exit 1; }

if [[ "$UPDATE_SCOPE" == "targeted" ]]; then
  update_payload="$(MANIFEST_VERSION="$manifest_version" MANIFEST_HASH="$manifest_hash" RUNTIME="$RUNTIME" VERSION="$VERSION" python3 - <<'PY'
import json, os
print(json.dumps({"category":"runtime", "manifest_version":int(os.environ["MANIFEST_VERSION"]),
 "manifest_hash":os.environ["MANIFEST_HASH"], "runtime":os.environ["RUNTIME"],
 "runtime_version":os.environ["VERSION"]}, separators=(",", ":")))
PY
)"
else
  update_payload="$(MANIFEST_VERSION="$manifest_version" MANIFEST_HASH="$manifest_hash" python3 - <<'PY'
import json, os
print(json.dumps({"category":"runtime", "manifest_version":int(os.environ["MANIFEST_VERSION"]),
 "manifest_hash":os.environ["MANIFEST_HASH"]}, separators=(",", ":")))
PY
)"
fi

update_status=""
for attempt in $(seq 1 "$UPDATE_RETRIES"); do
  if [[ "$USE_SYNC_HINT" == "1" ]]; then
    curl -sS -X POST "$BASE/hives/$HIVE_ID/sync-hint" -H 'Content-Type: application/json' \
      -d '{"channel":"dist","wait_for_idle":true,"timeout_ms":30000}' >>"$LOG_FILE"
  fi
  response="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/update" -H 'Content-Type: application/json' -d "$update_payload" | tee -a "$LOG_FILE")"
  update_status="$(json_status "$response")"
  [[ "$update_status" == "ok" ]] && break
  [[ "$update_status" == "sync_pending" ]] || { log "runtime update failed status=$update_status"; exit 1; }
  sleep "$RETRY_DELAY_S"
done
if [[ "$update_status" != "ok" && "$ALLOW_SYNC_PENDING" != "1" ]]; then
  log "runtime update remains $update_status"
  exit 1
fi

if [[ -z "$NODE_NAME" || "$SKIP_SPAWN" == "1" ]]; then
  log "deploy complete: runtime published and update requested"
  exit 0
fi

runtime_config="$(build_runtime_config)"
desired_publish="$(derive_publish "$runtime_config")"

if [[ "$UPDATE_EXISTING" == "1" ]]; then
  restart_existing || { log "existing IO.api unit not found"; exit 1; }
elif [[ "$DO_SPAWN" == "1" ]]; then
  [[ -n "$TENANT_ID" ]] || { log "new spawn requires --tenant-id"; exit 1; }
  spawn_payload="$(NODE_NAME="$NODE_NAME" TENANT_ID="$TENANT_ID" RUNTIME="$RUNTIME" RUNTIME_VERSION="$RUNTIME_VERSION" RUNTIME_CONFIG_JSON="$runtime_config" python3 - <<'PY'
import json, os
print(json.dumps({
  "node_name":os.environ["NODE_NAME"], "tenant_id":os.environ["TENANT_ID"],
  "runtime":os.environ["RUNTIME"], "runtime_version":os.environ["RUNTIME_VERSION"],
  "config":json.loads(os.environ["RUNTIME_CONFIG_JSON"])
}, separators=(",", ":")))
PY
)"
  if [[ "$KILL_FIRST" == "1" ]]; then
    curl -sS -X DELETE "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME" >>"$LOG_FILE" || true
  fi
  response="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes" -H 'Content-Type: application/json' -d "$spawn_payload" | tee -a "$LOG_FILE")"
  [[ "$(json_status "$response")" == "ok" ]] || { log "spawn failed"; exit 1; }
fi

entry_token=""
if [[ "$SKIP_CONFIG_SET" != "1" ]]; then
  config_version="$(python3 - <<'PY'
import time
print(int(time.time() * 1000))
PY
)"
  config_payload="$(NODE_NAME="$NODE_NAME" CONFIG_VERSION="$config_version" RUNTIME_CONFIG_JSON="$runtime_config" python3 - <<'PY'
import json, os
print(json.dumps({"requested_by":"deploy-io-api.sh", "schema_version":1,
 "config_version":int(os.environ["CONFIG_VERSION"]), "apply_mode":"replace",
 "config":json.loads(os.environ["RUNTIME_CONFIG_JSON"])}, separators=(",", ":")))
PY
)"
  response="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-set" -H 'Content-Type: application/json' -d "$config_payload")"
  [[ "$(json_status "$response")" == "ok" ]] || { log "CONFIG_SET failed"; exit 1; }
  entry_token="$(publication_field "$response" entry_token)"
  log "CONFIG_SET applied; credential_issued=$([[ -n "$entry_token" ]] && echo true || echo false)"
fi

expected="published"
[[ "$desired_publish" == "false" ]] && expected="disabled"
for attempt in $(seq 1 30); do
  response="$(curl -sS -X POST "$BASE/hives/$HIVE_ID/nodes/$NODE_NAME/control/config-get" -H 'Content-Type: application/json' -d '{}' | tee -a "$LOG_FILE")"
  publication="$(publication_field "$response" status)"
  if [[ "$publication" == "$expected" ]]; then
    publication_url="$(publication_field "$response" url)"
    log "deploy complete node=$NODE_NAME publication=$publication url=$publication_url"
    if [[ -n "$entry_token" ]]; then
      printf 'entry_token=%s\n' "$entry_token"
      printf 'entry_token_one_time=true\n'
    else
      log "entry token was not reissued; retain the credential from the original externalize"
    fi
    exit 0
  fi
  sleep 1
done

log "publication did not reach expected state=$expected"
exit 1
