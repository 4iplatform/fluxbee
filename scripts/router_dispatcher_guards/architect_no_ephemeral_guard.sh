#!/usr/bin/env bash
# Guard from sy_architect_rpc_multiplexing_tasks_v2_stable_agents.md §A2 /
# §9 CI gate.
#
# `SY.architect` must own exactly one canonical Arc<RouterDispatcher> and
# must not create per-call identities or open additional ephemeral router
# connections. This guard enforces that surface inside the architect
# binary.
#
# Forbidden patterns in src/bin/sy_architect.rs and src/bin/sy_admin.rs:
#   - `SY.architect.<purpose>.{}` literal node names
#   - `format!("SY.architect.{purpose}..."` purpose-templated names
#   - `NodeUuidMode::Ephemeral` (admin executor + architect Vault must
#     use the canonical persistent dispatcher, not a per-call ephemeral)
#   - `router_connect_loop` / `router_recv_loop` (the deleted bespoke
#     transport — must not come back)
#   - `state.router_sender` / `state.router_connected` field accesses
#     (the deleted state, must not come back)
#
# Exit code: 0 if clean, 1 if any pattern surfaces.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXIT=0

check_pattern() {
    local pattern="$1"
    local label="$2"
    local file
    for file in "$REPO_ROOT/src/bin/sy_architect.rs" "$REPO_ROOT/src/bin/sy_admin.rs"; do
        [ -f "$file" ] || continue
        local hits
        hits=$(grep -nE "$pattern" "$file" 2>/dev/null \
            | grep -vE '^[0-9]+:[[:space:]]*//' \
            | grep -vE '^[0-9]+:[[:space:]]*\*' \
            || true)
        if [ -n "$hits" ]; then
            echo "GUARD FAIL — $label in $(basename "$file"):"
            echo "$hits"
            EXIT=1
        fi
    done
}

check_pattern '"SY\.architect\.[a-z_]*\.\{\}"' \
    'per-call SY.architect.<purpose>.{} client name'
check_pattern 'format!\("SY\.architect\.\{purpose\}' \
    'purpose-templated SY.architect.{purpose} literal'
check_pattern 'NodeUuidMode::Ephemeral' \
    'NodeUuidMode::Ephemeral (canonical dispatcher must be Persistent)'
check_pattern '\b(router_connect_loop|router_recv_loop)\b' \
    'deleted bespoke router_*_loop'
check_pattern 'state\.(router_sender|router_connected)\b' \
    'deleted ArchitectState.router_sender / router_connected'

if [ $EXIT -eq 0 ]; then
    echo "architect_no_ephemeral_guard: OK — no banned ephemeral / bespoke-router patterns in architect or admin."
fi
exit $EXIT
