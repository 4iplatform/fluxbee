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
//! control plane (`SY.orchestrator@<any-hive>`).

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
];

/// The primary hive — the single motherbee. Canonical source for the whole system; the bins
/// import THIS one instead of each redefining it, so the value cannot drift.
pub const PRIMARY_HIVE_ID: &str = "motherbee";

/// The single node authorized to COMMAND an edge (open/close/list URLs) and to receive its
/// control responses: `SY.admin` on the primary hive. Canonical name — the router's
/// edge-control reachability gates and the edge's own service-command check all reference
/// THIS constant so the copies of the literal cannot drift. (`policy/system.rego` rule (1)
/// composes the same decision from role + PRIMARY_HIVE_ID.)
pub const EDGE_CONTROL_AUTHORITY: &str = "SY.admin@motherbee";

fn is_edge_service_action(action: &str) -> bool {
    matches!(
        action,
        "EDGE_OPEN_URL"
            | "EDGE_CLOSE_URL"
            | "EDGE_LIST_URLS"
            | "EDGE_PUBLISH_BLOB"
            | "EDGE_UNPUBLISH_BLOB"
    )
}

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
        tracing::error!(action, "system authority policy mutex poisoned; failing closed (deny)");
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
        let allow = ["SY.architect", "SY.config-routes", "SY.vault"];
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
                                // SY.orchestrator: any hive (cross-hive forwards).
        assert!(authorize_system(act, Some("SY.orchestrator@motherbee"), hive));
        assert!(authorize_system(act, Some("SY.orchestrator@worker1"), hive));
        assert!(authorize_system(act, Some("  SY.orchestrator@worker1  "), hive));
        // Exact role label (rejects relay/sub-name spoof) + non-empty hive.
        assert!(!authorize_system(
            act,
            Some("SY.orchestrator.relay.123@motherbee"),
            hive
        ));
        assert!(!authorize_system(act, Some("SY.orchestrator@"), hive));
        // same-hive control plane.
        for role in ["SY.admin", "SY.wf-rules", "WF.orch.diag"] {
            assert!(authorize_system(act, Some(&format!("{role}@motherbee")), hive));
            assert!(!authorize_system(act, Some(&format!("{role}@worker1")), hive));
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
            Some("SY.config-routes@motherbee"),
            Some("SY.identity@motherbee"),
        ] {
            assert!(
                !authorize_system(act, bad, hive),
                "{bad:?} must be rejected for {act}"
            );
        }
        // NODE_STATUS_GET read-probe opens to config-routes/vault same-hive only.
        assert!(authorize_system(
            "NODE_STATUS_GET",
            Some("SY.config-routes@motherbee"),
            hive
        ));
        assert!(authorize_system(
            "NODE_STATUS_GET",
            Some("SY.vault@motherbee"),
            hive
        ));
        assert!(!authorize_system(
            "NODE_STATUS_GET",
            Some("SY.config-routes@worker1"),
            hive
        ));
        assert!(!authorize_system(
            "SPAWN_NODE",
            Some("SY.config-routes@motherbee"),
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
            assert!(authorize_system(action, Some("SY.admin@motherbee"), "worker-220"));
            assert!(!authorize_system(
                action,
                Some("SY.admin@worker-220"),
                "worker-220"
            ));
            assert!(!authorize_system(action, Some("SY.admin@other"), "worker-220"));
        }
        assert!(!authorize_system(
            "SPAWN_NODE",
            Some("SY.admin@motherbee"),
            "worker-220"
        ));
    }

    #[test]
    fn config_control_denies_non_sy_origins_and_admits_orchestrator() {
        // Lock-in for the CONFIG_SET/CONFIG_GET origin-authz gate (the io.api revamp
        // moved these into node_control_actions). Only the motherbee Admin and
        // an SY.orchestrator may drive node config; any non-SY origin (a compromised or
        // rogue application node on the same VPN) MUST be denied at the delivery gate,
        // and the router remains the authority regardless of msg_type letter-case.
        let hive = "worker-220";
        for action in ["CONFIG_GET", "CONFIG_SET"] {
            // Admitted authorities.
            assert!(
                authorize_system(action, Some("SY.admin@motherbee"), hive),
                "{action}: SY.admin@motherbee must be admitted"
            );
            assert!(
                authorize_system(action, Some("SY.orchestrator@worker-220"), hive),
                "{action}: SY.orchestrator@<hive> must be admitted"
            );
            // Denied: non-SY application origins on the same VPN.
            for rogue in [
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

    #[test]
    fn protected_set_is_the_26_actions() {
        for action in [
            "SYSTEM_UPDATE",
            "SYSTEM_SYNC_HINT",
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
            "EDGE_PUBLISH_BLOB",
            "EDGE_UNPUBLISH_BLOB",
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
        ] {
            assert!(
                is_protected_system_action(action),
                "{action} must be protected"
            );
        }
        assert_eq!(PROTECTED_SYSTEM_ACTIONS.len(), 26);
        for action in ["RUNTIME_UPDATE", "HELLO", "LSA", "", "TOTALLY_UNKNOWN"] {
            assert!(!is_protected_system_action(action));
        }
    }

    #[test]
    fn edge_service_actions_are_motherbee_admin_only() {
        let ingress_hive = "edge-1";
        for action in [
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
            "EDGE_PUBLISH_BLOB",
            "EDGE_UNPUBLISH_BLOB",
        ] {
            assert!(authorize_system(action, Some("SY.admin@motherbee"), ingress_hive));
            assert!(!authorize_system(action, Some("SY.admin@edge-1"), ingress_hive));
            assert!(!authorize_system(
                action,
                Some("SY.orchestrator@motherbee"),
                ingress_hive
            ));
            assert!(!authorize_system(action, Some("IO.cloud@motherbee"), ingress_hive));
            assert!(!authorize_system(action, None, ingress_hive));
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
