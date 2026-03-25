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
  --mode <default|gov>     Default AI_NODE_MODE for published start.sh (default: default)
  --forced-node-name <n>   Deprecated emergency override for --node-name in start.sh
  --forced-dynamic-config-dir <p>
                            Deprecated emergency override for --dynamic-config-dir in start.sh
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
MODE="default"
FORCED_NODE_NAME=""
FORCED_DYNAMIC_CONFIG_DIR=""

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
    --mode)
      MODE="${2:-}"
      shift 2
      ;;
    --forced-node-name)
      FORCED_NODE_NAME="${2:-}"
      shift 2
      ;;
    --forced-dynamic-config-dir)
      FORCED_DYNAMIC_CONFIG_DIR="${2:-}"
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

case "$MODE" in
  default|gov)
    ;;
  *)
    echo "Error: --mode must be default or gov (got '$MODE')" >&2
    exit 1
    ;;
esac

if [[ "$SKIP_BUILD" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found (use --skip-build if binary already exists)" >&2
    exit 1
  fi
  (cd "$ROOT_DIR" && cargo build --release -p fluxbee-ai-nodes --bin ai_node_runner)
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

DIST_ROOT_EFFECTIVE="${DIST_ROOT:-/var/lib/fluxbee/dist}"
RUNTIME_DIR="$DIST_ROOT_EFFECTIVE/runtimes/$RUNTIME/$VERSION"
BIN_DIR="$RUNTIME_DIR/bin"
START_SH="$BIN_DIR/start.sh"
BINARY_NAME="$(basename "$BINARY")"

tmp_start="$(mktemp)"
cat >"$tmp_start" <<EOF
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
if [[ -z "\${AI_NODE_MODE:-}" ]]; then
  export AI_NODE_MODE="$MODE"
fi
if [[ -z "\${RUST_LOG:-}" ]]; then
  export RUST_LOG="info,fluxbee_ai_nodes=debug,fluxbee_ai_sdk=info,fluxbee_ai_sdk::runtime=warn"
fi
SCRIPT_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
forced_node_name="${FORCED_NODE_NAME}"
forced_dynamic_dir="${FORCED_DYNAMIC_CONFIG_DIR}"
runner_args=()
if [[ -n "\${forced_node_name}" && -z "\${AI_NODE_NAME:-}" ]]; then
  runner_args+=(--node-name "\${forced_node_name}")
fi
if [[ -n "\${forced_dynamic_dir}" && -z "\${AI_DYNAMIC_CONFIG_DIR:-}" ]]; then
  runner_args+=(--dynamic-config-dir "\${forced_dynamic_dir}")
fi
exec "\$SCRIPT_DIR/$BINARY_NAME" "\${runner_args[@]}" "\$@"
EOF

if [[ "$USE_SUDO" == "1" ]]; then
  sudo install -m 0755 "$tmp_start" "$START_SH"
else
  install -m 0755 "$tmp_start" "$START_SH"
fi
rm -f "$tmp_start"

echo "Configured AI start.sh mode default: $MODE"
if [[ -n "$FORCED_NODE_NAME" ]]; then
  echo "Configured AI start.sh forced node name: $FORCED_NODE_NAME"
fi
if [[ -n "$FORCED_DYNAMIC_CONFIG_DIR" ]]; then
  echo "Configured AI start.sh forced dynamic config dir: $FORCED_DYNAMIC_CONFIG_DIR"
fi
