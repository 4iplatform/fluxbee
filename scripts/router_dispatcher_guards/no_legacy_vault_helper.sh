#!/usr/bin/env bash
# Guard from routerdispatcher_unification_plan.md §8.
#
# `fluxbee_sdk::resolve_resource(&NodeSender, &mut NodeReceiver, ...)` was
# deleted. Any reappearance — direct call, `use ... resolve_resource`, or
# re-export — means someone reintroduced the legacy Vault helper.
#
# Comments in production source mentioning the helper name are allowed
# (some doc comments still describe historical behavior). The guard greps
# for actual code references: `resolve_resource(` followed by a paren or
# a multi-line argument list.
#
# Exit code: 0 if clean, 1 if any banned pattern surfaces.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXIT=0

# Direct call patterns. We allow:
#   - `vault_client.resolve_resource(` (method on VaultClient — that's the new API).
#   - `VaultClient::resolve_resource` (the method declaration in vault.rs).
#   - Comment lines containing the name (`///` or `//` prefixed).
# We forbid:
#   - `fluxbee_sdk::resolve_resource(` — free function call.
#   - `use fluxbee_sdk::resolve_resource` — direct import.
#   - `use fluxbee_sdk::{..., resolve_resource, ...}` — list import.

# 1. Direct path-qualified call.
hits=$(grep -RnE 'fluxbee_sdk::resolve_resource\b' \
    --include='*.rs' \
    --exclude-dir=target \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT" 2>/dev/null \
    | grep -vE '(^|:)[[:space:]]*//' \
    | grep -vE '(^|:)[[:space:]]*\*' \
    || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — fluxbee_sdk::resolve_resource() reintroduced as a call:"
    echo "$hits"
    EXIT=1
fi

# 2. Import of the free function by name.
hits=$(grep -RnE 'use[[:space:]]+fluxbee_sdk::resolve_resource\b' \
    --include='*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — use fluxbee_sdk::resolve_resource reintroduced:"
    echo "$hits"
    EXIT=1
fi

# 3. resolve_resource in a use-list (handles `use fluxbee_sdk::{..., resolve_resource, ...}`
#    even across line breaks).
python3 - "$REPO_ROOT" <<'PY' || EXIT=$?
import os, re, sys
root = sys.argv[1]
banned = re.compile(r'use[ \t]+fluxbee_sdk::\{[^}]*\bresolve_resource\b[^}]*\}', re.DOTALL)
found = []
for dirpath, dirs, files in os.walk(root):
    if any(skip in dirpath for skip in ('/target/', '/.git/', '/worktrees/', '/.claude/')):
        continue
    for f in files:
        if not f.endswith('.rs'):
            continue
        p = os.path.join(dirpath, f)
        try:
            text = open(p, 'r', errors='replace').read()
        except OSError:
            continue
        for m in banned.finditer(text):
            line = text[:m.start()].count('\n') + 1
            found.append(f"{p}:{line}: {m.group(0)[:120]}")
if found:
    print("GUARD FAIL — `resolve_resource` reintroduced inside a multi-name use-list:")
    for line in found:
        print(line)
    sys.exit(1)
PY

# 4. Legacy public Vault functions must not be restored. The only public
# Vault transport surface is VaultClient over RouterDispatcher.
hits=$(grep -RnE '^pub[[:space:]]+async[[:space:]]+fn[[:space:]]+vault_(get|get_metadata|put|list|delete|rotate|rollback|get_with_retry)\b' \
    --include='*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT/crates/fluxbee_sdk/src/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — legacy public vault_* helper reintroduced:"
    echo "$hits"
    EXIT=1
fi

if [ $EXIT -eq 0 ]; then
    echo "no_legacy_vault_helper: OK — legacy resolve_resource() free function not referenced."
fi
exit $EXIT
