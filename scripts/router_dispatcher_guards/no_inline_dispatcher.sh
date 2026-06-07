#!/usr/bin/env bash
# Guard from routerdispatcher_unification_plan.md §8.
#
# Forbids the 5 inline dispatchers that the unification eliminated. Any
# reintroduction is a regression: the global plan committed to a single
# `RouterDispatcher` abstraction, and these structs/funcs were the
# evidence of organic divergence we deleted.
#
# Patterns:
#   - struct RouterInbox                              (Rust, was in nodes/io/common)
#   - struct SharedRouterConnection                   (Rust, was in ai-generic + ai-frontdesk-gov)
#   - struct RouterClient                             (Rust, inside crates/fluxbee_ai_sdk/)
#   - type RouterClient struct                        (Go, was in sy-opa-rules)
#   - type messageMux struct                          (Go, was in sy-wf-rules/node/mux.go)
#   - func (...) forwardOutgoing(...)                 (Go, was in sy-opa-rules/main.go)
#
# Exit code: 0 if clean, 1 if any banned pattern surfaces.

set -eo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXIT=0

# RouterInbox / SharedRouterConnection: must not appear anywhere in the tree.
for name_pat in 'RouterInbox::struct RouterInbox\b' \
                'SharedRouterConnection::struct SharedRouterConnection\b'; do
    name="${name_pat%%::*}"
    pattern="${name_pat#*::}"
    hits=$(grep -RnE "$pattern" \
        --include='*.rs' \
        --exclude-dir=target \
        --exclude-dir=.git \
        --exclude-dir=worktrees \
        "$REPO_ROOT" 2>/dev/null || true)
    if [ -n "$hits" ]; then
        echo "GUARD FAIL — $name struct reintroduced:"
        echo "$hits"
        EXIT=1
    fi
done

# RouterClient: bare `struct RouterClient` inside fluxbee_ai_sdk is the
# forbidden pattern. We scan strictly inside fluxbee_ai_sdk/.
hits=$(grep -RnE 'struct RouterClient\b' \
    --include='*.rs' \
    --exclude-dir=target \
    "$REPO_ROOT/crates/fluxbee_ai_sdk/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — fluxbee_ai_sdk::RouterClient struct reintroduced:"
    echo "$hits"
    EXIT=1
fi

# Go: messageMux
hits=$(grep -RnE '^type messageMux struct\b' \
    --include='*.go' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT/go/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — Go messageMux struct reintroduced:"
    echo "$hits"
    EXIT=1
fi

# Go: RouterClient wrapper
hits=$(grep -RnE '^type RouterClient struct\b' \
    --include='*.go' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT/go/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — Go RouterClient wrapper reintroduced:"
    echo "$hits"
    EXIT=1
fi

# Go: forwardOutgoing method
hits=$(grep -RnE 'func \([^)]+\) forwardOutgoing\(' \
    --include='*.go' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT/go/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — Go forwardOutgoing method reintroduced:"
    echo "$hits"
    EXIT=1
fi

if [ $EXIT -eq 0 ]; then
    echo "no_inline_dispatcher: OK — no banned inline dispatcher patterns found."
fi
exit $EXIT
