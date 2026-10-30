// Frozen contract types for the Fluxbee pipeline rearchitecture.
// Source spec: fluxbee_rearchitecture_master_spec_v4.md
// These types are contract-frozen — do not change without updating the spec and task list.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

// ── CFZ-6 / CFZ-7 — Loop policy constants ──────────────────────────────────

/// Maximum design loop iterations before escalating to operator.
pub const MAX_DESIGN_ITERATIONS: usize = 3;

/// Minimum audit score improvement per design iteration to continue looping.
pub const MIN_DESIGN_SCORE_IMPROVEMENT: u8 = 1;

/// Maximum artifact generation attempts per build_task_packet.
pub const MAX_ARTIFACT_ATTEMPTS: u32 = 3;

// ── CFZ-1 — solution_manifest v2 ───────────────────────────────────────────

/// Top-level structure of a solution manifest v2.
/// The reconciler reads only `desired_state`.
/// The design auditor reads both `desired_state` and `advisory`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolutionManifestV2 {
    pub manifest_version: String,
    pub solution: serde_json::Value,
    pub desired_state: DesiredStateV2,
    pub advisory: serde_json::Value,
}

/// Reconciler-facing desired state. Only these sections are valid.
/// Any unsupported section (policy, identity) must be rejected by validate_manifest_v2.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DesiredStateV2 {
    pub topology: Option<serde_json::Value>,
    pub runtimes: Option<Vec<serde_json::Value>>,
    pub nodes: Option<Vec<serde_json::Value>>,
    pub routing: Option<Vec<serde_json::Value>>,
    pub wf_deployments: Option<Vec<serde_json::Value>>,
    /// The solution's user OPA policy (`DesiredOpaPolicy`): one policy for every hive.
    pub opa: Option<serde_json::Value>,
    pub ownership: Option<serde_json::Value>,
    #[serde(flatten, default)]
    pub extra_sections: HashMap<String, serde_json::Value>,
}

/// Sections that must be rejected when found in desired_state.
/// These are out of scope for this architecture version.
pub const UNSUPPORTED_DESIRED_STATE_SECTIONS: &[&str] = &["policy", "identity"];

pub fn desired_state_unknown_sections(desired_state: &DesiredStateV2) -> Vec<String> {
    desired_state.extra_sections.keys().cloned().collect()
}

pub fn is_valid_ownership_label(label: &str) -> bool {
    matches!(label, "" | "solution" | "system" | "external")
}

// ── CFZ-2 — actual_state_snapshot ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActualStateSnapshot {
    pub snapshot_version: String,
    pub snapshot_id: String,
    pub captured_at_start: String,
    pub captured_at_end: String,
    pub scope: SnapshotScope,
    pub atomicity: SnapshotAtomicity,
    pub hive_status: HashMap<String, HiveSnapshotStatus>,
    pub resources: HashMap<String, HiveResources>,
    /// The user OPA policy, read once from the motherbee (it is the same on every hive). None when
    /// OPA is not in scope for this run.
    #[serde(default)]
    pub opa: Option<ActualOpaPolicy>,
    pub completeness: SnapshotCompleteness,
}

/// The user policy running on every hive. Empty `hash` when there is none.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ActualOpaPolicy {
    pub version: u64,
    pub hash: String,
    pub rego: String,
    pub entrypoint: String,
    /// The solution whose saved manifest most recently declared exactly this rego. None when no
    /// solution did (the policy was written by hand) or none is running.
    pub declared_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotScope {
    pub hives: Vec<String>,
    pub resources: Vec<String>,
}

/// Snapshot is never globally atomic. Always best_effort_multi_call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotAtomicity {
    pub mode: String,    // always "best_effort_multi_call"
    pub is_atomic: bool, // always false
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveSnapshotStatus {
    pub reachable: bool,
    pub error: Option<String>,
}

/// One hive's answers, each as SY.admin hands it over (the producer's payload); the reconciler
/// reads each shape exactly (`reconciler.rs`, "Snapshot answers").
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HiveResources {
    /// SY.orchestrator's `list_runtimes` answer.
    pub runtimes: Option<serde_json::Value>,
    /// SY.orchestrator's `list_nodes` answer for the managed instances (`managed: true`).
    pub nodes: Option<serde_json::Value>,
    /// SY.orchestrator's `get_node_config` answer for each declared node with a declared config
    /// that the hive runs as a managed instance, by node name.
    #[serde(default)]
    pub node_configs: HashMap<String, serde_json::Value>,
    /// SY.config.routes' `list_routes` answer.
    pub routes: Option<serde_json::Value>,
    /// SY.config.routes' `list_vpns` answer.
    pub vpns: Option<serde_json::Value>,
    /// SY.wf-rules' `list_workflows` answer: `{status, hive, workflows: [{workflow_name,
    /// current_hash, ...}]}`.
    pub wf_state: Option<serde_json::Value>,
    /// workflow_name -> the solution whose saved manifest most recently declared, as its own
    /// (`ownership: solution`), exactly the definition this hive runs under that name. SY.wf-rules
    /// records no owner, so this is what Archi knows it deployed, as for the user policy
    /// (`ActualOpaPolicy::declared_by`). A workflow no solution declared is not listed.
    #[serde(default)]
    pub wf_declared_by: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotCompleteness {
    pub is_partial: bool,
    pub missing_sections: Vec<String>,
    /// If true, reconciler must not emit destructive deltas for missing hives.
    pub blocking: bool,
}

// ── CFZ-3 — delta_report ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeltaReportStatus {
    Ready,
    BlockedPartialSnapshot,
    BlockedUnsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    Create,
    Update,
    Delete,
    Noop,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaReport {
    pub delta_report_version: String,
    pub status: DeltaReportStatus,
    pub manifest_ref: String,
    pub snapshot_ref: String,
    pub summary: DeltaSummary,
    pub operations: Vec<DeltaOperation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeltaSummary {
    pub creates: usize,
    pub updates: usize,
    pub deletes: usize,
    pub noops: usize,
    pub blocked: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeltaOperation {
    pub op_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub change_type: ChangeType,
    /// "solution" | "system" | "external"
    pub ownership: String,
    pub compiler_class: CompilerClass,
    pub desired_ref: Option<String>,
    pub actual_ref: Option<String>,
    /// If true and snapshot is partial, reconciler blocks the full report.
    pub blocking: bool,
    /// Reference to the approved artifact_bundle required for this op, if any.
    pub payload_ref: Option<String>,
    pub notes: Vec<String>,
}

// ── CFZ-4 — compiler_class ─────────────────────────────────────────────────
// No generic "update" class is allowed. Every change must map to one of these.
// If a change cannot be expressed, emit Blocked* rather than a vague class.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CompilerClass {
    // Topology
    HiveCreate,
    VpnCreate,
    VpnDelete,
    /// The same pattern with another id, match kind or priority: delete and add.
    VpnReplace,
    // Runtimes
    RuntimePublishOnly,
    RuntimePublishAndDistribute,
    RuntimeDeleteVersion,
    // Nodes
    NodeRunMissing,
    NodeConfigApplyHot,
    NodeConfigApplyRestart,
    NodeRestartOnly,
    NodeKill,
    /// Only emitted when desired node has reconcile_policy.on_immutable_change = "recreate".
    NodeRecreate,
    /// Default when immutable field changes without explicit recreate policy.
    BlockedImmutableChange,
    // Routing
    RouteAdd,
    RouteDelete,
    RouteReplace,
    // WF
    WfDeployApply,
    WfDeployRestart,
    /// Removes a workflow the solution deployed and no longer declares (`wf_rules_delete`).
    WfRemove,
    // OPA (one user policy for every hive)
    OpaApply,
    /// Clears the user policy when the solution whose policy runs stops declaring it.
    OpaRemove,
    // Meta
    Noop,
    BlockedPartialSnapshot,
    ScopeNotSupported,
}

// ── CFZ-5 — risk class and translation table ────────────────────────────────
// This is the frozen contract between the reconciler and the plan compiler.
// The plan compiler MUST import and use these functions; must not redefine them.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    NonDestructive,
    Restarting,
    Destructive,
    BlockedUntilOperator,
}

/// Returns the risk class for a compiler_class.
/// Used by the host to build the CONFIRM 2 payload — destructive and restarting
/// ops are explicitly highlighted to the operator.
pub fn compiler_class_risk(class: &CompilerClass) -> RiskClass {
    match class {
        CompilerClass::HiveCreate
        | CompilerClass::VpnCreate
        | CompilerClass::RuntimePublishOnly
        | CompilerClass::RuntimePublishAndDistribute
        | CompilerClass::NodeRunMissing
        | CompilerClass::NodeConfigApplyHot
        | CompilerClass::RouteAdd
        | CompilerClass::WfDeployApply
        | CompilerClass::OpaApply
        | CompilerClass::Noop => RiskClass::NonDestructive,

        CompilerClass::NodeConfigApplyRestart
        | CompilerClass::NodeRestartOnly
        | CompilerClass::WfDeployRestart => RiskClass::Restarting,

        CompilerClass::VpnDelete
        | CompilerClass::VpnReplace
        | CompilerClass::RuntimeDeleteVersion
        | CompilerClass::NodeKill
        | CompilerClass::NodeRecreate
        | CompilerClass::RouteDelete
        | CompilerClass::RouteReplace
        | CompilerClass::WfRemove
        | CompilerClass::OpaRemove => RiskClass::Destructive,

        CompilerClass::BlockedImmutableChange
        | CompilerClass::BlockedPartialSnapshot
        | CompilerClass::ScopeNotSupported => RiskClass::BlockedUntilOperator,
    }
}

/// Returns the canonical ordered admin-step sequence for a compiler_class.
/// Returns None for blocked classes — plan compiler must not emit steps for these.
/// Plan compiler may only use admin help to fill exact args within this sequence;
/// it may not choose a different sequence.
pub fn compiler_class_admin_steps(class: &CompilerClass) -> Option<Vec<&'static str>> {
    match class {
        CompilerClass::HiveCreate => Some(vec!["add_hive"]),
        CompilerClass::VpnCreate => Some(vec!["add_vpn"]),
        CompilerClass::VpnDelete => Some(vec!["delete_vpn"]),
        CompilerClass::VpnReplace => Some(vec!["delete_vpn", "add_vpn"]),
        CompilerClass::RuntimePublishOnly => Some(vec!["publish_runtime_package"]),
        // sync_to + update_to are encoded in step args, not separate steps
        CompilerClass::RuntimePublishAndDistribute => Some(vec!["publish_runtime_package"]),
        CompilerClass::RuntimeDeleteVersion => Some(vec!["remove_runtime_version"]),
        CompilerClass::NodeRunMissing => Some(vec!["run_node"]),
        CompilerClass::NodeConfigApplyHot => Some(vec!["node_control_config_set"]),
        CompilerClass::NodeConfigApplyRestart => {
            Some(vec!["node_control_config_set", "restart_node"])
        }
        CompilerClass::NodeRestartOnly => Some(vec!["restart_node"]),
        CompilerClass::NodeKill => Some(vec!["kill_node"]),
        CompilerClass::NodeRecreate => Some(vec!["kill_node", "run_node"]),
        CompilerClass::RouteAdd => Some(vec!["add_route"]),
        CompilerClass::RouteDelete => Some(vec!["delete_route"]),
        CompilerClass::RouteReplace => Some(vec!["delete_route", "add_route"]),
        CompilerClass::WfDeployApply => Some(vec!["wf_rules_compile_apply"]),
        CompilerClass::WfDeployRestart => Some(vec!["wf_rules_compile_apply", "restart_node"]),
        CompilerClass::WfRemove => Some(vec!["wf_rules_delete"]),
        CompilerClass::OpaApply => Some(vec!["opa_compile_apply"]),
        CompilerClass::OpaRemove => Some(vec!["opa_clear"]),
        // Meta / blocked — no steps
        CompilerClass::Noop
        | CompilerClass::BlockedImmutableChange
        | CompilerClass::BlockedPartialSnapshot
        | CompilerClass::ScopeNotSupported => None,
    }
}

// ── CFZ-8 — failure class ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FailureClass {
    DesignIncomplete,
    DesignConflict,
    SnapshotPartialBlocking,
    /// Snapshot builder received a scope it cannot yet read.
    SnapshotSectionUnsupported,
    DeltaUnsupported,
    ArtifactTaskUnderspecified,
    ArtifactContractInvalid,
    ArtifactLayoutInvalid,
    PlanInvalid,
    PlanContractInvalid,
    ExecutionEnvironmentMissing,
    ExecutionActionFailed,
    ExecutionTimeout,
    /// No deterministic rule matched — escalate to operator or residual AI classifier.
    UnknownResidual,
}

// ── CFZ-11 — pipeline_runs ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineRunStatus {
    InProgress,
    Blocked,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStage {
    Design,
    DesignAudit,
    Confirm1,
    Reconcile,
    ArtifactLoop,
    PlanCompile,
    PlanValidation,
    Confirm2,
    Execute,
    Verify,
    Completed,
    Failed,
    Blocked,
    Interrupted,
}

impl std::fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("{self:?}"));
        write!(f, "{s}")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineRunRecord {
    pub pipeline_run_id: String,
    pub session_id: String,
    pub solution_id: Option<String>,
    pub status: PipelineRunStatus,
    pub current_stage: PipelineStage,
    pub current_loop: u32,
    pub current_attempt: u32,
    /// Serialized PipelineRunState with refs to all active artifacts.
    pub state_json: String,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub interrupted_at_ms: Option<u64>,
}

// ── CFZ-12 — restart policy table ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum RestartPolicy {
    /// Config change can be applied live without restart.
    Hot,
    /// Config change requires a node restart to take effect.
    RequiresRestart,
    /// Field is immutable — change cannot be applied without recreate or operator decision.
    Blocked,
}

/// Returns the restart policy for a node given its name and whether the changed
/// field is classified as immutable. The reconciler calls this to choose between
/// NODE_CONFIG_APPLY_HOT, NODE_CONFIG_APPLY_RESTART, and BLOCKED_IMMUTABLE_CHANGE.
pub fn restart_policy_for_node(node_name: &str, field_is_immutable: bool) -> RestartPolicy {
    if field_is_immutable {
        return RestartPolicy::Blocked;
    }
    // WF.* and SY.* require restart by default (conservative policy).
    // AI.* and IO.* are hot by default.
    // Unknown prefix -> Blocked. The reconciler must not guess semantics.
    if node_name.starts_with("WF.") || node_name.starts_with("SY.") {
        RestartPolicy::RequiresRestart
    } else if node_name.starts_with("AI.") || node_name.starts_with("IO.") {
        RestartPolicy::Hot
    } else {
        RestartPolicy::Blocked
    }
}

// ── CFZ-13 — CookbookEntryV2 ───────────────────────────────────────────────
// All four cookbooks use this exact shape. No layer-specific top-level fields.

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CookbookLayer {
    Design,
    Artifact,
    PlanCompile,
    Repair,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookbookEntryV2 {
    pub layer: CookbookLayer,
    /// Unique key per layer, lowercase with underscores.
    pub pattern_key: String,
    /// One sentence describing when to use this pattern.
    pub trigger: String,
    pub inputs_signature: serde_json::Value,
    pub successful_shape: serde_json::Value,
    pub constraints: Vec<String>,
    pub do_not_repeat: Vec<String>,
    /// Only populated for repair layer entries.
    pub failure_class: Option<String>,
    /// pipeline_run_id if observed; null if manually seeded.
    pub recorded_from_run: Option<String>,
    /// "manual" for hand-written seeds; "observed" for pipeline-generated.
    pub seed_kind: String,
    /// The version of the built-in seed a "manual" entry comes from; none on an "observed" one.
    /// Archi replaces a cookbook's manual entries when its built-in seeds are newer
    /// (`seed_cookbooks_from_defaults`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_version: Option<u32>,
}

// ── CFZ-9 — get_manifest_current path constants ─────────────────────────────

/// Cookbook file paths relative to state_dir.
pub mod cookbook_paths {
    pub const DESIGN: &str = "cookbook/design_cookbook_v1.json";
    pub const ARTIFACT: &str = "cookbook/artifact_cookbook_v1.json";
    pub const PLAN_COMPILE: &str = "cookbook/plan_compile_cookbook_v1.json";
    pub const REPAIR: &str = "cookbook/repair_cookbook_v1.json";
}

/// Manifest storage path for a given solution and version.
pub fn manifest_path(
    state_dir: &std::path::Path,
    solution_id: &str,
    version: &str,
) -> std::path::PathBuf {
    state_dir
        .join("manifests")
        .join(solution_id)
        .join(format!("{version}.json"))
}

// ── Desired-state typed views (used by reconciler) ─────────────────────────
// These are typed projections of the desired_state JSON objects.
// The reconciler deserializes desired_state sections into these types.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredHive {
    pub hive_id: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub ownership: String,
}

/// A VPN rule as SY.config.routes keeps it (`list_vpns`, `add_vpn`): the nodes of `hive` whose names
/// match `pattern` are in VPN `vpn_id`. Identified by its hive and pattern: config.routes keeps one
/// rule per pattern on a hive (a second one is DUPLICATE_PATTERN).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredVpn {
    pub hive: String,
    pub pattern: String,
    pub vpn_id: u32,
    /// EXACT, PREFIX or GLOB (`VPN_MATCH_KINDS`); PREFIX when omitted, as in SY.config.routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_kind: Option<String>,
    /// 100 when omitted, as in SY.config.routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u16>,
    #[serde(default)]
    pub ownership: String,
}

/// The route actions SY.config.routes accepts (case-sensitive).
pub const ROUTE_ACTIONS: [&str; 2] = ["FORWARD", "DROP"];

/// The longest prefix SY.config.routes hands to the router whole (its shared-memory field).
pub const ROUTE_PREFIX_MAX_BYTES: usize = 256;

/// The longest next hop hive name SY.config.routes hands to the router whole.
pub const ROUTE_NEXT_HOP_MAX_BYTES: usize = 32;

/// The runtime naming rule (`runtime_name_regex` in src/runtime_package.rs,
/// `^[a-z][a-z0-9]*(\.[a-z0-9]+)*$`): a package with another name is refused, and a name the
/// manifest spells otherwise never matches the published one.
pub fn valid_runtime_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let first_ok = parts.next().is_some_and(|first| {
        let mut chars = first.chars();
        chars.next().is_some_and(|ch| ch.is_ascii_lowercase())
            && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
    });
    first_ok
        && parts.all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        })
}

/// The match kinds SY.config.routes accepts for a VPN rule.
pub const VPN_MATCH_KINDS: [&str; 3] = ["EXACT", "PREFIX", "GLOB"];

/// The longest pattern SY.config.routes hands to the router whole (its shared-memory field).
pub const VPN_PATTERN_MAX_BYTES: usize = 256;

impl DesiredVpn {
    /// Identity key for deduplication and delta comparison: `<hive>:<pattern>`.
    pub fn identity_key(&self) -> String {
        format!("{}:{}", self.hive, self.pattern)
    }

    /// Whether a `list_vpns` rule with this pattern is this VPN as declared: the same id, match
    /// kind and priority, taking SY.config.routes' defaults (PREFIX, 100) where either names none.
    pub fn holds(&self, rule: &serde_json::Value) -> bool {
        let rule_kind = rule.get("match_kind").and_then(serde_json::Value::as_str);
        let rule_priority = rule.get("priority").and_then(serde_json::Value::as_u64);
        rule.get("vpn_id").and_then(serde_json::Value::as_u64) == Some(u64::from(self.vpn_id))
            && rule_kind.unwrap_or("PREFIX") == self.match_kind.as_deref().unwrap_or("PREFIX")
            && rule_priority.unwrap_or(100) == u64::from(self.priority.unwrap_or(100))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredRuntime {
    pub name: String,
    #[serde(default)]
    pub version: String,
    /// "inline_package" | "bundle_upload" | "pre_published"
    #[serde(default)]
    pub package_source: String,
    #[serde(default)]
    pub sync_to: Vec<String>,
    #[serde(default)]
    pub ownership: String,
}

impl DesiredRuntime {
    pub fn needs_artifact(&self) -> bool {
        matches!(
            self.package_source.as_str(),
            "inline_package" | "bundle_upload" | ""
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReconcilePolicy {
    /// "block" (default) | "recreate"
    #[serde(default)]
    pub on_immutable_change: String,
}

impl ReconcilePolicy {
    pub fn allows_recreate(&self) -> bool {
        self.on_immutable_change == "recreate"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredNode {
    pub node_name: String,
    pub hive: String,
    pub runtime: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub reconcile_policy: ReconcilePolicy,
    #[serde(default = "default_solution_ownership")]
    pub ownership: String,
}

fn default_solution_ownership() -> String {
    "solution".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredRoute {
    pub hive: String,
    pub prefix: String,
    pub action: String,
    #[serde(default)]
    pub next_hop_hive: Option<String>,
    #[serde(default = "default_solution_ownership")]
    pub ownership: String,
}

impl DesiredRoute {
    /// Identity key for deduplication and delta comparison: SY.config.routes keeps one route per
    /// prefix on a hive (a second one is DUPLICATE_PREFIX), whatever its action.
    pub fn identity_key(&self) -> String {
        format!("{}:{}", self.hive, self.prefix)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredWfDeployment {
    pub hive: String,
    pub workflow_name: String,
    pub definition: serde_json::Value,
    /// External when omitted, as the designer is told: only a workflow declared `solution` is the
    /// solution's to remove.
    #[serde(default = "default_external_ownership")]
    pub ownership: String,
}

fn default_external_ownership() -> String {
    "external".to_string()
}

impl DesiredWfDeployment {
    /// The `current_hash` SY.wf-rules reports once this definition is deployed.
    pub fn definition_hash(&self) -> String {
        wf_rules_definition_hash(&self.definition)
    }
}

/// SY.wf-rules' workflow name rule (`workflowNamePattern` in go/sy-wf-rules/node/types.go,
/// `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$`).
pub fn valid_workflow_name(name: &str) -> bool {
    name.split('.').all(|part| {
        let mut chars = part.chars();
        chars.next().is_some_and(|first| first.is_ascii_lowercase())
            && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    })
}

/// What is wrong with the shape of a workflow definition as SY.wf-rules checks it (`wfcel`
/// `ValidateDefinition`): `wf_schema_version` "1", a `workflow_type`, a `start_from` that names
/// the nodes whose messages start an instance, an `input_schema`, states each with a name of its
/// own, and an `initial_state` and `terminal_states` among them. None when the shape is right; its
/// node names, guards and actions are SY.wf-rules' to check.
pub fn workflow_definition_shape_error(definition: &serde_json::Value) -> Option<String> {
    use serde_json::Value;
    let text = |key: &str| {
        definition
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    if !definition.is_object() {
        return Some("the definition must be a JSON object".to_string());
    }
    if text("wf_schema_version") != "1" {
        return Some("wf_schema_version must be \"1\"".to_string());
    }
    if text("workflow_type").trim().is_empty() {
        return Some("workflow_type must not be empty".to_string());
    }
    if definition
        .get("start_from")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Some("start_from must name the nodes whose messages start an instance".to_string());
    }
    if definition
        .get("input_schema")
        .and_then(Value::as_object)
        .is_none_or(serde_json::Map::is_empty)
    {
        return Some("input_schema must not be empty".to_string());
    }
    let Some(states) = definition
        .get("states")
        .and_then(Value::as_array)
        .filter(|states| !states.is_empty())
    else {
        return Some("states must contain at least one state".to_string());
    };
    let mut names = HashSet::new();
    for (idx, state) in states.iter().enumerate() {
        let name = state
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if name.trim().is_empty() || !names.insert(name) {
            return Some(format!("states[{idx}] needs a name of its own"));
        }
    }
    if !names.contains(text("initial_state")) {
        return Some("initial_state must be one of the states".to_string());
    }
    let terminal = definition
        .get("terminal_states")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if terminal
        .iter()
        .any(|name| !name.as_str().is_some_and(|name| names.contains(name)))
    {
        return Some("every terminal state must be one of the states".to_string());
    }
    None
}

/// The hash SY.wf-rules gives a workflow definition (`current_hash` in `list_workflows`):
/// `sha256:` and the hex SHA-256 of the bytes it stores, which are Go's
/// `json.MarshalIndent(definition, "", "  ")` and a newline, the definition decoded with its
/// numbers as written (`decodeCompileRequest`, `normalizeDefinitionBytes` and `HashDefinition` in
/// go/sy-wf-rules/node). A number is written as serde_json wrote it: the definition travels to
/// SY.wf-rules as serde_json writes it, from Archi and again from SY.admin.
pub fn wf_rules_definition_hash(definition: &serde_json::Value) -> String {
    let mut stored = String::new();
    write_go_json(definition, 0, &mut stored);
    stored.push('\n');
    format!("sha256:{:x}", Sha256::digest(stored.as_bytes()))
}

/// `value` as Go's `json.MarshalIndent(value, "", "  ")` writes what it decoded into `any` with
/// `UseNumber`: object keys sorted, every number as it came (serde_json's form of it, since 0.1.90;
/// before, every number was a float64, so `1.0` was stored as `1`), strings with Go's escaping.
fn write_go_json(value: &serde_json::Value, depth: usize, out: &mut String) {
    use serde_json::Value;
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => out.push_str(&number.to_string()),
        Value::String(text) => write_go_string(text, out),
        Value::Array(items) if items.is_empty() => out.push_str("[]"),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_go_indent(depth + 1, out);
                write_go_json(item, depth + 1, out);
            }
            push_go_indent(depth, out);
            out.push(']');
        }
        Value::Object(map) if map.is_empty() => out.push_str("{}"),
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_go_indent(depth + 1, out);
                write_go_string(key, out);
                out.push_str(": ");
                write_go_json(&map[key], depth + 1, out);
            }
            push_go_indent(depth, out);
            out.push('}');
        }
    }
}

fn push_go_indent(depth: usize, out: &mut String) {
    out.push('\n');
    for _ in 0..depth {
        out.push_str("  ");
    }
}

/// A string as Go's encoding/json writes it: `json.Marshal` escapes `<`, `>`, `&`, U+2028 and
/// U+2029 too.
fn write_go_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch if u32::from(ch) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(ch));
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
}

/// `desired_state.opa`: the solution's user OPA policy. There is one user policy for every hive
/// (the motherbee compiles and publishes it, every hive installs it), so it names no hive. When
/// several solutions declare one, the last one applied wins (operator decision 2026-10-01,
/// FINDINGS A-23).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredOpaPolicy {
    pub rego_source: String,
    #[serde(default = "default_opa_entrypoint")]
    pub entrypoint: String,
    #[serde(default = "default_solution_ownership")]
    pub ownership: String,
}

fn default_opa_entrypoint() -> String {
    "router/target".to_string()
}

// ── BuildTaskPacket (TC-1, used by reconciler artifact gap detection) ───────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildTaskKnownContext {
    #[serde(default)]
    pub available_runtimes: Vec<String>,
    #[serde(default)]
    pub running_nodes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildTaskPacket {
    #[serde(default = "default_task_packet_version")]
    pub task_packet_version: String,
    pub task_id: String,
    /// "runtime_package" | "workflow_definition" | "opa_bundle" | "config_bundle"
    pub artifact_kind: String,
    pub source_delta_op: String,
    pub runtime_name: Option<String>,
    /// "inline_package" | "workflow_def" | "opa_source"
    pub target_kind: String,
    #[serde(default)]
    pub requirements: serde_json::Value,
    #[serde(default)]
    pub constraints: serde_json::Value,
    pub known_context: BuildTaskKnownContext,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default = "default_max_artifact_attempts")]
    pub max_attempts: u32,
    #[serde(default)]
    pub cookbook_context: Vec<serde_json::Value>,
}

fn default_task_packet_version() -> String {
    "0.1".to_string()
}
fn default_max_artifact_attempts() -> u32 {
    crate::pipeline_types::MAX_ARTIFACT_ATTEMPTS
}

// ── ReconcilerOutput ─────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ReconcilerOutput {
    pub delta_report: DeltaReport,
    pub build_task_packets: Vec<BuildTaskPacket>,
    pub diagnostics: Vec<String>,
}

// ── TC-2 — ArtifactBundle ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactBundleStatus {
    Pending,
    Approved,
    Rejected,
    Repairable,
}

/// Output produced by the programmer agent for a single BuildTaskPacket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactBundle {
    #[serde(default = "default_bundle_version")]
    pub bundle_version: String,
    pub bundle_id: String,
    pub source_task_id: String,
    /// "runtime_package" | "workflow_definition" | "opa_bundle" | "config_bundle"
    pub artifact_kind: String,
    pub status: ArtifactBundleStatus,
    pub summary: String,
    /// The actual generated artifact — shape is artifact_kind-specific.
    pub artifact: serde_json::Value,
    #[serde(default)]
    pub assumptions: Vec<String>,
    #[serde(default)]
    pub verification_hints: Vec<String>,
    pub content_digest: String,
    pub generated_at_ms: u64,
    pub generator_model: String,
}

fn default_bundle_version() -> String {
    "0.1".to_string()
}

// ── TC-3 — ArtifactAuditVerdict ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuditSeverity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AuditStatus {
    Approved,
    Repairable,
    Rejected,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditFinding {
    pub code: String,
    pub field: Option<String>,
    pub message: String,
    pub severity: AuditSeverity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairHint {
    pub finding_code: String,
    pub instruction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactAuditVerdict {
    pub verdict_id: String,
    pub source_bundle_id: String,
    pub source_task_id: String,
    pub status: AuditStatus,
    pub failure_class: Option<FailureClass>,
    pub findings: Vec<AuditFinding>,
    pub blocking_issues: Vec<String>,
    pub repair_hints: Vec<RepairHint>,
}

// ── TC-4 — RepairPacket ───────────────────────────────────────────────────────

/// Sent to the programmer on a retry attempt. Contains mandatory corrections.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairPacket {
    #[serde(default = "default_repair_packet_version")]
    pub repair_packet_version: String,
    pub repair_id: String,
    pub source_task_id: String,
    pub previous_bundle_id: String,
    pub previous_content_digest: String,
    pub failure_class: FailureClass,
    pub required_corrections: Vec<String>,
    pub do_not_repeat: Vec<String>,
    #[serde(default)]
    pub must_preserve: Vec<String>,
    pub retry_allowed: bool,
    pub attempt: u32,
}

fn default_repair_packet_version() -> String {
    "0.1".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The expected values come from Go 1.25 running what SY.wf-rules runs on these definitions as
    /// serde_json writes them (decodeCompileRequest, which keeps numbers as written,
    /// normalizeDefinitionBytes, HashDefinition): SY.wf-rules stores a number as it came, and it
    /// comes as serde_json wrote it (`1.0` stays `1.0`, `1e21` is `1e+21`, `-0` is `-0.0`).
    #[test]
    fn the_definition_hash_is_the_one_wf_rules_reports() {
        let simple = json!({ "b": 1.0, "a": [2, { "d": "<x>", "c": null }] });
        let mut stored = String::new();
        write_go_json(&simple, 0, &mut stored);
        assert_eq!(
            stored,
            "{\n  \"a\": [\n    2,\n    {\n      \"c\": null,\n      \"d\": \"\\u003cx\\u003e\"\n    }\n  ],\n  \"b\": 1.0\n}"
        );
        assert_eq!(
            wf_rules_definition_hash(&simple),
            "sha256:ba4dca861b73e3a2cb38bcd173342c790be4f4bfed640ee36115c06e4db043d9"
        );

        // Key order, numbers as serde_json writes them, and Go's string escapes.
        let tricky: serde_json::Value = serde_json::from_str(
            r#"{"workflow_type":"demo","wf_schema_version":"1","initial_state":"start","states":[{"name":"start","entry_actions":[],"exit_actions":[{}],"transitions":[{"guard":"event.payload.n > 2 && a < b","target_state":"done","actions":[{"type":"set_variable","name":"x","value":"'<b>&amp;</b>'"}]}]},{"name":"done"}],"numbers":{"int":3,"float_int":1.0,"half":0.5,"neg":-7,"big":12345678901234567890,"tiny":1e-7,"small":0.000001,"huge":1e21,"just_under":1e20,"third":0.30000000000000004,"neg_zero":-0,"exp":2.5e-10,"neg_huge":-1e22},"text":{"quote":"say \"hi\"","backslash":"a\\b","newline":"l1\nl2","tab":"a\tb","cr":"a\rb","bs":"a\bb","ff":"a\fb","ctl":"a\u0001b\u001fc","del":"a\u007fb","seps":"x\u2028y\u2029z","accents":"Mendoza \u00e9 \u00f1 \u00fc","emoji":"\ud83d\udc1d","slash":"a/b"},"empty_obj":{},"empty_arr":[],"nested":[[],[{}],[1,[2,[3]]]],"flags":[true,false,null],"Zeta":1,"alpha":2,"Alpha":3,"_u":4,"10":5,"9":6}"#,
        )
        .expect("definition");
        assert_eq!(
            wf_rules_definition_hash(&tricky),
            "sha256:a559c5156e375fcf423e6d0b2ecb3d0338eddb18bf06e93891199ab9fa4c05a7"
        );

        let deployment: DesiredWfDeployment = serde_json::from_value(json!({
            "hive": "motherbee",
            "workflow_name": "invoice",
            "definition": simple,
        }))
        .expect("deployment");
        assert_eq!(
            deployment.definition_hash(),
            wf_rules_definition_hash(&simple)
        );
    }
}
