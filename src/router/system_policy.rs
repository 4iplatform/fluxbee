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
//! `authorize_system()` evaluates that policy at the delivery gate; `authority()` — the
//! byte-identical Rust table, SHADOW-VERIFIED against the Rego in tests — is the load-failure
//! FALLBACK and the sync spec. A future runtime-editable backing (a second SHM region +
//! privileged writer, "OPA-dual Phase 4") only changes how the resolver is FED — not the call
//! sites nor `authorize_system()`'s contract.
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
/// THIS constant so the copies of the literal cannot drift. (`authority()`'s edge branch
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

/// SYSTEM authority decision for a protected SYSTEM `action`, keyed on the
/// router-resolved (authoritative) `src_l2_name`. `hive_id` is THIS router's hive.
/// Returns `true` to ALLOW.
///
/// - `SY.orchestrator@<any hive>`: cross-hive forwards (a peer orchestrator
///   relays SPAWN_NODE/etc. into this hive). Authorized from ANY hive — this is
///   the cross-hive control plane and must never be denied by a user layer.
/// - `SY.admin` / `SY.wf-rules` / `WF.orch.diag`: same-hive control plane, all
///   protected actions.
/// - `SY.admin@motherbee`: cross-hive `CONFIG_GET` / `CONFIG_SET` and runtime distribution,
///   because the global Admin forwards those requests directly to managed nodes/orchestrators.
/// - `SY.config-routes` / `SY.vault`: same-hive, ONLY for `NODE_STATUS_GET` — a
///   read-only health probe that SY.architect intentionally opens to these nodes;
///   honoring it here keeps one consistent probe policy across every receiver.
///
/// `src_l2_name` is router-authoritative (resolved from the source UUID against
/// the node registry, overwriting any sender-supplied value), so this is a real
/// boundary, not a self-asserted one. The `SY.` patterns are also surfaced as
/// frozen SHM route entries for observability (see `crate::shm::FLAG_FROZEN`).
pub fn authority(action: &str, src_l2_name: Option<&str>, hive_id: &str) -> bool {
    let Some(name) = src_l2_name.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some((role, hive)) = name.split_once('@') else {
        return false;
    };
    if is_edge_service_action(action) {
        return role == "SY.admin" && hive == PRIMARY_HIVE_ID;
    }
    // Option B (WAN multi-hop reachability): only the primary hub's gateway router may vouch
    // transitive reachability. Byte-identical to system.rego rule (6); shadow-verified.
    if action == "WAN_REACHABILITY_VOUCH" {
        return role == "RT.gateway" && hive == PRIMARY_HIVE_ID;
    }
    if matches!(
        action,
        "CONFIG_GET" | "CONFIG_SET" | "SYSTEM_UPDATE" | "SYSTEM_SYNC_HINT"
    ) {
        return role == "SY.admin" && hive == PRIMARY_HIVE_ID
            || role == "SY.orchestrator" && !hive.is_empty();
    }
    if role == "SY.orchestrator" {
        // Any hive, but the hive label must be non-empty (rejects "SY.orchestrator@").
        return !hive.is_empty();
    }
    if hive != hive_id {
        return false; // every remaining role is same-hive only
    }
    if matches!(role, "SY.admin" | "SY.wf-rules" | "WF.orch.diag") {
        return true;
    }
    // Read-only health probe opened to the config/vault nodes (never mutations).
    action == "NODE_STATUS_GET" && matches!(role, "SY.config-routes" | "SY.vault")
}

/// Option B (WAN multi-hop reachability, edge-multihop-reachability-spec-v1): may the mTLS-
/// authenticated `voucher_hive` VOUCH transitive reachability of other hives' nodes to a spoke?
/// Only the primary hub may — it is the star's single relay.
///
/// OPA-BACKED, exactly like the rest of the SYSTEM authority surface: it evaluates the baked
/// `system.wasm` (`policy/system.rego` rule (6), action `WAN_REACHABILITY_VOUCH`) through
/// `authorize_system`, with the byte-identical Rust `authority()` table as the load-failure
/// fallback (shadow-verified). The voucher is identified by its gateway router name
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

/// The BAKED system policy resolver: `policy/system.wasm` (compiled from `policy/system.rego`).
/// Hive-agnostic — `hive_id` is an INPUT, not baked — so ONE resolver serves every hive. Lazily
/// loaded once; on load failure it stays unloaded and `authorize_system` falls back to the
/// byte-identical Rust `authority()` table (fail-safe).
static SYSTEM_POLICY_OPA: OnceLock<Mutex<OpaResolver>> = OnceLock::new();

fn system_policy_opa() -> &'static Mutex<OpaResolver> {
    SYSTEM_POLICY_OPA.get_or_init(|| {
        const WASM: &[u8] = include_bytes!("../../policy/system.wasm");
        let mut resolver = OpaResolver::new();
        if let Err(err) = resolver.reload(1, Some("fluxbee/system/allow".to_string()), WASM, None)
        {
            tracing::warn!(error = %err, "system-policy OPA failed to load; using the Rust authority() fallback");
        }
        Mutex::new(resolver)
    })
}

/// SYSTEM authority decision, OPA-BACKED — this is what the router's delivery gate calls.
/// Evaluates the baked Rego policy (entrypoint `fluxbee/system/allow`) with input
/// `{action, src_l2_name, hive_id}`, and FALLS BACK to the byte-identical Rust `authority()` if
/// the policy is not loaded or errs (so a wasm load failure never opens or closes the gate
/// wrongly). The Rego and the fallback are kept in lock-step by the shadow-verify test.
///
/// NOTE: locks a Mutex + runs a wasm eval per protected-action delivery — control-plane path
/// only (protected SYSTEM actions), and the policy is a tiny fixed program.
pub fn authorize_system(action: &str, src_l2_name: Option<&str>, hive_id: &str) -> bool {
    if let Ok(mut resolver) = system_policy_opa().lock() {
        let input = serde_json::json!({
            "action": action,
            "src_l2_name": src_l2_name,
            "hive_id": hive_id,
        });
        if let Ok(allow) = resolver.evaluate_allow(&input, None) {
            return allow;
        }
    }
    authority(action, src_l2_name, hive_id)
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
/// router's `authority()` rule (F-07). `name` is allowed iff it starts with one of
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
    fn authorize_system_matches_authority_via_baked_wasm() {
        // The production gate entry (authorize_system, OPA-backed by the lazily-loaded baked
        // system.wasm) must agree with the Rust authority() fallback across the matrix — proving
        // the wired-in Rego policy is byte-identical and the gate's behavior is unchanged.
        let actions = [
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
            "EDGE_PUBLISH_BLOB",
            "EDGE_UNPUBLISH_BLOB",
            "SPAWN_NODE",
            "KILL_NODE",
            "NODE_STATUS_GET",
            "NODE_CONFIG_SET",
            "CONFIG_SET",
            "CONFIG_GET",
            "SYSTEM_SYNC_HINT",
            "ADD_HIVE_FINALIZE",
            "SYSTEM_UPDATE",
            "WAN_REACHABILITY_VOUCH",
        ];
        let names: [Option<&str>; 16] = [
            Some("SY.admin@motherbee"),
            Some("SY.admin@worker1"),
            Some("SY.admin@edge-1"),
            Some("SY.orchestrator@motherbee"),
            Some("SY.orchestrator@worker1"),
            Some("SY.orchestrator@"),
            Some("SY.wf-rules@motherbee"),
            Some("WF.orch.diag@motherbee"),
            Some("SY.config-routes@motherbee"),
            Some("SY.vault@motherbee"),
            Some("IO.cloud@motherbee"),
            Some("RT.gateway@motherbee"),
            Some("RT.gateway@worker1"),
            Some("RT.gateway@ingress1"),
            Some(""),
            None,
        ];
        for action in actions {
            for name in names {
                for hive in ["motherbee", "ingress1"] {
                    assert_eq!(
                        authorize_system(action, name, hive),
                        authority(action, name, hive),
                        "authorize_system != authority for action={action} name={name:?} hive={hive}"
                    );
                }
            }
        }
    }

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
    fn authority_enforces_role_and_hive_scope() {
        let hive = "motherbee";
        let act = "SPAWN_NODE"; // a mutation, strict scope
                                // SY.orchestrator: any hive (cross-hive forwards).
        assert!(authority(act, Some("SY.orchestrator@motherbee"), hive));
        assert!(authority(act, Some("SY.orchestrator@worker1"), hive));
        assert!(authority(act, Some("  SY.orchestrator@worker1  "), hive));
        // Exact role label (rejects relay/sub-name spoof) + non-empty hive.
        assert!(!authority(
            act,
            Some("SY.orchestrator.relay.123@motherbee"),
            hive
        ));
        assert!(!authority(act, Some("SY.orchestrator@"), hive));
        // same-hive control plane.
        for role in ["SY.admin", "SY.wf-rules", "WF.orch.diag"] {
            assert!(authority(act, Some(&format!("{role}@motherbee")), hive));
            assert!(!authority(act, Some(&format!("{role}@worker1")), hive));
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
                !authority(act, bad, hive),
                "{bad:?} must be rejected for {act}"
            );
        }
        // NODE_STATUS_GET read-probe opens to config-routes/vault same-hive only.
        assert!(authority(
            "NODE_STATUS_GET",
            Some("SY.config-routes@motherbee"),
            hive
        ));
        assert!(authority(
            "NODE_STATUS_GET",
            Some("SY.vault@motherbee"),
            hive
        ));
        assert!(!authority(
            "NODE_STATUS_GET",
            Some("SY.config-routes@worker1"),
            hive
        ));
        assert!(!authority(
            "SPAWN_NODE",
            Some("SY.config-routes@motherbee"),
            hive
        ));
        assert!(!authority(
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
            assert!(authority(action, Some("SY.admin@motherbee"), "worker-220"));
            assert!(!authority(
                action,
                Some("SY.admin@worker-220"),
                "worker-220"
            ));
            assert!(!authority(action, Some("SY.admin@other"), "worker-220"));
        }
        assert!(!authority(
            "SPAWN_NODE",
            Some("SY.admin@motherbee"),
            "worker-220"
        ));
    }

    #[test]
    fn protected_set_is_the_25_actions() {
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
        ] {
            assert!(
                is_protected_system_action(action),
                "{action} must be protected"
            );
        }
        assert_eq!(PROTECTED_SYSTEM_ACTIONS.len(), 25);
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
            assert!(authority(action, Some("SY.admin@motherbee"), ingress_hive));
            assert!(!authority(action, Some("SY.admin@edge-1"), ingress_hive));
            assert!(!authority(
                action,
                Some("SY.orchestrator@motherbee"),
                ingress_hive
            ));
            assert!(!authority(action, Some("IO.cloud@motherbee"), ingress_hive));
            assert!(!authority(action, None, ingress_hive));
        }
    }
}
