#!/usr/bin/env bash
set -euo pipefail

# Enforces M8:
# - no direct `jsr_client` paths in product Rust code
# - no new `jsr-client` dependency declarations outside legacy crate
#
# Scope (product code):
# - src/**/*.rs
# - crates/fluxbee_sdk/**/*.rs
# Exclusions:
# - crates/jsr_client/** (legacy crate kept during migration)

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
  src crates/fluxbee_sdk --glob '*.rs'
then
  echo "FAIL: direct jsr_client usage detected in product Rust code." >&2
  violations=1
fi

echo "Checking Cargo manifests for direct jsr-client dependency..."
if rg -n --no-heading \
  -e '^\s*jsr-client\s*=' \
  -e 'package\s*=\s*"jsr-client"' \
  --glob '**/Cargo.toml' \
  --glob '!crates/jsr_client/Cargo.toml' \
  .
then
  echo "FAIL: direct jsr-client dependency detected outside legacy crate." >&2
  violations=1
fi

if [[ "$violations" -ne 0 ]]; then
  exit 1
fi

echo "OK: no direct jsr_client usage in product code."
