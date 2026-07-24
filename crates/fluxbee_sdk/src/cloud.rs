//! Canonical Fluxbee Cloud relay vocabulary — the SINGLE source shared by SY.admin (the relay
//! authorization gate `authorize_cloud_relay`) and IO.cloud (the `op → admin action` translation),
//! so the two can never drift. IO.cloud lives in a SEPARATE cargo workspace, so this crate
//! (`fluxbee_sdk`, a dependency of both) is the only place both can import. See EDGE-06:
//! advertised == enforced.

/// Cloud op → admin action mapping. IO.cloud accepts these ops and relays each as the mapped admin
/// action; SY.admin authorizes ONLY the mapped actions over the io.cloud relay. Adding a Cloud
/// capability = add one row here (plus the op's param-translation in io.cloud and, if the action is
/// new, the admin handler) — nowhere else.
pub const CLOUD_OP_ACTIONS: &[(&str, &str)] = &[
    ("create_tenant", "create_tenant"),
    ("put_token", "vault_put"),
    ("provision_node", "run_node"),
];

/// The admin actions IO.cloud may relay over the mesh — the security allowlist SY.admin enforces in
/// `authorize_cloud_relay`. Kept as a `&[&str]` const for ergonomic `.contains()` at the gate; a
/// unit test pins it to be exactly the dedup range of [`CLOUD_OP_ACTIONS`], so the two SDK constants
/// (and therefore SY.admin's enforcement and IO.cloud's translation) can never diverge.
pub const CLOUD_EXPOSED_ACTIONS: &[&str] = &["create_tenant", "vault_put", "run_node"];

/// The [`CLOUD_EXPOSED_ACTIONS`] set computed from [`CLOUD_OP_ACTIONS`] (deduped, first-seen order).
pub fn cloud_exposed_actions() -> Vec<&'static str> {
    let mut actions: Vec<&'static str> = Vec::new();
    for (_, action) in CLOUD_OP_ACTIONS {
        if !actions.contains(action) {
            actions.push(action);
        }
    }
    actions
}

/// The admin action a Cloud `op` maps to, or `None` if the op is not exposed.
pub fn admin_action_for_cloud_op(op: &str) -> Option<&'static str> {
    CLOUD_OP_ACTIONS
        .iter()
        .find(|(cloud_op, _)| *cloud_op == op)
        .map(|(_, action)| *action)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposed_actions_are_the_dedup_range_of_the_map() {
        // The allowlist SY.admin enforces is exactly the set of actions io.cloud can produce.
        assert_eq!(
            cloud_exposed_actions(),
            vec!["create_tenant", "vault_put", "run_node"]
        );
    }

    #[test]
    fn const_allowlist_matches_the_derived_range() {
        // CLOUD_EXPOSED_ACTIONS (the const used at the gate) must equal the range of CLOUD_OP_ACTIONS,
        // so a new op row can never silently leave the security allowlist stale.
        assert_eq!(CLOUD_EXPOSED_ACTIONS.to_vec(), cloud_exposed_actions());
    }

    #[test]
    fn op_lookup_matches_the_table() {
        assert_eq!(admin_action_for_cloud_op("create_tenant"), Some("create_tenant"));
        assert_eq!(admin_action_for_cloud_op("put_token"), Some("vault_put"));
        assert_eq!(admin_action_for_cloud_op("provision_node"), Some("run_node"));
        assert_eq!(admin_action_for_cloud_op("kill_node"), None);
    }
}
