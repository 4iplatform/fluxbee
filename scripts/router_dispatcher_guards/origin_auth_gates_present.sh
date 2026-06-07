#!/usr/bin/env bash
# Guard from routerdispatcher_unification_followups.md H5.5.
#
# Both `SY.architect` and `SY.admin` MUST gate inbound protected SYSTEM_KIND
# actions (`CONFIG_GET`, `CONFIG_SET`, plus architect's `NODE_STATUS_GET`)
# against an allowlist of `src_l2_name` values. If somebody deletes the gate
# functions during a "simplification", CI fails here.
#
# We assert presence of the four symbols by name. The unit tests in each
# binary cover the *behavior* (VAULT_SECRET_CHANGED must not be in the
# protected set, the allowlist accepts the right callers and rejects the
# rest, etc.).
#
# Exit code: 0 if every expected symbol is present, 1 if any is missing.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXIT=0

require_symbol() {
    local file="$1"
    local symbol="$2"
    local label="$3"
    if ! grep -qE "fn[[:space:]]+${symbol}\b" "$file" 2>/dev/null; then
        echo "GUARD FAIL — missing $label (expected \`fn ${symbol}\`) in $(basename "$file")"
        EXIT=1
    fi
}

ARCHITECT="$REPO_ROOT/src/bin/sy_architect.rs"
ADMIN="$REPO_ROOT/src/bin/sy_admin.rs"

if [ -f "$ARCHITECT" ]; then
    require_symbol "$ARCHITECT" \
        protected_architect_system_action_response \
        "architect Section E gate predicate"
    require_symbol "$ARCHITECT" \
        architect_origin_authorized \
        "architect Section E allowlist check"
    require_symbol "$ARCHITECT" \
        build_architect_forbidden_response \
        "architect Section E FORBIDDEN response builder"
fi

if [ -f "$ADMIN" ]; then
    require_symbol "$ADMIN" \
        protected_admin_system_action_response \
        "admin H5 gate predicate"
    require_symbol "$ADMIN" \
        admin_origin_authorized \
        "admin H5 allowlist check"
    require_symbol "$ADMIN" \
        build_admin_forbidden_response \
        "admin H5 FORBIDDEN response builder"
fi

if [ $EXIT -eq 0 ]; then
    echo "origin_auth_gates_present: OK — architect Section E + admin H5 gate symbols all present."
fi
exit $EXIT
