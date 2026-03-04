#!/usr/bin/env bash
set -euo pipefail

CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee/ai-nodes}"
UNIT_PREFIX="${UNIT_PREFIX:-fluxbee-ai-node@}"

usage() {
  cat <<'EOF'
Usage:
  ai-nodectl.sh list
  ai-nodectl.sh add <name> <yaml_path>
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

