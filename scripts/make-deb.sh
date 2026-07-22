#!/usr/bin/env bash
# make-deb.sh — build a Fluxbee .deb from a clean GitHub checkout.
#
# The "any dev, sin pensar" entry point for a Fluxbee build box: it clones (or fast-forwards)
# the repo and runs the packaging build. Point a dev at a build box that has the toolchain
# (git + rust + go + protoc; see docs/packaging-and-build.md) and:
#
#   scripts/make-deb.sh                       # main, version 0.1.0, ~/fluxbee
#   scripts/make-deb.sh --branch daily_onworking_coa --version 0.1.0
#   REPO=... BRANCH=... VERSION=... DIR=... scripts/make-deb.sh
#
# Output: <DIR>/dist/fluxbee_<VERSION>_amd64.deb
set -euo pipefail

REPO="${REPO:-git@github.com:4iplatform/fluxbee.git}"
BRANCH="${BRANCH:-main}"
VERSION="${VERSION:-0.1.0}"
DIR="${DIR:-$HOME/fluxbee}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) REPO="${2:?}"; shift 2 ;;
    --branch) BRANCH="${2:?}"; shift 2 ;;
    --version) VERSION="${2:?}"; shift 2 ;;
    --dir) DIR="${2:?}"; shift 2 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

for tool in git cargo go protoc python3 dpkg-deb; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "Error: missing build tool '$tool'. See docs/packaging-and-build.md (build box setup)." >&2
    exit 1
  }
done

if [[ ! -d "$DIR/.git" ]]; then
  echo "== clone $REPO -> $DIR =="
  git clone "$REPO" "$DIR"
fi
cd "$DIR"
echo "== sync $BRANCH =="
git fetch --prune origin "$BRANCH"
git checkout -q "$BRANCH"
git reset -q --hard "origin/$BRANCH"
echo "   at $(git rev-parse --short HEAD)"

echo "== build .deb (version $VERSION) =="
packaging/build-deb.sh "$VERSION"

OUT="$DIR/dist/fluxbee_${VERSION}_amd64.deb"
if [[ -f "$OUT" ]]; then
  echo "OK: $OUT ($(du -h "$OUT" | awk '{print $1}'))"
else
  echo "Error: build finished but $OUT is missing" >&2
  exit 1
fi
