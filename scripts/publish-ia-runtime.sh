#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  publish-ia-runtime.sh --runtime <AI.runtime> --version <version> [options]

Required:
  --runtime <AI.runtime>   Runtime key (example: AI.chat)
  --version <version>      Runtime version (example: 0.1.0)

Options:
  --binary <path>          Binary path (default: target/release/ai_node_runner)
  --dist-root <path>       Dist root (default: /var/lib/fluxbee/dist)
  --set-current            Set current version in manifest
  --sudo                   Use sudo for writes
  --skip-build             Skip cargo build
  -h, --help               Show help
EOF
}

RUNTIME=""
VERSION=""
BINARY="$ROOT_DIR/target/release/ai_node_runner"
DIST_ROOT=""
SET_CURRENT=0
USE_SUDO=0
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runtime)
      RUNTIME="${2:-}"
      shift 2
      ;;
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --binary)
      BINARY="${2:-}"
      shift 2
      ;;
    --dist-root)
      DIST_ROOT="${2:-}"
      shift 2
      ;;
    --set-current)
      SET_CURRENT=1
      shift
      ;;
    --sudo)
      USE_SUDO=1
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Error: unknown option: $1" >&2
      usage
      exit 1
      ;;
  esac
done

if [[ -z "$RUNTIME" || -z "$VERSION" ]]; then
  echo "Error: --runtime and --version are required" >&2
  usage
  exit 1
fi

if [[ "$SKIP_BUILD" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found (use --skip-build if binary already exists)" >&2
    exit 1
  fi
  (cd "$ROOT_DIR" && cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner)
fi

cmd=("$PUBLISH_SCRIPT" --runtime "$RUNTIME" --version "$VERSION" --binary "$BINARY")
if [[ -n "$DIST_ROOT" ]]; then
  cmd+=(--dist-root "$DIST_ROOT")
fi
if [[ "$SET_CURRENT" == "1" ]]; then
  cmd+=(--set-current)
fi
if [[ "$USE_SUDO" == "1" ]]; then
  cmd+=(--sudo)
fi

"${cmd[@]}"
