#!/usr/bin/env bash
# Tests packaging/fluxbee-migrate-config (FINDINGS A-46): it moves exactly the architect.listen value
# older packages shipped, "0.0.0.0:3000", to loopback, and touches nothing else.
set -euo pipefail
cd "$(dirname "$0")/../.."
MIGRATE="$PWD/packaging/fluxbee-migrate-config"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
fails=0
check() { if [ "$2" = "$3" ]; then echo "ok   $1"; else echo "FAIL $1"; echo "  expected: $3"; echo "  got:      $2"; fails=$((fails + 1)); fi; }
mode_of() { python3 -c 'import os,stat,sys; print(oct(stat.S_IMODE(os.stat(sys.argv[1]).st_mode)))' "$1"; }

# The block every package up to 0.1.56 shipped (packaging/hive.yaml.example), as PROD has it.
cat > "$work/shipped.yaml" <<'YAML'
admin:
  listen: "127.0.0.1:8080"

# Local architect HTTP UI/API (typically exposed through reverse proxy).
architect:
  listen: "0.0.0.0:3000"

# Shared filesystem root used by admin/orchestrator.
storage:
  path: "/var/lib/fluxbee"
YAML
cp "$work/shipped.yaml" "$work/a.yaml"
chmod 0640 "$work/a.yaml"
out="$("$MIGRATE" "$work/a.yaml")"
check "shipped value is rewritten" "$(grep -c '"127.0.0.1:3000"' "$work/a.yaml")" "1"
check "only that line changes" "$(diff "$work/shipped.yaml" "$work/a.yaml" | grep -c '^[<>]')" "2"
check "it says what it did" "$(echo "$out" | grep -c 'A-46')" "1"
check "file mode is kept" "$(mode_of "$work/a.yaml")" "0o640"
out2="$("$MIGRATE" "$work/a.yaml")"
check "second run is a no-op" "$out2" ""

# A trailing comment on the listen line survives.
printf 'architect:\n  listen: "0.0.0.0:3000"   # set by the installer\n' > "$work/b.yaml"
"$MIGRATE" "$work/b.yaml" >/dev/null
check "trailing comment kept" "$(cat "$work/b.yaml")" "$(printf 'architect:\n  listen: "127.0.0.1:3000"   # set by the installer')"

# A symlinked hive.yaml is fixed at its target, and the link stays a link.
cp "$work/shipped.yaml" "$work/target.yaml"
ln -s "$work/target.yaml" "$work/link.yaml"
"$MIGRATE" "$work/link.yaml" >/dev/null
check "symlink target is rewritten" "$(grep -c '"127.0.0.1:3000"' "$work/target.yaml")" "1"
check "symlink is kept" "$([ -L "$work/link.yaml" ] && echo link || echo file)" "link"

# A file saved with CRLF line endings.
printf 'architect:\r\n  listen: "0.0.0.0:3000"\r\n' > "$work/crlf.yaml"
"$MIGRATE" "$work/crlf.yaml" >/dev/null
check "CRLF file is rewritten" "$(grep -c '127.0.0.1:3000' "$work/crlf.yaml")" "1"
check "CRLF endings kept" "$(grep -c $'\r$' "$work/crlf.yaml")" "2"

# Values that are not the shipped default are the operator's: left alone.
for case in 'listen: "127.0.0.1:3000"' 'listen: "10.0.0.5:3000"' 'listen: "0.0.0.0:3001"' "listen: '0.0.0.0:3000'" 'listen: 0.0.0.0:3000'; do
  printf 'architect:\n  %s\n' "$case" > "$work/c.yaml"
  cp "$work/c.yaml" "$work/c.orig"
  "$MIGRATE" "$work/c.yaml" >/dev/null
  check "left alone: $case" "$(cmp -s "$work/c.yaml" "$work/c.orig" && echo same || echo changed)" "same"
done

# The same value under another section is not Archi's.
printf 'admin:\n  listen: "0.0.0.0:3000"\narchitect:\n  listen: "127.0.0.1:3000"\n' > "$work/d.yaml"
cp "$work/d.yaml" "$work/d.orig"
"$MIGRATE" "$work/d.yaml" >/dev/null
check "other sections untouched" "$(cmp -s "$work/d.yaml" "$work/d.orig" && echo same || echo changed)" "same"

# A file without an architect section, and a missing file.
printf 'storage:\n  path: "/var/lib/fluxbee"\n' > "$work/e.yaml"
cp "$work/e.yaml" "$work/e.orig"
"$MIGRATE" "$work/e.yaml" >/dev/null
check "no architect section" "$(cmp -s "$work/e.yaml" "$work/e.orig" && echo same || echo changed)" "same"
"$MIGRATE" "$work/missing.yaml" >/dev/null; check "missing file exits 0" "$?" "0"

# No temp files are left behind.
check "no temp files left" "$(ls "$work" | grep -c 'migrate' || true)" "0"

if [ "$fails" -ne 0 ]; then echo "$fails check(s) failed"; exit 1; fi
echo "fluxbee-migrate-config: all checks passed"
