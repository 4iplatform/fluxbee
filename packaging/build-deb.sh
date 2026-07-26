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

# Declarative base-node set (packaging/base-nodes.json) — the single source of truth for which
# IO/AI nodes ship in a from-scratch install, shared with scripts/install.sh. Adding a node is a
# one-line edit there. mf <section> emits TSV rows; node_bin_src resolves a built binary path.
MANIFEST="$ROOT_DIR/packaging/base-nodes.json"
mf() { # singletons -> node\tcrate\tbin\tworkspace\tunit\trole_gate ; runtimes -> runtime\tcrate\tbin\tworkspace\tlang\tboot\tinstance
  python3 - "$MANIFEST" "$1" <<'PY'
import json, sys
m = json.load(open(sys.argv[1])); sec = sys.argv[2]
for e in m.get(sec, []):
    if sec == "singletons":
        print("\t".join([e["node"], e["crate"], e["bin"], e["workspace"], e.get("unit", ""), e.get("role_gate", "")]))
    else:
        print("\t".join([e.get("runtime", ""), e["crate"], e["bin"], e["workspace"],
                         e.get("lang", "rust"), str(e.get("boot", False)).lower(), e.get("instance", "")]))
PY
}
node_bin_src() { # <workspace> <bin> -> path to the built binary
  case "$1" in
    nodes/io) echo "nodes/io/target/release/$2" ;;
    nodes/ai) echo "target/release/$2" ;;            # nodes/ai is a ROOT workspace member (built by --bins)
    go) echo "go/nodes/wf/wf-generic/$2" ;;
    *) echo "target/release/$2" ;;
  esac
}

# core service binaries: <installed-name>=<cargo --bin or go marker>
RUST_BINS=(rt-gateway:json-router sy-admin:sy_admin sy-config-routes:sy_config_routes
  sy-architect:sy_architect sy-vault:sy_vault sy-orchestrator:sy_orchestrator
  sy-storage:sy_storage sy-identity:sy_identity sy-cognition:sy_cognition sy-policy:sy_policy
  sy-edge:sy_edge)
GO_BINS=(sy-opa-rules:go/sy-opa-rules sy-timer:go/sy-timer sy-wf-rules:go/sy-wf-rules
  wf-generic:go/nodes/wf/wf-generic)
# units: every core service EXCEPT wf-generic (a runtime the orchestrator spawns).
# sy-edge gets a dedicated unit below (custom rt-gateway ordering).
UNITS=(rt-gateway sy-config-routes sy-opa-rules sy-admin sy-architect sy-vault
  sy-orchestrator sy-storage sy-identity sy-cognition sy-policy sy-timer sy-wf-rules sy-frontdesk-gov)

echo "== [1/5] build rust =="
cargo build --release --bins
cargo build --release -p sy-frontdesk-gov --bin sy-frontdesk-gov
# Build every nodes/io crate the base-node manifest references (singleton infra nodes +
# io.* runtimes). ai.generic (nodes/ai) is a root workspace member already built by --bins;
# wf-generic (go) is built in the go step below.
IO_PKGS="$( { mf singletons; mf runtimes; } | awk -F'\t' '$4=="nodes/io"{print $2}' | sort -u )"
[ -n "$IO_PKGS" ] && cargo build --release --manifest-path nodes/io/Cargo.toml $(printf -- '-p %s ' $IO_PKGS)
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
# Singletons (motherbee-only infra nodes, e.g. IO.blob/IO.cloud) install to /usr/bin ONLY
# (not dist/core/bin): they are not role-synced core components, so they must not enter the
# core manifest the orchestrator ships to workers. Their systemd units are defined below.
# Driven by packaging/base-nodes.json.
while IFS=$'\t' read -r node crate bin ws unit role; do
  [ -n "${bin:-}" ] || continue
  install -m0755 "$(node_bin_src "$ws" "$bin")" "$DEST/usr/bin/$bin"
done < <(mf singletons)

# Runtimes: seed each into dist/runtimes/<runtime>/<version>/ (instanced, orchestrator-spawned;
# no unit and no /usr/bin copy — Orchestrator launches named instances from this synced tree).
# Driven by the manifest, so adding an IO/AI runtime to the install is a one-line edit there;
# both this .deb path and scripts/install.sh consume the same list (they must not diverge).
while IFS=$'\t' read -r rt crate bin ws lang boot inst; do
  [ -n "${rt:-}" ] || continue
  bash scripts/publish-runtime.sh \
    --runtime "$rt" \
    --version "$VERSION" \
    --binary "$(node_bin_src "$ws" "$bin")" \
    --dist-root "$DEST/var/lib/fluxbee/dist" \
    --set-current
done < <(mf runtimes)

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

# Vendor runtime: bundle the syncthing binary + a vendor manifest so the
# orchestrator blob-sync watchdog is satisfied (without it the orchestrator WARNs
# every ~5s and blob/dist sync is non-operational). syncthing self-generates its
# config.xml in --home at runtime, so only the binary is strictly required; the
# config.xml template is bundled too if the vendor bundle ships one.
ST_SRC=""
for c in vendor/syncthing/syncthing vendor/syncthing-linux-amd64-*/syncthing vendor/*/syncthing; do
  [ -x "$c" ] && { ST_SRC="$c"; break; }
done
if [ -n "$ST_SRC" ]; then
  install -d "$DEST/var/lib/fluxbee/dist/vendor/syncthing"
  install -m0755 "$ST_SRC" "$DEST/var/lib/fluxbee/dist/vendor/syncthing/syncthing"
  ST_CFG=""
  for c in "$(dirname "$ST_SRC")/config.xml" vendor/syncthing/config.xml; do
    [ -f "$c" ] && { ST_CFG="$c"; break; }
  done
  [ -n "$ST_CFG" ] && install -m0644 "$ST_CFG" "$DEST/var/lib/fluxbee/dist/vendor/syncthing/config.xml"
  python3 - "$DEST/var/lib/fluxbee/dist/vendor" "$ST_SRC" "$BUILD_ID" "${ST_CFG:-}" <<'PY'
import json, hashlib, os, sys, re
vroot, st_src, build_id, st_cfg = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
def comp(path, rel):
    b = open(path, "rb").read()
    return {"upstream_version": ver, "hash": "sha256:" + hashlib.sha256(b).hexdigest(),
            "size": len(b), "path": rel}
m = re.search(r"-v([0-9][A-Za-z0-9.+-]*)", os.path.basename(os.path.dirname(st_src)))
ver = m.group(1) if m else "repo-seeded"
comps = {"syncthing": comp(st_src, "syncthing/syncthing")}
if st_cfg:
    comps["syncthing_config"] = comp(st_cfg, "syncthing/config.xml")
json.dump({"schema_version": 1, "version": int(build_id), "components": comps},
          open(os.path.join(vroot, "manifest.json"), "w"), indent=2, sort_keys=True)
print("vendor manifest: syncthing %s (%d components)" % (ver, len(comps)))
PY
elif [ "${FLUXBEE_ALLOW_NO_SYNCTHING:-0}" = "1" ]; then
  echo "WARN: no vendored syncthing under vendor/ (FLUXBEE_ALLOW_NO_SYNCTHING=1) — blob/dist sync will be non-operational; add_hive with require_dist_sync will FAIL far from this cause"
else
  # Fail-closed (mirrors scripts/install.sh): a .deb without syncthing ships an
  # orchestrator whose blob/dist-sync is dead, and a later add_hive with
  # require_dist_sync fails far from the cause. Opt out with FLUXBEE_ALLOW_NO_SYNCTHING=1.
  echo "ERROR: no vendored syncthing under vendor/ (expected vendor/syncthing/syncthing or vendor/<bundle>/syncthing)." >&2
  echo "       Set FLUXBEE_ALLOW_NO_SYNCTHING=1 to build anyway (blob/dist sync will be non-operational)." >&2
  exit 1
fi

# systemd units (mirrors scripts/install.sh install_unit). Not enabled here; the
# postinst enables only sy-orchestrator, which brings up the rest (its bootstrap
# starts rt-gateway + the SY services in the Model D' order).
gen_unit() { # <name> [after] [wants] [role_regex]
  local name="$1" after="${2:-network.target}" wants="${3:-}" role_regex="${4:-}"
  { echo "[Unit]"; echo "Description=Fluxbee $name"; echo "After=$after"
    [ -n "$wants" ] && echo "Wants=$wants"
    echo; echo "[Service]"; echo "Type=simple"
    # Role-restricted units mirror the binary's own hive-role guard as an ExecCondition,
    # so a foreign-role start (e.g. pulled in via another unit's Wants=) skips cleanly
    # instead of crash-looping on the binary's wrong-role exit under Restart=always.
    [ -n "$role_regex" ] && echo "ExecCondition=/bin/sh -c 'grep -qE \"^role:[[:space:]]*($role_regex)\" /etc/fluxbee/hive.yaml'"
    echo "ExecStart=/usr/bin/$name"
    echo "Restart=always"; echo "RestartSec=5"
    # Bound shutdown so a stop/restart/upgrade can't hang on the systemd default (90s) then SIGKILL.
    # The binaries exit promptly on SIGTERM; sy-orchestrator no longer tears the hive down on exit.
    echo "TimeoutStopSec=15"; echo
    echo "[Install]"; echo "WantedBy=multi-user.target"; } > "$DEST/lib/systemd/system/$name.service"
}
# Units whose binary exits on the wrong hive role (sy-admin/sy-storage=motherbee,
# sy-identity=motherbee|worker) get an ExecCondition mirroring that guard — otherwise a
# Wants= pull-in (e.g. io-cloud.service) starts them on a foreign role and Restart=always
# turns the binary's clean wrong-role exit into an infinite crash-loop.
unit_role_gate() { case "$1" in
  sy-admin|sy-storage) echo "motherbee" ;;
  sy-identity) echo "motherbee|worker" ;;
  *) echo "" ;;
esac; }
for u in "${UNITS[@]}"; do gen_unit "$u" "network.target" "" "$(unit_role_gate "$u")"; done
gen_unit sy-frontdesk-gov "network.target rt-gateway.service sy-identity.service" \
  "rt-gateway.service sy-identity.service"
# sy-edge (ingress public door) needs the local router up to connect. Its public
# TLS material lives in the motherbee vault over the mesh, so systemd cannot model
# that dependency locally; the binary fails closed and Restart=always retries until
# the remote vault path is reachable.
gen_unit sy-edge "network.target rt-gateway.service" \
  "rt-gateway.service"

# io-cloud: the singleton in-mesh Fluxbee Cloud adapter — one per SYSTEM, on motherbee.
# Custom unit (not gen_unit): it needs SY.identity (register its own ICH) + SY.admin
# (externalize) up first, an optional EnvironmentFile for deployment overrides (e.g.
# IO_CLOUD_EDGE_NODE to publish a URL), and an ExecCondition that gates it to
# `role: motherbee` — so even though the unit is enabled, it only runs on the motherbee.
cat > "$DEST/lib/systemd/system/io-cloud.service" <<'UNIT'
[Unit]
Description=Fluxbee IO.cloud (singleton in-mesh Fluxbee Cloud adapter, motherbee only)
After=network.target rt-gateway.service sy-identity.service sy-admin.service
Wants=rt-gateway.service sy-identity.service sy-admin.service

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/io-cloud.env
ExecCondition=/bin/sh -c 'grep -qE "^role:[[:space:]]*motherbee" /etc/fluxbee/hive.yaml'
ExecStart=/usr/bin/io-cloud
Restart=always
RestartSec=5
TimeoutStopSec=15

[Install]
WantedBy=multi-user.target
UNIT

# io-blob: the motherbee-local public artifact curator. SY.admin is the only
# accepted mesh caller; the node has no ICH and exposes no network listener.
cat > "$DEST/lib/systemd/system/io-blob.service" <<'UNIT'
[Unit]
Description=Fluxbee IO.blob (public artifact curator, motherbee only)
After=network.target rt-gateway.service sy-admin.service
Wants=rt-gateway.service sy-admin.service

[Service]
Type=simple
EnvironmentFile=-/etc/fluxbee/io-blob.env
ExecCondition=/bin/sh -c 'grep -qE "^role:[[:space:]]*motherbee" /etc/fluxbee/hive.yaml'
Group=fluxbee
UMask=0027
ExecStart=/usr/bin/io-blob
Restart=always
RestartSec=5
TimeoutStopSec=15

[Install]
WantedBy=multi-user.target
UNIT

# config template + first-boot helper. The packaging template is a clean
# fresh-motherbee config (no lab uplink; wan.mtls set) — distinct from the dev
# config/hive.yaml the lab uses.
install -m0644 packaging/hive.yaml.example "$DEST/etc/fluxbee/hive.yaml.example"
install -m0600 packaging/io-cloud.env.example "$DEST/etc/fluxbee/io-cloud.env.example"
install -m0600 packaging/io-blob.env.example "$DEST/etc/fluxbee/io-blob.env.example"
install -m0755 packaging/fluxbee-firstboot "$DEST/usr/share/fluxbee/fluxbee-firstboot"
ln -sf ../share/fluxbee/fluxbee-firstboot "$DEST/usr/bin/fluxbee-firstboot"
# The base-node manifest travels to the target too: fluxbee-firstboot reads it to know which
# runtimes to auto-spawn as default instances at boot (boot=true) and under which names.
install -m0644 packaging/base-nodes.json "$DEST/usr/share/fluxbee/base-nodes.json"

echo "== [4/5] debian metadata =="
INSTALLED_KB="$(du -sk "$DEST" | awk '{print $1}')"
cat > "$DEST/DEBIAN/control" <<EOF
Package: fluxbee
Version: ${VERSION}
Section: net
Priority: optional
Architecture: ${ARCH}
Depends: adduser, openssl, libc6 (>= 2.39), postgresql, curl, python3
Installed-Size: ${INSTALLED_KB}
Maintainer: 4i Platform <ops@4iplatform.com>
Description: Fluxbee internal-network orchestration mesh
 Core services (router, orchestrator, identity, vault, storage, admin,
 architect, cognition, policy, timer, wf-rules, opa-rules, frontdesk, edge)
 plus the singleton IO.cloud adapter and IO.blob public artifact curator (motherbee),
 and the instanced IO/AI runtimes (io.api, io.slack, io.wapp, ai.generic,
 wf.engine, io.linkedhelper) seeded under dist/runtimes per packaging/base-nodes.json.
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

# Preflight: dpkg-deb needs room for the compressed .deb PLUS xz working space next to the ~1GB
# staged tree. On a full disk it can exit 0 while writing a truncated (data-less) .deb — so refuse
# to build unless there is comfortable headroom, and fail LOUD (a silent 1806-byte .deb once shipped
# a "successful" build that could never install). Threshold: staged size + 1 GiB margin.
DEST_KB="$(du -sk "$DEST" | awk '{print $1}')"
NEED_KB=$(( DEST_KB + 1048576 ))
FREE_KB="$(df -Pk "$ROOT_DIR/dist" | awk 'NR==2{print $4}')"
if [ "$FREE_KB" -lt "$NEED_KB" ]; then
  echo "ERROR: insufficient disk to build the .deb: need ~$((NEED_KB/1024)) MB free on $(dirname "$OUT"), have $((FREE_KB/1024)) MB." >&2
  echo "       Free space (old .debs in dist/, cargo target/, /tmp) and retry." >&2
  exit 1
fi

dpkg-deb --root-owner-group --build "$DEST" "$OUT"

# Post-build integrity: a healthy fluxbee .deb is ~250 MB with a listable data archive. A tiny .deb
# (only control.tar, no data.tar) means dpkg-deb was truncated (e.g. ENOSPC swallowed by xz). Catch
# it here instead of "publishing" a package that installs to nothing.
OUT_BYTES="$(stat -c %s "$OUT" 2>/dev/null || echo 0)"
if [ "$OUT_BYTES" -lt $((50 * 1024 * 1024)) ] || ! dpkg-deb -c "$OUT" >/dev/null 2>&1; then
  echo "ERROR: built .deb is broken (size=${OUT_BYTES} bytes, data archive unreadable) — likely a" >&2
  echo "       truncated dpkg-deb write (disk full during xz). Removing it; NOT a usable package." >&2
  rm -f "$OUT"
  exit 1
fi
echo "built: $OUT ($((OUT_BYTES/1024/1024)) MB)"
dpkg-deb --info "$OUT" | sed -n '1,12p'
