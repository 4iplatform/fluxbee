#!/usr/bin/env bash
set -euo pipefail

if ! command -v systemctl >/dev/null 2>&1; then
  echo "Error: systemctl not found." >&2
  exit 1
fi

SUDO=""
if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
  SUDO="sudo"
  $SUDO -v
fi

services=(
  "sy-orchestrator"
  "sy-identity"
  "sy-admin"
  "sy-storage"
  "sy-config-routes"
  "sy-opa-rules"
  "rt-gateway"
)

service_exists() {
  local name="$1"
  systemctl list-unit-files --type=service --no-legend 2>/dev/null | awk '{print $1}' | grep -qx "${name}.service"
}

echo "Stopping Fluxbee services..."
for svc in "${services[@]}"; do
  if service_exists "$svc"; then
    echo " - stopping ${svc}.service"
    $SUDO systemctl stop "$svc" || true
  fi
done

echo "Current service states:"
for svc in "${services[@]}"; do
  if service_exists "$svc"; then
    state="$($SUDO systemctl is-active "$svc" 2>/dev/null || true)"
    echo " - ${svc}: ${state:-unknown}"
  fi
done
