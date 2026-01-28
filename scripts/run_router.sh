#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "Usage: $0 <router-name>" >&2
  echo "Example: $0 RT.gateway" >&2
  exit 1
fi

ROUTER_NAME="$1"

JSR_ROUTER_NAME="$ROUTER_NAME" \
cargo run --bin json-router
