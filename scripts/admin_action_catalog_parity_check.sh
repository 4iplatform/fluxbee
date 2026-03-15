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
        r'handle_admin_(?:query|command)\(\s*ctx,\s*client,\s*"([^"]+)"',
        surface,
        flags=re.S,
    )
)

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
}
for variant, action in opa_http_map.items():
    pat = rf'handle_opa_http\([\s\S]*?OpaAction::{variant}\)'
    if re.search(pat, surface):
        http_actions.add(action)

if re.search(r'handle_opa_query\(\s*ctx,\s*client,\s*"get_policy"', surface, flags=re.S):
    http_actions.add("opa_get_policy")
if re.search(r'handle_opa_query\(\s*ctx,\s*client,\s*"get_status"', surface, flags=re.S):
    http_actions.add("opa_get_status")

missing_in_registry = sorted(http_actions - registry_actions)
extra_in_registry = sorted(registry_actions - http_actions)

# v1 explicit HTTP-only exclusions (not expected in registry)
v1_http_only = {
    "get_storage_metrics",  # /config/storage/metrics
}
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
