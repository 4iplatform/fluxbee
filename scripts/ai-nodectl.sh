#!/usr/bin/env bash
set -euo pipefail

CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee/ai-nodes}"
DYNAMIC_CONFIG_DIR="${DYNAMIC_CONFIG_DIR:-/var/lib/fluxbee/state/ai-nodes}"
UNIT_PREFIX="${UNIT_PREFIX:-fluxbee-ai-node@}"

usage() {
  cat <<'EOF'
Usage:
  ai-nodectl.sh list
  ai-nodectl.sh add <name> <yaml_path>
  ai-nodectl.sh init-state <name> <config_json_path> [--schema-version N] [--config-version N] [--force]
  ai-nodectl.sh remove <name> [--purge-config]
  ai-nodectl.sh start <name>
  ai-nodectl.sh stop <name>
  ai-nodectl.sh restart <name>
  ai-nodectl.sh status <name>
  ai-nodectl.sh logs <name> [--follow]
  ai-nodectl.sh enable <name>
  ai-nodectl.sh disable <name>

Notes:
  - Systemd instance name: fluxbee-ai-node@<name>.service
  - Config path: /etc/fluxbee/ai-nodes/<name>.yaml
  - Dynamic state path: /var/lib/fluxbee/state/ai-nodes/<name>.json
EOF
}

if ! command -v systemctl >/dev/null 2>&1; then
  echo "Error: systemctl not found." >&2
  exit 1
fi

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
fi

unit_name() {
  local name="$1"
  echo "${UNIT_PREFIX}${name}.service"
}

require_args() {
  local expected="$1"
  local got="$2"
  if (( got < expected )); then
    usage >&2
    exit 1
  fi
}

cmd="${1:-}"
case "$cmd" in
  list)
    $SUDO systemctl list-unit-files "${UNIT_PREFIX}*"
    echo
    $SUDO systemctl list-units "${UNIT_PREFIX}*"
    ;;
  add)
    require_args 3 "$#"
    name="$2"
    src_yaml="$3"
    dst_yaml="${CONFIG_DIR}/${name}.yaml"
    if [[ ! -f "$src_yaml" ]]; then
      echo "Error: yaml not found: $src_yaml" >&2
      exit 1
    fi
    $SUDO install -d "$CONFIG_DIR"
    $SUDO install -m 0644 "$src_yaml" "$dst_yaml"
    echo "Installed config: $dst_yaml"
    echo "Next:"
    echo "  sudo systemctl enable --now $(unit_name "$name")"
    ;;
  init-state)
    require_args 3 "$#"
    name="$2"
    src_json="$3"
    schema_version="1"
    config_version="1"
    force_write="0"
    shift 3
    while (( "$#" )); do
      case "$1" in
        --schema-version)
          schema_version="${2:-}"
          shift 2
          ;;
        --config-version)
          config_version="${2:-}"
          shift 2
          ;;
        --force)
          force_write="1"
          shift 1
          ;;
        *)
          echo "Error: unknown option for init-state: $1" >&2
          exit 1
          ;;
      esac
    done

    if [[ ! "$schema_version" =~ ^[0-9]+$ ]]; then
      echo "Error: --schema-version must be an integer" >&2
      exit 1
    fi
    if [[ ! "$config_version" =~ ^[0-9]+$ ]]; then
      echo "Error: --config-version must be an integer" >&2
      exit 1
    fi
    if [[ ! -f "$src_json" ]]; then
      echo "Error: json not found: $src_json" >&2
      exit 1
    fi

    if ! command -v python3 >/dev/null 2>&1; then
      echo "Error: python3 is required for init-state." >&2
      exit 1
    fi

    dst_json="${DYNAMIC_CONFIG_DIR}/${name}.json"
    if [[ -f "$dst_json" && "$force_write" != "1" ]]; then
      echo "Error: destination exists: $dst_json (use --force to overwrite)" >&2
      exit 1
    fi

    $SUDO install -d "$DYNAMIC_CONFIG_DIR"

    tmp_json="$(mktemp)"
    trap 'rm -f "$tmp_json"' EXIT
    python3 - "$name" "$schema_version" "$config_version" "$src_json" "$tmp_json" <<'PY'
import datetime
import json
import sys

node_name = sys.argv[1]
schema_version = int(sys.argv[2])
config_version = int(sys.argv[3])
src_json = sys.argv[4]
dst_tmp = sys.argv[5]

with open(src_json, "r", encoding="utf-8") as f:
    cfg = json.load(f)
if not isinstance(cfg, dict):
    raise SystemExit("config_json_path must contain a JSON object")

state = {
    "schema_version": schema_version,
    "config_version": config_version,
    "node_name": node_name,
    "config": cfg,
    "updated_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z"),
}
with open(dst_tmp, "w", encoding="utf-8") as f:
    json.dump(state, f, indent=2, ensure_ascii=False)
    f.write("\n")
PY

    if [[ "$force_write" == "1" ]]; then
      $SUDO install -m 0644 "$tmp_json" "$dst_json"
    else
      if $SUDO test -e "$dst_json"; then
        echo "Error: destination exists: $dst_json (use --force to overwrite)" >&2
        exit 1
      fi
      $SUDO install -m 0644 "$tmp_json" "$dst_json"
    fi
    rm -f "$tmp_json"
    trap - EXIT

    echo "Created effective state file: $dst_json"
    echo "Next:"
    echo "  sudo systemctl restart $(unit_name "$name")"
    ;;
  remove)
    require_args 2 "$#"
    name="$2"
    purge_config="${3:-}"
    unit="$(unit_name "$name")"
    $SUDO systemctl disable --now "$unit" 2>/dev/null || true
    if [[ "$purge_config" == "--purge-config" ]]; then
      $SUDO rm -f "${CONFIG_DIR}/${name}.yaml"
      echo "Removed config: ${CONFIG_DIR}/${name}.yaml"
    fi
    echo "Removed service instance: $unit"
    ;;
  start|stop|restart|status|enable|disable)
    require_args 2 "$#"
    name="$2"
    unit="$(unit_name "$name")"
    $SUDO systemctl "$cmd" "$unit"
    ;;
  logs)
    require_args 2 "$#"
    name="$2"
    follow="${3:-}"
    unit="$(unit_name "$name")"
    if [[ "$follow" == "--follow" ]]; then
      $SUDO journalctl -u "$unit" -f
    else
      $SUDO journalctl -u "$unit" --no-pager -n 200
    fi
    ;;
  ""|-h|--help|help)
    usage
    ;;
  *)
    echo "Unknown command: $cmd" >&2
    usage >&2
    exit 1
    ;;
esac

