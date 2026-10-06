#!/usr/bin/env bash
# Guard (host posture stage 2, docs/host-posture-and-exposure-spec-v1.md §3.5): Syncthing's
# config.xml is written only through update_syncthing_config in sy_orchestrator.rs, under one lock
# and with an atomic rename that keeps the file's owner. Three unlocked writers used to race each
# other.
set -euo pipefail
cd "$(dirname "$0")/.."
f=src/bin/sy_orchestrator.rs
fail=0
[ -f "$f" ] || { echo "syncthing single-writer guard: $f is missing"; exit 1; }
grep -q 'fn update_syncthing_config' "$f" || { echo "syncthing single-writer guard: update_syncthing_config is missing"; fail=1; }
hits=$(grep -nE 'fs::write\(\s*&?(config_path|sync\.sync_data_dir\.join\("config\.xml"\))' "$f" || true)
if [ -n "$hits" ]; then
  echo "syncthing single-writer guard: config.xml written outside update_syncthing_config:"
  echo "$hits"
  fail=1
fi
[ "$fail" -eq 0 ] || exit 1
echo "syncthing single-writer guard: OK"
