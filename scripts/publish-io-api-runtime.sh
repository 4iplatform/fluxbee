#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PUBLISH_SCRIPT="$ROOT_DIR/scripts/publish-runtime.sh"

usage() {
  cat <<'EOF'
Usage:
  publish-io-api-runtime.sh --version <version> [options]

Required:
  --version <version>      Runtime version (example: 0.1.0)

Options:
  --runtime <name>         Override runtime key (default: io.api)
  --binary <path>          Override binary path (default: nodes/io/target/release/io-api)
  --dist-root <path>       Dist root (default: /var/lib/fluxbee/dist)
  --set-current            Set current version in manifest
  --sudo                   Use sudo for writes
  --skip-build             Skip cargo build
  -h, --help               Show help
EOF
}

VERSION=""
RUNTIME="io.api"
BINARY=""
DIST_ROOT=""
SET_CURRENT=0
USE_SUDO=0
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) VERSION="${2:-}"; shift 2 ;;
    --runtime) RUNTIME="${2:-}"; shift 2 ;;
    --binary) BINARY="${2:-}"; shift 2 ;;
    --dist-root) DIST_ROOT="${2:-}"; shift 2 ;;
    --set-current) SET_CURRENT=1; shift ;;
    --sudo) USE_SUDO=1; shift ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Error: unknown option: $1" >&2; usage; exit 1 ;;
  esac
done

if [[ -z "$VERSION" ]]; then
  echo "Error: --version is required" >&2
  usage
  exit 1
fi

if [[ -z "$BINARY" ]]; then
  BINARY="$ROOT_DIR/nodes/io/target/release/io-api"
fi

if [[ "$SKIP_BUILD" != "1" ]]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found (use --skip-build if binary already exists)" >&2
    exit 1
  fi
  (cd "$ROOT_DIR" && cargo build --release --manifest-path nodes/io/Cargo.toml -p io-api)
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
DEFAULT_RUST_LOG="info,io_api=debug,io_common=debug,fluxbee_sdk=info"

tmp_start="$(mktemp)"
cat >"$tmp_start" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [[ -z "\${RUST_LOG:-}" ]]; then
  export RUST_LOG="$DEFAULT_RUST_LOG"
fi
SCRIPT_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
exec "\$SCRIPT_DIR/$BINARY_NAME" "\$@"
EOF

if [[ "$USE_SUDO" == "1" ]]; then
  sudo install -m 0755 "$tmp_start" "$START_SH"
else
  install -m 0755 "$tmp_start" "$START_SH"
fi
rm -f "$tmp_start"

echo "Configured io.api start.sh default RUST_LOG: $DEFAULT_RUST_LOG"
