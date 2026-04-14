#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  publish-wf-runtime.sh --version <version> [options]

Required:
  --version <version>      Runtime version (example: 0.1.0)

Options:
  --runtime <name>         Override runtime key (default: wf.engine)
  --binary <path>          Override binary path (default: go/nodes/wf/wf-generic/wf-generic)
  --dist-root <path>       Dist root (default: /var/lib/fluxbee/dist)
  --set-current            Set current version in manifest
  --sudo                   Use sudo for writes
  --skip-build             Skip go build
  -h, --help               Show help

Notes:
  - Publishes the WF base runtime used by workflow packages and managed WF.* nodes.
  - The installed binary name remains wf-generic; the runtime name defaults to wf.engine.
EOF
}

RUNTIME="wf.engine"
VERSION=""
BINARY="$ROOT_DIR/go/nodes/wf/wf-generic/wf-generic"
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

if [[ -z "$VERSION" ]]; then
  echo "Error: --version is required" >&2
  usage
  exit 1
fi

if [[ "$SKIP_BUILD" != "1" ]]; then
  if ! command -v go >/dev/null 2>&1; then
    echo "Error: go not found (use --skip-build if binary already exists)" >&2
    exit 1
  fi
  (cd "$ROOT_DIR/go/nodes/wf/wf-generic" && go build -o wf-generic .)
fi

cmd=(bash "$PUBLISH_SCRIPT" --runtime "$RUNTIME" --version "$VERSION" --binary "$BINARY")
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

echo "Configured WF runtime: runtime=$RUNTIME binary=$(basename "$BINARY")"
