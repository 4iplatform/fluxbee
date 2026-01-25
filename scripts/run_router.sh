#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <router-name> [config-dir]" >&2
  echo "Example: $0 RT.gateway ./config" >&2
  exit 1
fi

ROUTER_NAME="$1"
CONFIG_DIR="${2:-./config}"
STATE_DIR="${JSR_STATE_DIR:-./state}"
SOCKET_DIR="${JSR_SOCKET_DIR:-./run}"

mkdir -p "$STATE_DIR" "$SOCKET_DIR"

JSR_CONFIG_DIR="$CONFIG_DIR" \
JSR_STATE_DIR="$STATE_DIR" \
JSR_SOCKET_DIR="$SOCKET_DIR" \
JSR_ROUTER_NAME="$ROUTER_NAME" \
cargo run --bin json-router
