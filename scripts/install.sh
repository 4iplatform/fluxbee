#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Set SKIP_BUILD=1 if binaries are already built." >&2
    exit 1
  fi

  echo "Building Rust binaries..."
  cargo build --release --bin json-router --bin sy-admin --bin sy-config-routes --bin sy-orchestrator
fi

if [[ -d "$ROOT_DIR/sy-opa-rules" ]]; then
  if [[ "${SKIP_BUILD:-}" == "1" ]]; then
    echo "SKIP_BUILD=1 set; skipping sy-opa-rules build."
  elif ! command -v go >/dev/null 2>&1; then
    echo "Warning: go not found. Skipping sy-opa-rules build." >&2
  else
    echo "Building sy-opa-rules (Go)..."
    (cd "$ROOT_DIR/sy-opa-rules" && go build -o sy-opa-rules .)
  fi
fi

sudo install -d /etc/json-router
sudo install -d /var/lib/json-router
sudo install -d /var/lib/json-router/state/nodes
sudo install -d /var/lib/json-router/islands
sudo install -d /var/lib/json-router/opa
sudo install -d /var/lib/json-router/opa/current
sudo install -d /var/lib/json-router/opa/staged
sudo install -d /var/lib/json-router/opa/backup
sudo install -d /var/lib/json-router/modules
sudo install -d /var/lib/json-router/blob
sudo install -d /var/run/json-router
sudo install -d /var/run/json-router/routers

echo "Installing binaries to /usr/bin..."
sudo install -m 0755 "$ROOT_DIR/target/release/json-router" /usr/bin/rt-gateway
sudo install -m 0755 "$ROOT_DIR/target/release/sy-admin" /usr/bin/sy-admin
sudo install -m 0755 "$ROOT_DIR/target/release/sy-config-routes" /usr/bin/sy-config-routes
sudo install -m 0755 "$ROOT_DIR/target/release/sy-orchestrator" /usr/bin/sy-orchestrator
if [[ -f "$ROOT_DIR/sy-opa-rules/sy-opa-rules" ]]; then
  sudo install -m 0755 "$ROOT_DIR/sy-opa-rules/sy-opa-rules" /usr/bin/sy-opa-rules
fi

if [[ -f "$ROOT_DIR/config/island.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/island.yaml" /etc/json-router/island.yaml
fi

if [[ -f "$ROOT_DIR/config/sy-config-routes.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/sy-config-routes.yaml" /etc/json-router/sy-config-routes.yaml
fi

echo "Installed config to /etc/json-router, binaries to /usr/bin, and created runtime directories."
