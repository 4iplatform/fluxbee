#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  publish-runtime.sh --runtime <name> --version <version> --binary <path> [options]

Required:
  --runtime <name>         Runtime key in dist manifest (example: ai.generic, io.slack)
  --version <version>      Runtime version (example: 0.1.0)
  --binary <path>          Built binary to publish

Options:
  --dist-root <path>       Dist root path (default: /var/lib/fluxbee/dist)
  --start-script <path>    The version's start script, instead of the plain one this script
                           writes. Its first line of the form exec "$SCRIPT_DIR/<binary>" "$@"
                           must run the binary published:
                             exec "$SCRIPT_DIR/<binary name>" "$@"
                           SY.orchestrator reads that line to know what the version runs.
  --file <path>            A file of the version, installed next to bin/ under its own name
                           (repeatable; ai.generic's tools.json, say)
  --set-current            Set this version as manifest.runtimes.<runtime>.current
  --prune-missing          Drop `available` versions of THIS runtime whose directory is gone
                           (requires --set-current). Used by the .deb install-time seed to
                           retire the base version dpkg just removed during unpack.
  --package                Record this version as manifest.runtimes.<runtime>.package_version:
                           the version the installed Fluxbee package brought. Used by the .deb
                           install-time seed; SY.admin's reset_base_runtime returns `current`
                           to it.
  --sudo                   Use sudo for writes to dist paths
  -h, --help               Show this help

Notes:
  - Every input is checked before anything is written.
  - Publishes binary to: <dist-root>/runtimes/<runtime>/<version>/bin/
  - Creates start script at: <dist-root>/runtimes/<runtime>/<version>/bin/start.sh
  - Installs each --file at: <dist-root>/runtimes/<runtime>/<version>/<its name>
  - Writes the manifest last, once every file of the version is in place:
    <dist-root>/runtimes/manifest.json. SY.orchestrator moves running nodes onto a version as
    soon as the manifest names it, so the manifest never names a version that is not all there.
  - Holds the manifest's lock, <dist-root>/runtimes-manifest.lock (beside the runtimes folder,
    which Syncthing shares, never inside it), from its read of the manifest to its write, as every
    writer of it does (SY.admin, fluxbee-publish, the .deb's seed): waits for another writer up to
    60 s, then fails with the manifest untouched.
  - A manifest that is there but cannot be read, does not parse, or is no manifest is refused
    before anything is written: merging over it would drop every other runtime. Only a manifest
    that does not exist starts empty. The manifest is written whole or not at all: a temp file
    beside it, renamed over it.
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: required command not found: $1" >&2
    exit 1
  fi
}

RUNTIME=""
VERSION=""
BINARY=""
DIST_ROOT="/var/lib/fluxbee/dist"
START_SCRIPT=""
FILES=()
SET_CURRENT=0
PRUNE_MISSING=0
PACKAGE=0
USE_SUDO=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --runtime)
      RUNTIME="${2:-}"
      shift 2
      ;;
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --binary)
      BINARY="${2:-}"
      shift 2
      ;;
    --dist-root)
      DIST_ROOT="${2:-}"
      shift 2
      ;;
    --start-script)
      START_SCRIPT="${2:-}"
      shift 2
      ;;
    --file)
      FILES+=("${2:-}")
      shift 2
      ;;
    --prune-missing)
      PRUNE_MISSING=1
      shift
      ;;
    --set-current)
      SET_CURRENT=1
      shift
      ;;
    --package)
      PACKAGE=1
      shift
      ;;
    --sudo)
      USE_SUDO=1
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

if [[ -z "$RUNTIME" || -z "$VERSION" || -z "$BINARY" ]]; then
  echo "Error: --runtime, --version and --binary are required" >&2
  usage
  exit 1
fi

if [[ "$PRUNE_MISSING" == "1" && "$SET_CURRENT" != "1" ]]; then
  echo "Error: --prune-missing requires --set-current" >&2
  exit 1
fi

if [[ ! -f "$BINARY" ]]; then
  echo "Error: binary does not exist: $BINARY" >&2
  exit 1
fi

BINARY_NAME="$(basename "$BINARY")"

if [[ -n "$START_SCRIPT" ]]; then
  if [[ ! -f "$START_SCRIPT" ]]; then
    echo "Error: start script does not exist: $START_SCRIPT" >&2
    exit 1
  fi
  # SY.orchestrator's `start_script_entry` reads what a version runs from the FIRST line that,
  # trimmed, has the form exec "$SCRIPT_DIR/<binary>" "$@". That line must run this binary: with
  # another one the version runs it, and with none the version is never ready.
  exec_line="exec \"\$SCRIPT_DIR/$BINARY_NAME\" \"\$@\""
  first_exec_line="$(sed -n \
    's/^[[:space:]]*//; s/[[:space:]]*$//; /^exec "\$SCRIPT_DIR\/.*" "\$@"$/{p;q;}' \
    "$START_SCRIPT")"
  if [[ "$first_exec_line" != "$exec_line" ]]; then
    echo "Error: start script $START_SCRIPT must run the binary with its first line of the form" \
      "exec \"\$SCRIPT_DIR/<binary>\" \"\$@\": $exec_line (found: ${first_exec_line:-none})" >&2
    exit 1
  fi
fi

# `${FILES[@]+...}`: an empty array under `set -u` is an error before bash 4.4.
for file in ${FILES[@]+"${FILES[@]}"}; do
  if [[ ! -f "$file" ]]; then
    echo "Error: file does not exist: $file" >&2
    exit 1
  fi
done

require_cmd python3
require_cmd sha256sum
require_cmd flock

SUDO=""
if [[ "$USE_SUDO" == "1" ]]; then
  SUDO="sudo"
fi

RUNTIMES_ROOT="$DIST_ROOT/runtimes"
RUNTIME_DIR="$RUNTIMES_ROOT/$RUNTIME/$VERSION"
BIN_DIR="$RUNTIME_DIR/bin"
MANIFEST_PATH="$RUNTIMES_ROOT/manifest.json"
# One writer of the manifest at a time (FINDINGS A-171): SY.admin, fluxbee-publish, this script and
# the .deb's seed each hold an flock on this file from their read of the manifest to their write, so
# none loses another's change. It sits in the dist root, beside the runtimes folder Syncthing shares
# with the workers (runtime_manifest_lock_path). The file stays: removing it would let a writer
# lock a new one beside one still held.
MANIFEST_LOCK="$DIST_ROOT/runtimes-manifest.lock"
# How long this script waits for another writer: above the longest hold, SY.admin's
# remove_runtime_version, whose read of the version's use (30 s) comes between its read and write.
MANIFEST_LOCK_WAIT_SECS=60
START_SH="$BIN_DIR/start.sh"

# The manifest's reader and merger, one program for both of its reads below (its mode first). A
# manifest that is there but cannot be read, does not parse, or is no manifest (a JSON object whose
# runtimes, when it has one, is an object) is refused, naming why, and nothing is written: merging
# over it from an empty document would drop every other runtime. Only a manifest that does not
# exist starts empty.
#   manifest_py package-version <manifest> <runtime>: the runtime's package_version, empty without
#     one;
#   manifest_py merge <manifest> <runtime> <version> <set_current> <runtimes_root> <prune_missing>
#     <package>: the merged manifest.
manifest_py() {
  python3 - "$@" <<'PY'
import datetime
import json
import pathlib
import sys
import time


def load_manifest(path):
    """The live manifest, None when it does not exist; any other failure ends the run."""
    try:
        raw = pathlib.Path(path).read_bytes()
    except FileNotFoundError:
        return None
    except OSError as err:
        sys.exit("Error: cannot read the runtimes manifest %s (%s); nothing was written"
                 % (path, err.strerror or err))
    try:
        doc = json.loads(raw.decode("utf-8"))
    except ValueError as err:
        sys.exit("Error: the runtimes manifest %s does not parse (%s); nothing was written"
                 % (path, err))
    if not isinstance(doc, dict) or not isinstance(doc.get("runtimes", {}), dict):
        sys.exit("Error: the runtimes manifest %s is not one (a JSON object whose runtimes is an"
                 " object); nothing was written" % path)
    return doc


mode = sys.argv[1]
if mode == "package-version":
    doc = load_manifest(sys.argv[2]) or {}
    entry = (doc.get("runtimes") or {}).get(sys.argv[3])
    print((entry.get("package_version") or "") if isinstance(entry, dict) else "")
    raise SystemExit(0)

manifest_path, runtime, version = sys.argv[2], sys.argv[3], sys.argv[4]
set_current = sys.argv[5] == "1"
runtimes_root = pathlib.Path(sys.argv[6])
prune_missing = sys.argv[7] == "1"
package = sys.argv[8] == "1"

doc = {
    "schema_version": 1,
    "version": 0,
    "updated_at": None,
    "runtimes": {},
}
loaded = load_manifest(manifest_path)
if loaded is not None:
    doc.update(loaded)

schema_version = doc.get("schema_version")
if not isinstance(schema_version, int) or schema_version < 1:
    schema_version = 1
doc["schema_version"] = schema_version
doc["version"] = int(time.time() * 1000)
doc["updated_at"] = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")

runtimes = doc.get("runtimes")
if not isinstance(runtimes, dict):
    runtimes = {}
doc["runtimes"] = runtimes

entry = runtimes.get(runtime)
if not isinstance(entry, dict):
    entry = {}

available = entry.get("available")
if not isinstance(available, list):
    available = []
available = [str(v) for v in available if str(v).strip()]
if version not in available:
    available.append(version)

# --prune-missing: retire versions of THIS runtime whose artifact directory is gone. Scoped to
# the runtime being published, so a hot-published entry is never touched. The version being
# published is always kept (its bin dir was just created).
if prune_missing:
    available = [
        v for v in available
        if v == version or (runtimes_root / runtime / v).is_dir()
    ]

entry["available"] = sorted(set(available))

if set_current or not isinstance(entry.get("current"), str) or not entry.get("current"):
    entry["current"] = version

# --package: the version the installed package brought, which SY.admin's reset_base_runtime
# returns `current` to (FINDINGS A-122). Any other publish keeps the one recorded.
if package:
    entry["package_version"] = version

runtimes[runtime] = entry

print(json.dumps(doc, indent=2, sort_keys=True))
PY
}

# The live manifest is read before anything is written: one this script cannot merge over stops it
# here (manifest_py). The version the installed package brought is the way back from a hand
# publish (reset_base_runtime, FINDINGS A-122): only the package's seed (--package) writes over it.
# A hand publish under the same version would replace the package's files and hide itself, current
# then being package_version.
if ! package_version="$(manifest_py package-version "$MANIFEST_PATH" "$RUNTIME")"; then
  exit 1
fi
if [[ "$PACKAGE" != "1" && -n "$package_version" && "$package_version" == "$VERSION" ]]; then
  echo "Error: $RUNTIME $VERSION is the version the installed package brought; publish under" \
    "another version (for example $VERSION-gate1)" >&2
  exit 1
fi

$SUDO install -d "$BIN_DIR"
# `--binary` may already BE the destination: the .deb ships the artifact tree and then
# registers it in place at install time (packaging/fluxbee-seed-runtimes). GNU install
# refuses src==dest with "are the same file", which would fail every postinst.
if [[ ! "$BINARY" -ef "$BIN_DIR/$BINARY_NAME" ]]; then
  $SUDO install -m 0755 "$BINARY" "$BIN_DIR/$BINARY_NAME"
fi

if [[ -n "$START_SCRIPT" ]]; then
  $SUDO install -m 0755 "$START_SCRIPT" "$START_SH"
else
  tmp_start="$(mktemp)"
  cat >"$tmp_start" <<EOF
#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="\$(cd "\$(dirname "\${BASH_SOURCE[0]}")" && pwd)"
exec "\$SCRIPT_DIR/$BINARY_NAME" "\$@"
EOF
  $SUDO install -m 0755 "$tmp_start" "$START_SH"
  rm -f "$tmp_start"
fi

for file in ${FILES[@]+"${FILES[@]}"}; do
  $SUDO install -m 0644 "$file" "$RUNTIME_DIR/$(basename "$file")"
done

# The manifest last: from here on SY.orchestrator may launch the version, and Syncthing carries
# it to the workers. Under its lock, from the read to the write.
[[ -e "$MANIFEST_LOCK" ]] || $SUDO touch "$MANIFEST_LOCK"
exec 9<"$MANIFEST_LOCK"
if ! flock -w "$MANIFEST_LOCK_WAIT_SECS" 9; then
  echo "Error: another writer of $MANIFEST_PATH held its lock ($MANIFEST_LOCK) for" \
    "${MANIFEST_LOCK_WAIT_SECS}s; the manifest is untouched" >&2
  exit 1
fi
tmp_manifest="$(mktemp)"
if ! manifest_py merge "$MANIFEST_PATH" "$RUNTIME" "$VERSION" "$SET_CURRENT" "$RUNTIMES_ROOT" \
    "$PRUNE_MISSING" "$PACKAGE" >"$tmp_manifest"; then
  rm -f "$tmp_manifest"
  exit 1
fi
# Written whole or not at all: a temp file beside the manifest, renamed over it, so a reader
# (SY.orchestrator, Syncthing) never sees half of one.
staged="$($SUDO mktemp "$RUNTIMES_ROOT/.manifest.json.XXXXXX")"
if ! $SUDO install -m 0644 "$tmp_manifest" "$staged" || ! $SUDO mv -f "$staged" "$MANIFEST_PATH"; then
  $SUDO rm -f "$staged"
  rm -f "$tmp_manifest"
  echo "Error: could not write $MANIFEST_PATH; it is as it was" >&2
  exit 1
fi
rm -f "$tmp_manifest"
exec 9<&-

manifest_version="$(python3 - "$MANIFEST_PATH" <<'PY'
import json, sys
with open(sys.argv[1], "r", encoding="utf-8") as f:
    d = json.load(f)
print(int(d.get("version", 0)))
PY
)"
manifest_hash="$($SUDO sha256sum "$MANIFEST_PATH" | awk '{print $1}')"

echo "Published runtime: $RUNTIME@$VERSION"
echo "Binary path: $BIN_DIR/$BINARY_NAME"
echo "Start script: $START_SH"
for file in ${FILES[@]+"${FILES[@]}"}; do
  echo "File: $RUNTIME_DIR/$(basename "$file")"
done
echo "Manifest: $MANIFEST_PATH"
echo "manifest_version=$manifest_version"
echo "manifest_hash=$manifest_hash"
