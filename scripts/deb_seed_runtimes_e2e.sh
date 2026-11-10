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
check "io.api records the package's version" "$(q "$M1" "d['runtimes']['io.api']['package_version']")" "$VER"
check "ai.generic records the package's version" "$(q "$M1" "d['runtimes']['ai.generic']['package_version']")" "$VER"

echo "== T2: THE U-3 CASE — dpkg deleted the manifest, a hot runtime must survive =="
D2="$WORK/t2"; seed_artifacts "$D2" "$VER"
# A hot-published runtime the operator created. NOTE: the previous base version 0.1.2 gets NO
# directory on purpose — dpkg owns those files and removes them during the unpack. Surviving the
# unpack is precisely what marks a version as operator-published rather than package-shipped.
mkdir -p "$D2/runtimes/wf.router/0.0.4/bin"
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
check "the version dpkg removed is pruned" "$(q "$M2" "'0.1.2' in d['runtimes']['io.api']['available']")" "False"
check "manifest version advanced" "$(q "$M2" "d['version'] > 1")" "True"
check "a hot runtime the package did not bring has no package version" "$(q "$M2" "'package_version' in d['runtimes']['wf.router']")" "False"
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

echo "== T2c: an upgrade must NOT demote a hot-published version of a BASE runtime =="
D2c="$WORK/t2c"; seed_artifacts "$D2c" "$VER"
# The operator published io.api 9.9.9 themselves and made it current. Its directory survives the
# unpack because dpkg does not own it — so the upgrade must leave `current` alone.
mkdir -p "$D2c/runtimes/io.api/9.9.9/bin"
cat >"$D2c/runtimes/manifest.json" <<JSON
{"schema_version":1,"version":1,"updated_at":null,"runtimes":{
 "io.api":{"available":["9.9.9"],"current":"9.9.9"}}}
JSON
run_seed "$D2c" >/dev/null
M2c="$D2c/runtimes/manifest.json"
check "hot-published current preserved" "$(q "$M2c" "d['runtimes']['io.api']['current']")" "9.9.9"
check "the package version is still offered" "$(q "$M2c" "'$VER' in d['runtimes']['io.api']['available']")" "True"
check "the package version is recorded, the way back (A-122)" "$(q "$M2c" "d['runtimes']['io.api']['package_version']")" "$VER"

echo "== T2d: a hand publish keeps the package version the seed recorded (A-122) =="
mkdir -p "$WORK/t2d-src"; printf '#!/bin/sh\ntrue\n' >"$WORK/t2d-src/io-api"; chmod 0755 "$WORK/t2d-src/io-api"
bash "$PUBLISH" --runtime io.api --version 9.9.10 --binary "$WORK/t2d-src/io-api" \
  --dist-root "$D2c" --set-current >/dev/null
check "the hand publish is current" "$(q "$M2c" "d['runtimes']['io.api']['current']")" "9.9.10"
check "the package version survives it" "$(q "$M2c" "d['runtimes']['io.api']['package_version']")" "$VER"
mkdir -p "$WORK/t2d-other"; printf '#!/bin/sh\nfalse\n' >"$WORK/t2d-other/io-api"; chmod 0755 "$WORK/t2d-other/io-api"
rc=0
bash "$PUBLISH" --runtime io.api --version "$VER" --binary "$WORK/t2d-other/io-api" \
  --dist-root "$D2c" --set-current >/dev/null 2>&1 || rc=$?
check "a hand publish over the package's own version is refused" "$([ "$rc" -ne 0 ] && echo yes || echo no)" "yes"
check "and its files stay the package's" "$(cmp -s "$WORK/t2d-other/io-api" "$D2c/runtimes/io.api/$VER/bin/io-api" && echo replaced || echo kept)" "kept"

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

echo "== T8: a CORRUPT live manifest must not silently erase hot-published runtimes =="
D8="$WORK/t8"; seed_artifacts "$D8" "$VER"
mkdir -p "$D8/runtimes/wf.router/0.0.4/bin"
SNAP8="$WORK/t8-snapshot.json"
cat >"$SNAP8" <<JSON
{"schema_version":1,"version":1,"updated_at":null,"runtimes":{
 "wf.router":{"available":["0.0.4"],"current":"0.0.4"}}}
JSON
# Present, but not valid JSON — the merger would swallow this and start from a blank document.
printf 'this is not json {{{' >"$D8/runtimes/manifest.json"
run_seed "$D8" "$SNAP8" >/dev/null 2>&1
check "hot runtime recovered from the snapshot" \
  "$(q "$D8/runtimes/manifest.json" "d['runtimes']['wf.router']['current']")" "0.0.4"
check "the corrupt file was kept for inspection" \
  "$(ls "$D8/runtimes/" | grep -c 'manifest.json.corrupt.' || true)" "1"

echo "== T9: corrupt manifest AND no snapshot -> refuse, never merge over it =="
D9="$WORK/t9"; seed_artifacts "$D9" "$VER"
printf 'not json either' >"$D9/runtimes/manifest.json"
rc=0; run_seed "$D9" "$WORK/absent-snapshot.json" >/dev/null 2>&1 || rc=$?
check "seed refuses" "$([ "$rc" -ne 0 ] && echo yes || echo no)" "yes"
check "the operator's file is untouched" \
  "$(cat "$D9/runtimes/manifest.json")" "not json either"

echo "== T10: registering ZERO runtimes is never success =="
D10="$WORK/t10"; seed_artifacts "$D10" "$VER"
EMPTY_NODES="$WORK/empty-nodes.json"
echo '{"schema_version":1,"singletons":[],"runtimes":[]}' >"$EMPTY_NODES"
SNAP10="$WORK/t10-snapshot.json"
echo '{"schema_version":1,"version":1,"runtimes":{}}' >"$SNAP10"
rc=0
FLUXBEE_BASE_NODES="$EMPTY_NODES" FLUXBEE_DIST_ROOT="$D10" \
  FLUXBEE_PUBLISH_RUNTIME="$PUBLISH" FLUXBEE_BASE_RUNTIME_VERSION="$VER" \
  FLUXBEE_MANIFEST_SNAPSHOT="$SNAP10" bash "$SEED" >/dev/null 2>&1 || rc=$?
check "seed exits non-zero on zero runtimes" "$([ "$rc" -ne 0 ] && echo yes || echo no)" "yes"
check "snapshot NOT consumed" "$([ -f "$SNAP10" ] && echo yes || echo no)" "yes"

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

echo "== T11: one writer of the manifest at a time (FINDINGS A-171) =="
D11="$WORK/t11"; seed_artifacts "$D11" "$VER"
run_seed "$D11" >/dev/null
M11="$D11/runtimes/manifest.json"
L11="$D11/runtimes-manifest.lock"
check "the lock sits in the dist root" "$([ -f "$L11" ] && echo yes || echo no)" "yes"
check "and nothing of it in the synced runtimes folder" \
  "$(ls -A "$D11/runtimes" | grep -c 'lock' || true)" "0"
mkdir -p "$WORK/t11-src"; printf '#!/bin/sh\ntrue\n' >"$WORK/t11-src/io-api"; chmod 0755 "$WORK/t11-src/io-api"
# Another writer (SY.admin, say) holds the lock for 3 s; a hand publish waits for it, then writes.
flock "$L11" sh -c 'touch "$1"; sleep 3' _ "$WORK/t11-held" &
holder=$!
until [ -f "$WORK/t11-held" ]; do sleep 0.1; done
bash "$PUBLISH" --runtime io.api --version 9.9.11 --binary "$WORK/t11-src/io-api" \
  --dist-root "$D11" --set-current >/dev/null &
publisher=$!
sleep 1
check "the publish waits while another writer holds the lock" \
  "$(q "$M11" "'9.9.11' in d['runtimes']['io.api']['available']")" "False"
wait "$holder"
rc=0; wait "$publisher" || rc=$?
check "and writes once the lock is free" "$rc $(q "$M11" "d['runtimes']['io.api']['current']")" "0 9.9.11"
# The seed's restore waits for it too; the hand publish stays current through the seed.
flock "$L11" sh -c 'touch "$1"; sleep 2' _ "$WORK/t11-held-2" &
holder=$!
until [ -f "$WORK/t11-held-2" ]; do sleep 0.1; done
started=$SECONDS
rc=0; run_seed "$D11" >/dev/null 2>&1 || rc=$?
waited=$((SECONDS - started))
wait "$holder"
check "the seed runs once the lock is free" "$rc $([ "$waited" -ge 1 ] && echo waited || echo "did not wait")" "0 waited"
check "the hand publish stays current" "$(q "$M11" "d['runtimes']['io.api']['current']")" "9.9.11"

echo "== T12: a hand publish never merges over a manifest it cannot read; it writes one whole =="
mkdir -p "$WORK/t12-src"; printf '#!/bin/sh\ntrue\n' >"$WORK/t12-src/io-api"; chmod 0755 "$WORK/t12-src/io-api"
# One that is there but does not parse, is no JSON object, or holds runtimes that are no object:
# refused before anything is written, the manifest as it was and no version directory.
n=0
for bad in 'this is not json {{{' '["io.api"]' '{"schema_version":1,"version":1,"runtimes":[]}'; do
  n=$((n + 1))
  D12="$WORK/t12-$n"; mkdir -p "$D12/runtimes"
  printf '%s' "$bad" >"$D12/runtimes/manifest.json"
  rc=0; err="$(bash "$PUBLISH" --runtime io.api --version 9.9.12 --binary "$WORK/t12-src/io-api" \
    --dist-root "$D12" --set-current 2>&1 >/dev/null)" || rc=$?
  check "refused over a manifest it cannot merge over ($n)" \
    "$([ "$rc" -ne 0 ] && echo refused || echo published) $(cat "$D12/runtimes/manifest.json")" \
    "refused $bad"
  check "and it says why, writing nothing ($n)" \
    "$(printf '%s' "$err" | grep -c 'nothing was written') $([ -e "$D12/runtimes/io.api" ] && echo written || echo nothing)" \
    "1 nothing"
done
# One it cannot read (as root, any file reads: not checked then).
if [ "$(id -u)" -ne 0 ]; then
  D12u="$WORK/t12-unreadable"; seed_artifacts "$D12u" "$VER"; run_seed "$D12u" >/dev/null
  chmod 000 "$D12u/runtimes/manifest.json"
  rc=0; bash "$PUBLISH" --runtime io.api --version 9.9.12 --binary "$WORK/t12-src/io-api" \
    --dist-root "$D12u" --set-current >/dev/null 2>&1 || rc=$?
  chmod 0644 "$D12u/runtimes/manifest.json"
  check "refused over a manifest it cannot read" \
    "$([ "$rc" -ne 0 ] && echo refused || echo published) $(q "$D12u/runtimes/manifest.json" "d['runtimes']['io.api']['current']")" \
    "refused $VER"
fi
# No manifest at all: the first publish starts one.
D12e="$WORK/t12-empty"; mkdir -p "$D12e"
rc=0; bash "$PUBLISH" --runtime io.api --version 9.9.12 --binary "$WORK/t12-src/io-api" \
  --dist-root "$D12e" --set-current >/dev/null 2>&1 || rc=$?
check "a dist without a manifest gets one" "$rc $(q "$D12e/runtimes/manifest.json" "d['runtimes']['io.api']['current']")" "0 9.9.12"
# Written whole: a temp file beside the manifest, renamed over it, and none left behind.
check "no temp file is left beside the manifest" \
  "$(ls -A "$D12e/runtimes" "$D11/runtimes" | grep -c '^\.manifest\.json\.' || true)" "0"

echo "== T13: the seed decides 'current' under the lock, and its publishes take it on its descriptor =="
D13="$WORK/t13"; seed_artifacts "$D13" "$VER"
L13="$D13/runtimes-manifest.lock"
# Each publish the seed runs finds the lock held, by the seed, from before the seed read `current`.
cat >"$WORK/t13-publish.sh" <<EOF
#!/usr/bin/env bash
if flock -n "$L13" true; then echo free; else echo held; fi >>"$WORK/t13-lock-states"
exec bash "$PUBLISH" "\$@"
EOF
rc=0
FLUXBEE_BASE_NODES="$BASE_NODES" FLUXBEE_DIST_ROOT="$D13" FLUXBEE_PUBLISH_RUNTIME="$WORK/t13-publish.sh" \
  FLUXBEE_BASE_RUNTIME_VERSION="$VER" FLUXBEE_MANIFEST_SNAPSHOT="$WORK/nosnap.json" \
  bash "$SEED" >/dev/null 2>&1 || rc=$?
check "the seed holds the lock through every publish it runs" "$rc $(sort -u "$WORK/t13-lock-states" | tr '\n' ' ')" "0 held "
check "and its runtimes are registered" "$(q "$D13/runtimes/manifest.json" "sorted(d['runtimes'])")" "['ai.generic', 'io.api']"
# A caller that holds the lock names its descriptor: the publish takes the lock on it, at once.
rc=0
(
  exec 8>>"$L13"
  flock 8
  FLUXBEE_MANIFEST_LOCK_FD=8 bash "$PUBLISH" --runtime io.api --version 9.9.13 \
    --binary "$WORK/t12-src/io-api" --dist-root "$D13" --set-current >/dev/null 2>&1
) || rc=$?
check "a publish under its caller's lock goes on" "$rc $(q "$D13/runtimes/manifest.json" "d['runtimes']['io.api']['current']")" "0 9.9.13"
# A descriptor of another file never stands for the lock: refused, the manifest as it was.
rc=0
FLUXBEE_MANIFEST_LOCK_FD=7 bash "$PUBLISH" --runtime io.api --version 9.9.14 \
  --binary "$WORK/t12-src/io-api" --dist-root "$D13" --set-current 7</dev/null >/dev/null 2>&1 || rc=$?
check "a descriptor that is not the lock file's is refused" \
  "$([ "$rc" -ne 0 ] && echo refused || echo published) $(q "$D13/runtimes/manifest.json" "d['runtimes']['io.api']['current']")" \
  "refused 9.9.13"

echo
echo "passed=$PASS failed=$FAIL"
[ "$FAIL" -eq 0 ]
