//! Canonical Fluxbee Cloud relay vocabulary — the SINGLE source shared by SY.admin (the relay
//! authorization gate `authorize_cloud_relay`) and IO.cloud (the `op → admin action` translation),
//! so the two can never drift. IO.cloud lives in a SEPARATE cargo workspace, so this crate
//! (`fluxbee_sdk`, a dependency of both) is the only place both can import. See EDGE-06:
//! advertised == enforced.
//!
//! **Two categories of Cloud op** (io-cloud-spec-v1 §3.3): RELAY ops (`CLOUD_OP_ACTIONS`, relayed to
//! `SY.admin`) and io.cloud-LOCAL ops (`CLOUD_LOCAL_OPS`, handled by IO.cloud itself — they NEVER
//! touch `SY.admin`, e.g. `register_human` provisions the ilk + hands off to the frontdesk).
//! `cloud_action_catalog()` is the discoverable surface (relay + local, with help) that IO.cloud's
//! `list_cloud_actions` op returns to Cloud.

use serde_json::{json, Value};

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

/// Cloud ops IO.cloud handles LOCALLY — it does the work itself instead of relaying to `SY.admin`.
/// Kept SEPARATE from `CLOUD_OP_ACTIONS` (the admin-relay allowlist that `authorize_cloud_relay`
/// enforces) so a local op can NEVER leak into the relay gate: local ops never send an ADMIN_COMMAND.
/// - `register_human`: provisions a temporary human ilk (`ILK_PROVISION`, IO-only) and hands the
///   frontdesk_handoff to `SY.frontdesk.gov` — admin can do neither the provision nor the register.
/// - `list_cloud_actions`: IO.cloud answers Cloud's discovery query from `cloud_action_catalog()`.
pub const CLOUD_LOCAL_OPS: &[&str] = &["register_human", "list_cloud_actions"];

/// Whether `op` is an io.cloud-local op (dispatched by IO.cloud itself, not relayed to admin).
pub fn is_cloud_local_op(op: &str) -> bool {
    CLOUD_LOCAL_OPS.contains(&op)
}

/// The discoverable Cloud action catalog — both categories (relay + local) with a one-line `summary`
/// so Fluxbee Cloud can DISCOVER what IO.cloud offers and how to use it (returned by the
/// `list_cloud_actions` op). Single source; the detailed contract lives in `docs/io-cloud-api.md`.
pub fn cloud_action_catalog() -> Value {
    json!([
        { "op": "create_tenant", "category": "relay",
          "summary": "Create (or find — idempotent by domain/name) a tenant. tenant_id ignored; params.name required, params.domain recommended as the dedup key." },
        { "op": "put_token", "category": "relay",
          "summary": "Store a provider credential in the hive vault. Needs tenant_id (root) + params.key ([a-z0-9:_-], GLOBAL namespace) + params.value + params.resource_type; optional owner_node (an existing IO.* node) scopes it." },
        { "op": "provision_node", "category": "relay",
          "summary": "Spawn an IO.* node. Needs tenant_id (root) + params.node_name (IO.*) + params.runtime. NOT idempotent." },
        { "op": "register_human", "category": "local",
          "summary": "Register a human's identity (mint the human ilk). Needs tenant_id (root) + params = a frontdesk_handoff {type:'frontdesk_handoff', schema_version:1, operation:'complete_registration', subject:{display_name,email,phone?,company_name?,attributes?}}. IO.cloud provisions the temporary human ilk and hands it to SY.frontdesk.gov; returns the frontdesk verdict + ilk_id." },
        { "op": "list_cloud_actions", "category": "local",
          "summary": "Return this catalog (the actions IO.cloud offers: relay + local) so Cloud can discover its surface." }
    ])
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

    #[test]
    fn local_ops_are_disjoint_from_relay_ops() {
        // A local op must NEVER be a relay op — else it would leak into authorize_cloud_relay's
        // admin allowlist. is_cloud_local_op and admin_action_for_cloud_op must never both hit.
        for op in CLOUD_LOCAL_OPS {
            assert!(is_cloud_local_op(op));
            assert_eq!(
                admin_action_for_cloud_op(op),
                None,
                "local op {op} must not map to an admin relay action"
            );
        }
        for (relay_op, _) in CLOUD_OP_ACTIONS {
            assert!(!is_cloud_local_op(relay_op), "relay op {relay_op} must not be local");
        }
    }

    #[test]
    fn catalog_covers_every_relay_and_local_op() {
        let catalog = cloud_action_catalog();
        let ops: std::collections::HashSet<&str> = catalog
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e.get("op").and_then(|v| v.as_str()).unwrap())
            .collect();
        for (relay_op, _) in CLOUD_OP_ACTIONS {
            assert!(ops.contains(relay_op), "catalog missing relay op {relay_op}");
        }
        for local_op in CLOUD_LOCAL_OPS {
            assert!(ops.contains(local_op), "catalog missing local op {local_op}");
        }
        // Reverse (no drift / mis-category): every catalog row is a KNOWN op in exactly one set, and
        // its "category" matches that set. Catches a hand-edited catalog with a bogus/mis-tagged op.
        for entry in catalog.as_array().unwrap() {
            let op = entry.get("op").and_then(|v| v.as_str()).unwrap();
            let category = entry.get("category").and_then(|v| v.as_str()).unwrap();
            let is_relay = admin_action_for_cloud_op(op).is_some();
            let is_local = is_cloud_local_op(op);
            assert!(is_relay ^ is_local, "catalog op {op} must be exactly one of relay|local");
            assert_eq!(
                category,
                if is_local { "local" } else { "relay" },
                "catalog op {op} has the wrong category"
            );
        }
    }
}
