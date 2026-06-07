#!/usr/bin/env bash
# Guard from routerdispatcher_unification_plan.md §8.
#
# `feedback_no_legacy_in_dev` forbids deprecation periods. The unification
# plan committed to delete (not deprecate) every legacy surface. This guard
# catches `#[deprecated...]` attributes on any of the canonical symbols.
#
# Symbols watched:
#   - RouterDispatcher
#   - RpcClient (the legacy name, should not reappear deprecated)
#   - connect_with_retry
#   - VaultClient
#   - resolve_resource (also should never reappear)
#
# Exit code: 0 if clean, 1 if any annotation surfaces.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXIT=0

python3 - "$REPO_ROOT" <<'PY' || EXIT=$?
import os, re, sys
root = sys.argv[1]

# Matches `#[deprecated...]` directly before an `impl`, `fn`, `struct`,
# `pub use`, or `pub fn`/`pub struct` declaring (or referencing) one of
# the watched names. We do a 4-line lookahead because attributes commonly
# span 1-3 lines (`#[deprecated(\n    note = "..."\n)]`).
WATCHED = ['RouterDispatcher', 'RpcClient', 'connect_with_retry',
           'VaultClient', 'resolve_resource']

re_dep_open = re.compile(r'^\s*#\[deprecated\b')
found = []

for dirpath, dirs, files in os.walk(root):
    if any(skip in dirpath for skip in ('/target/', '/.git/', '/worktrees/', '/.claude/')):
        continue
    for f in files:
        if not f.endswith('.rs'):
            continue
        p = os.path.join(dirpath, f)
        try:
            lines = open(p, 'r', errors='replace').readlines()
        except OSError:
            continue
        for i, line in enumerate(lines):
            if not re_dep_open.match(line):
                continue
            # Look ahead up to 6 lines for the declaration target.
            window = ''.join(lines[i:i+6])
            for name in WATCHED:
                # name must appear and must look like a declared item or
                # signature, not a comment / doc-string. Quick check:
                if re.search(rf'\b{name}\b', window):
                    found.append(f"{p}:{i+1}: #[deprecated] near `{name}`")
                    break

if found:
    print("GUARD FAIL — #[deprecated] attribute on canonical RouterDispatcher symbols:")
    for line in found:
        print(line)
    sys.exit(1)
PY

if [ $EXIT -eq 0 ]; then
    echo "no_deprecated_attribute_on_dispatcher: OK — no deprecation annotations on canonical symbols."
fi
exit $EXIT
