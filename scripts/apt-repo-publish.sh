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
# Copy under a hidden temp name, then rename: a client downloading the same version (re-publish)
# never reads a half-written .deb (rename is atomic; an in-flight download keeps the old inode).
# The temp name ends in .partial, so dpkg-scanpackages never indexes it.
TMP_DEB="$REPO/.$(basename "$DEB").partial"
$SUDO cp -f "$DEB" "$TMP_DEB"
$SUDO mv -f "$TMP_DEB" "$REPO/$(basename "$DEB")"
# Flat repo: rebuild the index into temp files, then swap them in with atomic renames. Writing
# `> Packages` in place truncated the LIVE index for the whole scan (~5 min with dozens of
# ~240 MB .debs, all re-hashed), so a client running `apt-get update` meanwhile saw an EMPTY
# repo. Release goes last, so it always describes the Packages already in place.
( cd "$REPO" && $SUDO sh -c '
    dpkg-scanpackages -m . > Packages.new &&
    gzip -c Packages.new > Packages.gz.new &&
    mv -f Packages.new Packages &&
    mv -f Packages.gz.new Packages.gz &&
    apt-ftparchive release . > Release.new &&
    mv -f Release.new Release' )
echo "   $(cd "$REPO" && grep -c '^Package:' Packages) package(s) indexed"

if [[ "$SERVE" == "1" ]]; then
  # PERSISTENT HTTP server for the repo: a real ENABLED unit, so it survives reboots of the build
  # box. It used to be a transient `systemd-run` unit: it died with every reboot, and its errors
  # were swallowed (`>/dev/null 2>&1 || true`), so a re-run could leave the repo down silently.
  # Idempotent: rewrites the unit only when its content changes, and restarts only then.
  if command -v systemctl >/dev/null 2>&1; then
    UNIT=/etc/systemd/system/fluxbee-apt.service
    WANT="[Unit]
Description=Fluxbee flat apt repo ($REPO on :$PORT)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
WorkingDirectory=$REPO
ExecStart=/usr/bin/python3 -m http.server $PORT --bind 0.0.0.0
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target"
    CHANGED=0
    if [[ "$($SUDO cat "$UNIT" 2>/dev/null || true)" != "$WANT" ]]; then
      printf '%s\n' "$WANT" | $SUDO tee "$UNIT" >/dev/null
      CHANGED=1
    fi
    # A leftover TRANSIENT unit of the same name (pre-fix) lives in /run/systemd/transient, which
    # outranks /etc in the unit search path: stop it so the persistent file takes over.
    if [[ "$($SUDO systemctl show fluxbee-apt -p Transient --value 2>/dev/null || true)" == "yes" ]]; then
      $SUDO systemctl stop fluxbee-apt
      $SUDO systemctl reset-failed fluxbee-apt 2>/dev/null || true
      CHANGED=1
    fi
    $SUDO systemctl daemon-reload
    $SUDO systemctl enable --now fluxbee-apt >/dev/null 2>&1
    if [[ "$CHANGED" == "1" ]]; then
      $SUDO systemctl restart fluxbee-apt
    fi
    $SUDO systemctl is-active --quiet fluxbee-apt \
      || { echo "Error: fluxbee-apt did not start — see: journalctl -u fluxbee-apt" >&2; exit 1; }
    echo "   serving $REPO on :$PORT (persistent systemd unit fluxbee-apt, enabled at boot)"
  else
    echo "   (no systemd; serve manually: cd $REPO && python3 -m http.server $PORT)"
  fi
  echo
  echo "Client one-liner (use the address of the network the client is on):"
  for IP in $(hostname -I 2>/dev/null); do
    [[ "$IP" == *:* ]] && continue   # IPv4 only
    echo "  echo 'deb [trusted=yes] http://$IP:$PORT ./' | sudo tee /etc/apt/sources.list.d/fluxbee.list && sudo apt-get update && sudo apt-get install -y fluxbee"
  done
fi
