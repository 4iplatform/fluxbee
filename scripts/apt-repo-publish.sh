#!/usr/bin/env bash
# apt-repo-publish.sh — publish a built Fluxbee .deb into a flat apt repo and (optionally) serve
# it over HTTP, so any box on the internal network installs with `apt install fluxbee` (apt
# resolves postgresql and friends from the Ubuntu archive automatically — no manual .deb copy).
#
# On the build+repo machine:
#   scripts/make-deb.sh --branch main --version 0.1.0     # build the .deb
#   scripts/apt-repo-publish.sh --serve                   # publish + serve on :8900
#
# On a client (fresh Ubuntu):
#   echo 'deb [trusted=yes] http://<build-host>:8900 ./' | sudo tee /etc/apt/sources.list.d/fluxbee.list
#   sudo apt-get update && sudo apt-get install -y fluxbee
#   sudo nano /etc/fluxbee/hive.yaml && sudo fluxbee-firstboot
#
# The repo is UNSIGNED (internal, [trusted=yes]). For a public/internet repo, sign Release with
# GPG (gpg --clearsign -> InRelease) and drop [trusted=yes]; the .deb itself needs no change.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DEB="${DEB:-}"
REPO="${REPO:-/var/lib/fluxbee-apt}"
PORT="${PORT:-8900}"
SERVE=0
USE_SUDO=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --deb) DEB="${2:?}"; shift 2 ;;
    --repo) REPO="${2:?}"; shift 2 ;;
    --port) PORT="${2:?}"; shift 2 ;;
    --serve) SERVE=1; shift ;;
    --sudo) USE_SUDO=1; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

SUDO=""; [[ "$USE_SUDO" == "1" ]] && SUDO="sudo"

for t in dpkg-scanpackages apt-ftparchive; do
  command -v "$t" >/dev/null 2>&1 || {
    echo "Error: missing '$t' — install with: apt-get install -y dpkg-dev apt-utils" >&2
    exit 1
  }
done

if [[ -z "$DEB" ]]; then
  DEB="$(ls -t "$ROOT_DIR"/dist/fluxbee_*_amd64.deb 2>/dev/null | head -1 || true)"
fi
[[ -f "$DEB" ]] || { echo "Error: no .deb found (build with scripts/make-deb.sh, or pass --deb)" >&2; exit 1; }

echo "== publish $(basename "$DEB") -> $REPO =="
$SUDO mkdir -p "$REPO"
$SUDO cp -f "$DEB" "$REPO/"
# Flat repo: index every .deb in the dir, then Packages.gz + Release at the root.
( cd "$REPO" && $SUDO sh -c 'dpkg-scanpackages -m . > Packages && gzip -kf Packages && apt-ftparchive release . > Release' )
echo "   $(cd "$REPO" && grep -c '^Package:' Packages) package(s) indexed"

if [[ "$SERVE" == "1" ]]; then
  # Idempotent HTTP server for the repo (systemd-run unit; survives this shell).
  if command -v systemctl >/dev/null 2>&1; then
    $SUDO systemctl is-active fluxbee-apt >/dev/null 2>&1 || \
      $SUDO systemd-run --unit=fluxbee-apt --working-directory="$REPO" \
        python3 -m http.server "$PORT" >/dev/null 2>&1 || true
    echo "   serving $REPO on :$PORT (systemd unit fluxbee-apt)"
  else
    echo "   (no systemd; serve manually: cd $REPO && python3 -m http.server $PORT)"
  fi
  IP="$(hostname -I 2>/dev/null | awk '{print $1}')"
  echo
  echo "Client one-liner:"
  echo "  echo 'deb [trusted=yes] http://${IP:-<build-host>}:$PORT ./' | sudo tee /etc/apt/sources.list.d/fluxbee.list && sudo apt-get update && sudo apt-get install -y fluxbee"
fi
