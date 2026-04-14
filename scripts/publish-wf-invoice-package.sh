#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PACKAGE_DIR="$ROOT_DIR/go/nodes/wf/examples/packages/wf.invoice"
PUBLISH_BIN="$ROOT_DIR/target/release/fluxbee-publish"
VERSION=""
DEPLOY_HIVE=""
SKIP_BUILD=0

usage() {
  cat <<'EOF'
Usage:
  publish-wf-invoice-package.sh --version <version> --deploy <hive_id> [options]

Required:
  --version <version>      Workflow package version override
  --deploy <hive_id>       Hive where the package should be deployed

Options:
  --skip-build             Skip cargo build of fluxbee-publish
  -h, --help               Show help

Notes:
  - Publishes the canonical workflow package wf.invoice.
  - Requires wf.engine to be published and ready first.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --deploy)
      DEPLOY_HIVE="${2:-}"
      shift 2
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

if [[ -z "$VERSION" || -z "$DEPLOY_HIVE" ]]; then
  echo "Error: --version and --deploy are required" >&2
  usage
  exit 1
fi

if [[ "$SKIP_BUILD" != "1" ]]; then
  (cd "$ROOT_DIR" && cargo build --release --bin fluxbee-publish >/dev/null)
fi

exec "$PUBLISH_BIN" "$PACKAGE_DIR" --version "$VERSION" --deploy "$DEPLOY_HIVE"
