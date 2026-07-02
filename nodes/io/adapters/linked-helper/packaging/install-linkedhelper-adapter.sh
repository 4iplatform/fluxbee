#!/usr/bin/env bash
# Installs the Fluxbee LinkedHelper adapter as a systemd service on Linux.
# Idempotent: re-running updates the binary + unit and (re)starts the service;
# enrollment is skipped if local state already exists.
#
# The binary is installed into a directory OWNED BY THE SERVICE USER so the
# self-update can swap it in place (rename dance) without root.
#
# Usage:
#   sudo ./install-linkedhelper-adapter.sh --cloud <url> --token <enroll-token> \
#        [--binary <path>] [--partitions-root <path>] [--interval <secs>]
set -euo pipefail

SERVICE_USER=fluxbee-lh
BIN_DIR=/opt/fluxbee/lh-adapter
STATE_DIR=/var/lib/fluxbee/lh-adapter
UNIT=/etc/systemd/system/fluxbee-lh-adapter.service
BIN="$BIN_DIR/adapter-rs"
STATE="$STATE_DIR/state.json"

CLOUD=""; TOKEN=""; SRC_BINARY=""; PARTITIONS_ROOT=""; INTERVAL=60; NO_SERVICE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --cloud) CLOUD="$2"; shift 2;;
    --token) TOKEN="$2"; shift 2;;
    --binary) SRC_BINARY="$2"; shift 2;;
    --partitions-root) PARTITIONS_ROOT="$2"; shift 2;;
    --interval) INTERVAL="$2"; shift 2;;
    --no-service) NO_SERVICE=1; shift;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

[ "$(id -u)" -eq 0 ] || { echo "must run as root (sudo)" >&2; exit 1; }
CLOUD="${CLOUD%/}"

# Locate the adapter binary if not given explicitly: first a local build (dev),
# then download it from the cloud using the enrollment token (the UI flow).
if [ -z "$SRC_BINARY" ]; then
  HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd 2>/dev/null || echo /tmp)"
  for cand in "$HERE/adapter-rs" \
              "$HERE/../adapter-rs/target/release/adapter-rs" \
              "$HERE/../adapter-rs/target/debug/adapter-rs"; do
    [ -x "$cand" ] && SRC_BINARY="$cand" && break
  done
fi
if [ -z "$SRC_BINARY" ] && [ -n "$CLOUD" ] && [ -n "$TOKEN" ]; then
  arch="$(uname -m)"; case "$arch" in x86_64) arch=x64;; aarch64|arm64) arch=arm64;; esac
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  dl="$CLOUD/api/adapters/linkedhelper/download?os=$os&arch=$arch"
  SRC_BINARY="$(mktemp)"
  echo "downloading adapter binary: $dl"
  curl -fsSL -H "Authorization: Bearer $TOKEN" "$dl" -o "$SRC_BINARY" || {
    echo "adapter binary download failed" >&2; exit 1; }
  chmod +x "$SRC_BINARY"
fi
[ -n "$SRC_BINARY" ] && [ -x "$SRC_BINARY" ] || {
  echo "adapter binary not found; pass --binary <path> or --cloud + --token to download" >&2; exit 1; }

# System user + directories (binary dir owned by the service user for self-update).
id "$SERVICE_USER" >/dev/null 2>&1 || \
  useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
install -d -o "$SERVICE_USER" -g "$SERVICE_USER" "$BIN_DIR" "$STATE_DIR"
install -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0755 "$SRC_BINARY" "$BIN"

# Enroll once (as root, then hand ownership to the service user). Skipped when
# state already exists (idempotent re-install/upgrade).
if [ -f "$STATE" ]; then
  echo "state exists, skipping enroll: $STATE"
else
  [ -n "$CLOUD" ] && [ -n "$TOKEN" ] || {
    echo "first install needs --cloud <url> and --token <enroll-token>" >&2; exit 1; }
  "$BIN" --state-file "$STATE" enroll --cloud "$CLOUD" --token "$TOKEN"
fi
chown -R "$SERVICE_USER:$SERVICE_USER" "$STATE_DIR" "$BIN_DIR" 2>/dev/null || true

# systemd unit. No ProtectSystem/ReadOnlyPaths over $BIN_DIR so the self-update
# can replace the binary; Restart=always drives the supervised update restart.
# Skipped when systemd is unavailable (e.g. containers) or --no-service is set;
# the binary + enrollment are still in place.
if [ "$NO_SERVICE" -eq 1 ] || ! command -v systemctl >/dev/null 2>&1; then
  echo "skipping systemd service setup (no-service or systemctl unavailable)"
  echo "installed: $BIN (enrolled, no service)"
else
  PART_ARG=""; [ -n "$PARTITIONS_ROOT" ] && PART_ARG="--partitions-root $PARTITIONS_ROOT"
  cat > "$UNIT" <<EOF
[Unit]
Description=Fluxbee LinkedHelper adapter
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
Group=$SERVICE_USER
ExecStart=$BIN --state-file $STATE run --interval-seconds $INTERVAL $PART_ARG
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

  systemctl daemon-reload
  systemctl enable --now fluxbee-lh-adapter.service
  systemctl --no-pager status fluxbee-lh-adapter.service || true
  echo "installed: $BIN (service: fluxbee-lh-adapter)"
fi
