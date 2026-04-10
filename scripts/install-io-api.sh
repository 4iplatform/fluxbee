#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee}"
STATE_DIR="${STATE_DIR:-/var/lib/fluxbee}"
RUN_DIR="${RUN_DIR:-/var/run/fluxbee}"
MANAGED_ROOT="${MANAGED_ROOT:-/var/lib/fluxbee/nodes}"
APPLY_DEV_OWNERSHIP="${APPLY_DEV_OWNERSHIP:-1}"
INSTALL_OWNER="${INSTALL_OWNER:-${SUDO_USER:-$USER}}"

usage() {
  cat <<'EOF'
Usage:
  install-io-api.sh [options]

Options:
  --node-name <name@hive>      Managed node name (default: IO.api.local@motherbee)
  --listen-address <addr>      Listen address for bootstrap config (default: 127.0.0.1)
  --listen-port <port>         Listen port for bootstrap config (default: 18080)
  --tenant-id <id>             Optional tenant_id for bootstrap config
  --dst-node <name>            Optional io.dst_node bootstrap value
  --identity-target <name>     Optional identity target bootstrap value
  --identity-timeout-ms <ms>   Optional identity timeout bootstrap value
  --blob-root <path>           Optional blob.path bootstrap value
  --bin-dir <path>             Built binaries directory (default: nodes/io/target/release)
  --rust-log <value>           RUST_LOG for env file
  --skip-build                 Do not run cargo build
  -h, --help                   Show help
EOF
}

NODE_NAME="IO.api.local@motherbee"
LISTEN_ADDRESS="127.0.0.1"
LISTEN_PORT="18080"
TENANT_ID=""
DST_NODE=""
IDENTITY_TARGET=""
IDENTITY_TIMEOUT_MS=""
BLOB_ROOT=""
BIN_DIR="$ROOT_DIR/nodes/io/target/release"
RUST_LOG_VALUE="info,io_api=debug,io_common=debug,fluxbee_sdk=info"
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --node-name) NODE_NAME="${2:-}"; shift 2 ;;
    --listen-address) LISTEN_ADDRESS="${2:-}"; shift 2 ;;
    --listen-port) LISTEN_PORT="${2:-}"; shift 2 ;;
    --tenant-id) TENANT_ID="${2:-}"; shift 2 ;;
    --dst-node) DST_NODE="${2:-}"; shift 2 ;;
    --identity-target) IDENTITY_TARGET="${2:-}"; shift 2 ;;
    --identity-timeout-ms) IDENTITY_TIMEOUT_MS="${2:-}"; shift 2 ;;
    --blob-root) BLOB_ROOT="${2:-}"; shift 2 ;;
    --bin-dir) BIN_DIR="${2:-}"; shift 2 ;;
    --rust-log) RUST_LOG_VALUE="${2:-}"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Error: unknown option: $1" >&2; usage; exit 1 ;;
  esac
done

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

if [[ "$SKIP_BUILD" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Set --skip-build if binary is already built." >&2
    exit 1
  fi
  echo "Building IO.api binary..."
  (cd "$ROOT_DIR" && cargo build --release --manifest-path nodes/io/Cargo.toml -p io-api)
fi

IO_API_BIN="$BIN_DIR/io-api"
if [[ ! -f "$IO_API_BIN" ]]; then
  echo "Missing binary: $IO_API_BIN" >&2
  echo "Build it first or set --bin-dir." >&2
  exit 1
fi

local_name="${NODE_NAME%@*}"
hive_name="${NODE_NAME##*@}"
kind_prefix="${local_name%%.*}"
if [[ -z "$local_name" || -z "$hive_name" || "$local_name" == "$NODE_NAME" || -z "$kind_prefix" ]]; then
  echo "Error: --node-name must look like <name>@<hive> (got '$NODE_NAME')" >&2
  exit 1
fi

MANAGED_NODE_DIR="$MANAGED_ROOT/$kind_prefix/$NODE_NAME"
MANAGED_CONFIG_PATH="$MANAGED_NODE_DIR/config.json"
ENV_PATH="$CONFIG_DIR/io-api.env"
UNIT_PATH="/etc/systemd/system/fluxbee-io-api.service"

$SUDO install -d "$CONFIG_DIR"
$SUDO install -d "$STATE_DIR"
$SUDO install -d "$STATE_DIR/state/nodes"
$SUDO install -d "$RUN_DIR"
$SUDO install -d "$RUN_DIR/routers"
$SUDO install -d "$MANAGED_NODE_DIR"

$SUDO install -m 0755 "$IO_API_BIN" /usr/bin/io-api

tmp_cfg="$(mktemp)"
python3 - <<PY >"$tmp_cfg"
import json

cfg = {
    "listen": {
        "address": "${LISTEN_ADDRESS}",
        "port": int("${LISTEN_PORT}")
    }
}
if "${TENANT_ID}":
    cfg["tenant_id"] = "${TENANT_ID}"
if "${DST_NODE}":
    cfg.setdefault("io", {})["dst_node"] = "${DST_NODE}"
if "${BLOB_ROOT}":
    cfg.setdefault("blob", {})["path"] = "${BLOB_ROOT}"
if "${IDENTITY_TARGET}":
    cfg["identity_target"] = "${IDENTITY_TARGET}"
if "${IDENTITY_TIMEOUT_MS}":
    cfg["identity_timeout_ms"] = int("${IDENTITY_TIMEOUT_MS}")

print(json.dumps(cfg, indent=2, sort_keys=True))
PY
$SUDO install -m 0644 "$tmp_cfg" "$MANAGED_CONFIG_PATH"
rm -f "$tmp_cfg"

tmp_env="$(mktemp)"
cat >"$tmp_env" <<EOF
# Required for managed local IO.api
FLUXBEE_NODE_NAME=$NODE_NAME

# Optional runtime config
ROUTER_SOCKET=/var/run/fluxbee/routers
UUID_PERSISTENCE_DIR=/var/lib/fluxbee/state/nodes
CONFIG_DIR=$CONFIG_DIR
STATE_DIR=$STATE_DIR
RUST_LOG=$RUST_LOG_VALUE

# Optional overrides
# NODE_VERSION=0.1.0
# BLOB_ROOT=/var/lib/fluxbee/blob
EOF
$SUDO install -m 0644 "$tmp_env" "$ENV_PATH"
rm -f "$tmp_env"

cat <<'EOF' | $SUDO tee "$UNIT_PATH" >/dev/null
[Unit]
Description=Fluxbee IO API Node
After=network.target rt-gateway.service

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/io-api.env
ExecStart=/usr/bin/io-api
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

$SUDO systemctl daemon-reload

if [[ "$APPLY_DEV_OWNERSHIP" == "1" ]]; then
  echo "Applying ownership for test/dev user: $INSTALL_OWNER"
  $SUDO chown "$INSTALL_OWNER":"$INSTALL_OWNER" "$ENV_PATH" || true
  $SUDO chown -R "$INSTALL_OWNER":"$INSTALL_OWNER" "$MANAGED_NODE_DIR" "$STATE_DIR/state/nodes" "$RUN_DIR/routers" || true
fi

echo "Installed: /usr/bin/io-api"
echo "Unit: $UNIT_PATH"
echo "Env file: $ENV_PATH"
echo "Managed config: $MANAGED_CONFIG_PATH"
echo "Start examples:"
echo "  sudo systemctl enable --now fluxbee-io-api"
echo "  sudo journalctl -u fluxbee-io-api -f"
