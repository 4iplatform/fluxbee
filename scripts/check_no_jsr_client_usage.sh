#!/usr/bin/env bash
set -euo pipefail

# Enforces post-migration SDK policy:
# - no direct `jsr_client` paths in product Rust code
# - no `jsr-client` dependency declarations in Cargo manifests
#
# Scope (product code):
# - src/**/*.rs
# - crates/fluxbee_sdk/**/*.rs
# - examples/**/*.rs

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing command '$1'" >&2
    exit 1
  fi
}

require_cmd rg

violations=0

echo "Checking product Rust code for direct jsr_client usage..."
if rg -n --no-heading \
  -e '\buse\s+jsr_client\b' \
  -e '\bjsr_client::' \
  src crates/fluxbee_sdk examples --glob '*.rs'
then
  echo "FAIL: direct jsr_client usage detected in product Rust code." >&2
  violations=1
fi

echo "Checking Cargo manifests for direct jsr-client dependency..."
if rg -n --no-heading \
  -e '^\s*jsr-client\s*=' \
  -e 'package\s*=\s*"jsr-client"' \
  --glob '**/Cargo.toml' \
  .
then
  echo "FAIL: direct jsr-client dependency detected in Cargo manifests." >&2
  violations=1
fi

if [[ "$violations" -ne 0 ]]; then
  exit 1
fi

echo "OK: no direct jsr_client usage in product code."
