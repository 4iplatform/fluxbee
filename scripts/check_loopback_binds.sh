#!/usr/bin/env bash
# Guard for FINDINGS A-46: Archi and the admin HTTP API have no authentication, so nothing we ship
# may bind them off loopback or send an operator to them on another host. Scope
# (docs/host-posture-and-exposure-spec-v1.md §3.4): the shipped hive.yaml files and the
# operator-facing install docs. History (logbooks, the HANDBOOK, FINDINGS, docs/legacy) is out of
# scope on purpose: it records what was true then.
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
report() { echo "A-46 guard: $*"; fail=1; }

# A renamed or missing input must fail the guard, not pass it silently.
for f in packaging/hive.yaml.example config/hive.yaml packaging/fluxbee-firstboot \
         docs/packaging-and-build.md docs/07-operaciones.md; do
  [ -f "$f" ] || report "$f is missing: update the guard's file list"
done

# 1. Shipped hive.yaml: architect.listen and admin.listen are loopback, or absent.
for f in packaging/hive.yaml.example config/hive.yaml; do
  [ -f "$f" ] || continue
  while IFS= read -r hit; do
    [ -n "$hit" ] && report "$f: $hit (must be 127.0.0.1, localhost or [::1])"
  done < <(awk '
    /^[^ \t#]/ { section = $0; sub(/:.*/, "", section) }
    (section == "architect" || section == "admin") && /^[ \t]+listen:/ {
      value = $0
      sub(/^[ \t]+listen:[ \t]*/, "", value); sub(/[ \t]*#.*$/, "", value); gsub(/["\047]/, "", value)
      if (value !~ /^(127\.[0-9.]+|localhost|\[::1\]):[0-9]+$/) printf "line %d: %s.listen is %s\n", NR, section, value
    }' "$f")
done

# 2. Operator-facing text: no URL that opens 3000/8080 on a non-loopback host, no 0.0.0.0 listen.
for f in packaging/fluxbee-firstboot docs/packaging-and-build.md docs/07-operaciones.md; do
  [ -f "$f" ] || continue
  while IFS= read -r hit; do
    [ -n "$hit" ] && report "$f:$hit"
  # One URL per output line (-o), so a loopback URL on the same line cannot hide a bad one.
  done < <(grep -noE 'https?://[^/[:space:]`)]*:(3000|8080)' "$f" | grep -vE ':https?://(127\.0\.0\.1|localhost|\[::1\]):(3000|8080)$' || true)
  while IFS= read -r hit; do
    [ -n "$hit" ] && report "$f:$hit"
  done < <(grep -nE 'listen:[[:space:]]*"?0\.0\.0\.0:(3000|8080)' "$f" || true)
done

if [ "$fail" -ne 0 ]; then
  echo "A-46 guard: FAILED. Archi and the admin API must stay on loopback (FINDINGS A-46)."
  exit 1
fi
echo "A-46 guard: OK (shipped binds and install docs keep Archi and admin on loopback)"
