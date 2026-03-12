#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG_DIR="${CONFIG_DIR:-/etc/fluxbee}"
STATE_DIR="${STATE_DIR:-/var/lib/fluxbee}"
RUN_DIR="${RUN_DIR:-/var/run/fluxbee}"
APPLY_DEV_OWNERSHIP="${APPLY_DEV_OWNERSHIP:-1}"
INSTALL_OWNER="${INSTALL_OWNER:-${SUDO_USER:-$USER}}"

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Set SKIP_BUILD=1 if binaries are already built." >&2
    exit 1
  fi

  echo "Building AI binary (ai_node_runner)..."
  (cd "$ROOT_DIR" && cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner)
fi

BIN_DIR="${BIN_DIR:-$ROOT_DIR/target/release}"
if [[ "${SKIP_BUILD:-}" == "1" ]]; then
  echo "SKIP_BUILD=1: installing only binaries from $BIN_DIR" >&2
fi

AI_BIN="$BIN_DIR/ai_node_runner"
NODECTL_SRC="$ROOT_DIR/scripts/ai-nodectl.sh"
if [[ ! -f "$AI_BIN" ]]; then
  echo "Missing binary: $AI_BIN" >&2
  echo "Build it first (e.g. cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner) or set BIN_DIR." >&2
  exit 1
fi
if [[ ! -f "$NODECTL_SRC" ]]; then
  echo "Missing script: $NODECTL_SRC" >&2
  exit 1
fi

$SUDO install -d "$CONFIG_DIR"
$SUDO install -d "$CONFIG_DIR/ai-nodes"
$SUDO install -d "$STATE_DIR"
$SUDO install -d "$STATE_DIR/state/nodes"
$SUDO install -d "$RUN_DIR"
$SUDO install -d "$RUN_DIR/routers"

$SUDO install -m 0755 "$AI_BIN" /usr/bin/ai-node-runner
$SUDO install -m 0755 "$NODECTL_SRC" /usr/bin/ai-nodectl

echo "Installing systemd template unit: fluxbee-ai-node@.service"
cat <<'EOF' | $SUDO tee /etc/systemd/system/fluxbee-ai-node@.service >/dev/null
[Unit]
Description=Fluxbee AI Node (%i)
After=network.target rt-gateway.service

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/ai-nodes/%i.env
ExecStart=/usr/bin/ai-node-runner --config /etc/fluxbee/ai-nodes/%i.yaml
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

$SUDO systemctl daemon-reload

if [[ "$APPLY_DEV_OWNERSHIP" == "1" ]]; then
  echo "Applying ownership for test/dev user: $INSTALL_OWNER"
  $SUDO chown -R "$INSTALL_OWNER":"$INSTALL_OWNER" "$CONFIG_DIR/ai-nodes" "$STATE_DIR/state/nodes" "$RUN_DIR/routers"
fi

echo "Installed: /usr/bin/ai-node-runner, /usr/bin/ai-nodectl"
echo "Template unit: /etc/systemd/system/fluxbee-ai-node@.service"
echo "Config location: $CONFIG_DIR/ai-nodes/<name>.yaml"
echo "Dynamic state location: $STATE_DIR/state/ai-nodes/<name>.json"
echo "Precreate state example: ai-nodectl init-state <name> /tmp/<name>_effective_config.json"
echo "Start example: sudo systemctl enable --now fluxbee-ai-node@ai-chat"
echo "Management example: ai-nodectl list"
