# Fluxbee SYSTEM policy (OPA-dual, capa SYSTEM) — the NON-user-editable, baked half of the
# two-layer policy model, and the SINGLE SOURCE OF TRUTH for the SYSTEM decisions below. There is
# NO Rust fallback table: if a baked wasm fails to load, the router FAILS CLOSED (refuses to run)
# — see `src/router/system_policy.rs::ensure_system_policy_loaded()`. Behavioral tests load the
# baked wasm and assert the expected truth table (they fail if a wasm diverges from intent).
#
# Authoring (baked stage): a developer edits THIS file, recompiles it to BOTH wasm artifacts (one
# per entrypoint), and commits all three files:
#   sy-opa-rules compile-file policy/system.rego fluxbee/system/allow           policy/system.wasm
#   sy-opa-rules compile-file policy/system.rego fluxbee/system/frontdesk_route policy/system_route.wasm
# (The Go tool is Linux-only, but the OPA v0.68.0 compile path is pure Go and reproducible on any
# host — only an embedded source-path annotation, never the code section, is host-specific.)
# The router BAKES each wasm via `include_bytes!` and evaluates it through a lazy singleton
# resolver. There is NO SHM region / runtime writer for the system layer (the future "OPA-dual
# Phase 4" would add one, changing only HOW the resolver is fed, not this policy). Non-user-editable
# by construction (unreachable from `/opa/policy*`); changes only on rebuild+redeploy.
#
# ── Entrypoint (1): fluxbee/system/allow — AUTHORITY (may this origin perform a protected action)
#   input  = { "action": <verb>, "src_l2_name": <router-stamped name>, "hive_id": <this hive> }
#   output = allow (boolean)
#   Only consulted for PROTECTED_SYSTEM_ACTIONS; is_protected_system_action() stays in Rust (the
#   "is this gated at all" check). `hive_id` is the local router hive. This entrypoint answers only
#   "may this origin perform this protected action".
#
# ── Entrypoint (2): fluxbee/system/frontdesk_route — SELECTION (force-route to the identity
#   frontdesk). This is the routing decision that used to be a hardcoded `if` in the router
#   (apply_identity_pre_resolve); it now lives HERE, visible as policy.
#   input  = { "src_ilk_present": <bool>, "registration_status": <"temporary"|"partial"|"complete"|null> }
#   output = frontdesk_route (boolean)
#   The frontdesk NODE NAME is NOT here — it is per-hive Rust config (SY.frontdesk.gov@<hive>,
#   hive.yaml-overridable); the router substitutes it when this rule yields true. A hive-agnostic
#   wasm cannot carry a per-hive name, so the DECISION is policy and the NAME stays config.

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

# ── SELECTION: fluxbee/system/frontdesk_route ────────────────────────────────────────────────
# Force an as-yet-unidentified sender to the identity frontdesk (the registrar). This replaces the
# router's old hardcoded `if registration_status == "temporary"` detour (apply_identity_pre_resolve)
# and additionally covers the no-ilk case. Two clauses, both "the sender has no usable identity yet":
default frontdesk_route := false

# (a) No ilk at all on the message — provisioning never happened / failed (identity SHM down, an
#     anonymous ingress path, a rejected seed). Contain it at the frontdesk rather than let it flow
#     onward to a specialist. Keys on ABSENCE of src_ilk, NOT on "status resolved to null": an
#     edge-stamped system self_ilk is present-but-unresolved (status null, src_ilk PRESENT) and is
#     correctly NOT caught here.
frontdesk_route if not input.src_ilk_present

# (b) A provisioned-but-unascended ilk (registration_status "temporary") — a first-contact handle;
#     send it to the frontdesk to complete registration. partial/complete ilks are NOT force-routed
#     (they flow to the operator routing policy).
frontdesk_route if input.registration_status == "temporary"
