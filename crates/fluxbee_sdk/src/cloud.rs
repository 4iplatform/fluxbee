//! Canonical Fluxbee Cloud relay vocabulary — the SINGLE source shared by SY.admin (the relay
//! authorization gate `authorize_cloud_relay`) and IO.cloud (the `op → admin action` translation),
//! so the two can never drift. IO.cloud lives in a SEPARATE cargo workspace, so this crate
//! (`fluxbee_sdk`, a dependency of both) is the only place both can import. See EDGE-06:
//! advertised == enforced.
//!
//! **Two categories of Cloud op** (docs/io-cloud-api.md §5): RELAY ops (`CLOUD_OP_ACTIONS`, relayed to
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
    // Full identity read (all `identification` PII + channels + tenant) — relayed to SY.admin's
    // `get_ilk`. The FAST existence/subset reads live in CLOUD_LOCAL_OPS (io.cloud reads the identity
    // SHM directly, no round-trip); this is the "give me everything" path that pays for the DB read.
    ("get_ilk_details", "get_ilk"),
    // Lifecycle (operator decision 2026-09-28): Cloud may MARK deleted and RESTORE -- reversible --
    // but never purge (purge_ilk / purge_tenant stay operator-only, outside this allowlist).
    // io.cloud always scopes them to the tenant Cloud claims.
    ("delete_ilk", "delete_ilk"),
    ("restore_ilk", "restore_ilk"),
    ("delete_tenant", "delete_tenant"),
    ("restore_tenant", "restore_tenant"),
    // The tenant's credentials (2026-10-13): list them (metadata only, never the value), delete
    // one, and edit the tenant's own name and domain. Always the tenant Cloud claims.
    ("list_tokens", "vault_list"),
    ("delete_token", "vault_delete"),
    ("update_tenant", "update_tenant"),
    // A tenant is a cost center (docs/ilk-tenant-model-v1.md): Cloud charges a person to another
    // one (an account from anywhere, a temporary only out of a deleted tenant).
    ("set_ilk_tenant", "set_ilk_tenant"),
    // Which of the tenant's nodes use a credential (stage 2, §2.2), without rewriting its value.
    ("assign_token", "vault_assign"),
    // Stage 4 (§2.4): the whole tenant record and every tenant but the genesis one, through the
    // admin (identity's store holds what the SHM does not: settings, sponsor, the deleted mark).
    ("get_tenant", "get_tenant"),
    ("list_tenants", "list_tenants"),
    // The tenant's nodes on every hive, and one node's status.
    ("list_nodes", "list_tenant_nodes"),
    ("get_node_status", "get_node_status"),
    // The lifecycle of the IO nodes Cloud launched, and only those (SY.admin stamps the
    // requirement; the hive that holds the node checks it): a stop lasts, a delete is the teardown.
    ("start_node", "start_node"),
    ("stop_node", "kill_node"),
    ("delete_node", "kill_node"),
    // The tenant's LLM usage, one record per model call (docs/router-stream-v1.md §7.6): a read,
    // always of the tenant Cloud claims, never every tenant's nor the root tenant's.
    ("get_usage", "get_llm_usage"),
];

/// The admin actions IO.cloud may relay over the mesh — the security allowlist SY.admin enforces in
/// `authorize_cloud_relay`. Kept as a `&[&str]` const for ergonomic `.contains()` at the gate; a
/// unit test pins it to be exactly the dedup range of [`CLOUD_OP_ACTIONS`], so the two SDK constants
/// (and therefore SY.admin's enforcement and IO.cloud's translation) can never diverge.
pub const CLOUD_EXPOSED_ACTIONS: &[&str] = &[
    "create_tenant",
    "vault_put",
    "run_node",
    "get_ilk",
    "delete_ilk",
    "restore_ilk",
    "delete_tenant",
    "restore_tenant",
    "vault_list",
    "vault_delete",
    "update_tenant",
    "set_ilk_tenant",
    "vault_assign",
    "get_tenant",
    "list_tenants",
    "list_tenant_nodes",
    "get_node_status",
    "start_node",
    "kill_node",
    "get_llm_usage",
];

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
pub const CLOUD_LOCAL_OPS: &[&str] = &[
    "register_human",
    "list_cloud_actions",
    // FAST identity reads served straight from the local identity SHM (io.cloud co-resides with
    // SY.identity on motherbee) — NO mesh round-trip, the io.api read pattern. They return the SHM
    // subset (identity + tenant + status + name); the full `identification` PII lives behind the
    // `get_ilk_details` relay. (`get_tenant` moved to the admin in stage 4: the whole record.)
    "get_ilk",
    "list_ilks",
];

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
          "summary": "Create (or find: idempotent by domain, then by name) a tenant. No envelope tenant_id: the tenant does not exist yet. params.name required, params.domain recommended (the dedup key)." },
        { "op": "update_tenant", "category": "relay",
          "summary": "Edit YOUR tenant (the envelope tenant_id), any of: params.name, params.domain (\"\" or null clears it), params.status (active or suspended: a suspended tenant gets no credentials and starts no node; active again, everything resumes), params.settings (an object), params.sponsor_tenant_id (another tenant, or null). A name or domain another tenant holds answers TENANT_NAME_TAKEN / TENANT_DOMAIN_TAKEN. Never the genesis tenant. Returns {status, tenant_id, sponsor_tenant_id}; get_tenant reads it again." },
        { "op": "get_tenant", "category": "relay",
          "summary": "The whole record of YOUR tenant (the envelope tenant_id): tenant {name, domain, status, settings, sponsor_tenant_id, deleted_at_ms}, its sponsor, children, ilk_count, people_count (people charged to it) and node_count (node identities charged to it)." },
        { "op": "list_tenants", "category": "relay",
          "summary": "Every tenant but the genesis one, deleted ones included (deleted_at_ms set, so you can restore them): tenant_id, name, domain, status, settings, sponsor_tenant_id, ilk_count, people_count, node_count. No envelope tenant_id." },
        { "op": "list_nodes", "category": "relay",
          "summary": "The nodes charged to YOUR tenant (the envelope tenant_id) on every hive: node_name, hive, runtime {name, version}, lifecycle_state (RUNNING, STARTING, STOPPED, FAILED), health_state, identity {ilk_id, tenant_id} (the ilk_id is what list_tokens shows in assigned_ilks), launched_by (cloud for yours) and relaunch_on_boot (false once stopped). A hive that does not answer is listed in unreachable_hives. Health is what each node reports: an older IO node may answer UNKNOWN." },
        { "op": "get_node_status", "category": "relay",
          "summary": "The status of one node of YOUR tenant: the envelope tenant_id + params.node_name (IO.*@hive). A node of another tenant answers NODE_NOT_FOUND." },
        { "op": "start_node", "category": "relay",
          "summary": "Start one IO node YOU launched (provision_node) in YOUR tenant: the envelope tenant_id + params.node_name. Any other node answers NODE_NOT_FOUND. Refused with TENANT_NOT_ACTIVE while the tenant is pending or suspended. Already running answers state already_running." },
        { "op": "stop_node", "category": "relay",
          "summary": "Stop one IO node YOU launched in YOUR tenant: the envelope tenant_id + params.node_name. It stays stopped, kept with its config and identity, until start_node. Any other node answers NODE_NOT_FOUND." },
        { "op": "delete_node", "category": "relay",
          "summary": "Delete one IO node YOU launched in YOUR tenant: the envelope tenant_id + params.node_name. It stops, and its timers, the credentials it owns and its identity go; credentials only assigned to it stay (they are the tenant's), and the name is free again. Final. Any other node answers NODE_NOT_FOUND." },
        { "op": "put_token", "category": "relay",
          "summary": "Store a credential for YOUR tenant (the envelope tenant_id, who pays for its use) + params.key ([a-z0-9:_-], one namespace for the whole hive: a key another tenant holds answers KEY_TAKEN) + params.value + params.resource_type. Who uses it: one IO.* node that owns it (params.owner_node), the nodes it is assigned to (params.nodes: node names or ILK ids, each charged to your tenant), or, with neither, every node of your tenant (a general credential). Storing the same value again changes nothing: change the nodes with assign_token. Infrastructure keys are refused." },
        { "op": "assign_token", "category": "relay",
          "summary": "Replace the nodes one credential of YOUR tenant is assigned to, without touching its value: the envelope tenant_id + params.key + params.nodes (node names or ILK ids, each charged to your tenant; [] leaves it unassigned, so with no owner it is general again). Every node that had it or has it now reloads it. A key that does not exist or belongs to another tenant answers KEY_NOT_FOUND." },
        { "op": "list_tokens", "category": "relay",
          "summary": "List YOUR tenant's credentials: key, metadata (with its owner ilk and assigned_ilks, the nodes that use it; neither = general), version, access_count and last_accessed_at, never the value. Needs the envelope tenant_id; optional params.resource_type, params.prefix, params.limit (default 200, max 1000)." },
        { "op": "delete_token", "category": "relay",
          "summary": "Delete one credential of YOUR tenant. Needs the envelope tenant_id + params.key. A key that does not exist or belongs to another tenant answers KEY_NOT_FOUND; infrastructure keys are refused." },
        { "op": "provision_node", "category": "relay",
          "summary": "Spawn an IO.* node in YOUR tenant. Needs the envelope tenant_id + params.node_name (IO.*) + params.runtime. A tenant that is not active (pending, suspended) starts no node: TENANT_NOT_ACTIVE. NOT idempotent." },
        { "op": "register_human", "category": "local",
          "summary": "Register a person's Fluxbee Cloud account, charged to YOUR tenant (the envelope tenant_id). params = a frontdesk_handoff {type:'frontdesk_handoff', schema_version:1, operation:'complete_registration', subject:{display_name,email,phone?,company_name?,attributes?}}. One email is one account: an email that already has one answers existing:true with its ilk_id and tenant, and nothing is created (charging it elsewhere is set_ilk_tenant). Otherwise IO.cloud provisions the person and hands it to SY.frontdesk.gov; returns the frontdesk verdict + ilk_id." },
        { "op": "set_ilk_tenant", "category": "relay",
          "summary": "Charge a person (params.ilk_id) to YOUR tenant (the envelope tenant_id): a tenant is a cost center, so this grants and revokes nothing, and the person's channels stay where they are. An account moves from anywhere; a contact that never registered only out of a deleted tenant (INVALID_TENANT_TRANSITION otherwise). The target must be active, never the root tenant. DUPLICATE_EMAIL when another person there holds the same email. Each change is recorded." },
        { "op": "list_cloud_actions", "category": "local",
          "summary": "Return this catalog (the actions IO.cloud offers: relay + local) so Cloud can discover its surface." },
        { "op": "get_ilk", "category": "local",
          "summary": "FAST existence+subset read from the identity SHM (no round-trip). Selector: params.ilk_id (ilk:<uuid>) OR params.email (the account's email, ASCII case-insensitive). By id: any ilk, whatever tenant it is charged to. By email: the one account -> {exists, ilk?, matches:[...]}; matches holds more than one entry only for an email several people still hold (ACCOUNT_AMBIGUOUS until the operator resolves it). Subset = {ilk_id, ilk_type, registration_status, tenant_id (the tenant it is charged to), display_name, pending_signup (true for a signup nobody finished; the next register_human of that email takes it over)}. For full identification + channels use get_ilk_details." },
        { "op": "list_ilks", "category": "local",
          "summary": "List the ilks charged to YOUR tenant (the envelope tenant_id) from the identity SHM (subset each). Returns {tenant_id, count, ilks:[{ilk_id, ilk_type, registration_status, tenant_id, display_name, pending_signup}]}." },
        { "op": "get_ilk_details", "category": "relay",
          "summary": "FULL identity read (all identification PII + channels + the tenant it is charged to) of one ilk — relayed to SY.admin. params.ilk_id (ilk:<uuid>) OR params.email (the account). Not tenant-filtered. One that does not exist answers NOT_FOUND. Slower than get_ilk (a DB read); use it when you need everything." },
        { "op": "delete_ilk", "category": "relay",
          "summary": "Mark one person's account deleted (reversible: hidden, its email/channels stay reserved until the operator purges it). params.ilk_id. People only: another type answers ILK_TYPE_MISMATCH (node identities are the orchestrator's). Not tenant-scoped. Idempotent." },
        { "op": "restore_ilk", "category": "relay",
          "summary": "Undo delete_ilk for one person. params.ilk_id. People only. Also while the tenant they are charged to is deleted: they come back charged to it, and set_ilk_tenant moves them." },
        { "op": "delete_tenant", "category": "relay",
          "summary": "Disable YOUR tenant (the envelope tenant_id): it is marked deleted, reversibly, and its name/domain stay reserved. No person is marked: the people charged to it stay usable and set_ilk_tenant charges them elsewhere. Answers people_charged. Refused while node identities live in it (kill them first) or while it sponsors tenants." },
        { "op": "restore_tenant", "category": "relay",
          "summary": "Undo delete_tenant for YOUR tenant: it comes back as it was." },
        { "op": "get_usage", "category": "relay",
          "summary": "Read YOUR tenant's LLM usage (the envelope tenant_id), one record per model call: a period (params.from + params.to, RFC 3339, at most 92 days) grouped by params.group_by (day, model or node) or record by record, or params.since for the records stored after a cursor, to sync as they arrive (they arrive late: a period is final only later). The answer's next is params.after for the next page or params.since for the next sync; params.limit 1 to 1000. Never another tenant's records, nor the root tenant's." }
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
            vec![
                "create_tenant",
                "vault_put",
                "run_node",
                "get_ilk",
                "delete_ilk",
                "restore_ilk",
                "delete_tenant",
                "restore_tenant",
                "vault_list",
                "vault_delete",
                "update_tenant",
                "set_ilk_tenant",
                "vault_assign",
                "get_tenant",
                "list_tenants",
                "list_tenant_nodes",
                "get_node_status",
                "start_node",
                "kill_node",
                "get_llm_usage"
            ]
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
        assert_eq!(admin_action_for_cloud_op("list_tokens"), Some("vault_list"));
        assert_eq!(
            admin_action_for_cloud_op("delete_token"),
            Some("vault_delete")
        );
        assert_eq!(
            admin_action_for_cloud_op("update_tenant"),
            Some("update_tenant")
        );
        assert_eq!(
            admin_action_for_cloud_op("set_ilk_tenant"),
            Some("set_ilk_tenant")
        );
        assert_eq!(
            admin_action_for_cloud_op("assign_token"),
            Some("vault_assign")
        );
        assert_eq!(admin_action_for_cloud_op("list_nodes"), Some("list_tenant_nodes"));
        assert_eq!(admin_action_for_cloud_op("stop_node"), Some("kill_node"));
        assert_eq!(admin_action_for_cloud_op("delete_node"), Some("kill_node"));
        assert_eq!(
            admin_action_for_cloud_op("get_usage"),
            Some("get_llm_usage")
        );
        // Cloud names its node ops; the admin action names are not Cloud ops.
        assert_eq!(admin_action_for_cloud_op("kill_node"), None);
        assert_eq!(admin_action_for_cloud_op("list_tenant_nodes"), None);
        assert_eq!(admin_action_for_cloud_op("get_llm_usage"), None);
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
