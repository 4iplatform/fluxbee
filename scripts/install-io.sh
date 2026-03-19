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

  echo "Building IO binaries (io-slack, io-sim)..."
  (cd "$ROOT_DIR" && cargo build --release --manifest-path io/Cargo.toml -p io-slack -p io-sim)
fi

BIN_DIR="${BIN_DIR:-$ROOT_DIR/io/target/release}"
if [[ "${SKIP_BUILD:-}" == "1" ]]; then
  echo "SKIP_BUILD=1: installing only binaries from $BIN_DIR" >&2
fi

SLACK_BIN="$BIN_DIR/io-slack"
SIM_BIN="$BIN_DIR/io-sim"

missing=0
if [[ ! -f "$SLACK_BIN" ]]; then
  echo "Missing binary: $SLACK_BIN" >&2
  missing=1
fi
if [[ ! -f "$SIM_BIN" ]]; then
  echo "Missing binary: $SIM_BIN" >&2
  missing=1
fi
if [[ "$missing" -eq 1 ]]; then
  echo "Build them first (e.g. cargo build --release --manifest-path io/Cargo.toml -p io-slack -p io-sim) or set BIN_DIR." >&2
  exit 1
fi

$SUDO install -d "$CONFIG_DIR"
$SUDO install -d "$STATE_DIR"
$SUDO install -d "$STATE_DIR/state/nodes"
$SUDO install -d "$RUN_DIR"
$SUDO install -d "$RUN_DIR/routers"

$SUDO install -m 0755 "$SLACK_BIN" /usr/bin/io-slack
$SUDO install -m 0755 "$SIM_BIN" /usr/bin/io-sim

if [[ ! -f "$CONFIG_DIR/io-slack.env" ]]; then
  cat <<'EOF' | $SUDO tee "$CONFIG_DIR/io-slack.env" >/dev/null
# Required for io-slack
SLACK_APP_TOKEN=xapp-REPLACE_ME
SLACK_BOT_TOKEN=xoxb-REPLACE_ME

# Optional runtime config
# NODE_NAME=IO.slack.T123
# NODE_VERSION=0.1
# ROUTER_SOCKET=/var/run/fluxbee/routers
# UUID_PERSISTENCE_DIR=/var/lib/fluxbee/state/nodes
# CONFIG_DIR=/etc/fluxbee
# ISLAND_ID=motherbee
# IDENTITY_TARGET=SY.identity@motherbee
# IDENTITY_TIMEOUT_MS=10000
# IO_SLACK_DST_NODE=AI.chat@sandbox
# DEV_MODE=true
# RUST_LOG=info,io_slack=debug,fluxbee_sdk=info
EOF
fi

if [[ ! -f "$CONFIG_DIR/io-sim.env" ]]; then
  cat <<'EOF' | $SUDO tee "$CONFIG_DIR/io-sim.env" >/dev/null
# Optional runtime config for io-sim
# NODE_NAME=IO.sim.local
# NODE_VERSION=0.1
# ROUTER_SOCKET=/var/run/fluxbee/routers
# UUID_PERSISTENCE_DIR=/var/lib/fluxbee/state/nodes
# CONFIG_DIR=/etc/fluxbee
# ISLAND_ID=motherbee
# IDENTITY_TARGET=SY.identity@motherbee
# IDENTITY_TIMEOUT_MS=10000
# SIM_DST_NODE=AI.chat@sandbox
# RUST_LOG=info,io_sim=debug,fluxbee_sdk=info
EOF
fi

echo "Installing systemd units: fluxbee-io-slack.service, fluxbee-io-sim.service"
cat <<'EOF' | $SUDO tee /etc/systemd/system/fluxbee-io-slack.service >/dev/null
[Unit]
Description=Fluxbee IO Slack Node
After=network.target rt-gateway.service

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/io-slack.env
ExecStart=/usr/bin/io-slack
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

cat <<'EOF' | $SUDO tee /etc/systemd/system/fluxbee-io-sim.service >/dev/null
[Unit]
Description=Fluxbee IO Sim Node
After=network.target rt-gateway.service

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/io-sim.env
ExecStart=/usr/bin/io-sim
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

$SUDO systemctl daemon-reload

if [[ "$APPLY_DEV_OWNERSHIP" == "1" ]]; then
  echo "Applying ownership for test/dev user: $INSTALL_OWNER"
  $SUDO chown "$INSTALL_OWNER":"$INSTALL_OWNER" "$CONFIG_DIR/io-slack.env" "$CONFIG_DIR/io-sim.env" || true
  $SUDO chown -R "$INSTALL_OWNER":"$INSTALL_OWNER" "$STATE_DIR/state/nodes" "$RUN_DIR/routers"
fi

echo "Installed: /usr/bin/io-slack, /usr/bin/io-sim"
echo "Units: /etc/systemd/system/fluxbee-io-slack.service, /etc/systemd/system/fluxbee-io-sim.service"
echo "Env files: $CONFIG_DIR/io-slack.env, $CONFIG_DIR/io-sim.env"
echo "Start examples:"
echo "  sudo systemctl enable --now fluxbee-io-slack"
echo "  sudo systemctl enable --now fluxbee-io-sim"
