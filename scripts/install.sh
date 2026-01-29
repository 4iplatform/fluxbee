#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

sudo install -d /etc/json-router
sudo install -d /var/lib/json-router
sudo install -d /var/lib/json-router/state/nodes
sudo install -d /var/lib/json-router/opa
sudo install -d /var/lib/json-router/opa/current
sudo install -d /var/lib/json-router/opa/staged
sudo install -d /var/lib/json-router/opa/backup
sudo install -d /var/lib/json-router/modules
sudo install -d /var/lib/json-router/blob
sudo install -d /var/run/json-router
sudo install -d /var/run/json-router/routers

if [[ -f "$ROOT_DIR/config/island.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/island.yaml" /etc/json-router/island.yaml
fi

if [[ -f "$ROOT_DIR/config/sy-config-routes.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/sy-config-routes.yaml" /etc/json-router/sy-config-routes.yaml
fi

echo "Installed config to /etc/json-router and created runtime directories."
