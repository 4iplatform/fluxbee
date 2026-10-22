//! The router's SYSTEM policy layer — the non-user-editable half of fluxbee's
//! two-layer (system + user) policy model ("OPA-dual").
//!
//! The USER layer is OPA: operator-authored Rego, compiled to WASM by the
//! `SY.opa.rules` node, loaded by the router's `OpaResolver` (see `crate::opa`).
//! It selects routing targets and (in future) may *narrow* authority.
//!
//! This SYSTEM layer is the authoritative half: the `SY.` origin-authority rules. They live as
//! Rego in `policy/system.rego`, compiled to the baked `policy/system.wasm` (build-time, with
//! the SAME OPA compiler the user path uses — the `sy-opa-rules compile-file` mode — so it is
//! non-user-editable and structurally unreachable from the `/opa/policy*` authoring endpoints).
//! `authorize_system()` evaluates that policy at the delivery gate. There is NO Rust fallback
//! table: the Rego is the SINGLE SOURCE OF TRUTH. If the baked wasm fails to load, the router
//! FAILS CLOSED — `ensure_system_policy_loaded()` (called at `Router::run` startup) refuses to
//! start rather than silently degrade. Behavioral tests load the baked wasm and assert the
//! expected truth table. A future runtime-editable backing (a second SHM region + privileged
//! writer, "OPA-dual Phase 4") only changes how the resolver is FED — not the call sites nor
//! `authorize_system()`'s contract.
//!
//! This file also hosts the SYSTEM routing-SELECTION entrypoint `frontdesk_route`
//! (`route_to_frontdesk()`): whether an unidentified (no ilk) or `temporary` sender is force-
//! routed to the identity frontdesk — the decision that used to be a hardcoded `if` in the router
//! (`apply_identity_pre_resolve`). Same baked-wasm / single-source / fail-closed model as `allow`.
//!
//! Composition contract: `final_allow = system_allow AND user_allow`, and a SYSTEM deny is
//! FINAL — the user layer may only narrow, never broaden, and can never sever the cross-hive
//! control plane (`SY.orchestrator@motherbee` toward every hive, rule (3)).

use std::sync::{Mutex, OnceLock};

use crate::opa::OpaResolver;

/// Protected SYSTEM actions (matched on `meta.msg`) that a node may only act on
/// when the router-resolved origin is authorized. Single source of truth — moved
/// out of the orchestrator so origin-authority is enforced once, centrally, at
/// delivery time.
pub const PROTECTED_SYSTEM_ACTIONS: &[&str] = &[
    "SYSTEM_UPDATE",
    "SYSTEM_CORE_ROLLBACK",
    "SYSTEM_SYNC_HINT",
    "EDGE_OPEN_URL",
    "EDGE_CLOSE_URL",
    "EDGE_LIST_URLS",
    "EDGE_PUBLISH_BLOB",
    "EDGE_UNPUBLISH_BLOB",
    // The edge's own certificate: what it serves, a reload from the vault, and the TLS-ALPN-01
    // challenge it answers while SY.admin orders (docs/acme-edge-certificates-v1.md §4).
    "EDGE_TLS_STATUS",
    "EDGE_TLS_RELOAD",
    "EDGE_ACME_CHALLENGE",
    "SPAWN_NODE",
    "KILL_NODE",
    "START_NODE",
    "RESTART_NODE",
    "REMOVE_NODE_INSTANCE",
    "NODE_CONFIG_SET",
    "NODE_CONFIG_GET",
    "CONFIG_SET",
    "CONFIG_GET",
    "NODE_STATE_GET",
    "NODE_STATUS_GET",
    "GET_VERSIONS",
    "GET_RUNTIMES",
    "GET_RUNTIME",
    "LIST_NODES",
    "INVENTORY_REQUEST",
    "ADD_HIVE_FINALIZE",
    "REMOVE_HIVE_CLEANUP",
    // Its receiver (SY.opa.rules) APPLIES the user OPA policy it carries. Only the primary Admin
    // sends it (system policy rule 2).
    "CONFIG_CHANGED",
];

/// The primary hive — the single motherbee. The bins import THIS one instead of each redefining
/// it; it is the SDK's value, so nodes outside this crate cannot drift from it either.
pub const PRIMARY_HIVE_ID: &str = fluxbee_sdk::protocol::PRIMARY_HIVE_ID;

/// The single node authorized to COMMAND an edge (its URLs and public blobs, its certificate) and
/// to receive its control responses: `SY.admin` on the primary hive. Canonical name — the router's
/// edge-control reachability gates and the edge's own service-command check all reference
/// THIS constant so the copies of the literal cannot drift. (`policy/system.rego` rule (1)
/// composes the same decision from role + PRIMARY_HIVE_ID.)
pub const EDGE_CONTROL_AUTHORITY: &str = "SY.admin@motherbee";

/// Whether `action` is a protected SYSTEM action subject to the system authority
/// gate.
pub fn is_protected_system_action(action: &str) -> bool {
    PROTECTED_SYSTEM_ACTIONS.contains(&action)
}

/// Option B (WAN multi-hop reachability, edge-multihop-reachability-spec-v1): may the mTLS-
/// authenticated `voucher_hive` VOUCH transitive reachability of other hives' nodes to a spoke?
/// Only the primary hub may — it is the star's single relay.
///
/// OPA-BACKED, exactly like the rest of the SYSTEM authority surface: it evaluates the baked
/// `system.wasm` (`policy/system.rego` rule (6), action `WAN_REACHABILITY_VOUCH`) through
/// `authorize_system` (single source of truth; no Rust fallback — the router fails closed at
/// startup if the wasm cannot load). The voucher is identified by its gateway router name
/// `RT.gateway@<voucher_hive>`; `hive_id` is irrelevant to this rule so the primary hive is passed.
///
/// A vouched node is admitted for DATA delivery ONLY; SYSTEM authority stays strict and is denied
/// for a `via_hub` origin at the delivery gate (see `serialize_for_local_delivery`). So allowing the
/// hub to vouch reachability cannot grant it the power to fabricate cross-hive control-plane
/// authority between spokes.
pub fn wan_reachability_voucher_allowed(voucher_hive: &str) -> bool {
    authorize_system(
        "WAN_REACHABILITY_VOUCH",
        Some(&format!("RT.gateway@{}", voucher_hive.trim())),
        PRIMARY_HIVE_ID,
    )
}

/// The BAKED system AUTHORITY resolver: `policy/system.wasm` (entrypoint `fluxbee/system/allow`,
/// compiled from `policy/system.rego`). Hive-agnostic — `hive_id` is an INPUT, not baked — so ONE
/// resolver serves every hive. Lazily loaded once; a load failure is a corrupt/incompatible baked
/// artifact and is caught at startup by `ensure_system_policy_loaded()`, which fails the router
/// closed (there is NO Rust fallback — the Rego is the single source of truth).
static SYSTEM_POLICY_OPA: OnceLock<Mutex<OpaResolver>> = OnceLock::new();

fn system_policy_opa() -> &'static Mutex<OpaResolver> {
    SYSTEM_POLICY_OPA.get_or_init(|| {
        const WASM: &[u8] = include_bytes!("../../policy/system.wasm");
        let mut resolver = OpaResolver::new();
        if let Err(err) = resolver.reload(1, Some("fluxbee/system/allow".to_string()), WASM, None)
        {
            tracing::error!(error = %err, "baked system.wasm (fluxbee/system/allow) failed to load; the router will fail closed at startup");
        }
        Mutex::new(resolver)
    })
}

/// The BAKED system SELECTION resolver: `policy/system_route.wasm` (entrypoint
/// `fluxbee/system/frontdesk_route`, compiled from the SAME `policy/system.rego`). Same baked /
/// single-source / fail-closed model as `SYSTEM_POLICY_OPA`.
static SYSTEM_ROUTE_OPA: OnceLock<Mutex<OpaResolver>> = OnceLock::new();

fn system_route_opa() -> &'static Mutex<OpaResolver> {
    SYSTEM_ROUTE_OPA.get_or_init(|| {
        const WASM: &[u8] = include_bytes!("../../policy/system_route.wasm");
        let mut resolver = OpaResolver::new();
        if let Err(err) = resolver.reload(
            1,
            Some("fluxbee/system/frontdesk_route".to_string()),
            WASM,
            None,
        ) {
            tracing::error!(error = %err, "baked system_route.wasm (fluxbee/system/frontdesk_route) failed to load; the router will fail closed at startup");
        }
        Mutex::new(resolver)
    })
}

/// Force-load BOTH baked SYSTEM policy resolvers and prove they evaluate. Called once at
/// `Router::run` startup: the wasms are baked into the binary via `include_bytes!`, so a load
/// failure means a corrupt/incompatible build artifact — the router has no business running
/// without its authority + routing policy, so it FAILS CLOSED (returns `Err`, refusing to start)
/// rather than silently degrading. This is the ONE place the "if the Rego doesn't load, don't
/// run" contract is enforced; there is deliberately no Rust fallback table anywhere.
pub fn ensure_system_policy_loaded() -> Result<(), String> {
    {
        let mut resolver = system_policy_opa()
            .lock()
            .map_err(|_| "system authority policy mutex poisoned".to_string())?;
        let probe = serde_json::json!({
            "action": "NODE_STATUS_GET",
            "src_l2_name": "SY.vault@motherbee",
            "hive_id": "motherbee",
        });
        resolver
            .evaluate_allow(&probe, None)
            .map_err(|err| format!("baked system.wasm (fluxbee/system/allow) unusable: {err}"))?;
    }
    {
        let mut resolver = system_route_opa()
            .lock()
            .map_err(|_| "system route policy mutex poisoned".to_string())?;
        let probe = serde_json::json!({
            "src_ilk_present": true,
            "registration_status": "complete",
        });
        resolver.evaluate_allow(&probe, None).map_err(|err| {
            format!("baked system_route.wasm (fluxbee/system/frontdesk_route) unusable: {err}")
        })?;
    }
    Ok(())
}

/// SYSTEM authority decision, OPA-BACKED — this is what the router's delivery gate calls.
/// Evaluates the baked Rego policy (entrypoint `fluxbee/system/allow`) with input
/// `{action, src_l2_name, hive_id}`. FAIL-CLOSED: a poisoned lock or eval error DENIES (there is
/// no Rust fallback). In practice unreachable — `ensure_system_policy_loaded()` proved the wasm
/// evaluates at startup or the router never got here.
///
/// NOTE: locks a Mutex + runs a wasm eval per protected-action delivery — control-plane path
/// only (protected SYSTEM actions), and the policy is a tiny fixed program.
pub fn authorize_system(action: &str, src_l2_name: Option<&str>, hive_id: &str) -> bool {
    let Ok(mut resolver) = system_policy_opa().lock() else {
        tracing::error!(
            action,
            "system authority policy mutex poisoned; failing closed (deny)"
        );
        return false;
    };
    let input = serde_json::json!({
        "action": action,
        "src_l2_name": src_l2_name,
        "hive_id": hive_id,
    });
    match resolver.evaluate_allow(&input, None) {
        Ok(allow) => allow,
        Err(err) => {
            tracing::error!(action, error = %err, "system authority policy eval failed; failing closed (deny)");
            false
        }
    }
}

/// SYSTEM routing-SELECTION decision: should this sender be force-routed to the identity
/// frontdesk? OPA-BACKED (entrypoint `fluxbee/system/frontdesk_route`), input
/// `{src_ilk_present, registration_status}`. Returns only the yes/no — the frontdesk NODE NAME is
/// per-hive Rust config, substituted by the router caller. Single source of truth = the Rego (no
/// fallback). On a poisoned lock or eval error (unreachable post-startup) it returns `false` (do
/// NOT force), preserving normal routing rather than mass-redirecting; startup already guaranteed
/// the wasm evaluates.
pub fn route_to_frontdesk(registration_status: Option<&str>, src_ilk_present: bool) -> bool {
    let Ok(mut resolver) = system_route_opa().lock() else {
        tracing::error!("system route policy mutex poisoned; not forcing frontdesk");
        return false;
    };
    let input = serde_json::json!({
        "src_ilk_present": src_ilk_present,
        "registration_status": registration_status,
    });
    match resolver.evaluate_allow(&input, None) {
        Ok(route) => route,
        Err(err) => {
            tracing::error!(error = %err, "system route policy eval failed; not forcing frontdesk");
            false
        }
    }
}

/// Shared "same-hive origin allowlist" primitive — the ONE place the per-subsystem origin
/// gates (SY.admin, SY.architect, ...) express the common rule "the router-authoritative
/// `src_l2_name` must parse to `<role>@<this-hive>` and `role` must be in the caller's
/// allowlist". Each subsystem passes only its allowlist (data); the parse + same-hive check
/// lives here so it can't drift across copies. Part of the system-policy seam: when the
/// backing moves to a Rego-backed system region, these callers keep calling one contract.
pub fn same_hive_role_allowed(
    hive_id: &str,
    src_l2_name: Option<&str>,
    allowed_roles: &[&str],
) -> bool {
    let Some(src) = src_l2_name.map(str::trim).filter(|v| !v.is_empty()) else {
        return false;
    };
    let Some((role, hive)) = src.split_once('@') else {
        return false;
    };
    if hive.is_empty() || hive != hive_id {
        return false;
    }
    allowed_roles.contains(&role)
}

/// Prefix-based origin allowlist with a same-hive constraint on privileged prefixes — the
/// identity-style authority shape, shared so its same-hive scoping can't drift from the
/// router's `authorize_system()` rule (F-07). `name` is allowed iff it starts with one of
/// `allowed_prefixes` AND — if that matched prefix is in `same_hive_only` — the `@<hive>`
/// suffix equals `hive_id` (fail-closed on a missing/malformed suffix). Prefixes NOT in
/// `same_hive_only` (e.g. `IO.`, `SY.orchestrator@`) are legitimately cross-hive. The
/// per-message allowlist DATA stays with the caller; only this scoping LOGIC is centralized.
pub fn prefix_allowed_same_hive_scoped(
    name: &str,
    hive_id: &str,
    allowed_prefixes: &[&str],
    same_hive_only: &[&str],
) -> bool {
    allowed_prefixes.iter().any(|prefix| {
        if !name.starts_with(prefix) {
            return false;
        }
        if same_hive_only.contains(prefix) {
            name.rsplit_once('@').map(|(_, h)| h) == Some(hive_id)
        } else {
            true
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_allowed_same_hive_scoped_enforces_scope() {
        let prefixes = ["IO.", "SY.admin@", "SY.orchestrator@"];
        let same_hive_only = ["SY.admin@"];
        // cross-hive prefix (IO.) allowed from any hive
        assert!(prefix_allowed_same_hive_scoped(
            "IO.wapp@worker1",
            "motherbee",
            &prefixes,
            &same_hive_only
        ));
        // cross-hive orchestrator allowed from another hive
        assert!(prefix_allowed_same_hive_scoped(
            "SY.orchestrator@worker1",
            "motherbee",
            &prefixes,
            &same_hive_only
        ));
        // same-hive-only prefix: allowed on own hive, denied cross-hive
        assert!(prefix_allowed_same_hive_scoped(
            "SY.admin@motherbee",
            "motherbee",
            &prefixes,
            &same_hive_only
        ));
        assert!(!prefix_allowed_same_hive_scoped(
            "SY.admin@worker1",
            "motherbee",
            &prefixes,
            &same_hive_only
        ));
        // no prefix match -> denied; malformed same-hive suffix -> fail-closed
        assert!(!prefix_allowed_same_hive_scoped(
            "WF.invoice@motherbee",
            "motherbee",
            &prefixes,
            &same_hive_only
        ));
        assert!(!prefix_allowed_same_hive_scoped(
            "SY.admin",
            "motherbee",
            &prefixes,
            &same_hive_only
        ));
    }

    #[test]
    fn same_hive_role_allowed_requires_same_hive_and_allowlist() {
        let allow = ["SY.architect", "SY.config.routes", "SY.vault"];
        // in-allowlist + same hive -> allowed
        assert!(same_hive_role_allowed(
            "motherbee",
            Some("SY.architect@motherbee"),
            &allow
        ));
        // trims whitespace like the origin gates did
        assert!(same_hive_role_allowed(
            "motherbee",
            Some("  SY.vault@motherbee  "),
            &allow
        ));
        // wrong hive -> denied (same-hive scope)
        assert!(!same_hive_role_allowed(
            "motherbee",
            Some("SY.architect@worker1"),
            &allow
        ));
        // role not in allowlist -> denied
        assert!(!same_hive_role_allowed(
            "motherbee",
            Some("IO.slack@motherbee"),
            &allow
        ));
        // None / empty / no '@' / empty hive -> denied
        assert!(!same_hive_role_allowed("motherbee", None, &allow));
        assert!(!same_hive_role_allowed("motherbee", Some("   "), &allow));
        assert!(!same_hive_role_allowed(
            "motherbee",
            Some("SY.architect"),
            &allow
        ));
        assert!(!same_hive_role_allowed(
            "motherbee",
            Some("SY.architect@"),
            &allow
        ));
    }

    #[test]
    fn authorize_system_enforces_role_and_hive_scope() {
        let hive = "motherbee";
        let act = "SPAWN_NODE"; // a mutation, strict scope
                                // SY.orchestrator: only the motherbee's (rule (3) has a direction).
        assert!(authorize_system(
            act,
            Some("SY.orchestrator@motherbee"),
            hive
        ));
        assert!(authorize_system(
            act,
            Some("  SY.orchestrator@motherbee  "),
            hive
        ));
        assert!(!authorize_system(
            act,
            Some("SY.orchestrator@worker1"),
            hive
        ));
        // Exact role label (rejects relay/sub-name spoof) + non-empty hive.
        assert!(!authorize_system(
            act,
            Some("SY.orchestrator.relay.123@motherbee"),
            hive
        ));
        assert!(!authorize_system(act, Some("SY.orchestrator@"), hive));
        // The same-hive Admin: the only node that acts on other nodes for a user.
        assert!(authorize_system(act, Some("SY.admin@motherbee"), hive));
        assert!(!authorize_system(act, Some("SY.admin@worker1"), hive));
        // SY.wf-rules goes through the Admin for its WF nodes, and the E2E name WF.orch.diag is
        // gone: a workflow named "orch.diag" would have carried it (A-68). No protected action
        // that rule (4) grants, on any hive.
        for role in ["SY.wf-rules", "WF.orch.diag"] {
            for action in [
                "SPAWN_NODE",
                "KILL_NODE",
                "RESTART_NODE",
                "NODE_CONFIG_SET",
                "NODE_CONFIG_GET",
                "NODE_STATUS_GET",
                "LIST_NODES",
                "INVENTORY_REQUEST",
            ] {
                for name_hive in ["motherbee", "worker1"] {
                    assert!(
                        !authorize_system(action, Some(&format!("{role}@{name_hive}")), name_hive),
                        "{role}@{name_hive} must not have {action}"
                    );
                }
            }
        }
        // Rejected for a mutation.
        for bad in [
            None,
            Some(""),
            Some("   "),
            Some("SY.admin"),
            Some("motherbee"),
            Some("AI.evil@motherbee"),
            Some("SY.vault@motherbee"),
            Some("SY.config.routes@motherbee"),
            Some("SY.identity@motherbee"),
        ] {
            assert!(
                !authorize_system(act, bad, hive),
                "{bad:?} must be rejected for {act}"
            );
        }
        // NODE_STATUS_GET read-probe opens to the vault, same hive only.
        assert!(authorize_system(
            "NODE_STATUS_GET",
            Some("SY.vault@motherbee"),
            hive
        ));
        assert!(!authorize_system(
            "NODE_STATUS_GET",
            Some("SY.vault@worker1"),
            hive
        ));
        assert!(!authorize_system(
            "NODE_STATUS_GET",
            Some("SY.config.routes@motherbee"),
            hive
        ));
        assert!(!authorize_system(
            "SPAWN_NODE",
            Some("SY.vault@motherbee"),
            hive
        ));
        assert!(!authorize_system(
            "NODE_STATUS_GET",
            Some("AI.evil@motherbee"),
            hive
        ));
    }

    #[test]
    fn primary_admin_can_control_managed_nodes_cross_hive() {
        for action in [
            "CONFIG_GET",
            "CONFIG_SET",
            "SYSTEM_UPDATE",
            "SYSTEM_SYNC_HINT",
        ] {
            assert!(authorize_system(
                action,
                Some("SY.admin@motherbee"),
                "worker-220"
            ));
            assert!(!authorize_system(
                action,
                Some("SY.admin@worker-220"),
                "worker-220"
            ));
            assert!(!authorize_system(
                action,
                Some("SY.admin@other"),
                "worker-220"
            ));
        }
        assert!(!authorize_system(
            "SPAWN_NODE",
            Some("SY.admin@motherbee"),
            "worker-220"
        ));
    }

    #[test]
    fn config_control_only_from_the_primary_admin() {
        // CONFIG_SET/CONFIG_GET (a node's live config) come only from the motherbee Admin. The
        // orchestrators never send them (verified 2026-10-01), so they are not admitted either:
        // a compromised orchestrator must not rewrite another hive's node config. Any non-SY
        // origin (a rogue application node on the same VPN) is denied at the delivery gate.
        let hive = "worker-220";
        for action in ["CONFIG_GET", "CONFIG_SET"] {
            assert!(
                authorize_system(action, Some("SY.admin@motherbee"), hive),
                "{action}: SY.admin@motherbee must be admitted"
            );
            for rogue in [
                Some("SY.orchestrator@worker-220"),
                Some("SY.orchestrator@motherbee"),
                Some("IO.api@worker-220"),
                Some("AI.evil@motherbee"),
                Some("IO.slack@worker-220"),
                Some("WF.orch.diag@worker-220"),
                Some("SY.admin@worker-220"), // non-motherbee Admin is NOT node_control authority
                None,                        // unstamped / transitively-vouched origin
            ] {
                assert!(
                    !authorize_system(action, rogue, hive),
                    "{action}: origin {rogue:?} must be denied"
                );
            }
        }
    }

    /// Rule (3)'s mutations other than the join/leave handshake (`orchestrator_mutations` in
    /// `policy/system.rego`).
    const ORCHESTRATOR_MUTATIONS: [&str; 7] = [
        "SPAWN_NODE",
        "KILL_NODE",
        "START_NODE",
        "RESTART_NODE",
        "REMOVE_NODE_INSTANCE",
        "NODE_CONFIG_SET",
        "SYSTEM_CORE_ROLLBACK",
    ];

    /// The join/leave handshake: mutations that run only on the hive being added or removed.
    const SPOKE_ONLY_ACTIONS: [&str; 2] = ["ADD_HIVE_FINALIZE", "REMOVE_HIVE_CLEANUP"];

    /// Rule (3)'s reads (`orchestrator_reads`).
    const ORCHESTRATOR_READS: [&str; 7] = [
        "NODE_CONFIG_GET",
        "NODE_STATE_GET",
        "NODE_STATUS_GET",
        "LIST_NODES",
        "GET_VERSIONS",
        "GET_RUNTIMES",
        "GET_RUNTIME",
    ];

    /// Rule (3) grants an orchestrator exactly what it forwards to another hive's orchestrator
    /// (node lifecycle and inspection, the hive join/leave handshake) — verified against
    /// sy_orchestrator on 2026-10-01 — and nothing else. Who may send each of them where is
    /// `rule_3_has_a_direction`.
    #[test]
    fn orchestrators_get_exactly_what_they_forward() {
        let forwarded: Vec<&str> = ORCHESTRATOR_MUTATIONS
            .iter()
            .chain(SPOKE_ONLY_ACTIONS.iter())
            .chain(ORCHESTRATOR_READS.iter())
            .copied()
            .collect();
        for action in &forwarded {
            assert!(
                authorize_system(action, Some("SY.orchestrator@motherbee"), "worker1"),
                "SY.orchestrator@motherbee must be admitted for {action} on worker1"
            );
        }
        let never_sent = [
            "CONFIG_SET",
            "CONFIG_GET",
            "CONFIG_CHANGED",
            "SYSTEM_UPDATE",
            "SYSTEM_SYNC_HINT",
            "INVENTORY_REQUEST",
        ];
        let edge = [
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
            "EDGE_PUBLISH_BLOB",
            "EDGE_UNPUBLISH_BLOB",
            "EDGE_TLS_STATUS",
            "EDGE_TLS_RELOAD",
            "EDGE_ACME_CHALLENGE",
        ];
        for action in never_sent
            .iter()
            .chain(edge.iter())
            .chain(["WAN_REACHABILITY_VOUCH"].iter())
        {
            for orchestrator in [
                "SY.orchestrator@motherbee",
                "SY.orchestrator@worker1",
                "SY.orchestrator@ingress1",
            ] {
                for receiving_hive in ["motherbee", "worker1", "ingress1"] {
                    assert!(
                        !authorize_system(action, Some(orchestrator), receiving_hive),
                        "{orchestrator} must not be admitted for {action} on {receiving_hive}"
                    );
                }
            }
        }
        // Completeness: every protected action is placed on purpose — forwarded by orchestrators,
        // never sent by them, or Admin-only edge. A new protected action must be placed here.
        let mut placed: Vec<&str> = forwarded
            .iter()
            .chain(never_sent.iter())
            .chain(edge.iter())
            .copied()
            .collect();
        placed.sort_unstable();
        let mut all = PROTECTED_SYSTEM_ACTIONS.to_vec();
        all.sort_unstable();
        assert_eq!(placed, all);
    }

    /// Rule (3) has a direction (FINDINGS A-20, docs/control-path-hardening-v1.md §3 and §10):
    /// every orchestrator message that crosses hives goes from the motherbee to a spoke. A
    /// mutation comes only from the motherbee's orchestrator, never from a spoke's, not even on its
    /// own hive; a read comes from the motherbee's orchestrator or from the answering hive's own;
    /// the join/leave handshake is never admitted on the motherbee itself.
    #[test]
    fn rule_3_has_a_direction() {
        // The cases the design names.
        assert!(!authorize_system(
            "SPAWN_NODE",
            Some("SY.orchestrator@ingress1"),
            "motherbee"
        ));
        assert!(!authorize_system(
            "NODE_CONFIG_SET",
            Some("SY.orchestrator@worker1"),
            "worker1"
        ));
        assert!(authorize_system(
            "NODE_STATUS_GET",
            Some("SY.orchestrator@worker1"),
            "worker1"
        ));
        assert!(!authorize_system(
            "LIST_NODES",
            Some("SY.orchestrator@ingress1"),
            "motherbee"
        ));
        assert!(!authorize_system(
            "ADD_HIVE_FINALIZE",
            Some("SY.orchestrator@motherbee"),
            "motherbee"
        ));
        assert!(authorize_system(
            "SYSTEM_CORE_ROLLBACK",
            Some("SY.orchestrator@motherbee"),
            "ingress1"
        ));
        // The whole table: every action of rule (3), from every hive's orchestrator to every hive.
        let hives = ["motherbee", "worker1", "ingress1", "egress1"];
        for receiving_hive in hives {
            for sending_hive in hives {
                let orchestrator = format!("SY.orchestrator@{sending_hive}");
                let from_motherbee = sending_hive == "motherbee";
                let on_a_spoke = receiving_hive != "motherbee";
                let same_hive = sending_hive == receiving_hive;
                let mut expected: Vec<(&str, bool)> = Vec::new();
                expected.extend(ORCHESTRATOR_MUTATIONS.map(|action| (action, from_motherbee)));
                expected.extend(
                    SPOKE_ONLY_ACTIONS.map(|action| (action, from_motherbee && on_a_spoke)),
                );
                expected
                    .extend(ORCHESTRATOR_READS.map(|action| (action, from_motherbee || same_hive)));
                for (action, admitted) in expected {
                    assert_eq!(
                        authorize_system(action, Some(&orchestrator), receiving_hive),
                        admitted,
                        "{orchestrator} sending {action} to {receiving_hive}"
                    );
                }
            }
        }
    }

    /// The join/leave handshake is never admitted on the motherbee, from anyone: neither rule (3)
    /// nor rule (4) grants it there, where it would restart or stop the core. On a spoke it comes
    /// from the motherbee's orchestrator (3) or the spoke's own admin (4).
    #[test]
    fn the_join_leave_handshake_is_never_admitted_on_the_motherbee() {
        let hives = ["motherbee", "worker1", "ingress1"];
        for receiving_hive in hives {
            for sending_hive in hives {
                for role in ["SY.orchestrator", "SY.admin"] {
                    let sender = format!("{role}@{sending_hive}");
                    let admitted = receiving_hive != "motherbee"
                        && match role {
                            "SY.orchestrator" => sending_hive == "motherbee",
                            _ => sending_hive == receiving_hive,
                        };
                    for action in SPOKE_ONLY_ACTIONS {
                        assert_eq!(
                            authorize_system(action, Some(&sender), receiving_hive),
                            admitted,
                            "{sender} sending {action} to {receiving_hive}"
                        );
                    }
                }
            }
        }
        // Rule (4) keeps every other protected action of the motherbee's admin there.
        assert!(authorize_system(
            "SPAWN_NODE",
            Some("SY.admin@motherbee"),
            "motherbee"
        ));
    }

    #[test]
    fn protected_set_is_the_30_actions() {
        for action in [
            "SYSTEM_UPDATE",
            "SYSTEM_SYNC_HINT",
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
            "EDGE_PUBLISH_BLOB",
            "EDGE_UNPUBLISH_BLOB",
            // The edge's certificate and its ACME challenge (docs/acme-edge-certificates-v1.md §5).
            "EDGE_TLS_STATUS",
            "EDGE_TLS_RELOAD",
            "EDGE_ACME_CHALLENGE",
            "SPAWN_NODE",
            "KILL_NODE",
            "START_NODE",
            "RESTART_NODE",
            "REMOVE_NODE_INSTANCE",
            "NODE_CONFIG_SET",
            "NODE_CONFIG_GET",
            "CONFIG_SET",
            "CONFIG_GET",
            "NODE_STATE_GET",
            "NODE_STATUS_GET",
            "GET_VERSIONS",
            "GET_RUNTIMES",
            "GET_RUNTIME",
            "LIST_NODES",
            "INVENTORY_REQUEST",
            "ADD_HIVE_FINALIZE",
            "REMOVE_HIVE_CLEANUP",
            // Swaps /usr/bin binaries and restarts the core on the TARGET hive.
            "SYSTEM_CORE_ROLLBACK",
            // Carries the user OPA policy its receiver applies.
            "CONFIG_CHANGED",
        ] {
            assert!(
                is_protected_system_action(action),
                "{action} must be protected"
            );
        }
        assert_eq!(PROTECTED_SYSTEM_ACTIONS.len(), 30);
        for action in [
            "RUNTIME_UPDATE",
            "HELLO",
            "LSA",
            "",
            "TOTALLY_UNKNOWN",
            // The answers are not commands: the router's edge gate admits them only back to the
            // Admin (src/router/mod.rs), and nothing acts on them.
            "EDGE_TLS_STATUS_RESPONSE",
            "EDGE_TLS_RELOAD_RESPONSE",
            "EDGE_ACME_CHALLENGE_RESPONSE",
        ] {
            assert!(!is_protected_system_action(action));
        }
    }

    #[test]
    fn config_changed_only_from_the_primary_admin() {
        // Any local node used to be able to rewrite its hive's routes/VPNs/taps/OPA policy by
        // sending CONFIG_CHANGED (lab 2026-09-30). Today it carries only the user OPA policy, and
        // only the primary Admin sends it (to every hive, rule 2). Orchestrators no longer send
        // any (the node_config signal nobody applied was removed in 0.1.45).
        let act = "CONFIG_CHANGED";
        for hive in ["motherbee", "worker1", "ingress1", "egress1"] {
            assert!(
                authorize_system(act, Some("SY.admin@motherbee"), hive),
                "{hive}"
            );
        }
        for rogue in [
            Some("SY.orchestrator@motherbee"),
            Some("SY.orchestrator@worker1"),
            Some("AI.chat@motherbee"),
            Some("IO.slack@motherbee"),
            Some("WF.invoice@motherbee"),
            Some("SY.opa.rules@motherbee"),
            Some("SY.config.routes@motherbee"),
            Some("SY.identity@motherbee"),
            Some("SY.wf-rules@motherbee"),
            Some("SY.admin@worker1"),
            None,
        ] {
            assert!(
                !authorize_system(act, rogue, "motherbee"),
                "{rogue:?} must not send CONFIG_CHANGED"
            );
        }
    }

    #[test]
    fn edge_service_actions_are_motherbee_admin_only() {
        for action in [
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
            "EDGE_PUBLISH_BLOB",
            "EDGE_UNPUBLISH_BLOB",
            "EDGE_TLS_STATUS",
            "EDGE_TLS_RELOAD",
            "EDGE_ACME_CHALLENGE",
        ] {
            // Rule (1) answers whatever hive the edge runs on, the motherbee included.
            for edge_hive in ["edge-1", "ingress1", "motherbee"] {
                assert!(
                    authorize_system(action, Some("SY.admin@motherbee"), edge_hive),
                    "SY.admin@motherbee must be admitted for {action} on {edge_hive}"
                );
                for rogue in [
                    Some("SY.admin@edge-1"),
                    Some("SY.admin@ingress1"),
                    Some("SY.orchestrator@motherbee"),
                    Some("SY.orchestrator@ingress1"),
                    Some("SY.edge@ingress1"),
                    Some("SY.vault@motherbee"),
                    Some("SY.architect@motherbee"),
                    Some("IO.cloud@motherbee"),
                    Some("SY.admin.relay@motherbee"),
                    Some("SY.admin"),
                    None,
                ] {
                    assert!(
                        !authorize_system(action, rogue, edge_hive),
                        "{rogue:?} must be denied {action} on {edge_hive}"
                    );
                }
            }
        }
    }

    #[test]
    fn ensure_system_policy_loaded_ok_for_baked_wasms() {
        // The baked system.wasm + system_route.wasm must load & evaluate, or the router fails
        // closed at startup. This is the single guard that the committed wasms are usable.
        ensure_system_policy_loaded().expect("baked SYSTEM policy wasms must load");
    }

    #[test]
    fn route_to_frontdesk_forces_none_and_temporary_only() {
        // The moved routing decision, now OPA-backed (policy/system_route.wasm). Truth table:
        //   no src_ilk at all              -> frontdesk (the NEW None case)
        //   present + "temporary"          -> frontdesk (the moved `if`)
        //   present + partial/complete/etc -> NOT forced (flow to operator OPA)
        //   present + null status (edge self_ilk) -> NOT forced

        // (a) No ilk on the message -> always force, regardless of status value.
        assert!(route_to_frontdesk(None, false));
        assert!(route_to_frontdesk(Some("temporary"), false));
        assert!(route_to_frontdesk(Some("complete"), false));

        // (b) Present ilk: only "temporary" forces.
        assert!(route_to_frontdesk(Some("temporary"), true));
        assert!(!route_to_frontdesk(Some("partial"), true));
        assert!(!route_to_frontdesk(Some("complete"), true));
        assert!(!route_to_frontdesk(Some("unknown"), true));
        // Present-but-unresolved (edge-stamped system self_ilk): status null, src_ilk PRESENT.
        assert!(!route_to_frontdesk(None, true));
    }
}
