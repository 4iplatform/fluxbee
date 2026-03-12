#!/usr/bin/env bash
set -euo pipefail

# IO identity example E2E wrapper.
# Reuses the hardened orchestrator/runtime pipeline from io_test_node_e2e.sh
# but exposes "example" naming defaults for integrators.
#
# Optional env:
#   BASE=http://127.0.0.1:8080
#   HIVE_ID=worker-220
#   BUILD_BIN=1
#   TEST_ID=ioidex-<ts>-<rnd>
#   RUNTIME_NAME=io.identity.example
#   NODE_NAME=IO.identity.example.<id>
#   IO_TEST_NODE_NAME=IO.identity.example
#   IO_TEST_CHANNEL_TYPE=io.identity.example
#   IO_TEST_ADDRESS=io.identity.example.<id>
#   IO_TEST_ALLOW_PROVISION=1
#   IO_TEST_IDENTITY_TARGET=SY.identity@worker-220
#   IO_TEST_IDENTITY_FALLBACK_TARGET=SY.identity@sandbox

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

TEST_ID="${TEST_ID:-ioidex-$(date +%s)-$RANDOM}"

export TEST_ID
export RUNTIME_NAME="${RUNTIME_NAME:-io.identity.example}"
export NODE_NAME="${NODE_NAME:-IO.identity.example.${TEST_ID}}"
export IO_TEST_NODE_NAME="${IO_TEST_NODE_NAME:-IO.identity.example}"
export IO_TEST_CHANNEL_TYPE="${IO_TEST_CHANNEL_TYPE:-io.identity.example}"
export IO_TEST_ADDRESS="${IO_TEST_ADDRESS:-io.identity.example.${TEST_ID}}"

exec bash "$ROOT_DIR/scripts/io_test_node_e2e.sh"
