#!/usr/bin/env bash
# Guard from routerdispatcher_unification_plan.md §4.2 + §8.
#
# Once `fluxbee_sdk::connect` is `pub(crate)`, no code outside the SDK
# should call it. While the flip is pending (see "Open gaps" §1 in the
# plan), this guard runs in two modes:
#
#   STRICT (default): no call sites outside `crates/fluxbee_sdk/` are
#     allowed. Pre-requisite: the `pub(crate)` flip + 4 timer examples
#     must be migrated to `RouterDispatcher`.
#
#   PHASE-1 (env var GUARD_NO_DIRECT_CONNECT_PHASE=1): the known-pending
#     timer example file allowlist passes through. Use this until Gap 1
#     in the plan is closed; remove the env var and the allowlist when
#     the timer examples are migrated.
#
# Also forbids local `async fn connect_with_retry` wrappers inside
# `src/bin/sy_*.rs` (those existed pre-migration and must not come back).
#
# Exit code: 0 if clean for the selected mode, 1 if violations remain.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PHASE="${GUARD_NO_DIRECT_CONNECT_PHASE:-strict}"
EXIT=0

# Allowlist used in PHASE-1 only: timer examples that still consume the
# legacy Rust `TimerClient<'a>` API. Remove these entries when the SDK
# `TimerClient::new_with_dispatcher` lands.
PHASE1_ALLOW=(
    'examples/timer_client.rs'
    'examples/timer_recurring.rs'
    'examples/timer_restart.rs'
    'examples/support/timer_example.rs'
)

is_allowlisted() {
    local path="$1"
    [ "$PHASE" = "1" ] || return 1
    for entry in "${PHASE1_ALLOW[@]}"; do
        [[ "$path" == *"$entry" ]] && return 0
    done
    return 1
}

# 1. Rust call sites outside the SDK.
hits=$(grep -RnE 'fluxbee_sdk::connect\(|use[[:space:]]+fluxbee_sdk::connect\b' \
    --include='*.rs' \
    --exclude-dir=target \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    --exclude-dir='fluxbee_sdk' \
    "$REPO_ROOT" 2>/dev/null \
    | grep -v '^[[:space:]]*//' \
    | grep -v 'crates/fluxbee_sdk/' \
    || true)
if [ -n "$hits" ]; then
    filtered=""
    while IFS= read -r line; do
        path="${line%%:*}"
        if is_allowlisted "$path"; then continue; fi
        filtered+="$line"$'\n'
    done <<< "$hits"
    if [ -n "$filtered" ]; then
        echo "GUARD FAIL — fluxbee_sdk::connect() called outside the SDK (mode=$PHASE):"
        printf '%s' "$filtered"
        EXIT=1
    fi
fi

# 2. Rust use-list import of `connect` from fluxbee_sdk (multi-line).
python3 - "$REPO_ROOT" "$PHASE" <<'PY' || EXIT=$?
import os, re, sys
root, phase = sys.argv[1], sys.argv[2]
allow_paths = (
    'examples/timer_client.rs',
    'examples/timer_recurring.rs',
    'examples/timer_restart.rs',
    'examples/support/timer_example.rs',
)
banned = re.compile(r'use[ \t]+fluxbee_sdk::\{[^}]*\bconnect\b[^}]*\}', re.DOTALL)
found = []
for dirpath, dirs, files in os.walk(root):
    if any(skip in dirpath for skip in ('/target/', '/.git/', '/worktrees/',
                                         '/fluxbee_sdk/', '/.claude/')):
        continue
    for f in files:
        if not f.endswith('.rs'):
            continue
        p = os.path.join(dirpath, f)
        rel = os.path.relpath(p, root)
        if phase == "1" and any(rel.endswith(a) for a in allow_paths):
            continue
        try:
            text = open(p, 'r', errors='replace').read()
        except OSError:
            continue
        for m in banned.finditer(text):
            inner = m.group(0)
            # Skip if `connect` is actually `connect_with_retry`,
            # `connect_with_client_config`, or another longer identifier.
            if re.search(r'\b(connect_with_retry|connect_with_client_config)\b', inner) \
               and not re.search(r'(?<![A-Za-z0-9_])connect(?![A-Za-z0-9_])', inner):
                continue
            line = text[:m.start()].count('\n') + 1
            found.append(f"{p}:{line}: connect imported in a use-list")
if found:
    print("GUARD FAIL — `connect` imported in a use-list outside the SDK:")
    for f in found:
        print(f)
    sys.exit(1)
PY

# 3. Local `async fn connect_with_retry` wrappers inside src/bin/sy_*.rs.
hits=$(grep -RnE '^async fn connect_with_retry\b' \
    --include='sy_*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT/src/bin/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — local async fn connect_with_retry wrapper reintroduced inside src/bin/sy_*.rs:"
    echo "$hits"
    EXIT=1
fi

# 4. Go call sites outside the Go SDK.
hits=$(grep -RnE 'fluxbeesdk\.Connect\(' \
    --include='*.go' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT/go/" 2>/dev/null \
    | grep -v 'fluxbee-go-sdk/' \
    || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — fluxbeesdk.Connect() called outside the Go SDK package:"
    echo "$hits"
    EXIT=1
fi

# 5. Go SDK raw Connect must remain package-private (`connect`). Re-exporting
# it recreates the sender/receiver escape hatch this migration removed.
hits=$(grep -RnE '^func[[:space:]]+Connect\(' \
    --include='*.go' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT/go/fluxbee-go-sdk/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — public Go sdk.Connect() reintroduced:"
    echo "$hits"
    EXIT=1
fi

# 6. Rust NodeReceiver must not be part of the public SDK surface.
hits=$(grep -RnE 'pub[[:space:]]+struct[[:space:]]+NodeReceiver\b|pub[[:space:]]+use[^{;]*NodeReceiver\b|pub[[:space:]]+use[^{;]*\{[^}]*\bNodeReceiver\b' \
    --include='*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT/crates/fluxbee_sdk/src/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — public Rust NodeReceiver surface reintroduced:"
    echo "$hits"
    EXIT=1
fi

# 7. Rust in-process test fixtures may not expose a public raw receiver entry.
hits=$(grep -RnE 'pub[[:space:]]+fn[[:space:]]+from_test_channels\b' \
    --include='*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT/crates/fluxbee_sdk/src/rpc.rs" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — public RouterDispatcher::from_test_channels reintroduced:"
    echo "$hits"
    EXIT=1
fi

# 8. Blob publish+confirm must stay on the dispatcher-backed API.
hits=$(grep -RnE 'pub[[:space:]]+async[[:space:]]+fn[[:space:]]+publish_blob_and_confirm[[:space:]]*\(' \
    --include='*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT/crates/fluxbee_sdk/src/blob/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — legacy BlobToolkit::publish_blob_and_confirm reintroduced:"
    echo "$hits"
    EXIT=1
fi

if [ $EXIT -eq 0 ]; then
    echo "no_direct_connect: OK — no raw connect()/receiver/blob legacy surface outside the dispatcher path (mode=$PHASE)."
fi
exit $EXIT
