#!/usr/bin/env bash
# Guard from routerdispatcher_unification_plan.md §8.
#
# Go-only. The latent bug §5 calls out was: `wf-generic` passed the same
# `*NodeReceiver` to `NewSDKTimerSender(sender, receiver, ...)` while the
# main loop was also doing `receiver.Recv(ctx)`. Whichever Recv won the
# next message won it; the workload was light enough that the race never
# bit. The dispatcher unification eliminates the aliasing by routing
# everything through the `RouterDispatcher` pending-matcher path.
#
# This guard catches the original anti-pattern: a Go function whose body
# both calls `<receiver>.Recv(...)` AND passes `<receiver>` as an argument
# to another function. The simplest reliable detection is a per-file scan
# for both forms of the same identifier name.
#
# Allowed: a function that ONLY reads from the receiver, or ONLY passes
# the receiver into a constructor. The forbidden case is the combination.
#
# Exit code: 0 if clean, 1 if the pattern surfaces.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
EXIT=0

# Public receiver-consuming Go SDK helpers and generated-script call sites must
# not come back. The only public transport entry point is RouterDispatcher.
hits=$(grep -RnE '\b(RequestSystemRPC|AwaitSystemResponse|NewTimerClientWithDispatcher)\b|sdk\.Connect\(' \
    --include='*.go' \
    --include='*.sh' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    --exclude-dir=router_dispatcher_guards \
    "$REPO_ROOT/go/" "$REPO_ROOT/scripts/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — legacy Go receiver-consuming helper or raw sdk.Connect() reference found:"
    echo "$hits"
    EXIT=1
fi

hits=$(grep -RnE '^func[[:space:]]+NewTimerClient\([[:space:]]*sender[[:space:]]+\*NodeSender\b' \
    --include='*.go' \
    --exclude-dir=.git \
    --exclude-dir=worktrees \
    "$REPO_ROOT/go/fluxbee-go-sdk/" 2>/dev/null || true)
if [ -n "$hits" ]; then
    echo "GUARD FAIL — legacy NewTimerClient(sender, receiver, ...) signature reintroduced:"
    echo "$hits"
    EXIT=1
fi

python3 - "$REPO_ROOT" <<'PY' || EXIT=$?
import os, re, sys
root = sys.argv[1]

# Match function bodies in Go: `func ... { ... }` non-greedy. Inside each
# body, we look for both `<ident>.Recv(` and `<ident>` appearing as a
# function argument (i.e. followed by `,` or `)` after a `(...,` or
# right after `(`). If both forms reference the same identifier we flag.
RE_FUNC = re.compile(r'func[ \t][^\n{]*\{', re.MULTILINE)
RE_RECV = re.compile(r'\b([A-Za-z_][A-Za-z_0-9]*)\.Recv\(')
# `\b<ident>` immediately after `(` or `,[ \t]*` and immediately before
# `,` or `)` (passes as an argument, not part of a chained call).
RE_PASS = re.compile(r'(?:\(|,[ \t]*)([A-Za-z_][A-Za-z_0-9]*)(?=[ \t]*[,)])')

# Allow-listed identifier names that are obviously not Go NodeReceivers
# (common short identifiers used in unrelated contexts).
SKIP = {'ctx', 'err', 'ok', 'nil', 'res', 'r', 'w', 'cfg', 'msg'}

found = []
for dirpath, dirs, files in os.walk(root):
    if any(skip in dirpath for skip in ('/target/', '/.git/', '/worktrees/',
                                         '/vendor/', '/.claude/')):
        continue
    if '/go/' not in dirpath:
        # Only Go is in scope.
        continue
    for f in files:
        if not f.endswith('.go'):
            continue
        p = os.path.join(dirpath, f)
        try:
            text = open(p, 'r', errors='replace').read()
        except OSError:
            continue
        # Brace-aware function-body extraction.
        i = 0
        while i < len(text):
            m = RE_FUNC.search(text, i)
            if not m:
                break
            start = m.end()  # right after `{`
            depth = 1
            j = start
            while j < len(text) and depth:
                c = text[j]
                if c == '{':
                    depth += 1
                elif c == '}':
                    depth -= 1
                j += 1
            body = text[start:j-1]
            recv_idents = {n for n in RE_RECV.findall(body)} - SKIP
            pass_idents = {n for n in RE_PASS.findall(body)} - SKIP
            shared = recv_idents & pass_idents
            for ident in shared:
                # Compute line number of `start`.
                line = text[:start].count('\n') + 1
                found.append(f"{p}:{line}: shared receiver `{ident}` both Recv() and passed to a function")
            i = j

if found:
    print("GUARD FAIL — Go function aliases a NodeReceiver (calls Recv() AND passes it down):")
    for line in found:
        print(line)
    sys.exit(1)
PY

if [ $EXIT -eq 0 ]; then
    echo "no_shared_receiver: OK — no Go function aliases a NodeReceiver across Recv() + arg-pass."
fi
exit $EXIT
