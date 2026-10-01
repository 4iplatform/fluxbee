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
}
for variant, action in wf_rules_http_map.items():
    if re.search(rf'handle_wf_rules_http\([\s\S]*?WfRulesAction::{variant}\)', surface):
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

print("status=ok")
print(f"source={src_path}")
print(f"http_actions={len(http_actions)}")
print(f"registry_actions={len(registry_actions)}")
print("admin action catalog parity FR9-T21 passed.")
PY
