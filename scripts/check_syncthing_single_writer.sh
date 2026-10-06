#!/usr/bin/env bash
# Guard (host posture stage 2, docs/host-posture-and-exposure-spec-v1.md §3.5): Syncthing's
# config.xml is written only through update_syncthing_config in sy_orchestrator.rs, under one lock
# and with an atomic rename that keeps the file's owner. Three unlocked writers used to race each
# other. Checked on the code before `mod tests {`.
set -euo pipefail
cd "$(dirname "$0")/.."
f=src/bin/sy_orchestrator.rs
fail=0
[ -f "$f" ] || { echo "syncthing single-writer guard: $f is missing"; exit 1; }
prod="$(awk '/^mod tests \{/{exit} {print}' "$f")"

grep -q 'fn update_syncthing_config' <<<"$prod" || {
  echo "syncthing single-writer guard: update_syncthing_config is missing"
  fail=1
}

# The atomic replace has exactly one caller: update_syncthing_config.
calls=$(grep -nE '(^|[^a-z_])replace_file_keeping_owner\(' <<<"$prod" | grep -v 'fn replace_file_keeping_owner' || true)
if [ "$(grep -c . <<<"$calls")" -ne 1 ]; then
  echo "syncthing single-writer guard: replace_file_keeping_owner must have exactly one caller:"
  echo "$calls"
  fail=1
fi

# No other write of config.xml, whatever the call or the variable.
hits=$(grep -nE '(fs::write|File::create|write_file_atomic|fs::copy|OpenOptions)' <<<"$prod" \
  | grep -E 'config_path|config\.xml' || true)
if [ -n "$hits" ]; then
  echo "syncthing single-writer guard: config.xml written outside update_syncthing_config:"
  echo "$hits"
  fail=1
fi

[ "$fail" -eq 0 ] || exit 1
echo "syncthing single-writer guard: OK"
