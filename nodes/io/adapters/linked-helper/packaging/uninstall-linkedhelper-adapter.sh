#!/usr/bin/env bash
# Removes the Fluxbee LinkedHelper adapter systemd service and binary.
# By default the state dir + service user are kept; --purge removes them too
# (a clean slate — important so install/uninstall residue can't hide bugs).
#
# Usage: sudo ./uninstall-linkedhelper-adapter.sh [--purge]
set -euo pipefail

SERVICE_USER=fluxbee-lh
BIN_DIR=/opt/fluxbee/lh-adapter
STATE_DIR=/var/lib/fluxbee/lh-adapter
UNIT=/etc/systemd/system/fluxbee-lh-adapter.service

PURGE=0
[ "${1:-}" = "--purge" ] && PURGE=1
[ "$(id -u)" -eq 0 ] || { echo "must run as root (sudo)" >&2; exit 1; }

systemctl disable --now fluxbee-lh-adapter.service 2>/dev/null || true
rm -f "$UNIT"
systemctl daemon-reload
rm -rf "$BIN_DIR"

if [ "$PURGE" -eq 1 ]; then
  rm -rf "$STATE_DIR"
  userdel "$SERVICE_USER" 2>/dev/null || true
  echo "purged state dir + service user"
fi
echo "uninstalled fluxbee-lh-adapter"
