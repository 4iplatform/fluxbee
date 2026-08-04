#!/usr/bin/env bash
# U-3 regression net: packaging/fluxbee-seed-runtimes must MERGE the base runtimes into the
# live dist manifest, never replace it.
#
# Hermetic: no hive, no dpkg, no root. Drives the seed purely through its FLUXBEE_* overrides
# against a `mktemp -d`, with dummy executables standing in for the runtime binaries.
#
# Usage: scripts/deb_seed_runtimes_e2e.sh
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
SEED="$ROOT_DIR/packaging/fluxbee-seed-runtimes"
PUBLISH="$ROOT_DIR/scripts/publish-runtime.sh"

PASS=0
FAIL=0
ok()   { PASS=$((PASS+1)); echo "  ok   — $1"; }
bad()  { FAIL=$((FAIL+1)); echo "  FAIL — $1" >&2; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (got '$2', want '$3')"; fi; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A base-node manifest with two runtimes, matching the real schema.
BASE_NODES="$WORK/base-nodes.json"
cat >"$BASE_NODES" <<'JSON'
{
  "schema_version": 1,
  "singletons": [],
  "runtimes": [
    {"runtime": "io.api",  "crate": "io-api",  "bin": "io-api",  "boot": true},
    {"runtime": "ai.generic", "crate": "ai-generic", "bin": "ai_node_runner", "boot": true}
  ]
}
JSON

VER=0.1.3

# Lay down the artifact tree the .deb would have unpacked.
seed_artifacts() {
  local root="$1" ver="$2"
  for pair in "io.api:io-api" "ai.generic:ai_node_runner"; do
    local rt="${pair%%:*}" bin="${pair##*:}"
    mkdir -p "$root/runtimes/$rt/$ver/bin"
    printf '#!/bin/sh\ntrue\n' > "$root/runtimes/$rt/$ver/bin/$bin"
    chmod 0755 "$root/runtimes/$rt/$ver/bin/$bin"
  done
}

run_seed() {
  local dist="$1" snap="${2:-$WORK/nosnap.json}"
  FLUXBEE_BASE_NODES="$BASE_NODES" \
  FLUXBEE_DIST_ROOT="$dist" \
  FLUXBEE_PUBLISH_RUNTIME="$PUBLISH" \
  FLUXBEE_BASE_RUNTIME_VERSION="$VER" \
  FLUXBEE_MANIFEST_SNAPSHOT="$snap" \
  bash "$SEED"
}

q() { # q <manifest> <python-expr over `d`>
  python3 -c "
import json,sys
d=json.load(open(sys.argv[1]))
print($2)
" "$1"
}

echo "== T1: fresh install — no manifest, no snapshot =="
D1="$WORK/t1"; seed_artifacts "$D1" "$VER"
run_seed "$D1" >/dev/null
M1="$D1/runtimes/manifest.json"
check "manifest created" "$([ -f "$M1" ] && echo yes || echo no)" "yes"
check "io.api current" "$(q "$M1" "d['runtimes']['io.api']['current']")" "$VER"
check "ai.generic current" "$(q "$M1" "d['runtimes']['ai.generic']['current']")" "$VER"

echo "== T2: THE U-3 CASE — dpkg deleted the manifest, a hot runtime must survive =="
D2="$WORK/t2"; seed_artifacts "$D2" "$VER"
# A hot-published runtime the operator created, plus the previous base version.
mkdir -p "$D2/runtimes/wf.router/0.0.4/bin" "$D2/runtimes/io.api/0.1.2/bin"
SNAP2="$WORK/t2-snapshot.json"
cat >"$SNAP2" <<JSON
{"schema_version":1,"version":1,"updated_at":null,"runtimes":{
 "wf.router":{"available":["0.0.4"],"current":"0.0.4"},
 "io.api":{"available":["0.1.2"],"current":"0.1.2"}}}
JSON
# The manifest itself is GONE — that is what dropping it from the payload does on the first
# upgrade, and why the preinst snapshot is mandatory.
run_seed "$D2" "$SNAP2" >/dev/null
M2="$D2/runtimes/manifest.json"
check "hot runtime survived" "$(q "$M2" "d['runtimes']['wf.router']['current']")" "0.0.4"
check "hot runtime dir intact" "$([ -d "$D2/runtimes/wf.router/0.0.4" ] && echo yes || echo no)" "yes"
check "base runtime advanced" "$(q "$M2" "d['runtimes']['io.api']['current']")" "$VER"
check "old base version kept (dir present)" "$(q "$M2" "'0.1.2' in d['runtimes']['io.api']['available']")" "True"
check "manifest version advanced" "$(q "$M2" "d['version'] > 1")" "True"
check "snapshot consumed on success" "$([ -f "$SNAP2" ] && echo yes || echo no)" "no"

echo "== T2b: --prune-missing retires a base version whose directory dpkg removed =="
D2b="$WORK/t2b"; seed_artifacts "$D2b" "$VER"
mkdir -p "$D2b/runtimes"
cat >"$D2b/runtimes/manifest.json" <<JSON
{"schema_version":1,"version":1,"updated_at":null,"runtimes":{
 "io.api":{"available":["0.1.2","$VER"],"current":"0.1.2"},
 "wf.router":{"available":["0.0.4"],"current":"0.0.4"}}}
JSON
run_seed "$D2b" >/dev/null
M2b="$D2b/runtimes/manifest.json"
check "dangling base version pruned" "$(q "$M2b" "'0.1.2' in d['runtimes']['io.api']['available']")" "False"
check "prune did NOT touch the hot runtime" "$(q "$M2b" "d['runtimes']['wf.router']['available']")" "['0.0.4']"

echo "== T3: steady state — manifest present, no snapshot =="
D3="$WORK/t3"; seed_artifacts "$D3" "$VER"
mkdir -p "$D3/runtimes/wf.router/0.0.4/bin"
cat >"$D3/runtimes/manifest.json" <<JSON
{"schema_version":1,"version":7,"updated_at":null,"runtimes":{
 "wf.router":{"available":["0.0.4"],"current":"0.0.4"}}}
JSON
run_seed "$D3" >/dev/null
check "merged in place, hot preserved" "$(q "$D3/runtimes/manifest.json" "d['runtimes']['wf.router']['current']")" "0.0.4"

echo "== T4: idempotence — a second run must not churn the runtimes object =="
D4="$WORK/t4"; seed_artifacts "$D4" "$VER"
run_seed "$D4" >/dev/null
A="$(q "$D4/runtimes/manifest.json" "json.dumps(d['runtimes'],sort_keys=True)")"
V1="$(q "$D4/runtimes/manifest.json" "d['version']")"
run_seed "$D4" >/dev/null
B="$(q "$D4/runtimes/manifest.json" "json.dumps(d['runtimes'],sort_keys=True)")"
V2="$(q "$D4/runtimes/manifest.json" "d['version']")"
check "runtimes byte-identical on re-run" "$A" "$B"
check "manifest version never goes backwards" "$([ "$V2" -ge "$V1" ] && echo yes || echo no)" "yes"

echo "== T5: missing artifact — fail loud, keep the snapshot, publish the rest =="
D5="$WORK/t5"; seed_artifacts "$D5" "$VER"
rm -f "$D5/runtimes/ai.generic/$VER/bin/ai_node_runner"
SNAP5="$WORK/t5-snapshot.json"
echo '{"schema_version":1,"version":1,"runtimes":{}}' >"$SNAP5"
rc=0; run_seed "$D5" "$SNAP5" >/dev/null 2>&1 || rc=$?
check "seed exits non-zero" "$([ "$rc" -ne 0 ] && echo yes || echo no)" "yes"
check "snapshot KEPT on failure" "$([ -f "$SNAP5" ] && echo yes || echo no)" "yes"
check "the healthy runtime still published" "$(q "$D5/runtimes/manifest.json" "d['runtimes']['io.api']['current']")" "$VER"

echo "== T6: --binary already AT the destination (GNU install src==dest) =="
D6="$WORK/t6"; seed_artifacts "$D6" "$VER"
rc=0
bash "$PUBLISH" --runtime io.api --version "$VER" \
  --binary "$D6/runtimes/io.api/$VER/bin/io-api" \
  --dist-root "$D6" --set-current >/dev/null 2>&1 || rc=$?
check "publish in place succeeds" "$rc" "0"
check "binary still there" "$([ -s "$D6/runtimes/io.api/$VER/bin/io-api" ] && echo yes || echo no)" "yes"

echo "== T7: --prune-missing requires --set-current =="
rc=0
bash "$PUBLISH" --runtime io.api --version "$VER" \
  --binary "$D6/runtimes/io.api/$VER/bin/io-api" \
  --dist-root "$D6" --prune-missing >/dev/null 2>&1 || rc=$?
check "rejected without --set-current" "$([ "$rc" -ne 0 ] && echo yes || echo no)" "yes"

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
