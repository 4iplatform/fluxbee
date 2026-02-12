#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PRIMARY_CONFIG_DIR="/etc/fluxbee"
PRIMARY_STATE_DIR="/var/lib/fluxbee"
PRIMARY_RUN_DIR="/var/run/fluxbee"
LEGACY_CONFIG_DIR="/etc/json-router"
LEGACY_STATE_DIR="/var/lib/json-router"
LEGACY_RUN_DIR="/var/run/json-router"

if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Set SKIP_BUILD=1 if binaries are already built." >&2
    exit 1
  fi

  echo "Building Rust binaries..."
  cargo build --release --bins
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

sudo install -d "$PRIMARY_CONFIG_DIR"
sudo install -d "$PRIMARY_STATE_DIR"
sudo install -d "$PRIMARY_STATE_DIR/state/nodes"
sudo install -d "$PRIMARY_STATE_DIR/islands"
sudo install -d "$PRIMARY_STATE_DIR/opa"
sudo install -d "$PRIMARY_STATE_DIR/opa/current"
sudo install -d "$PRIMARY_STATE_DIR/opa/staged"
sudo install -d "$PRIMARY_STATE_DIR/opa/backup"
sudo install -d "$PRIMARY_STATE_DIR/modules"
sudo install -d "$PRIMARY_STATE_DIR/blob"
sudo install -d "$PRIMARY_STATE_DIR/nats"
sudo install -d "$PRIMARY_RUN_DIR"
sudo install -d "$PRIMARY_RUN_DIR/routers"

# Compatibilidad temporal con layout legacy.
sudo install -d "$LEGACY_CONFIG_DIR"
sudo install -d "$LEGACY_STATE_DIR"
sudo install -d "$LEGACY_STATE_DIR/state/nodes"
sudo install -d "$LEGACY_STATE_DIR/islands"
sudo install -d "$LEGACY_STATE_DIR/opa"
sudo install -d "$LEGACY_STATE_DIR/opa/current"
sudo install -d "$LEGACY_STATE_DIR/opa/staged"
sudo install -d "$LEGACY_STATE_DIR/opa/backup"
sudo install -d "$LEGACY_STATE_DIR/modules"
sudo install -d "$LEGACY_STATE_DIR/blob"
sudo install -d "$LEGACY_RUN_DIR"
sudo install -d "$LEGACY_RUN_DIR/routers"

BIN_DIR="${BIN_DIR:-$ROOT_DIR/target/release}"
if [[ "${SKIP_BUILD:-}" == "1" ]]; then
  for candidate in "$ROOT_DIR/target/release" "$ROOT_DIR/target/debug" "$ROOT_DIR/bin" "$ROOT_DIR/dist"; do
    if [[ -d "$candidate" ]]; then
      BIN_DIR="$candidate"
      break
    fi
  done
fi

echo "Installing binaries to /usr/bin from $BIN_DIR..."

pick_bin() {
  local primary="$1"
  local fallback="$2"
  if [[ -f "$BIN_DIR/$primary" ]]; then
    echo "$BIN_DIR/$primary"
    return 0
  fi
  if [[ -n "$fallback" && -f "$BIN_DIR/$fallback" ]]; then
    echo "$BIN_DIR/$fallback"
    return 0
  fi
  return 1
}

missing=0
json_router_bin="$(pick_bin json-router "")" || { echo "Missing binary: $BIN_DIR/json-router" >&2; missing=1; }
sy_admin_bin="$(pick_bin sy-admin sy_admin)" || { echo "Missing binary: $BIN_DIR/sy-admin or $BIN_DIR/sy_admin" >&2; missing=1; }
sy_config_bin="$(pick_bin sy-config-routes sy_config_routes)" || { echo "Missing binary: $BIN_DIR/sy-config-routes or $BIN_DIR/sy_config_routes" >&2; missing=1; }
sy_orch_bin="$(pick_bin sy-orchestrator sy_orchestrator)" || { echo "Missing binary: $BIN_DIR/sy-orchestrator or $BIN_DIR/sy_orchestrator" >&2; missing=1; }
sy_storage_bin="$(pick_bin sy-storage sy_storage)" || { echo "Missing binary: $BIN_DIR/sy-storage or $BIN_DIR/sy_storage" >&2; missing=1; }
sy_identity_bin="$(pick_bin sy-identity sy_identity || true)"
sy_opa_rules_bin=""
if [[ -f "$ROOT_DIR/sy-opa-rules/sy-opa-rules" ]]; then
  sy_opa_rules_bin="$ROOT_DIR/sy-opa-rules/sy-opa-rules"
elif sy_opa_rules_bin="$(pick_bin sy-opa-rules sy_opa_rules || true)"; then
  :
fi
if [[ -z "${sy_opa_rules_bin:-}" ]]; then
  echo "Missing binary: $ROOT_DIR/sy-opa-rules/sy-opa-rules or $BIN_DIR/sy-opa-rules or $BIN_DIR/sy_opa_rules" >&2
  missing=1
fi

if [[ "$missing" -eq 1 ]]; then
  echo "Build them first (e.g. cargo build --release --bins) or set BIN_DIR to where they exist." >&2
  exit 1
fi

sudo install -m 0755 "$json_router_bin" /usr/bin/rt-gateway
sudo install -m 0755 "$sy_admin_bin" /usr/bin/sy-admin
sudo install -m 0755 "$sy_config_bin" /usr/bin/sy-config-routes
sudo install -m 0755 "$sy_orch_bin" /usr/bin/sy-orchestrator
sudo install -m 0755 "$sy_storage_bin" /usr/bin/sy-storage
if [[ -n "${sy_identity_bin:-}" ]]; then
  sudo install -m 0755 "$sy_identity_bin" /usr/bin/sy-identity
else
  echo "Warning: sy-identity binary not found; skipping install." >&2
fi
sudo install -m 0755 "$sy_opa_rules_bin" /usr/bin/sy-opa-rules

if [[ -f "$ROOT_DIR/config/island.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/island.yaml" "$PRIMARY_CONFIG_DIR/island.yaml"
  sudo install -m 0644 "$ROOT_DIR/config/island.yaml" "$LEGACY_CONFIG_DIR/island.yaml"
fi

if [[ -f "$ROOT_DIR/config/sy-config-routes.yaml" ]]; then
  sudo install -m 0644 "$ROOT_DIR/config/sy-config-routes.yaml" "$PRIMARY_CONFIG_DIR/sy-config-routes.yaml"
  sudo install -m 0644 "$ROOT_DIR/config/sy-config-routes.yaml" "$LEGACY_CONFIG_DIR/sy-config-routes.yaml"
else
  if [[ ! -f "$PRIMARY_CONFIG_DIR/sy-config-routes.yaml" ]]; then
    echo "Creating default $PRIMARY_CONFIG_DIR/sy-config-routes.yaml"
    cat <<'EOF' | sudo tee "$PRIMARY_CONFIG_DIR/sy-config-routes.yaml" >/dev/null
version: 1
routes: []
vpns: []
EOF
  fi
  if [[ ! -f "$LEGACY_CONFIG_DIR/sy-config-routes.yaml" ]]; then
    cat <<'EOF' | sudo tee "$LEGACY_CONFIG_DIR/sy-config-routes.yaml" >/dev/null
version: 1
routes: []
vpns: []
EOF
  fi
fi

install_unit() {
  local name="$1"
  local exec="$2"
  local path="/etc/systemd/system/${name}.service"
  cat <<EOF | sudo tee "$path" >/dev/null
[Unit]
Description=Fluxbee ${name}
After=network.target

[Service]
Type=simple
ExecStart=${exec}
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
}

echo "Installing systemd units..."
install_unit "rt-gateway" "/usr/bin/rt-gateway"
install_unit "sy-config-routes" "/usr/bin/sy-config-routes"
install_unit "sy-opa-rules" "/usr/bin/sy-opa-rules"
install_unit "sy-admin" "/usr/bin/sy-admin"
install_unit "sy-orchestrator" "/usr/bin/sy-orchestrator"
install_unit "sy-storage" "/usr/bin/sy-storage"
if [[ -n "${sy_identity_bin:-}" ]]; then
  install_unit "sy-identity" "/usr/bin/sy-identity"
else
  echo "Warning: sy-identity unit not installed (binary missing)." >&2
fi
sudo systemctl daemon-reload

echo "Installed config to $PRIMARY_CONFIG_DIR (and legacy $LEGACY_CONFIG_DIR), binaries to /usr/bin, systemd units, and runtime directories."
