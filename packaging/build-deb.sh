#!/usr/bin/env bash
# Build a fluxbee .deb (Ubuntu 24.04, amd64) from source. Separates BUILD (here,
# on a host with the toolchain) from INSTALL (the .deb postinst on the target).
# The package bakes the dist/core manifest hashes at build time, killing the
# "manifest hash mismatch" crash-loop that a manual binary copy causes.
#
#   packaging/build-deb.sh [VERSION]
#
# Output: dist/fluxbee_<version>_amd64.deb
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"
VERSION="${1:-0.1.0}"
ARCH="amd64"
BUILD_ID="$(date -u +%Y%m%d%H%M%S)"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# core service binaries: <installed-name>=<cargo --bin or go marker>
RUST_BINS=(rt-gateway:json-router sy-admin:sy_admin sy-config-routes:sy_config_routes
  sy-architect:sy_architect sy-vault:sy_vault sy-orchestrator:sy_orchestrator
  sy-storage:sy_storage sy-identity:sy_identity sy-cognition:sy_cognition sy-policy:sy_policy)
GO_BINS=(sy-opa-rules:go/sy-opa-rules sy-timer:go/sy-timer sy-wf-rules:go/sy-wf-rules
  wf-generic:go/nodes/wf/wf-generic)
# units (14): every core service EXCEPT wf-generic (a runtime the orchestrator spawns)
UNITS=(rt-gateway sy-config-routes sy-opa-rules sy-admin sy-architect sy-vault
  sy-orchestrator sy-storage sy-identity sy-cognition sy-policy sy-timer sy-wf-rules sy-frontdesk-gov)

echo "== [1/5] build rust =="
cargo build --release --bins
cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
echo "== [2/5] build go =="
(cd go/sy-opa-rules && go build -o sy-opa-rules .)
(cd go/sy-timer && go build -o sy-timer .)
(cd go/sy-wf-rules && go build -o sy-wf-rules .)
(cd go/nodes/wf/wf-generic && go build -o wf-generic .)

echo "== [3/5] stage files =="
DEST="$STAGE/fluxbee"
install -d "$DEST/usr/bin" "$DEST/var/lib/fluxbee/dist/core/bin" \
  "$DEST/lib/systemd/system" "$DEST/etc/fluxbee" "$DEST/usr/share/fluxbee" "$DEST/DEBIAN"

stage_bin() { # <name> <src>
  install -m0755 "$2" "$DEST/usr/bin/$1"
  install -m0755 "$2" "$DEST/var/lib/fluxbee/dist/core/bin/$1"
}
for pair in "${RUST_BINS[@]}"; do
  stage_bin "${pair%%:*}" "target/release/${pair##*:}"
done
stage_bin sy-frontdesk-gov "target/release/sy-frontdesk-gov"
for pair in "${GO_BINS[@]}"; do
  stage_bin "${pair%%:*}" "${pair##*:}/$(basename "${pair##*:}")"
done

# dist/core manifest with the staged binaries' real hashes (baked in).
python3 - "$DEST/var/lib/fluxbee/dist/core" "$VERSION" "$BUILD_ID" <<'PY'
import json, hashlib, os, sys
core, version, build_id = sys.argv[1], sys.argv[2], sys.argv[3]
bind = os.path.join(core, "bin")
comps = {}
for svc in sorted(os.listdir(bind)):
    b = open(os.path.join(bind, svc), "rb").read()
    comps[svc] = {"service": svc, "version": version, "build_id": build_id,
                  "sha256": hashlib.sha256(b).hexdigest(), "size": len(b)}
json.dump({"schema_version": 1, "components": comps},
          open(os.path.join(core, "manifest.json"), "w"), indent=2, sort_keys=True)
print(f"manifest: {len(comps)} components")
PY

# systemd units (mirrors scripts/install.sh install_unit). Not enabled here; the
# postinst enables only sy-orchestrator, which brings up the rest (its bootstrap
# starts rt-gateway + the SY services in the Model D' order).
gen_unit() { # <name> [after] [wants]
  local name="$1" after="${2:-network.target}" wants="${3:-}"
  { echo "[Unit]"; echo "Description=Fluxbee $name"; echo "After=$after"
    [ -n "$wants" ] && echo "Wants=$wants"
    echo; echo "[Service]"; echo "Type=simple"; echo "ExecStart=/usr/bin/$name"
    echo "Restart=always"; echo "RestartSec=5"; echo
    echo "[Install]"; echo "WantedBy=multi-user.target"; } > "$DEST/lib/systemd/system/$name.service"
}
for u in "${UNITS[@]}"; do gen_unit "$u"; done
gen_unit sy-frontdesk-gov "network.target rt-gateway.service sy-identity.service" \
  "rt-gateway.service sy-identity.service"

# config template + first-boot helper
[ -f config/hive.yaml ] && install -m0644 config/hive.yaml "$DEST/etc/fluxbee/hive.yaml.example" || true
install -m0755 packaging/fluxbee-firstboot "$DEST/usr/share/fluxbee/fluxbee-firstboot"
ln -sf ../share/fluxbee/fluxbee-firstboot "$DEST/usr/bin/fluxbee-firstboot"

echo "== [4/5] debian metadata =="
INSTALLED_KB="$(du -sk "$DEST" | awk '{print $1}')"
cat > "$DEST/DEBIAN/control" <<EOF
Package: fluxbee
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${ARCH}
Depends: adduser, openssl, libc6 (>= 2.39)
Recommends: postgresql
Installed-Size: ${INSTALLED_KB}
Maintainer: 4i Platform <ops@4iplatform.com>
Description: Fluxbee internal-network orchestration mesh
 Core services (router, orchestrator, identity, vault, storage, admin,
 architect, cognition, policy, timer, wf-rules, opa-rules, frontdesk).
 Binaries + dist/core manifest (hashes baked at build), systemd units, and a
 first-boot helper. Run 'sudo fluxbee-firstboot' after install.
EOF
# /etc/fluxbee/* is config — track example as conffile so upgrades don't clobber.
echo "/etc/fluxbee/hive.yaml.example" > "$DEST/DEBIAN/conffiles"
install -m0755 packaging/deb-postinst "$DEST/DEBIAN/postinst"
install -m0755 packaging/deb-prerm "$DEST/DEBIAN/prerm"

echo "== [5/5] dpkg-deb =="
install -d "$ROOT_DIR/dist"
OUT="$ROOT_DIR/dist/fluxbee_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$DEST" "$OUT"
echo "built: $OUT"
dpkg-deb --info "$OUT" | sed -n '1,12p'
