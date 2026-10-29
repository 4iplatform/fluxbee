#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  publish-ia-runtime.sh --runtime <ai.runtime> --version <version> [options]

Required:
  --runtime <ai.runtime>   Runtime key (example: ai.generic)
  --version <version>      Runtime version (example: 0.1.0)

Options:
  --binary <path>          Binary path (default: target/release/ai_node_runner)
  --catalog <path>         ai.generic's tool catalog, published as the version's tools.json
                           (default: docs/reference/agent-tools.json of this checkout;
                           ai.generic only)
  --dist-root <path>       Dist root (default: /var/lib/fluxbee/dist)
  --set-current            Set current version in manifest
  --sudo                   Use sudo for writes
  --skip-build             Skip cargo build
  -h, --help               Show help

Notes:
  - Source home for the generic AI runner: nodes/ai/ai-generic
  - Every file of the version is in place before the manifest names it: the binary, the AI
    start.sh and, for ai.generic, tools.json (the catalog with this version in it). A missing
    or unreadable input fails before anything is written (publish-runtime.sh).
EOF
}

RUNTIME=""
VERSION=""
BINARY="$ROOT_DIR/target/release/ai_node_runner"
CATALOG=""
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
    --catalog)
      CATALOG="${2:-}"
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

# ai.generic's tool catalog goes next to the runtime, as the .deb ships it (packaging/build-deb.sh).
if [[ "$RUNTIME" == "ai.generic" ]]; then
  CATALOG="${CATALOG:-$ROOT_DIR/docs/reference/agent-tools.json}"
  if [[ ! -f "$CATALOG" ]]; then
    echo "Error: tool catalog does not exist: $CATALOG" >&2
    exit 1
  fi
elif [[ -n "$CATALOG" ]]; then
  echo "Error: --catalog is ai.generic's tool catalog; $RUNTIME ships none" >&2
  exit 1
fi

if [[ "$SKIP_BUILD" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found (use --skip-build if binary already exists)" >&2
    exit 1
  fi
  (cd "$ROOT_DIR" && cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner)
fi

# The version's own files are written here first and handed to publish-runtime.sh, which installs
# them before it writes the manifest.
STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT
BINARY_NAME="$(basename "$BINARY")"

cat >"$STAGE_DIR/start.sh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ -n "\${FLUXBEE_NODE_NAME:-}" && -z "\${AI_NODE_NAME:-}" ]]; then
  export AI_NODE_NAME="\${FLUXBEE_NODE_NAME}"
fi
if [[ -n "\${NODE_NAME:-}" && -z "\${AI_NODE_NAME:-}" ]]; then
  export AI_NODE_NAME="\${NODE_NAME}"
fi
if [[ -n "\${AI_NODE_NAME:-}" && -z "\${FLUXBEE_NODE_NAME:-}" ]]; then
  export FLUXBEE_NODE_NAME="\${AI_NODE_NAME}"
fi
if [[ -n "\${ROUTER_SOCKET:-}" && -z "\${AI_ROUTER_SOCKET:-}" ]]; then
  export AI_ROUTER_SOCKET="\${ROUTER_SOCKET}"
fi
if [[ -n "\${UUID_PERSISTENCE_DIR:-}" && -z "\${AI_UUID_PERSISTENCE_DIR:-}" ]]; then
  export AI_UUID_PERSISTENCE_DIR="\${UUID_PERSISTENCE_DIR}"
fi
if [[ -n "\${CONFIG_DIR:-}" && -z "\${AI_CONFIG_DIR:-}" ]]; then
  export AI_CONFIG_DIR="\${CONFIG_DIR}"
fi
if [[ -n "\${AI_DYNAMIC_CONFIG_DIR:-}" ]]; then
  :
elif [[ -n "\${STATE_DIR:-}" ]]; then
  export AI_DYNAMIC_CONFIG_DIR="\${STATE_DIR}/ai-nodes"
fi
if [[ -z "\${RUST_LOG:-}" ]]; then
  export RUST_LOG="info,fluxbee_ai_nodes=debug,fluxbee_ai_sdk=info,fluxbee_ai_sdk::runtime=warn"
fi
SCRIPT_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
exec "\$SCRIPT_DIR/$BINARY_NAME" "\$@"
EOF

cmd=(bash "$PUBLISH_SCRIPT" --runtime "$RUNTIME" --version "$VERSION" --binary "$BINARY"
  --start-script "$STAGE_DIR/start.sh")
if [[ "$RUNTIME" == "ai.generic" ]]; then
  # The catalog with this version in it, as the .deb ships it. A catalog that does not parse
  # stops here, before anything is written.
  if ! python3 -c 'import json, sys
catalog = json.load(open(sys.argv[1], encoding="utf-8"))
catalog["runtime_version"] = sys.argv[2]
open(sys.argv[3], "w", encoding="utf-8").write(json.dumps(catalog, indent=2, ensure_ascii=False) + "\n")' \
    "$CATALOG" "$VERSION" "$STAGE_DIR/tools.json"; then
    echo "Error: tool catalog is not readable JSON: $CATALOG" >&2
    exit 1
  fi
  cmd+=(--file "$STAGE_DIR/tools.json")
fi
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

echo "Configured AI start.sh runtime profile: $RUNTIME"
if [[ "$RUNTIME" == "ai.generic" ]]; then
  echo "Published the tool catalog from $CATALOG"
fi
