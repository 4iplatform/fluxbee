# Fluxbee SYSTEM origin-authority policy (OPA-dual, capa SYSTEM).
#
# This is the Rego backing for `src/router/system_policy.rs :: authority()`. It is the
# NON-user-editable half of the two-layer policy model: compiled to WASM and loaded by the
# router into the dedicated `/jsr-opa-sys-<hive>` region (never the user `/opa/policy*` path).
#
# Contract (must stay BYTE-IDENTICAL to authority() — shadow-verified in tests before it
# becomes the backing):
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
edge_service_actions := {"EDGE_OPEN_URL", "EDGE_CLOSE_URL", "EDGE_LIST_URLS"}

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

# (2) SY.orchestrator@<any non-empty hive> — the cross-hive control plane (system-final).
allow if {
	not input.action in edge_service_actions
	parsed.role == "SY.orchestrator"
	parsed.hive != ""
}

# (3) Same-hive privileged control-plane roles, all protected actions.
allow if {
	not input.action in edge_service_actions
	parsed.role != "SY.orchestrator"
	parsed.hive == input.hive_id
	parsed.role in {"SY.admin", "SY.wf-rules", "WF.orch.diag"}
}

# (4) Read-only health probe opened to config/vault, same hive only (never mutations).
allow if {
	not input.action in edge_service_actions
	parsed.role != "SY.orchestrator"
	parsed.hive == input.hive_id
	input.action == "NODE_STATUS_GET"
	parsed.role in {"SY.config-routes", "SY.vault"}
}
