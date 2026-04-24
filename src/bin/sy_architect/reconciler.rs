// Deterministic manifest reconciler — Track B.
// Source spec: fluxbee_rearchitecture_master_spec_v4.md — sections 6, 7, 8
//
// This module contains only PURE deterministic functions.
// No AI calls, no network calls. The snapshot is built by build_actual_state_snapshot()
// in sy_architect.rs and passed in. Same inputs → same outputs always.

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::pipeline_types::{
    desired_state_unknown_sections, is_valid_ownership_label, restart_policy_for_node,
    ActualStateSnapshot, BuildTaskKnownContext, BuildTaskPacket, ChangeType, CompilerClass,
    DeltaOperation, DeltaReport, DeltaReportStatus, DeltaSummary, DesiredHive, DesiredNode,
    DesiredOpaDeployment, DesiredRoute, DesiredRuntime, DesiredVpn, DesiredWfDeployment,
    ReconcilerOutput, RestartPolicy, SolutionManifestV2, MAX_ARTIFACT_ATTEMPTS,
    UNSUPPORTED_DESIRED_STATE_SECTIONS,
};

// ── Manifest validation ───────────────────────────────────────────────────────

pub fn validate_manifest_sections(manifest: &SolutionManifestV2) -> Result<(), String> {
    let unknown_sections = desired_state_unknown_sections(&manifest.desired_state);
    if let Some(section) = unknown_sections.first() {
        let detail = if UNSUPPORTED_DESIRED_STATE_SECTIONS.contains(&section.as_str()) {
            "is not supported in this architecture version"
        } else {
            "is not a recognized desired_state section"
        };
        return Err(format!(
            "UNSUPPORTED_DESIRED_STATE_SECTION: '{}' {}. Remove it from desired_state or use advisory.",
            section, detail
        ));
    }

    if let Some(runtimes) = &manifest.desired_state.runtimes {
        for (idx, item) in runtimes.iter().enumerate() {
            let runtime: DesiredRuntime = serde_json::from_value(item.clone())
                .map_err(|err| format!("desired_state.runtimes[{idx}] invalid: {err}"))?;
            if runtime.name.trim().is_empty() {
                return Err(format!(
                    "desired_state.runtimes[{idx}].name must not be empty"
                ));
            }
            if !is_valid_ownership_label(&runtime.ownership) {
                return Err(format!(
                    "desired_state.runtimes[{idx}].ownership must be one of solution/system/external"
                ));
            }
        }
    }

    Ok(())
}

// ── Helper: extract typed list from a JSON value ─────────────────────────────

fn extract_list<T: serde::de::DeserializeOwned>(value: Option<&Vec<serde_json::Value>>) -> Vec<T> {
    value
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value::<T>(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn snapshot_item_values(payload: Option<&serde_json::Value>) -> Vec<serde_json::Value> {
    if let Some(arr) = payload.and_then(|v| v.as_array()) {
        return arr.clone();
    }
    if let Some(arr) = payload
        .and_then(|v| v.get("items"))
        .and_then(|v| v.as_array())
    {
        return arr.clone();
    }
    if let Some(arr) = payload
        .and_then(|v| v.get("responses"))
        .and_then(|v| v.as_array())
    {
        return arr
            .iter()
            .filter_map(|entry| {
                let mut payload = entry.get("payload")?.clone();
                if let (Some(obj), Some(hive)) = (
                    payload.as_object_mut(),
                    entry.get("hive").and_then(|v| v.as_str()),
                ) {
                    obj.entry("hive".to_string())
                        .or_insert_with(|| serde_json::json!(hive));
                }
                Some(payload)
            })
            .collect();
    }
    Vec::new()
}

fn extract_snapshot_keys(payload: Option<&serde_json::Value>, key_field: &str) -> HashSet<String> {
    let arr = snapshot_item_values(payload);

    arr.iter()
        .filter_map(|item| {
            item.get(key_field)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect()
}

fn new_op_id() -> String {
    format!("op-{}", Uuid::new_v4().simple())
}

// ── TB-4 — Topology: hives and VPNs ──────────────────────────────────────────

pub fn reconcile_topology(
    desired_topology: &serde_json::Value,
    snapshot: &ActualStateSnapshot,
) -> (Vec<DeltaOperation>, Vec<String>) {
    let mut ops = Vec::new();
    let mut diagnostics = Vec::new();

    let hives: Vec<DesiredHive> = desired_topology
        .get("hives")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value::<DesiredHive>(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let snapshot_hives: HashSet<String> = snapshot.resources.keys().cloned().collect();

    for hive in &hives {
        if !snapshot_hives.contains(&hive.hive_id) {
            ops.push(DeltaOperation {
                op_id: new_op_id(),
                resource_type: "hive".to_string(),
                resource_id: hive.hive_id.clone(),
                change_type: ChangeType::Create,
                ownership: if hive.ownership.is_empty() {
                    "solution".to_string()
                } else {
                    hive.ownership.clone()
                },
                compiler_class: CompilerClass::HiveCreate,
                desired_ref: Some(format!("desired_state.topology.hives[{}]", hive.hive_id)),
                actual_ref: None,
                blocking: false,
                payload_ref: None,
                notes: vec![],
            });
        }
    }

    let vpns: Vec<DesiredVpn> = desired_topology
        .get("vpns")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| serde_json::from_value::<DesiredVpn>(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default();

    let desired_vpn_ids: HashSet<String> = vpns.iter().map(|v| v.vpn_id.clone()).collect();

    for vpn in &vpns {
        let hive_res = snapshot.resources.get(&vpn.hive_id);
        let snapshot_vpns = extract_snapshot_keys(hive_res.and_then(|r| r.vpns.as_ref()), "vpn_id");
        if !snapshot_vpns.contains(&vpn.vpn_id) {
            ops.push(DeltaOperation {
                op_id: new_op_id(),
                resource_type: "vpn".to_string(),
                resource_id: vpn.vpn_id.clone(),
                change_type: ChangeType::Create,
                ownership: if vpn.ownership.is_empty() {
                    "solution".to_string()
                } else {
                    vpn.ownership.clone()
                },
                compiler_class: CompilerClass::VpnCreate,
                desired_ref: Some(format!("desired_state.topology.vpns[{}]", vpn.vpn_id)),
                actual_ref: None,
                blocking: false,
                payload_ref: None,
                notes: vec![],
            });
        }
    }

    // Emit VPN_DELETE for solution-owned VPNs in snapshot but absent in desired
    for (hive_id, hive_res) in &snapshot.resources {
        for item in snapshot_item_values(hive_res.vpns.as_ref()) {
            let vpn_id = item
                .get("vpn_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if vpn_id.is_empty() || desired_vpn_ids.contains(&vpn_id) {
                continue;
            }
            let ownership = item
                .get("ownership")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if ownership == "solution" {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "vpn".to_string(),
                    resource_id: vpn_id.clone(),
                    change_type: ChangeType::Delete,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::VpnDelete,
                    desired_ref: None,
                    actual_ref: Some(format!("snapshot.vpns.{hive_id}.{vpn_id}")),
                    blocking: false,
                    payload_ref: None,
                    notes: vec!["solution-owned vpn absent from desired_state".to_string()],
                });
            } else {
                diagnostics.push(format!(
                    "VPN '{vpn_id}' on hive '{hive_id}' not in desired_state but ownership is not confirmed as solution"
                ));
            }
        }
    }

    (ops, diagnostics)
}

// ── TB-5 — Runtimes ───────────────────────────────────────────────────────────

pub fn reconcile_runtimes(
    desired_runtimes: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
    multi_hive: bool,
) -> (Vec<DeltaOperation>, Vec<BuildTaskPacket>) {
    let mut ops = Vec::new();
    let mut packets = Vec::new();

    let desired: Vec<DesiredRuntime> = desired_runtimes
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredRuntime>(item.clone()).ok())
        .collect();

    // Collect all runtime names present in any hive snapshot
    let mut snapshot_runtimes: HashMap<String, String> = HashMap::new(); // name -> version
    for (_hive, hive_res) in &snapshot.resources {
        for item in &snapshot_item_values(hive_res.runtimes.as_ref()) {
            let name = item
                .get("name")
                .or_else(|| item.get("runtime"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let version = item
                .get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                snapshot_runtimes.entry(name).or_insert(version);
            }
        }
    }

    // Collect available runtime names for BuildTaskPacket context
    let available_runtimes: Vec<String> = snapshot_runtimes.keys().cloned().collect();

    for (idx, desired_rt) in desired.iter().enumerate() {
        let actual_version = snapshot_runtimes.get(&desired_rt.name);

        let (change_type, compiler_class, needs_artifact) = match actual_version {
            None => {
                // Missing entirely — needs publish
                let cc = if multi_hive || !desired_rt.sync_to.is_empty() {
                    CompilerClass::RuntimePublishAndDistribute
                } else {
                    CompilerClass::RuntimePublishOnly
                };
                (ChangeType::Create, cc, desired_rt.needs_artifact())
            }
            Some(snapshot_ver) => {
                let desired_ver = &desired_rt.version;
                if !desired_ver.is_empty() && snapshot_ver != desired_ver {
                    // Version drift — re-publish
                    (
                        ChangeType::Update,
                        CompilerClass::RuntimePublishAndDistribute,
                        desired_rt.needs_artifact(),
                    )
                } else {
                    // Up to date
                    (ChangeType::Noop, CompilerClass::Noop, false)
                }
            }
        };

        if change_type == ChangeType::Noop {
            continue;
        }

        let op_id = new_op_id();

        if needs_artifact {
            packets.push(BuildTaskPacket {
                task_packet_version: "0.1".to_string(),
                task_id: format!("art-{}", Uuid::new_v4().simple()),
                artifact_kind: "runtime_package".to_string(),
                source_delta_op: op_id.clone(),
                runtime_name: Some(desired_rt.name.clone()),
                target_kind: if desired_rt.package_source == "bundle_upload" {
                    "bundle_upload".to_string()
                } else {
                    "inline_package".to_string()
                },
                requirements: serde_json::json!({
                    "package_type": "config_only",
                    "runtime_name": desired_rt.name,
                    "version": desired_rt.version,
                }),
                constraints: serde_json::json!({
                    "must_be_publishable": true,
                    "must_match_naming_policy": true,
                }),
                known_context: BuildTaskKnownContext {
                    available_runtimes: available_runtimes.clone(),
                    running_nodes: vec![],
                },
                attempt: 0,
                max_attempts: MAX_ARTIFACT_ATTEMPTS,
                cookbook_context: vec![],
            });
        }

        ops.push(DeltaOperation {
            op_id,
            resource_type: "runtime".to_string(),
            resource_id: desired_rt.name.clone(),
            change_type,
            ownership: if desired_rt.ownership.is_empty() {
                "solution".to_string()
            } else {
                desired_rt.ownership.clone()
            },
            compiler_class,
            desired_ref: Some(format!("desired_state.runtimes[{idx}]")),
            actual_ref: actual_version.map(|v| format!("snapshot.runtimes.{}", v)),
            blocking: needs_artifact, // blocks plan compilation until artifact is approved
            payload_ref: None,        // filled in after artifact approval
            notes: vec![],
        });
    }

    (ops, packets)
}

// ── TB-6 — Nodes ──────────────────────────────────────────────────────────────

pub fn reconcile_nodes(
    desired_nodes: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredNode> = desired_nodes
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredNode>(item.clone()).ok())
        .collect();

    // Build snapshot node map: node_name -> node_json
    let mut snapshot_nodes: HashMap<String, serde_json::Value> = HashMap::new();
    for (_hive, hive_res) in &snapshot.resources {
        for item in snapshot_item_values(hive_res.nodes.as_ref()) {
            let name = item
                .get("node_name")
                .or_else(|| item.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                snapshot_nodes.insert(name, item);
            }
        }
    }

    let desired_node_names: HashSet<String> = desired.iter().map(|n| n.node_name.clone()).collect();

    for (idx, node) in desired.iter().enumerate() {
        match snapshot_nodes.get(&node.node_name) {
            None => {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "node".to_string(),
                    resource_id: node.node_name.clone(),
                    change_type: ChangeType::Create,
                    ownership: node.ownership.clone(),
                    compiler_class: CompilerClass::NodeRunMissing,
                    desired_ref: Some(format!("desired_state.nodes[{idx}]")),
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                });
            }
            Some(actual_node) => {
                let actual_runtime = actual_node
                    .get("runtime")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let runtime_changed = !node.runtime.is_empty() && actual_runtime != node.runtime;

                // Check config drift
                let desired_config = serde_json::to_string(&node.config).unwrap_or_default();
                let actual_config = actual_node
                    .get("config")
                    .map(|c| serde_json::to_string(c).unwrap_or_default())
                    .unwrap_or_default();
                let config_changed = desired_config != actual_config
                    && !node.config.is_null()
                    && desired_config != "{}";

                if runtime_changed || config_changed {
                    // Runtime change is always immutable — treated same as blocked field

                    let policy = restart_policy_for_node(&node.node_name, runtime_changed);
                    let compiler_class = match policy {
                        RestartPolicy::Hot => CompilerClass::NodeConfigApplyHot,
                        RestartPolicy::RequiresRestart => CompilerClass::NodeConfigApplyRestart,
                        RestartPolicy::Blocked => {
                            if node.reconcile_policy.allows_recreate() {
                                CompilerClass::NodeRecreate
                            } else {
                                CompilerClass::BlockedImmutableChange
                            }
                        }
                    };

                    ops.push(DeltaOperation {
                        op_id: new_op_id(),
                        resource_type: "node".to_string(),
                        resource_id: node.node_name.clone(),
                        change_type: ChangeType::Update,
                        ownership: node.ownership.clone(),
                        blocking: compiler_class == CompilerClass::BlockedImmutableChange,
                        compiler_class,
                        desired_ref: Some(format!("desired_state.nodes[{idx}]")),
                        actual_ref: Some(format!("snapshot.nodes.{}", node.node_name)),
                        payload_ref: None,
                        notes: if runtime_changed {
                            vec!["runtime field changed — immutable without recreate policy"
                                .to_string()]
                        } else {
                            vec![]
                        },
                    });
                }
                // Noop if config matches
            }
        }
    }

    // Emit NODE_KILL for solution-owned nodes in snapshot but absent in desired
    for (node_name, actual_node) in &snapshot_nodes {
        if !desired_node_names.contains(node_name) {
            let ownership = actual_node
                .get("ownership")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if ownership == "solution" {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "node".to_string(),
                    resource_id: node_name.clone(),
                    change_type: ChangeType::Delete,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::NodeKill,
                    desired_ref: None,
                    actual_ref: Some(format!("snapshot.nodes.{node_name}")),
                    blocking: false,
                    payload_ref: None,
                    notes: vec!["solution-owned node absent from desired_state".to_string()],
                });
            }
        }
    }

    ops
}

// ── TB-7 — Routes ─────────────────────────────────────────────────────────────

pub fn reconcile_routes(
    desired_routes: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredRoute> = desired_routes
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredRoute>(item.clone()).ok())
        .collect();

    // Build snapshot route map: identity_key -> full route json
    let mut snapshot_routes: HashMap<String, serde_json::Value> = HashMap::new();
    for (hive_id, hive_res) in &snapshot.resources {
        for item in snapshot_item_values(hive_res.routes.as_ref()) {
            let prefix = item.get("prefix").and_then(|v| v.as_str()).unwrap_or("");
            let action = item.get("action").and_then(|v| v.as_str()).unwrap_or("");
            let key = format!("{hive_id}:{prefix}:{action}");
            snapshot_routes.insert(key, item);
        }
    }

    let desired_route_keys: HashSet<String> = desired.iter().map(|r| r.identity_key()).collect();

    for (idx, route) in desired.iter().enumerate() {
        let key = route.identity_key();
        match snapshot_routes.get(&key) {
            None => {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "route".to_string(),
                    resource_id: key,
                    change_type: ChangeType::Create,
                    ownership: route.ownership.clone(),
                    compiler_class: CompilerClass::RouteAdd,
                    desired_ref: Some(format!("desired_state.routing[{idx}]")),
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                });
            }
            Some(actual) => {
                // Check if fields differ (e.g. next_hop_hive changed)
                let actual_hop = actual.get("next_hop_hive").and_then(|v| v.as_str());
                let desired_hop = route.next_hop_hive.as_deref();
                if desired_hop != actual_hop {
                    ops.push(DeltaOperation {
                        op_id: new_op_id(),
                        resource_type: "route".to_string(),
                        resource_id: key,
                        change_type: ChangeType::Update,
                        ownership: route.ownership.clone(),
                        compiler_class: CompilerClass::RouteReplace,
                        desired_ref: Some(format!("desired_state.routing[{idx}]")),
                        actual_ref: Some(format!("snapshot.routes.{}", route.prefix)),
                        blocking: false,
                        payload_ref: None,
                        notes: vec!["next_hop_hive changed — requires delete+add".to_string()],
                    });
                }
                // else: noop
            }
        }
    }

    // Emit ROUTE_DELETE for solution-owned routes absent from desired
    for (key, actual_route) in &snapshot_routes {
        if !desired_route_keys.contains(key) {
            let ownership = actual_route
                .get("ownership")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            if ownership == "solution" {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "route".to_string(),
                    resource_id: key.clone(),
                    change_type: ChangeType::Delete,
                    ownership: "solution".to_string(),
                    compiler_class: CompilerClass::RouteDelete,
                    desired_ref: None,
                    actual_ref: Some(format!("snapshot.routes.{key}")),
                    blocking: false,
                    payload_ref: None,
                    notes: vec!["solution-owned route absent from desired_state".to_string()],
                });
            }
        }
    }

    ops
}

// ── TB-8 — WF deployments ─────────────────────────────────────────────────────

pub fn reconcile_wf(
    desired_wf: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredWfDeployment> = desired_wf
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredWfDeployment>(item.clone()).ok())
        .collect();

    let mut snapshot_wf: HashMap<String, serde_json::Value> = HashMap::new();
    for (hive_id, hive_res) in &snapshot.resources {
        for item in snapshot_item_values(hive_res.wf_state.as_ref()) {
            let name = item
                .get("workflow_name")
                .or_else(|| item.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let key = format!("{hive_id}:{name}");
            snapshot_wf.insert(key, item);
        }
    }

    for (idx, wf) in desired.iter().enumerate() {
        let key = format!("{}:{}", wf.hive, wf.workflow_name);
        match snapshot_wf.get(&key) {
            None => {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "wf_deployment".to_string(),
                    resource_id: key,
                    change_type: ChangeType::Create,
                    ownership: wf.ownership.clone(),
                    compiler_class: CompilerClass::WfDeployApply,
                    desired_ref: Some(format!("desired_state.wf_deployments[{idx}]")),
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                });
            }
            Some(actual) => {
                let actual_hash = actual
                    .get("definition_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let desired_hash = wf.definition_hash();

                if actual_hash != desired_hash {
                    // WF.* nodes require restart on definition change (CFZ-12)
                    ops.push(DeltaOperation {
                        op_id: new_op_id(),
                        resource_type: "wf_deployment".to_string(),
                        resource_id: key,
                        change_type: ChangeType::Update,
                        ownership: wf.ownership.clone(),
                        compiler_class: CompilerClass::WfDeployRestart,
                        desired_ref: Some(format!("desired_state.wf_deployments[{idx}]")),
                        actual_ref: Some(format!("snapshot.wf.{}", wf.workflow_name)),
                        blocking: false,
                        payload_ref: None,
                        notes: vec!["workflow definition changed".to_string()],
                    });
                }
            }
        }
    }

    let desired_wf_keys: HashSet<String> = desired
        .iter()
        .map(|wf| format!("{}:{}", wf.hive, wf.workflow_name))
        .collect();
    for (key, actual) in &snapshot_wf {
        if desired_wf_keys.contains(key) {
            continue;
        }
        let ownership = actual
            .get("ownership")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        if ownership == "solution" {
            ops.push(DeltaOperation {
                op_id: new_op_id(),
                resource_type: "wf_deployment".to_string(),
                resource_id: key.clone(),
                change_type: ChangeType::Delete,
                ownership: "solution".to_string(),
                compiler_class: CompilerClass::WfRemove,
                desired_ref: None,
                actual_ref: Some(format!("snapshot.wf.{key}")),
                blocking: true,
                payload_ref: None,
                notes: vec!["solution-owned workflow absent from desired_state; canonical remove action still unavailable".to_string()],
            });
        }
    }

    ops
}

// ── TB-9 — OPA deployments ────────────────────────────────────────────────────

pub fn reconcile_opa(
    desired_opa: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredOpaDeployment> = desired_opa
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredOpaDeployment>(item.clone()).ok())
        .collect();

    let mut snapshot_opa: HashMap<String, serde_json::Value> = HashMap::new();
    for (hive_id, hive_res) in &snapshot.resources {
        for item in snapshot_item_values(hive_res.opa_state.as_ref()) {
            let policy_id = item
                .get("policy_id")
                .or_else(|| item.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if policy_id.is_empty() {
                continue;
            }
            let key = format!("{hive_id}:{policy_id}");
            snapshot_opa.insert(key, item);
        }
    }

    for (idx, opa) in desired.iter().enumerate() {
        let key = format!("{}:{}", opa.hive, opa.policy_id);
        match snapshot_opa.get(&key) {
            None => {
                ops.push(DeltaOperation {
                    op_id: new_op_id(),
                    resource_type: "opa_deployment".to_string(),
                    resource_id: key,
                    change_type: ChangeType::Create,
                    ownership: opa.ownership.clone(),
                    compiler_class: CompilerClass::OpaApply,
                    desired_ref: Some(format!("desired_state.opa_deployments[{idx}]")),
                    actual_ref: None,
                    blocking: false,
                    payload_ref: None,
                    notes: vec![],
                });
            }
            Some(actual) => {
                let actual_hash = actual
                    .get("rego_hash")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if actual_hash != opa.rego_hash() {
                    ops.push(DeltaOperation {
                        op_id: new_op_id(),
                        resource_type: "opa_deployment".to_string(),
                        resource_id: key,
                        change_type: ChangeType::Update,
                        ownership: opa.ownership.clone(),
                        compiler_class: CompilerClass::OpaApply,
                        desired_ref: Some(format!("desired_state.opa_deployments[{idx}]")),
                        actual_ref: Some(format!("snapshot.opa.{}", opa.policy_id)),
                        blocking: false,
                        payload_ref: None,
                        notes: vec!["rego source changed".to_string()],
                    });
                }
            }
        }
    }

    let desired_opa_keys: HashSet<String> = desired
        .iter()
        .map(|opa| format!("{}:{}", opa.hive, opa.policy_id))
        .collect();
    for (key, actual) in &snapshot_opa {
        if desired_opa_keys.contains(key) {
            continue;
        }
        let ownership = actual
            .get("ownership")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        if ownership == "solution" {
            ops.push(DeltaOperation {
                op_id: new_op_id(),
                resource_type: "opa_deployment".to_string(),
                resource_id: key.clone(),
                change_type: ChangeType::Delete,
                ownership: "solution".to_string(),
                compiler_class: CompilerClass::OpaRemove,
                desired_ref: None,
                actual_ref: Some(format!("snapshot.opa.{key}")),
                blocking: true,
                payload_ref: None,
                notes: vec!["solution-owned OPA deployment absent from desired_state; canonical remove action still unavailable".to_string()],
            });
        }
    }

    ops
}

// ── TB-10 — Artifact gap detection ───────────────────────────────────────────
// Already embedded in reconcile_runtimes — this entry point collects all packets
// produced by all reconcilers into one list.

// ── TB-11 — Reconciler entry point ───────────────────────────────────────────

pub fn run_reconciler(
    manifest: &SolutionManifestV2,
    snapshot: &ActualStateSnapshot,
    manifest_ref: &str,
) -> Result<ReconcilerOutput, String> {
    validate_manifest_sections(manifest)?;

    if snapshot.completeness.blocking {
        return Ok(ReconcilerOutput {
            delta_report: DeltaReport {
                delta_report_version: "0.1".to_string(),
                status: DeltaReportStatus::BlockedPartialSnapshot,
                manifest_ref: manifest_ref.to_string(),
                snapshot_ref: snapshot.snapshot_id.clone(),
                summary: DeltaSummary::default(),
                operations: vec![],
            },
            build_task_packets: vec![],
            diagnostics: snapshot
                .completeness
                .missing_sections
                .iter()
                .map(|s| format!("snapshot missing: {s}"))
                .collect(),
        });
    }

    let ds = &manifest.desired_state;
    let mut all_ops: Vec<DeltaOperation> = Vec::new();
    let mut all_packets: Vec<BuildTaskPacket> = Vec::new();
    let mut all_diagnostics: Vec<String> = Vec::new();

    // Topology
    if let Some(topology) = &ds.topology {
        let (ops, diag) = reconcile_topology(topology, snapshot);
        all_ops.extend(ops);
        all_diagnostics.extend(diag);
    }

    // Runtimes
    let multi_hive = ds
        .topology
        .as_ref()
        .and_then(|t| t.get("hives"))
        .and_then(|h| h.as_array())
        .map(|a| a.len() > 1)
        .unwrap_or(false);

    if let Some(runtimes) = &ds.runtimes {
        let (ops, packets) = reconcile_runtimes(runtimes, snapshot, multi_hive);
        all_ops.extend(ops);
        all_packets.extend(packets);
    }

    // Nodes
    if let Some(nodes) = &ds.nodes {
        let ops = reconcile_nodes(nodes, snapshot);
        all_ops.extend(ops);
    }

    // Routes
    if let Some(routing) = &ds.routing {
        let ops = reconcile_routes(routing, snapshot);
        all_ops.extend(ops);
    }

    // WF
    if let Some(wf) = &ds.wf_deployments {
        let ops = reconcile_wf(wf, snapshot);
        all_ops.extend(ops);
    }

    // OPA
    if let Some(opa) = &ds.opa_deployments {
        let ops = reconcile_opa(opa, snapshot);
        all_ops.extend(ops);
    }

    // Summary
    let mut summary = DeltaSummary::default();
    for op in &all_ops {
        match op.change_type {
            ChangeType::Create => summary.creates += 1,
            ChangeType::Update => summary.updates += 1,
            ChangeType::Delete => summary.deletes += 1,
            ChangeType::Noop => summary.noops += 1,
            ChangeType::Blocked => summary.blocked += 1,
        }
    }

    let has_blocking = all_ops.iter().any(|op| op.blocking);
    let status = if has_blocking {
        DeltaReportStatus::BlockedUnsupported
    } else {
        DeltaReportStatus::Ready
    };

    Ok(ReconcilerOutput {
        delta_report: DeltaReport {
            delta_report_version: "0.1".to_string(),
            status,
            manifest_ref: manifest_ref.to_string(),
            snapshot_ref: snapshot.snapshot_id.clone(),
            summary,
            operations: all_ops,
        },
        build_task_packets: all_packets,
        diagnostics: all_diagnostics,
    })
}

// ── TB-12 — Unit tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::pipeline_types::*;
    use super::*;
    use serde_json::json;

    fn empty_snapshot(hives: &[&str]) -> ActualStateSnapshot {
        let mut resources = HashMap::new();
        let mut hive_status = HashMap::new();
        for h in hives {
            resources.insert(h.to_string(), HiveResources::default());
            hive_status.insert(
                h.to_string(),
                HiveSnapshotStatus {
                    reachable: true,
                    error: None,
                },
            );
        }
        ActualStateSnapshot {
            snapshot_version: "0.1".to_string(),
            snapshot_id: "snap-test-001".to_string(),
            captured_at_start: "2026-01-01T00:00:00Z".to_string(),
            captured_at_end: "2026-01-01T00:00:01Z".to_string(),
            scope: SnapshotScope {
                hives: hives.iter().map(|s| s.to_string()).collect(),
                resources: vec![],
            },
            atomicity: SnapshotAtomicity {
                mode: "best_effort_multi_call".to_string(),
                is_atomic: false,
            },
            hive_status,
            resources,
            completeness: SnapshotCompleteness {
                is_partial: false,
                missing_sections: vec![],
                blocking: false,
            },
        }
    }

    fn manifest_with(desired_state: serde_json::Value) -> SolutionManifestV2 {
        let ds: DesiredStateV2 = serde_json::from_value(desired_state).unwrap_or_default();
        SolutionManifestV2 {
            manifest_version: "1.0".to_string(),
            solution: json!({ "name": "test-solution" }),
            desired_state: ds,
            advisory: json!({}),
        }
    }

    // ── Topology ──

    #[test]
    fn new_hive_in_desired_emits_hive_create() {
        let topology = json!({ "hives": [{ "hive_id": "worker-new", "role": "worker" }] });
        let snapshot = empty_snapshot(&["motherbee"]);
        let (ops, _) = reconcile_topology(&topology, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::HiveCreate);
        assert_eq!(ops[0].resource_id, "worker-new");
    }

    #[test]
    fn existing_hive_emits_no_ops() {
        let topology = json!({ "hives": [{ "hive_id": "motherbee", "role": "primary" }] });
        let snapshot = empty_snapshot(&["motherbee"]);
        let (ops, _) = reconcile_topology(&topology, &snapshot);
        assert!(ops.is_empty(), "existing hive should produce no ops");
    }

    // ── Runtimes ──

    #[test]
    fn missing_runtime_emits_publish_and_distribute() {
        let desired = vec![json!({
            "name": "ai.support.demo",
            "version": "0.1.0",
            "package_source": "inline_package"
        })];
        let snapshot = empty_snapshot(&["motherbee", "worker-220"]);
        let (ops, packets) = reconcile_runtimes(&desired, &snapshot, true);
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0].compiler_class,
            CompilerClass::RuntimePublishAndDistribute
        );
        assert_eq!(ops[0].change_type, ChangeType::Create);
        assert_eq!(
            packets.len(),
            1,
            "missing inline_package runtime needs a build packet"
        );
        assert_eq!(packets[0].artifact_kind, "runtime_package");
    }

    #[test]
    fn existing_runtime_same_version_emits_noop() {
        let desired = vec![json!({
            "name": "AI.common",
            "version": "1.0.0",
            "package_source": "pre_published"
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().runtimes = Some(json!([
            { "name": "AI.common", "version": "1.0.0" }
        ]));
        let (ops, packets) = reconcile_runtimes(&desired, &snapshot, false);
        assert!(ops.is_empty(), "same version should be noop");
        assert!(packets.is_empty());
    }

    #[test]
    fn runtime_version_drift_emits_publish_update() {
        let desired = vec![json!({
            "name": "AI.common",
            "version": "1.1.0",
            "package_source": "pre_published"
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().runtimes = Some(json!([
            { "name": "AI.common", "version": "1.0.0" }
        ]));
        let (ops, _) = reconcile_runtimes(&desired, &snapshot, false);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].change_type, ChangeType::Update);
        assert_eq!(
            ops[0].compiler_class,
            CompilerClass::RuntimePublishAndDistribute
        );
    }

    // ── Nodes ──

    #[test]
    fn missing_node_emits_run_node() {
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "AI.common"
        })];
        let snapshot = empty_snapshot(&["motherbee"]);
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::NodeRunMissing);
        assert_eq!(ops[0].resource_id, "AI.coa@motherbee");
    }

    #[test]
    fn ai_node_config_drift_emits_hot_apply() {
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "AI.common",
            "config": { "tenant_id": "tnt:acme-v2" }
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().nodes = Some(json!([{
            "node_name": "AI.coa@motherbee",
            "runtime": "AI.common",
            "config": { "tenant_id": "tnt:acme-v1" }
        }]));
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0].compiler_class,
            CompilerClass::NodeConfigApplyHot,
            "AI.* node config change should be HOT"
        );
    }

    #[test]
    fn wf_node_config_drift_emits_restart() {
        let desired = vec![json!({
            "node_name": "WF.router@motherbee",
            "hive": "motherbee",
            "runtime": "wf.engine",
            "config": { "rules_version": "v2" }
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().nodes = Some(json!([{
            "node_name": "WF.router@motherbee",
            "runtime": "wf.engine",
            "config": { "rules_version": "v1" }
        }]));
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0].compiler_class,
            CompilerClass::NodeConfigApplyRestart,
            "WF.* node config change should require restart"
        );
    }

    #[test]
    fn node_runtime_change_without_recreate_policy_blocks() {
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "AI.coa.v2",
            "config": {}
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().nodes = Some(json!([{
            "node_name": "AI.coa@motherbee",
            "runtime": "AI.coa.v1",
            "config": {}
        }]));
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(
            ops[0].compiler_class,
            CompilerClass::BlockedImmutableChange,
            "runtime change without recreate policy should block"
        );
    }

    #[test]
    fn unknown_node_prefix_config_change_blocks_instead_of_guessing_restart_policy() {
        let desired = vec![json!({
            "node_name": "ZZ.experimental@motherbee",
            "hive": "motherbee",
            "runtime": "zz.experimental",
            "config": { "mode": "v2" }
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().nodes = Some(json!([{
            "node_name": "ZZ.experimental@motherbee",
            "runtime": "zz.experimental",
            "config": { "mode": "v1" }
        }]));
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::BlockedImmutableChange);
        assert!(ops[0].blocking);
    }

    // ── Routes ──

    #[test]
    fn missing_route_emits_route_add() {
        let desired = vec![json!({
            "hive": "motherbee",
            "prefix": "tenant.acme.",
            "action": "next_hop_hive",
            "next_hop_hive": "worker-220"
        })];
        let snapshot = empty_snapshot(&["motherbee"]);
        let ops = reconcile_routes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::RouteAdd);
    }

    #[test]
    fn route_field_change_emits_route_replace() {
        let desired = vec![json!({
            "hive": "motherbee",
            "prefix": "tenant.acme.",
            "action": "next_hop_hive",
            "next_hop_hive": "worker-221"
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().routes = Some(json!([{
            "prefix": "tenant.acme.",
            "action": "next_hop_hive",
            "next_hop_hive": "worker-220"
        }]));
        let ops = reconcile_routes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::RouteReplace);
    }

    // ── WF ──

    #[test]
    fn missing_wf_deployment_emits_wf_deploy_apply() {
        let desired = vec![json!({
            "hive": "motherbee",
            "workflow_name": "support-router",
            "definition": { "steps": [{ "id": "s1" }] }
        })];
        let snapshot = empty_snapshot(&["motherbee"]);
        let ops = reconcile_wf(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::WfDeployApply);
    }

    #[test]
    fn wf_definition_change_emits_wf_deploy_restart() {
        let desired = vec![json!({
            "hive": "motherbee",
            "workflow_name": "support-router",
            "definition": { "steps": [{ "id": "s1" }, { "id": "s2" }] }
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().wf_state = Some(json!([{
            "workflow_name": "support-router",
            "definition_hash": "oldhashabcdef01"
        }]));
        let ops = reconcile_wf(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::WfDeployRestart);
    }

    #[test]
    fn missing_desired_solution_owned_wf_emits_wf_remove_blocked() {
        let desired: Vec<serde_json::Value> = vec![];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().wf_state = Some(json!([{
            "workflow_name": "support-router",
            "definition_hash": "oldhashabcdef01",
            "ownership": "solution"
        }]));
        let ops = reconcile_wf(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::WfRemove);
        assert!(ops[0].blocking);
    }

    // ── OPA ──

    #[test]
    fn missing_opa_deployment_emits_opa_apply() {
        let desired = vec![json!({
            "hive": "motherbee",
            "policy_id": "solution-policy",
            "rego_source": "package fluxbee\nallow = true"
        })];
        let snapshot = empty_snapshot(&["motherbee"]);
        let ops = reconcile_opa(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::OpaApply);
    }

    #[test]
    fn missing_desired_solution_owned_opa_emits_opa_remove_blocked() {
        let desired: Vec<serde_json::Value> = vec![];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().opa_state = Some(json!([
            {
                "policy_id": "solution-policy",
                "rego_hash": "deadbeef",
                "ownership": "solution"
            }
        ]));
        let ops = reconcile_opa(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::OpaRemove);
        assert!(ops[0].blocking);
    }

    #[test]
    fn missing_desired_solution_owned_vpn_emits_vpn_delete() {
        let topology = json!({ "vpns": [] });
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().vpns = Some(json!([
            { "vpn_id": "worker-*", "ownership": "solution" }
        ]));
        let (ops, _) = reconcile_topology(&topology, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::VpnDelete);
        assert_eq!(ops[0].change_type, ChangeType::Delete);
    }

    // ── Ownership ──

    #[test]
    fn external_owned_runtime_not_deleted() {
        let desired: Vec<serde_json::Value> = vec![];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        // runtime present in snapshot with external ownership should not be touched
        snapshot.resources.get_mut("motherbee").unwrap().runtimes = Some(json!([
            { "name": "AI.chat", "version": "1.0.0", "ownership": "system" }
        ]));
        let (ops, _) = reconcile_runtimes(&desired, &snapshot, false);
        assert!(
            ops.is_empty(),
            "system/external runtimes must not be deleted"
        );
    }

    // ── Idempotency ──

    #[test]
    fn reconciler_is_idempotent() {
        let manifest = manifest_with(json!({
            "nodes": [{ "node_name": "AI.coa@motherbee", "hive": "motherbee", "runtime": "AI.common" }]
        }));
        let snapshot = empty_snapshot(&["motherbee"]);
        let out1 = run_reconciler(&manifest, &snapshot, "manifest://test/1.0").unwrap();
        let out2 = run_reconciler(&manifest, &snapshot, "manifest://test/1.0").unwrap();
        assert_eq!(
            out1.delta_report.summary.creates, out2.delta_report.summary.creates,
            "reconciler must be idempotent"
        );
    }

    // ── Partial snapshot ──

    #[test]
    fn blocking_partial_snapshot_returns_blocked_report() {
        let manifest = manifest_with(json!({
            "nodes": [{ "node_name": "AI.coa@worker-220", "hive": "worker-220", "runtime": "AI.common" }]
        }));
        let mut snapshot = empty_snapshot(&["motherbee", "worker-220"]);
        snapshot.completeness.is_partial = true;
        snapshot.completeness.blocking = true;
        snapshot.completeness.missing_sections = vec!["worker-220.nodes".to_string()];

        let out = run_reconciler(&manifest, &snapshot, "manifest://test/1.0").unwrap();
        assert_eq!(
            out.delta_report.status,
            DeltaReportStatus::BlockedPartialSnapshot,
            "blocking partial snapshot must produce blocked report"
        );
        assert!(
            out.delta_report.operations.is_empty(),
            "no ops should be emitted when snapshot is blocked"
        );
    }

    // ── Manifest validation ──

    #[test]
    fn manifest_with_unsupported_section_is_rejected() {
        let mut manifest = manifest_with(json!({}));
        let mut ds_value = serde_json::to_value(&manifest.desired_state).unwrap();
        ds_value["policy"] = json!({ "rules": [] });
        manifest.desired_state = serde_json::from_value(ds_value).expect("desired_state parses");

        let result = validate_manifest_sections(&manifest);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("UNSUPPORTED_DESIRED_STATE_SECTION"));
    }
}
