#!/usr/bin/env bash
# Run every RouterDispatcher architectural guard in strict mode.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

guards=(
    "architect_no_ephemeral_guard.sh"
    "no_deprecated_attribute_on_dispatcher.sh"
    "no_direct_connect.sh"
    "no_inline_dispatcher.sh"
    "no_legacy_vault_helper.sh"
    "no_shared_receiver.sh"
    "origin_auth_gates_present.sh"
)

for guard in "${guards[@]}"; do
    echo "==> ${guard}"
    bash "${SCRIPT_DIR}/${guard}"
done

echo "router_dispatcher_guards: OK -- all ${#guards[@]} guards passed."
