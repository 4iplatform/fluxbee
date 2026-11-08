#!/usr/bin/env bash
set -euo pipefail

# FR9-T21:
# Static parity check between HTTP-exposed admin actions and
# internal socket action registry in SY.admin.
#
# Ensures action catalog drift is detected early.
# Usage:
#   bash scripts/admin_action_catalog_parity_check.sh

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="${ADMIN_SRC:-$ROOT_DIR/src/bin/sy_admin.rs}"

if [[ ! -f "$SRC" ]]; then
  echo "FAIL: source file not found: $SRC" >&2
  exit 1
fi

python3 - "$SRC" <<'PY'
import re
import sys
from pathlib import Path

src_path = Path(sys.argv[1])
text = src_path.read_text(encoding="utf-8")


def fail(msg: str, code: int = 1) -> None:
    print(f"FAIL: {msg}", file=sys.stderr)
    raise SystemExit(code)


def section_between(start_marker: str, end_marker: str) -> str:
    i = text.find(start_marker)
    if i < 0:
        fail(f"missing marker: {start_marker!r}")
    j = text.find(end_marker, i)
    if j < 0:
        fail(f"missing marker: {end_marker!r}")
    return text[i:j]


registry_actions = set(
    re.findall(r'InternalActionSpec\s*\{\s*action:\s*"([^"]+)"', text)
)
if not registry_actions:
    fail("could not parse INTERNAL_ACTION_REGISTRY actions")

http_block = section_between("async fn handle_http(", "\nasync fn handle_inventory_http(")
hive_block = section_between("async fn handle_hive_paths(", "\nasync fn handle_modules_paths(")
surface = http_block + "\n" + hive_block

http_actions = set(
    re.findall(
        r'handle_admin_(?:query|command)(?:_with_payload)?\(\s*ctx,\s*client,\s*"([^"]+)"',
        surface,
        flags=re.S,
    )
)
# Other ways an HTTP route reaches an action: the timer RPCs, the generic internal dispatch
# (artifacts), and dedicated handlers.
for pattern in (
    r'handle_timer_rpc\(\s*ctx,\s*client,\s*"([^"]+)"',
    r'dispatch_internal_admin_command\(\s*ctx,\s*client,\s*"([^"]+)"',
):
    http_actions.update(re.findall(pattern, surface, flags=re.S))
if "handle_publish_cloud_endpoint(" in surface:
    http_actions.add("publish_cloud_endpoint")

wf_rules_http_map = {
    "CompileApply": "wf_rules_compile_apply",
    "Compile": "wf_rules_compile",
    "Apply": "wf_rules_apply",
    "Rollback": "wf_rules_rollback",
    "Delete": "wf_rules_delete",
    "Check": "wf_rules_check",
}
for variant, action in wf_rules_http_map.items():
    # The call also names who asked for the deploy (A-68): match the variant, not the paren.
    if re.search(rf'handle_wf_rules_http\([\s\S]*?WfRulesAction::{variant}\b', surface):
        http_actions.add(action)
for query in ("get_status", "get_workflow", "list_workflows"):
    if "handle_wf_rules_query(" in surface and f'"{query}"' in surface:
        http_actions.add(f"wf_rules_{query}")

if "handle_hive_update_command(" in surface:
    http_actions.add("update")
if "handle_hive_sync_hint_command(" in surface:
    http_actions.add("sync_hint")
if "handle_inventory_http(" in surface:
    http_actions.add("inventory")
if "handle_list_streams(" in surface:
    http_actions.add("list_streams")
if "handle_llm_usage(" in surface:
    http_actions.add("get_llm_usage")

opa_http_map = {
    "CompileApply": "opa_compile_apply",
    "Compile": "opa_compile",
    "Apply": "opa_apply",
    "Rollback": "opa_rollback",
    "Check": "opa_check",
    "Clear": "opa_clear",
}
for variant, action in opa_http_map.items():
    pat = rf'handle_opa_http\([\s\S]*?OpaAction::{variant}\)'
    if re.search(pat, surface):
        http_actions.add(action)

if re.search(r'handle_opa_query\(\s*ctx,\s*client,\s*"get_policy"', surface, flags=re.S):
    http_actions.add("opa_get_policy")
if re.search(r'handle_opa_query\(\s*ctx,\s*client,\s*"get_status"', surface, flags=re.S):
    http_actions.add("opa_get_status")

# Served by dedicated arms of dispatch_internal_admin_command (authorized by the caller's
# router-stamped name), not by registry entries.
dedicated_arms = {"externalize", "unexternalize", "list_externalized"}
for action in sorted(dedicated_arms):
    if f'"{action}" =>' not in text:
        fail(f"dedicated-arm list names {action!r}, which dispatch_internal_admin_command no longer has")

missing_in_registry = sorted(http_actions - registry_actions - dedicated_arms)
extra_in_registry = sorted(registry_actions - http_actions)

# v1 explicit HTTP-only exclusions (not expected in registry)
v1_http_only = {
    "get_storage_metrics",  # /config/storage/metrics
}
# Registry actions with no HTTP route on purpose: they arrive over the mesh only.
mesh_only = {
    "publish_artifact",  # the producer (IO.blob) asks the admin directly
    "list_cloud_actions",  # IO.cloud asks which actions it may relay
}
extra_in_registry = sorted(set(extra_in_registry) - mesh_only)
stale_mesh_only = sorted(mesh_only - registry_actions)
if stale_mesh_only:
    fail("mesh-only list names actions the registry no longer has: " + ", ".join(stale_mesh_only))
if v1_http_only & registry_actions:
    fail(
        "registry includes v1 HTTP-only actions: "
        + ", ".join(sorted(v1_http_only & registry_actions))
    )

if missing_in_registry:
    fail(
        "HTTP exposes actions missing in INTERNAL_ACTION_REGISTRY: "
        + ", ".join(missing_in_registry)
    )

if extra_in_registry:
    fail(
        "INTERNAL_ACTION_REGISTRY has actions not exposed in HTTP mapping: "
        + ", ".join(extra_in_registry)
    )

# FINDINGS A-172: the executor sends a step's args under the names the help gives an action's
# path parameters and body fields, while each HTTP route stores its path segments in the payload
# under keys of its own. A key the help does not name reaches the handler from HTTP and never
# from Archi's plans.
def section(start, end):
    i = text.find(start)
    if i < 0:
        fail(f"missing marker: {start!r}")
    j = text.find(end, i + len(start))
    return text[i:j if j > 0 else len(text)]


def closing(src, open_at):
    """The index just past the bracket that closes the one at `open_at`, skipping string literals."""
    depth, i, n = 0, open_at, len(src)
    while i < n:
        c = src[i]
        if c == '"':
            i += 1
            while i < n and src[i] != '"':
                i += 2 if src[i] == "\\" else 1
        elif src[i:i + 2] == 'r#':
            end = src.find('"#', i + 3)
            i = end + 1 if end > 0 else n
        elif c == "[":
            depth += 1
        elif c == "]":
            depth -= 1
            if depth == 0:
                return i + 1
        i += 1
    return n


def names_by_action(fn_start):
    """Each action's field names in one help function: its `"a" | "b" => vec![...]` arms."""
    body = section(fn_start, "\nfn ")
    out = {}
    for head in re.finditer(r'((?:"[a-z_]+"\s*\|\s*)*"[a-z_]+")\s*=>\s*vec!\[', body):
        arm = body[head.end() - 1:closing(body, head.end() - 1)]
        names = set(re.findall(r'admin_action_(?:path_param|body_field)\(\s*"([^"]+)"', arm))
        for action in re.findall(r'"([a-z_]+)"', head.group(1)):
            out.setdefault(action, set()).update(names)
    return out


schema = {}
for fn in ("fn admin_action_path_params(", "fn admin_action_body_required_fields(",
           "fn admin_action_body_optional_fields("):
    for action, names in names_by_action(fn).items():
        schema.setdefault(action, set()).update(names)
# The intended translations: the executor's `hive` becomes inventory's `filter_hive`
# (`internal_inventory_payload`), and get_admin_action_help takes its action as `action_name`.
translated = {("inventory", "filter_hive"), ("get_admin_action_help", "action_name")}

call_re = re.compile(
    r'(?:handle_admin_(?:command|query)(?:_with_payload)?|handle_timer_rpc|'
    r'dispatch_internal_admin_command|handle_identity_query)\(\s*ctx,\s*client,\s*"([^"]+)"', re.S)
# A route's arm: `("METHOD", [segments]) =>`, or `"METHOD" =>` where a block matches the method alone
# (`/hives/{hive}` itself).
head_re = re.compile(
    r'^\s*(?:\(\s*"[A-Z]+"\s*,\s*\[([^\]]*)\]\s*\)|"[A-Z]+")\s*(?:if [^=]*)?=>', re.M)
mismatches = []
for start, end in (("async fn handle_hive_paths(", "\nasync fn handle_modules_paths("),
                   ("async fn handle_http(", "\nasync fn handle_inventory_http(")):
    block = section(start, end)
    heads = list(head_re.finditer(block))
    for i, head in enumerate(heads):
        arm = block[head.start():heads[i + 1].start() if i + 1 < len(heads) else len(block)]
        segments = [tok.strip() for tok in (head.group(1) or "").split(",")]
        segments = [tok for tok in segments if re.fullmatch(r"[a-z_][a-z0-9_]*", tok)]
        if "handle_hive_paths" in start:
            segments.append("hive")
        stored = set()
        for var in segments:
            for form in (r'"(\w+)"\s*:\s*(?:decode_percent\(\s*)?&?\*?' + var + r'\b',
                         r'\["(\w+)"\]\s*=\s*[^;]*\b' + var + r'\b',
                         r'insert\(\s*"(\w+)"\.to_string\(\)\s*,[^;]*?\b' + var + r'\b'):
                stored.update(re.findall(form, arm, flags=re.S))
        for action in set(call_re.findall(arm)):
            if action not in schema:
                if stored:
                    mismatches.append(f"{action}: the route stores {sorted(stored)}, the help names no field")
                continue
            for key in sorted(stored):
                if key not in schema[action] and (action, key) not in translated:
                    mismatches.append(f"{action}: the route stores '{key}', the help names {sorted(schema[action])}")
if mismatches:
    fail("a path segment the executor cannot send (A-172): " + "; ".join(sorted(set(mismatches))))
print("status=ok")
print(f"source={src_path}")
print(f"http_actions={len(http_actions)}")
print(f"registry_actions={len(registry_actions)}")
print("admin action catalog parity FR9-T21 passed.")
PY
