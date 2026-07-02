#!/usr/bin/env bash
# Real onboarding test: exercise the actual UI-driven install flow against the
# REAL cloud (not the mock), from a clean Ubuntu container.
#
# It proves that, with ONLY an enrollment token (the tenant is bound to it
# server-side), the installer downloads the adapter binary from the cloud and
# enrolls — i.e. "advance with just the token".
#
# Flow: build a linux-x64 binary in a rust container -> stage it + a manifest
# into the cloud's release dir -> (pre-check) download it from the cloud with the
# token -> run a clean ubuntu container that fetches /install.sh and runs it with
# --token --no-service (systemd isn't available in a plain container; the service
# path is covered by the Proxmox VM harness) -> assert the adapter enrolled.
#
# PRECONDITIONS (you do these):
#   1. Cloud running locally started WITH:
#        FLUXBEE_LH_ADAPTER_RELEASES_PATH=<releases-dir>/manifest.json
#        FLUXBEE_LH_ADAPTER_INSTALL_SCRIPT=<fluxbee>/nodes/io/adapters/linked-helper/packaging/install-linkedhelper-adapter.sh
#      (releases-dir must match --releases-dir below; default printed on run)
#   2. An enrollment token issued from the UI ("Generate installation token").
#
# Usage:
#   bash scripts/test-adapter-onboarding-docker.sh --token lhenr_XXXX \
#     [--releases-dir DIR] [--cloud http://host.docker.internal:3002] \
#     [--host-cloud http://localhost:3002] [--crate PATH]
set -euo pipefail

TOKEN=""
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="${ADAPTER_CRATE:-$HERE/../nodes/io/adapters/linked-helper/adapter-rs}"
RELEASES_DIR="${RELEASES_DIR:-/private/tmp/lh-releases}"
CLOUD="http://host.docker.internal:3002"     # reachable from inside the container
HOST_CLOUD="http://localhost:3002"           # reachable from this Mac (pre-checks)
while [ $# -gt 0 ]; do
  case "$1" in
    --token) TOKEN="$2"; shift 2;;
    --releases-dir) RELEASES_DIR="$2"; shift 2;;
    --cloud) CLOUD="$2"; shift 2;;
    --host-cloud) HOST_CLOUD="$2"; shift 2;;
    --crate) CRATE="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
[ -n "$TOKEN" ] || { echo "need --token <enrollment-token> (issue it from the UI)" >&2; exit 1; }
command -v docker >/dev/null || { echo "docker not found" >&2; exit 1; }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/lh-onb-XXXXXX")"
PASS=0; FAIL=0
log() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok()  { printf '\033[1;32m  PASS\033[0m %s\n' "$*"; PASS=$((PASS+1)); }
bad() { printf '\033[1;31m  FAIL\033[0m %s\n' "$*"; FAIL=$((FAIL+1)); }
trap 'rm -rf "$WORK"' EXIT

VER="$(grep -m1 '^version' "$CRATE/Cargo.toml" | sed 's/.*"\(.*\)".*/\1/')"
RELEASE_ID="lh-adapter-$VER-linux-x64"
INSTALL_SCRIPT="$(cd "$CRATE/.." && pwd)/packaging/install-linkedhelper-adapter.sh"

log "cloud must have been started with:"
echo "    FLUXBEE_LH_ADAPTER_RELEASES_PATH=$RELEASES_DIR/manifest.json"
echo "    FLUXBEE_LH_ADAPTER_INSTALL_SCRIPT=$INSTALL_SCRIPT"

# --- 0. cloud reachable from the Mac ----------------------------------------
log "checking cloud health at $HOST_CLOUD"
curl -fsS "$HOST_CLOUD/health" >/dev/null || { echo "cloud not reachable at $HOST_CLOUD/health" >&2; exit 1; }

# --- 1. build a linux-x64 binary in a rust container ------------------------
log "building linux-x64 adapter binary in a rust container (first run pulls the image)"
docker run --rm -v "$CRATE":/src -v "$WORK/target":/target -e CARGO_TARGET_DIR=/target \
  -w /src rust:1 cargo build --release --quiet
BIN="$WORK/target/release/adapter-rs"
[ -x "$BIN" ] || { echo "build did not produce $BIN" >&2; exit 1; }

# --- 2. stage artifact + manifest into the cloud release dir ----------------
log "staging release into $RELEASES_DIR"
mkdir -p "$RELEASES_DIR"
cp "$BIN" "$RELEASES_DIR/$RELEASE_ID"
SHA="$(shasum -a 256 "$RELEASES_DIR/$RELEASE_ID" | awk '{print $1}')"
SIZE="$(wc -c < "$RELEASES_DIR/$RELEASE_ID" | tr -d ' ')"
cat > "$RELEASES_DIR/manifest.json" <<EOF
{ "channels": { "stable": { "linux-x64": {
  "latestVersion": "$VER", "minSupportedVersion": "0.0.0",
  "releaseId": "$RELEASE_ID", "artifact": "$RELEASE_ID",
  "sha256": "$SHA", "size": $SIZE, "sig": null } } } }
EOF
log "release $RELEASE_ID  sha=${SHA:0:12}  size=$SIZE"

# --- 3. pre-check: cloud serves the binary with the token (fail fast) -------
log "pre-check: downloading the binary from the cloud with the token"
code="$(curl -s -o "$WORK/dl" -w '%{http_code}' -H "Authorization: Bearer $TOKEN" \
  "$HOST_CLOUD/api/adapters/linkedhelper/download?os=linux&arch=x64")"
if [ "$code" = "200" ] && [ "$(shasum -a 256 "$WORK/dl" | awk '{print $1}')" = "$SHA" ]; then
  ok "cloud served the binary via token (HTTP 200, sha matches)"
else
  bad "download pre-check failed (HTTP $code) — is the cloud started with the releases env + a valid token?"
  echo; log "RESULT: $PASS passed, $FAIL failed"; exit 1
fi

# --- 4. run the real installer in a clean ubuntu container ------------------
log "running the real installer in a clean ubuntu:24.04 container"
CONTAINER_SCRIPT='set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null && apt-get install -y -qq curl ca-certificates >/dev/null
curl -fsSL "'"$CLOUD"'/api/adapters/linkedhelper/install.sh" | bash -s -- --cloud "'"$CLOUD"'" --token "'"$TOKEN"'" --no-service
echo "===STATUS==="
/opt/fluxbee/lh-adapter/adapter-rs --state-file /var/lib/fluxbee/lh-adapter/state.json status
echo "===RUNONCE==="
/opt/fluxbee/lh-adapter/adapter-rs --state-file /var/lib/fluxbee/lh-adapter/state.json run --once
echo "===END==="'
docker run --rm --add-host=host.docker.internal:host-gateway ubuntu:24.04 \
  bash -c "$CONTAINER_SCRIPT" >"$WORK/container.log" 2>&1 || true

# --- 5. assert the adapter enrolled against the real cloud ------------------
sed -n '/===STATUS===/,/===RUNONCE===/p' "$WORK/container.log" | sed '1d;$d' > "$WORK/status.json"
ADAPTER_ID="$(python3 -c "import sys,json; print(json.load(open('$WORK/status.json'))['state']['adapterId'])" 2>/dev/null || echo '')"
TENANT_ID="$(python3 -c "import sys,json; print(json.load(open('$WORK/status.json'))['state']['tenantId'])" 2>/dev/null || echo '')"

case "$ADAPTER_ID" in adp*) ok "enrolled: cloud issued adapterId=$ADAPTER_ID";; *) bad "no cloud-issued adapterId (got '$ADAPTER_ID')";; esac
[ -n "$TENANT_ID" ] && ok "tenant bound from token: tenantId=$TENANT_ID" || bad "no tenantId resolved from token"
grep -q '"action": "run_cycle_completed"' "$WORK/container.log" && ok "adapter completed an alive cycle against the real cloud" || bad "no successful alive cycle (see log)"

echo
if [ "$FAIL" -ne 0 ]; then
  log "container log tail:"; tail -30 "$WORK/container.log"
fi
log "RESULT: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
