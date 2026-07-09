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
    "EDGE_OPEN_URL",
    "EDGE_CLOSE_URL",
    "EDGE_LIST_URLS",
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

const PRIMARY_HIVE_ID: &str = "motherbee";

/// The single node authorized to COMMAND an edge (open/close/list URLs) and to receive its
/// control responses: `SY.admin` on the primary hive. Canonical name — the router's
/// edge-control reachability gates and the edge's own service-command check all reference
/// THIS constant so the copies of the literal cannot drift. (`authority()`'s edge branch
/// composes the same decision from role + PRIMARY_HIVE_ID.)
pub const EDGE_CONTROL_AUTHORITY: &str = "SY.admin@motherbee";

fn is_edge_service_action(action: &str) -> bool {
    matches!(
        action,
        "EDGE_OPEN_URL" | "EDGE_CLOSE_URL" | "EDGE_LIST_URLS"
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
        assert!(!same_hive_role_allowed("motherbee", Some("SY.architect"), &allow));
        assert!(!same_hive_role_allowed("motherbee", Some("SY.architect@"), &allow));
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
    fn protected_set_is_the_21_actions() {
        for action in [
            "SYSTEM_UPDATE",
            "SYSTEM_SYNC_HINT",
            "EDGE_OPEN_URL",
            "EDGE_CLOSE_URL",
            "EDGE_LIST_URLS",
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
            assert!(
                is_protected_system_action(action),
                "{action} must be protected"
            );
        }
        assert_eq!(PROTECTED_SYSTEM_ACTIONS.len(), 21);
        for action in ["RUNTIME_UPDATE", "HELLO", "LSA", "", "TOTALLY_UNKNOWN"] {
            assert!(!is_protected_system_action(action));
        }
    }

    #[test]
    fn edge_service_actions_are_motherbee_admin_only() {
        let ingress_hive = "edge-1";
        for action in ["EDGE_OPEN_URL", "EDGE_CLOSE_URL", "EDGE_LIST_URLS"] {
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
