# Fluxbee SYSTEM origin-authority policy (OPA-dual, capa SYSTEM).
#
# This is the Rego backing for `src/router/system_policy.rs :: authority()`. It is the
# NON-user-editable half of the two-layer policy model. Authoring/loading TODAY (baked stage):
#   1. a developer edits THIS file, then recompiles it to `policy/system.wasm` with the same OPA
#      compiler the user path uses: `sy-opa-rules compile-file policy/system.rego fluxbee/system/allow policy/system.wasm`
#      (build in the fxbuild Linux container; the Go tool is Linux-only). Commit both files.
#   2. the router BAKES that wasm into the binary via `include_bytes!` and evaluates it through
#      `authorize_system()` (a lazy singleton) — there is NO SHM region and NO runtime writer for
#      the system layer (that is the future "OPA-dual Phase 4": a privileged `/jsr-opa-sys-<hive>`
#      region + writer, which only changes HOW the resolver is fed, not this policy).
# So the system rules are non-user-editable by construction (unreachable from `/opa/policy*`) and
# only change on a rebuild+redeploy. Keeping this `.rego` and `system.wasm` in sync is a MANUAL
# dev step, guarded by the shadow-verify tests (they fail if the baked wasm diverges from authority()).
#
# Contract (must stay BYTE-IDENTICAL to authority() — shadow-verified in tests):
#   input  = { "action": <verb>, "src_l2_name": <router-stamped name>, "hive_id": <this hive> }
#   output = allow (boolean)   ; entrypoint: fluxbee/system/allow
#
# NOTE: `hive_id` is NEW in the OPA input — authority() takes the local router hive as a
# parameter, so build_opa_input must be enriched with it for this entrypoint (additive).
#
# authority() is only consulted for PROTECTED_SYSTEM_ACTIONS; is_protected_system_action()
# stays in Rust (the "is this gated at all" check). This policy answers only "may this origin
# perform this protected action".

package fluxbee.system

import rego.v1

default allow := false

# Edge service commands (open/close/list URLs): only SY.admin on the primary hive.
edge_service_actions := {"EDGE_OPEN_URL", "EDGE_CLOSE_URL", "EDGE_LIST_URLS", "EDGE_PUBLISH_BLOB", "EDGE_UNPUBLISH_BLOB"}

# Live node config and runtime distribution are forwarded directly by the singleton Admin to
# managed nodes or orchestrators on workers.
node_control_actions := {"CONFIG_GET", "CONFIG_SET", "SYSTEM_UPDATE", "SYSTEM_SYNC_HINT"}

# Option B (WAN multi-hop reachability): router-internal vouch action, decided ONLY by rule (6)
# below. Excluded from the broad control-plane grants (3)/(4) so it is never granted to
# SY.orchestrator/SY.admin — only the primary hub's gateway router may vouch.
reachability_actions := {"WAN_REACHABILITY_VOUCH"}

# Parse "<role>@<hive>" from the router-authoritative (already-stamped) src_l2_name, splitting
# on the FIRST '@' (mirrors Rust split_once('@')). Undefined when the name is empty or has no
# '@' with a non-empty role — which makes every allow rule below fail -> default false.
name := trim_space(input.src_l2_name)

parsed.role := substring(name, 0, idx) if {
	name != ""
	idx := indexof(name, "@")
	idx > 0
}

parsed.hive := substring(name, idx + 1, -1) if {
	name != ""
	idx := indexof(name, "@")
	idx > 0
}

# (1) Edge service command -> exactly SY.admin@motherbee. Returns regardless of hive_id, and
# does NOT fall through to the rules below (they all guard `not edge`).
allow if {
	input.action in edge_service_actions
	parsed.role == "SY.admin"
	parsed.hive == "motherbee"
}

# (2) The primary Admin may forward managed-node config and runtime distribution cross-hive.
allow if {
	input.action in node_control_actions
	parsed.role == "SY.admin"
	parsed.hive == "motherbee"
}

# (3) SY.orchestrator@<any non-empty hive> — the cross-hive control plane (system-final).
allow if {
	not input.action in edge_service_actions
	not input.action in reachability_actions
	parsed.role == "SY.orchestrator"
	parsed.hive != ""
}

# (4) Same-hive privileged control-plane roles, all protected actions.
allow if {
	not input.action in edge_service_actions
	not input.action in node_control_actions
	not input.action in reachability_actions
	parsed.role != "SY.orchestrator"
	parsed.hive == input.hive_id
	parsed.role in {"SY.admin", "SY.wf-rules", "WF.orch.diag"}
}

# (5) Read-only health probe opened to config/vault, same hive only (never mutations).
allow if {
	not input.action in edge_service_actions
	parsed.role != "SY.orchestrator"
	parsed.hive == input.hive_id
	input.action == "NODE_STATUS_GET"
	parsed.role in {"SY.config-routes", "SY.vault"}
}

# (6) Option B (WAN multi-hop reachability, edge-multihop-reachability-spec-v1): only the primary
# hub's gateway router may VOUCH transitive reachability of other hives' nodes. The router asks
# with action WAN_REACHABILITY_VOUCH and src_l2_name = the advertising peer's gateway router name
# (RT.gateway@<peer_hive>). A vouch grants DATA-plane reachability only; SYSTEM authority stays
# strict (a via_hub origin is denied at the delivery gate, independent of this rule).
allow if {
	input.action == "WAN_REACHABILITY_VOUCH"
	parsed.role == "RT.gateway"
	parsed.hive == "motherbee"
}
