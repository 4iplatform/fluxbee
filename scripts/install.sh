#!/usr/bin/env bash
# install.sh [VERSION]: the development install. It builds the .deb from this checkout with
# packaging/build-deb.sh and installs it on this host with apt, so a development host runs exactly
# what a package install runs (docs/reference/operations.md, "Install the motherbee"). Everything
# an install does is the package's maintainer scripts' job: this wrapper writes no config and
# changes no owner. A first install still needs /etc/fluxbee/hive.yaml edited and
# `sudo fluxbee-firstboot`.
#
# VERSION defaults to the installed fluxbee version (0.1.0 when none is installed). A VERSION lower
# than the installed one is refused before building: apt refuses a downgrade. `--reinstall` makes a
# rebuild at the version already installed install again instead of being skipped.
#
# Run it as yourself: the build uses your toolchain; only apt runs with sudo.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SUDO=""
[[ "$(id -u)" -eq 0 ]] || SUDO="sudo"

INSTALLED=""
if status="$(dpkg-query -W -f='${db:Status-Status}' fluxbee 2>/dev/null)" &&
  [[ "$status" == installed ]]; then
  INSTALLED="$(dpkg-query -W -f='${Version}' fluxbee)"
fi
VERSION="${1:-${INSTALLED:-0.1.0}}"
if [[ -n "$INSTALLED" ]] && dpkg --compare-versions "$VERSION" lt "$INSTALLED"; then
  echo "install.sh: fluxbee $INSTALLED is installed; $VERSION is older and apt refuses a downgrade." >&2
  echo "Give $INSTALLED or a later version, or no VERSION to rebuild $INSTALLED." >&2
  exit 1
fi

bash "$ROOT_DIR/packaging/build-deb.sh" "$VERSION"
$SUDO apt-get install -y --reinstall "$ROOT_DIR/dist/fluxbee_${VERSION}_amd64.deb"

# An older install wrote units under /etc/systemd/system, which shadow the package's own: systemd
# keeps running those definitions. Reported here, never removed by this script.
shadowing=()
while read -r unit; do
  if [[ -e "/etc/systemd/system/${unit##*/}" || -L "/etc/systemd/system/${unit##*/}" ]]; then
    shadowing+=("/etc/systemd/system/${unit##*/}")
  fi
done < <(dpkg-query -L fluxbee | grep -E '^/(usr/)?lib/systemd/system/[^/]+\.service$')
if (( ${#shadowing[@]} )); then
  echo "WARNING: these unit files shadow the fluxbee package's units in /lib/systemd/system:" >&2
  printf '  %s\n' "${shadowing[@]}" >&2
  echo "Remove them, reload systemd and enable sy-orchestrator from the package's unit:" >&2
  echo "  sudo rm ${shadowing[*]} && sudo systemctl daemon-reload && sudo systemctl reenable sy-orchestrator.service" >&2
fi
