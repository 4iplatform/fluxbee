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
# (The Go tool is Linux-only; elsewhere, run it in the golang:1.25 image with the repository
# mounted. The OPA v0.68.0 compile path is pure Go and reproducible: only the random temp path of
# the rego, embedded as a source annotation, changes between builds, and the data offsets in the
# code with it when its length does.)
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

# Edge service commands (public URLs and blobs, the edge's own certificate and its ACME challenge,
# docs/acme-edge-certificates-v1.md §5, and the hosts it serves only through io.web,
# docs/io-web-v2.md §11.1): only SY.admin on the primary hive.
edge_service_actions := {
	"EDGE_OPEN_URL", "EDGE_CLOSE_URL", "EDGE_LIST_URLS", "EDGE_PUBLISH_BLOB", "EDGE_UNPUBLISH_BLOB",
	"EDGE_TLS_STATUS", "EDGE_TLS_RELOAD", "EDGE_ACME_CHALLENGE",
	"EDGE_WEB_ROUTES",
}

# Live node config and runtime distribution are forwarded directly by the singleton Admin to
# managed nodes or orchestrators on workers. CONFIG_CHANGED carries the user OPA policy (writes to
# the motherbee's SY.opa.rules, the notice of a newly published one to every hive): Admin only.
node_control_actions := {"CONFIG_GET", "CONFIG_SET", "SYSTEM_UPDATE", "SYSTEM_SYNC_HINT", "CONFIG_CHANGED"}

# What an orchestrator sends to another hive's orchestrator: the node lifecycle and inspection it
# forwards for the Admin, and the hive join/leave handshake (verified in sy_orchestrator,
# 2026-10-01). Nothing else: no node live config (CONFIG_SET/GET), no CONFIG_CHANGED, no
# SYSTEM_UPDATE. Every one that crosses hives goes from the motherbee to a spoke, and the watchdog
# acts locally (verified 2026-10-22), so rule (3) has a direction: a spoke's orchestrator, the DMZ
# ingress's included, commands no hive (FINDINGS A-20, docs/control-path-hardening-v1.md §3).
#
# The mutations: only from the motherbee's orchestrator.
orchestrator_mutations := {
	"SPAWN_NODE", "KILL_NODE", "START_NODE", "RESTART_NODE", "REMOVE_NODE_INSTANCE",
	"NODE_CONFIG_SET", "SYSTEM_CORE_ROLLBACK", "ADD_HIVE_FINALIZE", "REMOVE_HIVE_CLEANUP",
}

# The join/leave handshake runs only on the hive being added or removed, never on the motherbee,
# where ADD_HIVE_FINALIZE would restart the core and REMOVE_HIVE_CLEANUP stop it (its orchestrator
# refuses them too: SPOKE_ONLY_SYSTEM_ACTIONS in sy_orchestrator). No rule admits them there, from
# anyone: rules (3) and (4) both exclude spoke_only_action_on_motherbee.
spoke_only_actions := {"ADD_HIVE_FINALIZE", "REMOVE_HIVE_CLEANUP"}

spoke_only_action_on_motherbee if {
	input.action in spoke_only_actions
	input.hive_id == "motherbee"
}

# The reads: from the motherbee's orchestrator, or from the orchestrator of the hive that answers.
orchestrator_reads := {
	"NODE_CONFIG_GET", "NODE_STATE_GET", "NODE_STATUS_GET", "LIST_NODES",
	"GET_VERSIONS", "GET_RUNTIMES", "GET_RUNTIME",
}

# Option B (WAN multi-hop reachability): router-internal vouch action, decided ONLY by rule (6)
# below. Never granted to SY.orchestrator (not in rule (3)'s lists) nor by rule (4) — only
# the primary hub's gateway router may vouch.
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

# (3) The orchestrators' cross-hive control plane, limited to what they forward and with a
# direction. A mutation comes only from SY.orchestrator@motherbee, and the spoke-only handshake
# never lands on the motherbee itself.
allow if {
	input.action in orchestrator_mutations
	parsed.role == "SY.orchestrator"
	parsed.hive == "motherbee"
	not spoke_only_action_on_motherbee
}

# A read comes from SY.orchestrator@motherbee, or from SY.orchestrator@<the hive that answers>.
allow if {
	input.action in orchestrator_reads
	parsed.role == "SY.orchestrator"
	parsed.hive in {"motherbee", input.hive_id}
}

# (4) The Admin, same hive: every other protected action, but the join/leave handshake on the
# motherbee. It is the only node that acts on other nodes for a user (every action goes through
# SY.admin). SY.wf-rules goes through it for its WF nodes, and the E2E name WF.orch.diag is gone: a
# workflow named "orch.diag" would have been WF.orch.diag@<hive>, with this authority
# (docs/wf-standard-path-v1.md, FINDINGS A-68).
allow if {
	not input.action in edge_service_actions
	not input.action in node_control_actions
	not input.action in reachability_actions
	not spoke_only_action_on_motherbee
	parsed.hive == input.hive_id
	parsed.role == "SY.admin"
}

# (5) Read-only health probe opened to the vault, same hive only (never mutations). It also named
# "SY.config-routes", which no node is called (the node is SY.config.routes) and which never sends
# this probe: removed in 0.1.50 (FINDINGS A-18).
allow if {
	not input.action in edge_service_actions
	parsed.role != "SY.orchestrator"
	parsed.hive == input.hive_id
	input.action == "NODE_STATUS_GET"
	parsed.role == "SY.vault"
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
