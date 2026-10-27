// Deterministic manifest reconciler — Track B.
// Source spec: fluxbee_rearchitecture_master_spec_v4.md — sections 6, 7, 8
//
// This module contains only PURE deterministic functions.
// No AI calls, no network calls. The snapshot is built by build_actual_state_snapshot()
// in sy_architect.rs and passed in. Same inputs → same outputs always.

use std::collections::{BTreeMap, HashMap, HashSet};

use fluxbee_sdk::protocol::PRIMARY_HIVE_ID;
use uuid::Uuid;

use super::pipeline_types::{
    desired_state_unknown_sections, is_valid_ownership_label, restart_policy_for_node,
    ActualStateSnapshot, BuildTaskKnownContext, BuildTaskPacket, ChangeType, CompilerClass,
    DeltaOperation, DeltaReport, DeltaReportStatus, DeltaSummary, DesiredHive, DesiredNode,
    DesiredOpaPolicy, DesiredRoute, DesiredRuntime, DesiredVpn, DesiredWfDeployment, HiveResources,
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

// ── Snapshot answers ──────────────────────────────────────────────────────────
// Each answer is the producer's payload, which SY.admin hands over as is; each is read exactly in
// the shape its producer sends.

/// The list at `pointer` in an answer; none when the answer has no such list.
fn listed<'a>(answer: Option<&'a serde_json::Value>, pointer: &str) -> &'a [serde_json::Value] {
    answer
        .and_then(|answer| answer.pointer(pointer))
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

/// `list_runtimes`, SY.orchestrator (`local_runtimes_snapshot`): `{status, hive_id,
/// manifest_version, manifest_hash, runtimes: [{runtime, current, available, type, runtime_base,
/// readiness, materialization_summary}]}`.
fn listed_runtimes(answer: Option<&serde_json::Value>) -> &[serde_json::Value] {
    listed(answer, "/runtimes")
}

/// `list_nodes` with `managed: true`, SY.orchestrator (`managed_node_inventory`): `{status, target,
/// hive, inventory_source, node_count, nodes: [{node_name, managed, runtime: {name, version},
/// config: {path, exists, valid, config_version}, ...}]}`.
pub fn listed_nodes(answer: Option<&serde_json::Value>) -> &[serde_json::Value] {
    listed(answer, "/nodes")
}

/// `list_routes`, SY.config.routes (`admin_success_payload`): `{status, action, payload:
/// {config_version, routes: [{prefix, match_kind, action, next_hop_hive, metric, priority}]}}`.
fn listed_routes(answer: Option<&serde_json::Value>) -> &[serde_json::Value] {
    listed(answer, "/payload/routes")
}

/// `list_vpns`, SY.config.routes: `{status, action, payload: {config_version, vpns: [{pattern,
/// match_kind, vpn_id, priority}]}}`; `vpn_id` is a number.
fn listed_vpns(answer: Option<&serde_json::Value>) -> &[serde_json::Value] {
    listed(answer, "/payload/vpns")
}

/// Whether a `list_nodes` entry is a node that exists: a managed instance while its config.json
/// does (run_node refuses its name then, NODE_ALREADY_EXISTS), running or not; a core service
/// always.
pub fn node_exists(item: &serde_json::Value) -> bool {
    match item.get("managed").and_then(serde_json::Value::as_bool) {
        Some(true) => item
            .pointer("/config/exists")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        Some(false) => true,
        None => false,
    }
}

/// Whether a node's stored config holds what the manifest declares for it: every declared key, at
/// every level, with the declared value. `stored` is `get_node_config`'s answer (`{status, target,
/// hive, node_name, path, config_version, config}`), the config.json the node runs, which the node
/// fills in (ai.generic saves `behavior` with `model_settings`, `base_url` and `multimodal` the
/// manifest may not declare) beside the runtime's defaults, `tenant_id` and the orchestrator's
/// `_system`: a key only the stored config has is not compared. A config declares nothing when it
/// is not a non-empty object.
pub fn stored_config_holds(
    declared: &serde_json::Value,
    stored: Option<&serde_json::Value>,
) -> bool {
    if declared.as_object().is_none_or(serde_json::Map::is_empty) {
        return true;
    }
    stored
        .and_then(|answer| answer.get("config"))
        .is_some_and(|config| declared_value_holds(declared, config))
}

/// A declared object holds when each of its keys holds in the stored object; any other declared
/// value must equal the stored one.
fn declared_value_holds(declared: &serde_json::Value, stored: &serde_json::Value) -> bool {
    match (declared, stored) {
        (serde_json::Value::Object(declared), serde_json::Value::Object(stored)) => {
            declared.iter().all(|(key, value)| {
                stored
                    .get(key)
                    .is_some_and(|stored| declared_value_holds(value, stored))
            })
        }
        _ => declared == stored,
    }
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

    let desired_vpn_keys: HashSet<String> = vpns.iter().map(DesiredVpn::identity_key).collect();

    for (idx, vpn) in vpns.iter().enumerate() {
        // SY.config.routes keeps one rule per pattern on a hive.
        let rule = snapshot.resources.get(&vpn.hive).and_then(|hive_res| {
            listed_vpns(hive_res.vpns.as_ref())
                .iter()
                .find(|rule| rule.get("pattern").and_then(|v| v.as_str()) == Some(&vpn.pattern))
        });
        let (change_type, compiler_class, notes) = match rule {
            None => (ChangeType::Create, CompilerClass::VpnCreate, vec![]),
            Some(rule) if !vpn.holds(rule) => (
                ChangeType::Update,
                CompilerClass::VpnReplace,
                vec!["vpn_id, match_kind or priority changed — requires delete+add".to_string()],
            ),
            Some(_) => continue,
        };
        let key = vpn.identity_key();
        ops.push(DeltaOperation {
            op_id: new_op_id(),
            resource_type: "vpn".to_string(),
            actual_ref: rule.map(|_| format!("snapshot.vpns.{key}")),
            resource_id: key,
            change_type,
            ownership: if vpn.ownership.is_empty() {
                "solution".to_string()
            } else {
                vpn.ownership.clone()
            },
            compiler_class,
            desired_ref: Some(format!("desired_state.topology.vpns[{idx}]")),
            blocking: false,
            payload_ref: None,
            notes,
        });
    }

    // A rule the manifest does not declare is reported, never removed: SY.config.routes records no
    // owner for it (it is removed through SY.admin).
    for (hive_id, hive_res) in &snapshot.resources {
        for rule in listed_vpns(hive_res.vpns.as_ref()) {
            let Some(pattern) = rule.get("pattern").and_then(|v| v.as_str()) else {
                continue;
            };
            if !desired_vpn_keys.contains(&format!("{hive_id}:{pattern}")) {
                diagnostics.push(format!(
                    "VPN rule '{pattern}' on hive '{hive_id}' is not in desired_state; the reconcile does not remove VPN rules"
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

    // Every runtime the snapshot's hives hold, with its current version. Runtimes are published on
    // the primary hive and synced to the others: where hives differ, the primary's version counts.
    let mut snapshot_runtimes: HashMap<String, String> = HashMap::new(); // name -> current
    let mut hives: Vec<&String> = snapshot.resources.keys().collect();
    hives.sort_by_key(|hive| (hive.as_str() != PRIMARY_HIVE_ID, hive.as_str()));
    for hive in hives {
        for item in listed_runtimes(snapshot.resources[hive].runtimes.as_ref()) {
            let Some(name) = item.get("runtime").and_then(|v| v.as_str()) else {
                continue;
            };
            let current = item.get("current").and_then(|v| v.as_str()).unwrap_or("");
            snapshot_runtimes
                .entry(name.to_string())
                .or_insert_with(|| current.to_string());
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
// A node the manifest stops declaring is not removed: SY.orchestrator records no owner for it (it
// is removed through SY.admin).

pub fn reconcile_nodes(
    desired_nodes: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredNode> = desired_nodes
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredNode>(item.clone()).ok())
        .collect();

    // node_name -> (its hive's answers, its list_nodes entry), for every node that exists
    let mut snapshot_nodes: HashMap<String, (&HiveResources, &serde_json::Value)> = HashMap::new();
    for hive_res in snapshot.resources.values() {
        for item in listed_nodes(hive_res.nodes.as_ref()) {
            let Some(name) = item.get("node_name").and_then(|v| v.as_str()) else {
                continue;
            };
            if node_exists(item) {
                snapshot_nodes.insert(name.to_string(), (hive_res, item));
            }
        }
    }

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
            Some((hive_res, actual_node)) => {
                let actual_runtime = actual_node
                    .pointer("/runtime/name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let runtime_changed = !node.runtime.is_empty() && actual_runtime != node.runtime;

                // Check config drift: a managed instance runs its config.json; a core service's
                // config is not the node's to declare.
                let managed = actual_node.get("managed").and_then(|v| v.as_bool()) == Some(true);
                let config_changed = managed
                    && !stored_config_holds(
                        &node.config,
                        hive_res.node_configs.get(&node.node_name),
                    );

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

    ops
}

// ── TB-7 — Routes ─────────────────────────────────────────────────────────────
// A route the manifest stops declaring is not removed: SY.config.routes records no owner for it
// (it is removed through SY.admin).

pub fn reconcile_routes(
    desired_routes: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredRoute> = desired_routes
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredRoute>(item.clone()).ok())
        .collect();

    // Build snapshot route map: identity_key (hive:prefix) -> route
    let mut snapshot_routes: HashMap<String, &serde_json::Value> = HashMap::new();
    for (hive_id, hive_res) in &snapshot.resources {
        for item in listed_routes(hive_res.routes.as_ref()) {
            let Some(prefix) = item.get("prefix").and_then(|v| v.as_str()) else {
                continue;
            };
            snapshot_routes.insert(format!("{hive_id}:{prefix}"), item);
        }
    }

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
                // The same prefix with another action or next hop is replaced.
                let actual_action = actual.get("action").and_then(|v| v.as_str());
                let actual_hop = actual.get("next_hop_hive").and_then(|v| v.as_str());
                let desired_hop = route.next_hop_hive.as_deref();
                if actual_action != Some(route.action.as_str()) || desired_hop != actual_hop {
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
                        notes: vec![
                            "action or next_hop_hive changed — requires delete+add".to_string()
                        ],
                    });
                }
                // else: noop
            }
        }
    }

    ops
}

// ── TB-8 — WF deployments ─────────────────────────────────────────────────────
// Each hive's SY.wf-rules lists its workflows (`list_workflows`); a workflow runs the definition its
// `current_hash` names, which `DesiredWfDeployment::definition_hash` computes the same way. wf-rules
// records no owner: a running workflow is this solution's when its saved manifests declared exactly
// that definition last (`HiveResources::wf_declared_by`), and only then is it removed once the
// solution stops declaring it, as the user policy is (TB-9).

/// The deployed workflows in one hive's `list_workflows` answer (`{status, hive, workflows: [...]}`),
/// as (workflow_name, current_hash). A workflow with only a staged definition is not deployed.
pub fn deployed_workflows(wf_state: Option<&serde_json::Value>) -> Vec<(String, String)> {
    listed(wf_state, "/workflows")
        .iter()
        .filter_map(|item| {
            let name = item.get("workflow_name")?.as_str()?.trim();
            let hash = item.get("current_hash")?.as_str()?.trim();
            (!name.is_empty() && !hash.is_empty()).then(|| (name.to_string(), hash.to_string()))
        })
        .collect()
}

pub fn reconcile_wf(
    desired_wf: &[serde_json::Value],
    snapshot: &ActualStateSnapshot,
    solution_id: &str,
) -> Vec<DeltaOperation> {
    let mut ops = Vec::new();

    let desired: Vec<DesiredWfDeployment> = desired_wf
        .iter()
        .filter_map(|item| serde_json::from_value::<DesiredWfDeployment>(item.clone()).ok())
        .collect();

    // "<hive>:<workflow_name>" -> (current_hash, the solution that declared it)
    let mut running: BTreeMap<String, (String, Option<&str>)> = BTreeMap::new();
    for (hive_id, hive_res) in &snapshot.resources {
        for (name, hash) in deployed_workflows(hive_res.wf_state.as_ref()) {
            let declared_by = hive_res.wf_declared_by.get(&name).map(String::as_str);
            running.insert(format!("{hive_id}:{name}"), (hash, declared_by));
        }
    }

    for (idx, wf) in desired.iter().enumerate() {
        let key = format!("{}:{}", wf.hive, wf.workflow_name);
        match running.get(&key) {
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
            Some((current_hash, _)) => {
                if *current_hash != wf.definition_hash() {
                    // WF.* nodes require restart on definition change (CFZ-12)
                    ops.push(DeltaOperation {
                        op_id: new_op_id(),
                        resource_type: "wf_deployment".to_string(),
                        resource_id: key.clone(),
                        change_type: ChangeType::Update,
                        ownership: wf.ownership.clone(),
                        compiler_class: CompilerClass::WfDeployRestart,
                        desired_ref: Some(format!("desired_state.wf_deployments[{idx}]")),
                        actual_ref: Some(format!("snapshot.wf.{key}")),
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
    for (key, (_, declared_by)) in &running {
        if desired_wf_keys.contains(key) || *declared_by != Some(solution_id) {
            continue;
        }
        ops.push(DeltaOperation {
            op_id: new_op_id(),
            resource_type: "wf_deployment".to_string(),
            resource_id: key.clone(),
            change_type: ChangeType::Delete,
            ownership: "solution".to_string(),
            compiler_class: CompilerClass::WfRemove,
            desired_ref: None,
            actual_ref: Some(format!("snapshot.wf.{key}")),
            blocking: false,
            payload_ref: None,
            notes: vec![
                "this solution deployed this workflow and no longer declares it".to_string(),
            ],
        });
    }

    ops
}

// ── TB-9 — the user OPA policy ───────────────────────────────────────────────
// One user policy for every hive: the snapshot reads it once from the motherbee and the plan writes
// it with one global opa_compile_apply / opa_clear. When several solutions declare one, the last
// one applied wins (operator decision 2026-10-01, FINDINGS A-23): a solution re-applies its policy
// when the running one differs, and clears it only while the running policy is still its own.

pub const OPA_POLICY_RESOURCE_TYPE: &str = "opa_policy";
pub const OPA_POLICY_RESOURCE_ID: &str = "global";

pub fn reconcile_opa(
    desired: Option<&serde_json::Value>,
    snapshot: &ActualStateSnapshot,
    solution_id: &str,
) -> Vec<DeltaOperation> {
    // Not in scope for this run: nothing was read, so nothing can be decided.
    let Some(actual) = snapshot.opa.as_ref() else {
        return Vec::new();
    };
    let running = !actual.hash.is_empty();
    let desired =
        desired.and_then(|value| serde_json::from_value::<DesiredOpaPolicy>(value.clone()).ok());

    let Some(policy) = desired else {
        if running && actual.declared_by.as_deref() == Some(solution_id) {
            return vec![DeltaOperation {
                op_id: new_op_id(),
                resource_type: OPA_POLICY_RESOURCE_TYPE.to_string(),
                resource_id: OPA_POLICY_RESOURCE_ID.to_string(),
                change_type: ChangeType::Delete,
                ownership: "solution".to_string(),
                compiler_class: CompilerClass::OpaRemove,
                desired_ref: None,
                actual_ref: Some("snapshot.opa".to_string()),
                blocking: false,
                payload_ref: None,
                notes: vec![
                    "this solution no longer declares the user policy it applied; it is cleared on every hive"
                        .to_string(),
                ],
            }];
        }
        return Vec::new();
    };

    let same_entrypoint = actual.entrypoint.is_empty() || actual.entrypoint == policy.entrypoint;
    if running && actual.rego == policy.rego_source && same_entrypoint {
        return Vec::new();
    }
    let (change_type, note) = if !running {
        (ChangeType::Create, "no user policy is running".to_string())
    } else {
        let note = match actual.declared_by.as_deref() {
            Some(owner) if owner == solution_id => {
                "updates the user policy this solution declared".to_string()
            }
            Some(owner) => format!(
                "replaces the user policy declared by solution '{owner}' on every hive (the last one applied wins)"
            ),
            None => "replaces a user policy that no solution declared (written by hand) on every hive"
                .to_string(),
        };
        (ChangeType::Update, note)
    };
    vec![DeltaOperation {
        op_id: new_op_id(),
        resource_type: OPA_POLICY_RESOURCE_TYPE.to_string(),
        resource_id: OPA_POLICY_RESOURCE_ID.to_string(),
        change_type,
        ownership: policy.ownership,
        compiler_class: CompilerClass::OpaApply,
        desired_ref: Some("desired_state.opa".to_string()),
        actual_ref: running.then(|| "snapshot.opa".to_string()),
        blocking: false,
        payload_ref: None,
        notes: vec![note],
    }]
}

// ── TB-10 — Artifact gap detection ───────────────────────────────────────────
// Already embedded in reconcile_runtimes — this entry point collects all packets
// produced by all reconcilers into one list.

// ── TB-11 — Reconciler entry point ───────────────────────────────────────────

pub fn run_reconciler(
    manifest: &SolutionManifestV2,
    snapshot: &ActualStateSnapshot,
    manifest_ref: &str,
    solution_id: &str,
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

    // WF: also when the manifest declares none, so a workflow this solution deployed is removed.
    all_ops.extend(reconcile_wf(
        ds.wf_deployments.as_deref().unwrap_or_default(),
        snapshot,
        solution_id,
    ));

    // OPA
    all_ops.extend(reconcile_opa(ds.opa.as_ref(), snapshot, solution_id));

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
            opa: None,
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

    // ── Snapshot answers, as their producers send them ──

    const TENANT: &str = "tnt:43d576a3-d712-4d91-9245-5d5463dd693e";

    /// SY.orchestrator's `list_runtimes` answer (`local_runtimes_snapshot`, `runtime_api_entry`).
    fn runtimes_answer(hive: &str, runtimes: &[(&str, &str)]) -> serde_json::Value {
        let entries: Vec<serde_json::Value> = runtimes
            .iter()
            .map(|(runtime, current)| {
                let mut readiness = serde_json::Map::new();
                readiness.insert(
                    current.to_string(),
                    json!({
                        "runtime_present": true,
                        "start_sh_executable": true,
                        "base_runtime_ready": true
                    }),
                );
                json!({
                    "runtime": runtime,
                    "current": current,
                    "available": [current],
                    "type": "full_runtime",
                    "runtime_base": null,
                    "readiness": readiness,
                    "materialization_summary": {
                        "known_versions": 1,
                        "materialized_versions": 1,
                        "current_version": current,
                        "current_materialized": true,
                        "current_blocking_reason": null
                    }
                })
            })
            .collect();
        json!({
            "status": "ok",
            "hive_id": hive,
            "manifest_version": 12,
            "manifest_hash": "6c1f0e2a9b8d7c3e5f4a1b2c3d4e5f60718293a4b5c6d7e8f90123456789abcd",
            "runtimes": entries
        })
    }

    /// One entry of SY.orchestrator's `list_nodes` answer for the managed instances
    /// (`managed_node_inventory_with_root`).
    fn managed_node(node_name: &str, runtime: &str, config_exists: bool) -> serde_json::Value {
        let kind = node_name.split('.').next().unwrap_or_default();
        let hive = node_name
            .rsplit_once('@')
            .map(|(_, hive)| hive)
            .unwrap_or_default();
        let dir = format!("/var/lib/fluxbee/nodes/{kind}/{node_name}");
        json!({
            "name": node_name,
            "node_name": node_name,
            "hive": hive,
            "kind": kind,
            "managed": true,
            "inventory_source": "managed_instance",
            "status": "running",
            "lifecycle_state": "RUNNING",
            "health_state": "HEALTHY",
            "visible_in_router": true,
            "runtime": { "name": runtime, "version": "0.1.79" },
            "config": {
                "path": format!("{dir}/config.json"),
                "exists": config_exists,
                "valid": config_exists,
                "config_version": if config_exists { json!(1) } else { json!(null) }
            },
            "state": { "path": format!("{dir}/state.json"), "exists": false },
            "identity": { "ilk_id": "ilk:7c1e0a52-1f0b-4a5e-9d64-0b6a1f2d3c4e", "tenant_id": TENANT },
            "launched_by": "archi",
            "relaunch_on_boot": true
        })
    }

    /// SY.orchestrator's `list_nodes` answer for the managed instances (`list_nodes_flow`).
    fn nodes_answer(nodes: Vec<serde_json::Value>) -> serde_json::Value {
        json!({
            "status": "ok",
            "target": "motherbee",
            "hive": "motherbee",
            "inventory_source": "managed_instances",
            "node_count": nodes.len(),
            "nodes": nodes
        })
    }

    /// SY.orchestrator's `get_node_config` answer (`get_node_config_flow`): the node's config.json,
    /// `operator` with the runtime's defaults under it, `tenant_id` and `_system`.
    fn node_config_answer(node_name: &str, operator: serde_json::Value) -> serde_json::Value {
        let mut config = json!({ "gc_retention_days": 7 });
        for (key, value) in operator.as_object().cloned().unwrap_or_default() {
            config[key] = value;
        }
        config["tenant_id"] = json!(TENANT);
        config["_system"] = json!({
            "runtime": "ai.generic",
            "runtime_version": "0.1.79",
            "ilk_id": "ilk:7c1e0a52-1f0b-4a5e-9d64-0b6a1f2d3c4e",
            "tenant_id": TENANT,
            "config_version": 1,
            "launched_by": "archi"
        });
        json!({
            "status": "ok",
            "target": "motherbee",
            "hive": "motherbee",
            "node_name": node_name,
            "path": format!("/var/lib/fluxbee/nodes/AI/{node_name}/config.json"),
            "config_version": 1,
            "config": config
        })
    }

    /// SY.config.routes' `list_routes` answer (`admin_success_payload`), as SY.admin hands it over.
    fn routes_answer(routes: serde_json::Value) -> serde_json::Value {
        json!({
            "status": "ok",
            "action": "list_routes",
            "payload": { "config_version": 4, "routes": routes }
        })
    }

    /// SY.config.routes' `list_vpns` answer.
    fn vpns_answer(vpns: serde_json::Value) -> serde_json::Value {
        json!({
            "status": "ok",
            "action": "list_vpns",
            "payload": { "config_version": 4, "vpns": vpns }
        })
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
            "name": "ai.common",
            "version": "1.0.0",
            "package_source": "pre_published"
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().runtimes =
            Some(runtimes_answer("motherbee", &[("ai.common", "1.0.0")]));
        let (ops, packets) = reconcile_runtimes(&desired, &snapshot, false);
        assert!(ops.is_empty(), "same version should be noop: {ops:?}");
        assert!(packets.is_empty());
    }

    #[test]
    fn runtime_version_drift_emits_publish_update() {
        let desired = vec![json!({
            "name": "ai.common",
            "version": "1.1.0",
            "package_source": "pre_published"
        })];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().runtimes =
            Some(runtimes_answer("motherbee", &[("ai.common", "1.0.0")]));
        let (ops, _) = reconcile_runtimes(&desired, &snapshot, false);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].change_type, ChangeType::Update);
        assert_eq!(
            ops[0].compiler_class,
            CompilerClass::RuntimePublishAndDistribute
        );
    }

    #[test]
    fn the_primary_hive_version_counts_where_hives_differ() {
        let desired = vec![json!({
            "name": "ai.common",
            "version": "1.1.0",
            "package_source": "pre_published"
        })];
        let mut snapshot = empty_snapshot(&["motherbee", "worker-220"]);
        snapshot.resources.get_mut("motherbee").unwrap().runtimes =
            Some(runtimes_answer("motherbee", &[("ai.common", "1.1.0")]));
        // The worker has not synced the new version yet.
        snapshot.resources.get_mut("worker-220").unwrap().runtimes =
            Some(runtimes_answer("worker-220", &[("ai.common", "1.0.0")]));
        let (ops, _) = reconcile_runtimes(&desired, &snapshot, true);
        assert!(ops.is_empty(), "{ops:?}");
    }

    // ── Nodes ──

    #[test]
    fn missing_node_emits_run_node() {
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "ai.generic"
        })];
        let snapshot = empty_snapshot(&["motherbee"]);
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::NodeRunMissing);
        assert_eq!(ops[0].resource_id, "AI.coa@motherbee");
    }

    fn snapshot_with_node(
        node_name: &str,
        runtime: &str,
        stored: Option<serde_json::Value>,
    ) -> ActualStateSnapshot {
        let mut snapshot = empty_snapshot(&["motherbee"]);
        let motherbee = snapshot.resources.get_mut("motherbee").unwrap();
        motherbee.nodes = Some(nodes_answer(vec![managed_node(node_name, runtime, true)]));
        if let Some(operator) = stored {
            motherbee.node_configs.insert(
                node_name.to_string(),
                node_config_answer(node_name, operator),
            );
        }
        snapshot
    }

    #[test]
    fn an_existing_node_as_declared_is_left_alone() {
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "ai.generic",
            "config": { "behavior": { "kind": "ai_chat", "model": "claude-sonnet-5-5" } }
        })];
        // What ai.generic saves: its behavior filled in with the fields it was not given.
        let snapshot = snapshot_with_node(
            "AI.coa@motherbee",
            "ai.generic",
            Some(json!({ "behavior": {
                "kind": "ai_chat",
                "model": "claude-sonnet-5-5",
                "model_settings": null,
                "base_url": null,
                "multimodal": true
            } })),
        );
        let ops = reconcile_nodes(&desired, &snapshot);
        assert!(ops.is_empty(), "{ops:?}");

        // A declared nested value that differs is still a change.
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "ai.generic",
            "config": { "behavior": { "kind": "ai_chat", "multimodal": false } }
        })];
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::NodeConfigApplyHot);
    }

    #[test]
    fn a_stopped_instance_exists_and_a_directory_without_its_config_does_not() {
        let desired = vec![
            json!({ "node_name": "AI.a@motherbee", "hive": "motherbee", "runtime": "ai.generic" }),
            json!({ "node_name": "AI.b@motherbee", "hive": "motherbee", "runtime": "ai.generic" }),
        ];
        let mut stopped = managed_node("AI.a@motherbee", "ai.generic", true);
        stopped["status"] = json!("stopped");
        stopped["lifecycle_state"] = json!("STOPPED");
        stopped["visible_in_router"] = json!(false);
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().nodes = Some(nodes_answer(vec![
            stopped,
            managed_node("AI.b@motherbee", "ai.generic", false),
        ]));
        let ops = reconcile_nodes(&desired, &snapshot);
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert_eq!(ops[0].compiler_class, CompilerClass::NodeRunMissing);
        assert_eq!(ops[0].resource_id, "AI.b@motherbee");
    }

    #[test]
    fn ai_node_config_drift_emits_hot_apply() {
        let desired = vec![json!({
            "node_name": "AI.coa@motherbee",
            "hive": "motherbee",
            "runtime": "ai.generic",
            "config": { "behavior": { "kind": "ai_chat", "model": "claude-sonnet-5-5" } }
        })];
        let snapshot = snapshot_with_node(
            "AI.coa@motherbee",
            "ai.generic",
            Some(json!({ "behavior": { "kind": "ai_chat", "model": "gpt-5.5" } })),
        );
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
            "runtime": "wf.router",
            "config": { "sy_timer_l2_name": "SY.timer@motherbee", "gc_retention_days": 14 }
        })];
        let snapshot = snapshot_with_node(
            "WF.router@motherbee",
            "wf.router",
            Some(json!({ "sy_timer_l2_name": "SY.timer@motherbee" })),
        );
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
            "runtime": "ai.coa.v2",
            "config": {}
        })];
        let snapshot = snapshot_with_node("AI.coa@motherbee", "ai.coa.v1", None);
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
        let snapshot = snapshot_with_node(
            "ZZ.experimental@motherbee",
            "zz.experimental",
            Some(json!({ "mode": "v1" })),
        );
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
            "prefix": "AI.specialist.",
            "action": "FORWARD",
            "next_hop_hive": "worker-220"
        })];
        let snapshot = empty_snapshot(&["motherbee"]);
        let ops = reconcile_routes(&desired, &snapshot);
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::RouteAdd);
    }

    fn snapshot_with_route(action: &str, next_hop_hive: &str) -> ActualStateSnapshot {
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().routes = Some(routes_answer(json!([{
            "prefix": "AI.specialist.",
            "action": action,
            "next_hop_hive": next_hop_hive,
            "metric": null,
            "priority": null
        }])));
        snapshot
    }

    #[test]
    fn an_existing_route_as_declared_is_left_alone() {
        let desired = vec![json!({
            "hive": "motherbee",
            "prefix": "AI.specialist.",
            "action": "FORWARD",
            "next_hop_hive": "worker-220"
        })];
        let ops = reconcile_routes(&desired, &snapshot_with_route("FORWARD", "worker-220"));
        assert!(ops.is_empty(), "{ops:?}");
    }

    #[test]
    fn route_field_change_emits_route_replace() {
        let desired = vec![json!({
            "hive": "motherbee",
            "prefix": "AI.specialist.",
            "action": "FORWARD",
            "next_hop_hive": "worker-221"
        })];
        // Another next hop, or another action on the same prefix (config.routes keeps one route per
        // prefix: adding a second would be DUPLICATE_PREFIX).
        for (action, next_hop_hive) in [("FORWARD", "worker-220"), ("DROP", "worker-221")] {
            let ops = reconcile_routes(&desired, &snapshot_with_route(action, next_hop_hive));
            assert_eq!(ops.len(), 1, "{action} {next_hop_hive}");
            assert_eq!(ops[0].compiler_class, CompilerClass::RouteReplace);
            assert_eq!(ops[0].resource_id, "motherbee:AI.specialist.");
        }
    }

    #[test]
    fn an_undeclared_route_or_node_is_not_removed() {
        // SY.config.routes and SY.orchestrator record no owner: the reconcile never removes them.
        let mut snapshot = snapshot_with_route("FORWARD", "worker-220");
        snapshot.resources.get_mut("motherbee").unwrap().nodes =
            Some(nodes_answer(vec![managed_node(
                "AI.coa@motherbee",
                "ai.generic",
                true,
            )]));
        assert!(reconcile_routes(&[], &snapshot).is_empty());
        assert!(reconcile_nodes(&[], &snapshot).is_empty());
    }

    // ── WF ──

    fn wf_definition(states: &[&str]) -> serde_json::Value {
        json!({
            "wf_schema_version": "1",
            "workflow_type": "support",
            "initial_state": states[0],
            "terminal_states": [states[states.len() - 1]],
            "states": states.iter().map(|name| json!({ "name": name })).collect::<Vec<_>>(),
        })
    }

    fn desired_wf(definition: &serde_json::Value) -> serde_json::Value {
        json!({
            "hive": "motherbee",
            "workflow_name": "support-router",
            "definition": definition,
            "ownership": "solution"
        })
    }

    /// One workflow of SY.wf-rules' `list_workflows` answer (go/sy-wf-rules/node/dispatch.go),
    /// deployed with `definition`.
    fn listed_workflow(name: &str, definition: &serde_json::Value) -> serde_json::Value {
        json!({
            "workflow_name": name,
            "current_version": 2,
            "current_hash": wf_rules_definition_hash(definition),
            "published_version": "0.0.2",
            "deployed_version": "0.0.2",
            "wf_node_running": true,
            "wf_node_reachable": true,
            "wf_node_health_state": "HEALTHY",
            "active_instances": 0,
            "wf_node_timeout": false,
            "instances_error": ""
        })
    }

    /// A workflow compiled but never applied: listed, with no current definition.
    fn staged_only_workflow(name: &str) -> serde_json::Value {
        json!({
            "workflow_name": name,
            "current_version": null,
            "current_hash": "",
            "published_version": null,
            "deployed_version": "",
            "wf_node_running": false,
            "wf_node_reachable": false,
            "wf_node_health_state": "",
            "active_instances": null,
            "wf_node_timeout": false,
            "instances_error": ""
        })
    }

    /// The motherbee's `list_workflows` answer as SY.admin hands it over: the query payload.
    fn snapshot_with_workflows(workflows: Vec<serde_json::Value>) -> ActualStateSnapshot {
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().wf_state = Some(json!({
            "status": "ok",
            "hive": "motherbee",
            "workflows": workflows
        }));
        snapshot
    }

    #[test]
    fn missing_wf_deployment_emits_wf_deploy_apply() {
        let desired = vec![desired_wf(&wf_definition(&["start", "done"]))];
        for workflows in [vec![], vec![staged_only_workflow("support-router")]] {
            let ops = reconcile_wf(&desired, &snapshot_with_workflows(workflows), "sol-a");
            assert_eq!(ops.len(), 1);
            assert_eq!(ops[0].compiler_class, CompilerClass::WfDeployApply);
            assert_eq!(ops[0].change_type, ChangeType::Create);
            assert_eq!(ops[0].resource_id, "motherbee:support-router");
        }
    }

    #[test]
    fn a_workflow_deployed_with_the_declared_definition_is_left_alone() {
        let definition = wf_definition(&["start", "done"]);
        let snapshot =
            snapshot_with_workflows(vec![listed_workflow("support-router", &definition)]);
        let ops = reconcile_wf(&[desired_wf(&definition)], &snapshot, "sol-a");
        assert!(ops.is_empty(), "{ops:?}");
    }

    #[test]
    fn wf_definition_change_emits_wf_deploy_restart() {
        let running = wf_definition(&["start", "done"]);
        let desired = vec![desired_wf(&wf_definition(&["start", "review", "done"]))];
        let snapshot = snapshot_with_workflows(vec![listed_workflow("support-router", &running)]);
        let ops = reconcile_wf(&desired, &snapshot, "sol-a");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::WfDeployRestart);
        assert_eq!(ops[0].change_type, ChangeType::Update);
    }

    #[test]
    fn a_workflow_this_solution_deployed_and_no_longer_declares_is_removed() {
        let mut snapshot = snapshot_with_workflows(vec![
            listed_workflow("support-router", &wf_definition(&["start", "done"])),
            listed_workflow("by-hand", &wf_definition(&["a", "b"])),
            staged_only_workflow("draft"),
        ]);
        let motherbee = snapshot.resources.get_mut("motherbee").unwrap();
        for name in ["support-router", "draft"] {
            motherbee
                .wf_declared_by
                .insert(name.to_string(), "sol-a".to_string());
        }

        let ops = reconcile_wf(&[], &snapshot, "sol-a");
        assert_eq!(ops.len(), 1, "{ops:?}");
        assert_eq!(ops[0].compiler_class, CompilerClass::WfRemove);
        assert_eq!(ops[0].change_type, ChangeType::Delete);
        assert_eq!(ops[0].resource_id, "motherbee:support-router");
        assert!(!ops[0].blocking);

        // Another solution's workflow, and one nobody declared ("by-hand"), are left alone.
        assert!(reconcile_wf(&[], &snapshot, "sol-b").is_empty());

        // A manifest that stops declaring workflows altogether still removes it.
        let out = run_reconciler(
            &manifest_with(json!({})),
            &snapshot,
            "manifest://sol-a/2",
            "sol-a",
        )
        .expect("reconcile");
        assert_eq!(out.delta_report.operations.len(), 1);
        assert_eq!(out.delta_report.status, DeltaReportStatus::Ready);
    }

    #[test]
    fn a_workflow_is_removed_with_wf_rules_delete() {
        assert_eq!(
            compiler_class_admin_steps(&CompilerClass::WfRemove),
            Some(vec!["wf_rules_delete"])
        );
        assert_eq!(
            compiler_class_risk(&CompilerClass::WfRemove),
            RiskClass::Destructive
        );
    }

    // ── OPA ──

    fn running_policy(rego: &str, declared_by: Option<&str>) -> ActualOpaPolicy {
        ActualOpaPolicy {
            version: 7,
            hash: "sha256:abc".to_string(),
            rego: rego.to_string(),
            entrypoint: "router/target".to_string(),
            declared_by: declared_by.map(str::to_string),
        }
    }

    const REGO: &str = "package router\n\ndefault target = null\n";

    #[test]
    fn opa_out_of_scope_emits_nothing() {
        let desired = json!({ "rego_source": REGO });
        let snapshot = empty_snapshot(&["motherbee"]);
        assert!(reconcile_opa(Some(&desired), &snapshot, "sol-a").is_empty());
    }

    #[test]
    fn a_declared_policy_with_none_running_is_applied_to_every_hive() {
        let desired = json!({ "rego_source": REGO });
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.opa = Some(ActualOpaPolicy::default());
        let ops = reconcile_opa(Some(&desired), &snapshot, "sol-a");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::OpaApply);
        assert_eq!(ops[0].change_type, ChangeType::Create);
        assert_eq!(ops[0].resource_id, "global");
        assert_eq!(ops[0].desired_ref.as_deref(), Some("desired_state.opa"));
        assert_eq!(ops[0].notes, vec!["no user policy is running".to_string()]);
    }

    #[test]
    fn the_same_policy_running_is_left_alone() {
        let desired = json!({ "rego_source": REGO });
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.opa = Some(running_policy(REGO, Some("sol-b")));
        assert!(reconcile_opa(Some(&desired), &snapshot, "sol-a").is_empty());
    }

    #[test]
    fn replacing_a_running_policy_says_whose_it_was() {
        let desired = json!({ "rego_source": REGO });
        let mut snapshot = empty_snapshot(&["motherbee"]);
        for (declared_by, says) in [
            (Some("sol-b"), "declared by solution 'sol-b'"),
            (Some("sol-a"), "this solution declared"),
            (None, "no solution declared"),
        ] {
            snapshot.opa = Some(running_policy("package router\n", declared_by));
            let ops = reconcile_opa(Some(&desired), &snapshot, "sol-a");
            assert_eq!(ops.len(), 1);
            assert_eq!(ops[0].compiler_class, CompilerClass::OpaApply);
            assert_eq!(ops[0].change_type, ChangeType::Update);
            assert!(ops[0].notes[0].contains(says), "{:?}", ops[0].notes);
        }
    }

    #[test]
    fn a_solution_clears_the_running_policy_only_while_it_is_its_own() {
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.opa = Some(running_policy(REGO, Some("sol-a")));
        let ops = reconcile_opa(None, &snapshot, "sol-a");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::OpaRemove);
        assert_eq!(ops[0].change_type, ChangeType::Delete);
        assert!(!ops[0].blocking);

        for running in [
            running_policy(REGO, Some("sol-b")),
            running_policy(REGO, None),
            ActualOpaPolicy::default(),
        ] {
            snapshot.opa = Some(running);
            assert!(reconcile_opa(None, &snapshot, "sol-a").is_empty());
        }
    }

    #[test]
    fn opa_steps_are_one_global_write() {
        assert_eq!(
            compiler_class_admin_steps(&CompilerClass::OpaApply),
            Some(vec!["opa_compile_apply"])
        );
        assert_eq!(
            compiler_class_admin_steps(&CompilerClass::OpaRemove),
            Some(vec!["opa_clear"])
        );
        assert_eq!(
            compiler_class_risk(&CompilerClass::OpaRemove),
            RiskClass::Destructive
        );
    }

    /// One rule of SY.config.routes' `list_vpns` answer (`VpnConfig`; `match_kind` is left out when
    /// the rule names none).
    fn snapshot_with_vpn_rule(rule: serde_json::Value) -> ActualStateSnapshot {
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().vpns = Some(vpns_answer(json!([rule])));
        snapshot
    }

    fn support_rule(vpn_id: u32) -> serde_json::Value {
        json!({ "pattern": "AI.support.*", "match_kind": "GLOB", "vpn_id": vpn_id, "priority": null })
    }

    fn declared_vpn(vpn: serde_json::Value) -> serde_json::Value {
        json!({ "vpns": [vpn] })
    }

    #[test]
    fn an_existing_vpn_as_declared_is_left_alone() {
        let topology = declared_vpn(json!({
            "hive": "motherbee", "pattern": "AI.support.*", "match_kind": "GLOB", "vpn_id": 10
        }));
        let (ops, diagnostics) =
            reconcile_topology(&topology, &snapshot_with_vpn_rule(support_rule(10)));
        assert!(ops.is_empty(), "{ops:?}");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");

        // SY.config.routes' defaults: no match_kind is PREFIX, no priority is 100.
        let topology =
            declared_vpn(json!({ "hive": "motherbee", "pattern": "AI.billing", "vpn_id": 7 }));
        let rule = json!({ "pattern": "AI.billing", "vpn_id": 7, "priority": 100 });
        let (ops, _) = reconcile_topology(&topology, &snapshot_with_vpn_rule(rule));
        assert!(ops.is_empty(), "{ops:?}");

        let (ops, _) = reconcile_topology(&topology, &empty_snapshot(&["motherbee"]));
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].compiler_class, CompilerClass::VpnCreate);
        assert_eq!(ops[0].resource_id, "motherbee:AI.billing");
    }

    #[test]
    fn the_same_pattern_with_another_vpn_is_replaced() {
        let topology = declared_vpn(json!({
            "hive": "motherbee", "pattern": "AI.support.*", "match_kind": "GLOB", "vpn_id": 10
        }));
        let mut prefix_rule = support_rule(10);
        prefix_rule["match_kind"] = json!("PREFIX");
        for rule in [support_rule(11), prefix_rule] {
            let (ops, _) = reconcile_topology(&topology, &snapshot_with_vpn_rule(rule.clone()));
            assert_eq!(ops.len(), 1, "{rule}");
            assert_eq!(ops[0].compiler_class, CompilerClass::VpnReplace);
            assert_eq!(ops[0].change_type, ChangeType::Update);
            assert_eq!(ops[0].resource_id, "motherbee:AI.support.*");
        }
        assert_eq!(
            compiler_class_admin_steps(&CompilerClass::VpnReplace),
            Some(vec!["delete_vpn", "add_vpn"])
        );
    }

    #[test]
    fn a_vpn_rule_the_manifest_does_not_declare_is_not_deleted() {
        // SY.config.routes records no owner for a rule: it is reported, never deleted.
        let topology = json!({ "vpns": [] });
        let (ops, diagnostics) =
            reconcile_topology(&topology, &snapshot_with_vpn_rule(support_rule(10)));
        assert!(ops.is_empty(), "{ops:?}");
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0].contains("VPN rule 'AI.support.*' on hive 'motherbee'"),
            "{diagnostics:?}"
        );
    }

    // ── Ownership ──

    #[test]
    fn an_undeclared_runtime_is_not_touched() {
        let desired: Vec<serde_json::Value> = vec![];
        let mut snapshot = empty_snapshot(&["motherbee"]);
        snapshot.resources.get_mut("motherbee").unwrap().runtimes =
            Some(runtimes_answer("motherbee", &[("ai.chat", "1.0.0")]));
        let (ops, _) = reconcile_runtimes(&desired, &snapshot, false);
        assert!(ops.is_empty(), "the reconciler never deletes a runtime");
    }

    // ── Idempotency ──

    #[test]
    fn reconciler_is_idempotent() {
        let manifest = manifest_with(json!({
            "nodes": [{ "node_name": "AI.coa@motherbee", "hive": "motherbee", "runtime": "AI.common" }]
        }));
        let snapshot = empty_snapshot(&["motherbee"]);
        let out1 = run_reconciler(&manifest, &snapshot, "manifest://test/1.0", "test").unwrap();
        let out2 = run_reconciler(&manifest, &snapshot, "manifest://test/1.0", "test").unwrap();
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

        let out = run_reconciler(&manifest, &snapshot, "manifest://test/1.0", "test").unwrap();
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
