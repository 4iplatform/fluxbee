//! The router's SYSTEM policy layer — the non-user-editable half of fluxbee's
//! two-layer (system + user) policy model ("OPA-dual").
//!
//! The USER layer is OPA: operator-authored Rego, compiled to WASM by the
//! `SY.opa.rules` node, loaded by the router's `OpaResolver` (see `crate::opa`).
//! It selects routing targets and (in future) may *narrow* authority.
//!
//! This SYSTEM layer is the authoritative half: the `SY.` origin-authority rules.
//! It is intentionally a fixed Rust table — structurally unreachable from the
//! `/opa/policy*` authoring endpoints (those only reach the user OPA blob), so it
//! is non-user-editable by construction, with no SHM region, file, or RPC to
//! tamper with. The operator accepts these `SY.` rules as hardcoded for now.
//!
//! `authority()` is a STABLE SEAM: a future protected, Rego-backed system layer
//! (a second OPA module/region) can replace the backing without changing the
//! router's call sites or the composition order. Composition contract:
//! `final_allow = system_allow AND user_allow`, and a SYSTEM deny is FINAL — the
//! user layer may only narrow, never broaden, and can never sever the cross-hive
//! control plane (`SY.orchestrator@<any-hive>`).

/// Protected SYSTEM actions (matched on `meta.msg`) that a node may only act on
/// when the router-resolved origin is authorized. Single source of truth — moved
/// out of the orchestrator so origin-authority is enforced once, centrally, at
/// delivery time.
pub const PROTECTED_SYSTEM_ACTIONS: &[&str] = &[
    "SYSTEM_UPDATE",
    "SYSTEM_SYNC_HINT",
    "SPAWN_NODE",
    "KILL_NODE",
    "START_NODE",
    "RESTART_NODE",
    "REMOVE_NODE_INSTANCE",
    "NODE_CONFIG_SET",
    "NODE_CONFIG_GET",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_enforces_role_and_hive_scope() {
        let hive = "motherbee";
        let act = "SPAWN_NODE"; // a mutation, strict scope
        // SY.orchestrator: any hive (cross-hive forwards).
        assert!(authority(act, Some("SY.orchestrator@motherbee"), hive));
        assert!(authority(act, Some("SY.orchestrator@worker1"), hive));
        assert!(authority(act, Some("  SY.orchestrator@worker1  "), hive));
        // Exact role label (rejects relay/sub-name spoof) + non-empty hive.
        assert!(!authority(act, Some("SY.orchestrator.relay.123@motherbee"), hive));
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
            assert!(!authority(act, bad, hive), "{bad:?} must be rejected for {act}");
        }
        // NODE_STATUS_GET read-probe opens to config-routes/vault same-hive only.
        assert!(authority("NODE_STATUS_GET", Some("SY.config-routes@motherbee"), hive));
        assert!(authority("NODE_STATUS_GET", Some("SY.vault@motherbee"), hive));
        assert!(!authority("NODE_STATUS_GET", Some("SY.config-routes@worker1"), hive));
        assert!(!authority("SPAWN_NODE", Some("SY.config-routes@motherbee"), hive));
        assert!(!authority("NODE_STATUS_GET", Some("AI.evil@motherbee"), hive));
    }

    #[test]
    fn protected_set_is_the_18_actions() {
        for action in [
            "SYSTEM_UPDATE",
            "SYSTEM_SYNC_HINT",
            "SPAWN_NODE",
            "KILL_NODE",
            "START_NODE",
            "RESTART_NODE",
            "REMOVE_NODE_INSTANCE",
            "NODE_CONFIG_SET",
            "NODE_CONFIG_GET",
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
            assert!(is_protected_system_action(action), "{action} must be protected");
        }
        assert_eq!(PROTECTED_SYSTEM_ACTIONS.len(), 18);
        for action in ["RUNTIME_UPDATE", "HELLO", "LSA", "", "TOTALLY_UNKNOWN"] {
            assert!(!is_protected_system_action(action));
        }
    }
}
