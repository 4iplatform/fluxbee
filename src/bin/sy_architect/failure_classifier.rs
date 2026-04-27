// Deterministic failure classifier for the Fluxbee pipeline.
// Source spec: fluxbee_rearchitecture_master_spec_v4.md — section 11
//
// Rule: deterministic matcher runs first. Only if no rule matches is the
// residual AI classifier invoked. This keeps retries predictable and testable.

use super::pipeline_types::{FailureClass, PipelineStage};

/// Context passed to the classifier alongside the raw error.
#[derive(Debug, Clone)]
pub struct FailureContext {
    pub stage: PipelineStage,
    pub error_code: Option<String>,
    pub resource_type: Option<String>,
}

/// Attempt to classify a failure deterministically.
/// Returns `None` when no rule matches — caller must invoke the residual AI classifier.
pub fn classify_failure_deterministic(
    error_text: &str,
    ctx: &FailureContext,
) -> Option<FailureClass> {
    let text = error_text.to_lowercase();
    let code = ctx.error_code.as_deref().unwrap_or("");

    // ── Snapshot-level failures ──────────────────────────────────────────────
    if code == "SNAPSHOT_PARTIAL_BLOCKING"
        || text.contains("snapshot partial")
        || text.contains("hive unreachable")
        || text.contains("hive not reachable")
    {
        return Some(FailureClass::SnapshotPartialBlocking);
    }

    if text.contains("snapshot section") && text.contains("unsupported") {
        return Some(FailureClass::SnapshotSectionUnsupported);
    }

    // ── Execution failures ───────────────────────────────────────────────────
    if text.contains("runtime not materialized")
        || text.contains("runtime is not materialized")
        || text.contains("not yet materialized")
        || text.contains("current_materialized: false")
    {
        return Some(FailureClass::ExecutionEnvironmentMissing);
    }

    if code == "EXECUTION_TIMEOUT"
        || text.contains("timed out waiting")
        || text.contains("timeout waiting")
        || text.contains("operation timed out")
    {
        return Some(FailureClass::ExecutionTimeout);
    }

    // ── Plan failures — resolved by stage context ────────────────────────────
    if text.contains("unknown action")
        || text.contains("action not found")
        || text.contains("no such action")
    {
        return Some(FailureClass::PlanInvalid);
    }

    if text.contains("missing required field") || text.contains("required field") {
        return match ctx.stage {
            PipelineStage::PlanValidation | PipelineStage::PlanCompile => {
                Some(FailureClass::PlanContractInvalid)
            }
            PipelineStage::ArtifactLoop => Some(FailureClass::ArtifactContractInvalid),
            _ => Some(FailureClass::PlanContractInvalid),
        };
    }

    if text.contains("additional properties")
        || text.contains("additionalproperties")
        || text.contains("invalid field")
    {
        return match ctx.stage {
            PipelineStage::ArtifactLoop => Some(FailureClass::ArtifactContractInvalid),
            _ => Some(FailureClass::PlanContractInvalid),
        };
    }

    // ── Artifact failures ────────────────────────────────────────────────────
    if text.contains("package missing")
        || text.contains("missing required file")
        || text.contains("required file not found")
        || text.contains("file not found in bundle")
    {
        return Some(FailureClass::ArtifactLayoutInvalid);
    }

    if text.contains("invalid runtime_base")
        || text.contains("runtime_base mismatch")
        || text.contains("unknown runtime_base")
    {
        return Some(FailureClass::ArtifactContractInvalid);
    }

    if text.contains("artifact task underspecified")
        || text.contains("task packet incomplete")
        || text.contains("missing artifact_kind")
    {
        return Some(FailureClass::ArtifactTaskUnderspecified);
    }

    // ── Design failures ──────────────────────────────────────────────────────
    if text.contains("manifest incomplete")
        || text.contains("desired_state incomplete")
        || text.contains("missing topology")
        || text.contains("no nodes defined")
    {
        return Some(FailureClass::DesignIncomplete);
    }

    if text.contains("manifest conflict")
        || text.contains("ownership conflict")
        || text.contains("topology conflict")
    {
        return Some(FailureClass::DesignConflict);
    }

    // ── Catch-all patterns ───────────────────────────────────────────────────
    if code == "PLAN_REVIEW_FAILED" || text.contains("plan validation failed") {
        return Some(FailureClass::PlanInvalid);
    }

    if text.contains("executor plan validation") && text.contains("failed") {
        return Some(FailureClass::PlanInvalid);
    }

    // No deterministic rule matched — caller invokes residual AI classifier.
    None
}

/// Route a classified failure to the appropriate repair action.
#[derive(Debug, Clone, PartialEq)]
pub enum FailureRoutingDecision {
    RetryDesign,
    RetryArtifact,
    RetryPlanCompile,
    RetryExecutor,
    EscalateToOperator,
}

impl FailureRoutingDecision {
    /// Human-readable message for the operator explaining the routing decision.
    pub fn operator_message(&self, failure_class: &FailureClass) -> String {
        let class_label = format!("{failure_class:?}");
        match self {
            FailureRoutingDecision::RetryDesign => format!(
                "The design phase will be retried automatically. \
                Failure class: {class_label}. \
                The designer will receive the audit feedback and produce a revised manifest."
            ),
            FailureRoutingDecision::RetryArtifact => format!(
                "Artifact generation will be retried with a repair packet. \
                Failure class: {class_label}. \
                The programmer will receive the audit findings and correct the artifact."
            ),
            FailureRoutingDecision::RetryPlanCompile => format!(
                "Plan compilation will be retried automatically. \
                Failure class: {class_label}. \
                The plan compiler will attempt to produce a valid executor plan."
            ),
            FailureRoutingDecision::RetryExecutor => format!(
                "Execution will be retried automatically. \
                Failure class: {class_label}. \
                A transient timeout or environment issue was detected."
            ),
            FailureRoutingDecision::EscalateToOperator => format!(
                "Operator action required — the pipeline cannot proceed automatically. \
                Failure class: {class_label}. \
                Review the error above and choose an action from operator_options."
            ),
        }
    }

    /// Structured operator action options — non-empty only for EscalateToOperator.
    pub fn operator_options(&self) -> &'static [&'static str] {
        match self {
            FailureRoutingDecision::EscalateToOperator => &[
                "fix_manifest_and_retry",
                "restart_pipeline_from_design",
                "cancel_pipeline",
            ],
            _ => &[],
        }
    }
}

pub fn route_failure(
    class: &FailureClass,
    design_iterations_used: usize,
    artifact_attempts_used: u32,
    plan_compile_attempts_used: u32,
) -> FailureRoutingDecision {
    use super::pipeline_types::{MAX_ARTIFACT_ATTEMPTS, MAX_DESIGN_ITERATIONS};

    match class {
        FailureClass::DesignIncomplete | FailureClass::DesignConflict => {
            if design_iterations_used < MAX_DESIGN_ITERATIONS {
                FailureRoutingDecision::RetryDesign
            } else {
                FailureRoutingDecision::EscalateToOperator
            }
        }

        FailureClass::ArtifactContractInvalid | FailureClass::ArtifactLayoutInvalid => {
            if artifact_attempts_used < MAX_ARTIFACT_ATTEMPTS {
                FailureRoutingDecision::RetryArtifact
            } else {
                FailureRoutingDecision::EscalateToOperator
            }
        }

        FailureClass::ArtifactTaskUnderspecified => {
            // Cannot retry with same packet — packet construction is the problem.
            FailureRoutingDecision::EscalateToOperator
        }

        FailureClass::PlanInvalid | FailureClass::PlanContractInvalid => {
            if plan_compile_attempts_used < 2 {
                FailureRoutingDecision::RetryPlanCompile
            } else {
                FailureRoutingDecision::EscalateToOperator
            }
        }

        FailureClass::ExecutionEnvironmentMissing => {
            // May mean a missing artifact — escalate for operator to decide
            // whether to rebuild the artifact or fix the environment.
            FailureRoutingDecision::EscalateToOperator
        }

        FailureClass::ExecutionTimeout => FailureRoutingDecision::RetryExecutor,

        FailureClass::ExecutionActionFailed => FailureRoutingDecision::EscalateToOperator,

        FailureClass::SnapshotPartialBlocking
        | FailureClass::SnapshotSectionUnsupported
        | FailureClass::DeltaUnsupported
        | FailureClass::UnknownResidual => FailureRoutingDecision::EscalateToOperator,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline_types::PipelineStage;

    fn ctx(stage: PipelineStage) -> FailureContext {
        FailureContext {
            stage,
            error_code: None,
            resource_type: None,
        }
    }

    fn ctx_code(stage: PipelineStage, code: &str) -> FailureContext {
        FailureContext {
            stage,
            error_code: Some(code.to_string()),
            resource_type: None,
        }
    }

    #[test]
    fn runtime_not_materialized_maps_to_env_missing() {
        let result = classify_failure_deterministic(
            "runtime not materialized on worker-220",
            &ctx(PipelineStage::Execute),
        );
        assert_eq!(result, Some(FailureClass::ExecutionEnvironmentMissing));
    }

    #[test]
    fn unknown_action_maps_to_plan_invalid() {
        let result = classify_failure_deterministic(
            "unknown action: publish_package",
            &ctx(PipelineStage::PlanValidation),
        );
        assert_eq!(result, Some(FailureClass::PlanInvalid));
    }

    #[test]
    fn missing_required_field_at_plan_stage_maps_to_plan_contract() {
        let result = classify_failure_deterministic(
            "missing required field 'hive'",
            &ctx(PipelineStage::PlanValidation),
        );
        assert_eq!(result, Some(FailureClass::PlanContractInvalid));
    }

    #[test]
    fn missing_required_field_at_artifact_stage_maps_to_artifact_contract() {
        let result = classify_failure_deterministic(
            "missing required field 'runtime_base'",
            &ctx(PipelineStage::ArtifactLoop),
        );
        assert_eq!(result, Some(FailureClass::ArtifactContractInvalid));
    }

    #[test]
    fn package_missing_required_file_maps_to_artifact_layout() {
        let result = classify_failure_deterministic(
            "package missing required file: package.json",
            &ctx(PipelineStage::ArtifactLoop),
        );
        assert_eq!(result, Some(FailureClass::ArtifactLayoutInvalid));
    }

    #[test]
    fn snapshot_partial_blocking_code_maps_correctly() {
        let result = classify_failure_deterministic(
            "hive worker-220 unreachable",
            &ctx_code(PipelineStage::Reconcile, "SNAPSHOT_PARTIAL_BLOCKING"),
        );
        assert_eq!(result, Some(FailureClass::SnapshotPartialBlocking));
    }

    #[test]
    fn timeout_code_maps_to_execution_timeout() {
        let result = classify_failure_deterministic(
            "timed out waiting for response",
            &ctx(PipelineStage::Execute),
        );
        assert_eq!(result, Some(FailureClass::ExecutionTimeout));
    }

    #[test]
    fn unmatched_error_returns_none() {
        let result = classify_failure_deterministic(
            "some completely unknown error XYZ",
            &ctx(PipelineStage::Execute),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn routing_design_within_limit_retries_design() {
        let decision = route_failure(&FailureClass::DesignIncomplete, 1, 0, 0);
        assert_eq!(decision, FailureRoutingDecision::RetryDesign);
    }

    #[test]
    fn routing_design_at_limit_escalates() {
        let decision = route_failure(&FailureClass::DesignIncomplete, 3, 0, 0);
        assert_eq!(decision, FailureRoutingDecision::EscalateToOperator);
    }

    #[test]
    fn routing_artifact_within_limit_retries_artifact() {
        let decision = route_failure(&FailureClass::ArtifactLayoutInvalid, 0, 1, 0);
        assert_eq!(decision, FailureRoutingDecision::RetryArtifact);
    }

    #[test]
    fn routing_artifact_at_limit_escalates() {
        let decision = route_failure(&FailureClass::ArtifactLayoutInvalid, 0, 3, 0);
        assert_eq!(decision, FailureRoutingDecision::EscalateToOperator);
    }

    #[test]
    fn routing_timeout_retries_executor() {
        let decision = route_failure(&FailureClass::ExecutionTimeout, 0, 0, 0);
        assert_eq!(decision, FailureRoutingDecision::RetryExecutor);
    }

    #[test]
    fn routing_unknown_residual_escalates() {
        let decision = route_failure(&FailureClass::UnknownResidual, 0, 0, 0);
        assert_eq!(decision, FailureRoutingDecision::EscalateToOperator);
    }
}
