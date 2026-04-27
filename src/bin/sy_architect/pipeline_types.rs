// Frozen contract types for the Fluxbee pipeline rearchitecture.
// Source spec: fluxbee_rearchitecture_master_spec_v4.md
// These types are contract-frozen — do not change without updating the spec and task list.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub opa_deployments: Option<Vec<serde_json::Value>>,
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
    pub completeness: SnapshotCompleteness,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HiveResources {
    pub runtimes: Option<serde_json::Value>,
    pub nodes: Option<serde_json::Value>,
    pub routes: Option<serde_json::Value>,
    pub vpns: Option<serde_json::Value>,
    pub wf_state: Option<serde_json::Value>,
    pub opa_state: Option<serde_json::Value>,
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
    /// Blocked until canonical WF remove action is defined in admin surface.
    WfRemove,
    // OPA
    OpaApply,
    /// Blocked until canonical OPA remove action is defined in admin surface.
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
        CompilerClass::OpaApply => Some(vec!["opa_compile_apply"]),
        // Blocked — no steps to emit until admin surface defines remove actions
        CompilerClass::WfRemove | CompilerClass::OpaRemove => None,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredVpn {
    pub vpn_id: String,
    pub hive_id: String,
    #[serde(default)]
    pub ownership: String,
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
    /// Identity key for deduplication and delta comparison.
    pub fn identity_key(&self) -> String {
        format!("{}:{}:{}", self.hive, self.prefix, self.action)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredWfDeployment {
    pub hive: String,
    pub workflow_name: String,
    pub definition: serde_json::Value,
    #[serde(default = "default_solution_ownership")]
    pub ownership: String,
}

impl DesiredWfDeployment {
    pub fn definition_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let s = serde_json::to_string(&self.definition).unwrap_or_default();
        let mut h = DefaultHasher::new();
        s.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DesiredOpaDeployment {
    pub hive: String,
    pub policy_id: String,
    pub rego_source: String,
    #[serde(default = "default_solution_ownership")]
    pub ownership: String,
}

impl DesiredOpaDeployment {
    pub fn rego_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut h = DefaultHasher::new();
        self.rego_source.hash(&mut h);
        format!("{:016x}", h.finish())
    }
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
