#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

CONFIG_ROOT="${CONFIG_DIR:-/etc/fluxbee}"
AI_CONFIG_DIR="${AI_CONFIG_DIR:-$CONFIG_ROOT/ai-nodes}"
STATE_ROOT="${STATE_DIR:-/var/lib/fluxbee}"
UUID_STATE_DIR="${UUID_PERSISTENCE_DIR:-$STATE_ROOT/state/nodes}"
AI_DYNAMIC_STATE_DIR="${AI_DYNAMIC_CONFIG_DIR:-$STATE_ROOT/state/ai-nodes}"
RUN_ROOT="${RUN_DIR:-/var/run/fluxbee}"
RUN_ROUTERS_DIR="${RUN_ROUTERS_DIR:-$RUN_ROOT/routers}"

APPLY_DEV_OWNERSHIP="${APPLY_DEV_OWNERSHIP:-1}"
INSTALL_OWNER="${INSTALL_OWNER:-${SUDO_USER:-$USER}}"

SYSTEMD_UNIT_PATH="/etc/systemd/system/fluxbee-ai-node@.service"
INSTALL_BIN_DIR="/usr/bin"
RUNNER_BIN_NAME="ai-node-runner"
NODECTL_BIN_NAME="ai-nodectl"

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Set SKIP_BUILD=1 to install existing binaries." >&2
    exit 1
  fi
  echo "Building AI runner (nodes/ai/ai-generic -> fluxbee-ai-nodes/ai_node_runner)..."
  (
    cd "$ROOT_DIR"
    cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner
  )
fi

BIN_DIR="${BIN_DIR:-$ROOT_DIR/target/release}"
if [[ "${SKIP_BUILD:-}" == "1" ]]; then
  echo "SKIP_BUILD=1: installing binaries from BIN_DIR=$BIN_DIR"
fi

RUNNER_BIN_SRC="$BIN_DIR/ai_node_runner"
NODECTL_SRC="$ROOT_DIR/scripts/ai-nodectl.sh"

if [[ ! -f "$RUNNER_BIN_SRC" ]]; then
  echo "Error: missing runner binary: $RUNNER_BIN_SRC" >&2
  echo "Hint: build with 'cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner' from nodes/ai/ai-generic or set BIN_DIR." >&2
  exit 1
fi
if [[ ! -f "$NODECTL_SRC" ]]; then
  echo "Error: missing helper script: $NODECTL_SRC" >&2
  exit 1
fi

echo "Creating runtime directories..."
$SUDO install -d "$CONFIG_ROOT"
$SUDO install -d "$AI_CONFIG_DIR"
$SUDO install -d "$STATE_ROOT"
$SUDO install -d "$UUID_STATE_DIR"
$SUDO install -d "$AI_DYNAMIC_STATE_DIR"
$SUDO install -d "$RUN_ROOT"
$SUDO install -d "$RUN_ROUTERS_DIR"

echo "Installing binaries into $INSTALL_BIN_DIR..."
$SUDO install -m 0755 "$RUNNER_BIN_SRC" "$INSTALL_BIN_DIR/$RUNNER_BIN_NAME"
$SUDO install -m 0755 "$NODECTL_SRC" "$INSTALL_BIN_DIR/$NODECTL_BIN_NAME"

echo "Installing systemd template unit: $(basename "$SYSTEMD_UNIT_PATH")"
cat <<'EOF' | $SUDO tee "$SYSTEMD_UNIT_PATH" >/dev/null
[Unit]
Description=Fluxbee AI Node (%i)
After=network-online.target rt-gateway.service
Wants=network-online.target

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/ai-nodes/%i.env
Environment=AI_NODE_MODE=default
ExecStart=/usr/bin/ai-node-runner --mode ${AI_NODE_MODE} --config /etc/fluxbee/ai-nodes/%i.yaml
Restart=always
RestartSec=5
NoNewPrivileges=yes

[Install]
WantedBy=multi-user.target
EOF

$SUDO systemctl daemon-reload

if [[ "$APPLY_DEV_OWNERSHIP" == "1" ]]; then
  echo "Applying dev ownership to $INSTALL_OWNER ..."
  $SUDO chown -R "$INSTALL_OWNER":"$INSTALL_OWNER" \
    "$AI_CONFIG_DIR" \
    "$UUID_STATE_DIR" \
    "$AI_DYNAMIC_STATE_DIR" \
    "$RUN_ROUTERS_DIR"
fi

cat <<EOF
AI runtime install complete.

Installed:
  - $INSTALL_BIN_DIR/$RUNNER_BIN_NAME
  - $INSTALL_BIN_DIR/$NODECTL_BIN_NAME
  - $SYSTEMD_UNIT_PATH

Expected paths:
  - Per-node YAML config: $AI_CONFIG_DIR/<name>.yaml
  - Per-node env (optional): $AI_CONFIG_DIR/<name>.env
  - Dynamic node state: $AI_DYNAMIC_STATE_DIR/<name>.json
  - UUID state (node client): $UUID_STATE_DIR

Examples:
  ai-nodectl add ai-chat /tmp/ai_chat.yaml
  sudo systemctl enable --now fluxbee-ai-node@ai-chat
  ai-nodectl status ai-chat
EOF
