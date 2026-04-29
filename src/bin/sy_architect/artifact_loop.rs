// Artifact loop — Track C.
// Source spec: fluxbee_rearchitecture_master_spec_v4.md — section 9
//
// Pure deterministic auditor + loop controller.
// The RealProgrammerTool (AI call) is in sy_architect.rs because it needs
// ArchitectAdminToolContext. This module contains: storage helpers, the
// deterministic ArtifactAuditor, and the loop controller logic.

use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::pipeline_types::{
    ArtifactAuditVerdict, ArtifactBundle, AuditFinding, AuditSeverity, AuditStatus,
    BuildTaskPacket, FailureClass, RepairHint, RepairPacket,
};

// ── TC-8: Artifact storage ────────────────────────────────────────────────

pub fn artifact_bundle_dir(state_dir: &Path, run_id: &str, bundle_id: &str) -> PathBuf {
    state_dir
        .join("pipeline")
        .join(run_id)
        .join("artifacts")
        .join(bundle_id)
}

pub fn task_packet_path(state_dir: &Path, run_id: &str, task_id: &str) -> PathBuf {
    state_dir
        .join("pipeline")
        .join(run_id)
        .join("task_packets")
        .join(format!("{task_id}.json"))
}

pub fn repair_packet_path(state_dir: &Path, run_id: &str, repair_id: &str) -> PathBuf {
    state_dir
        .join("pipeline")
        .join(run_id)
        .join("repairs")
        .join(format!("{repair_id}.json"))
}

pub fn save_task_packet(
    state_dir: &Path,
    run_id: &str,
    packet: &BuildTaskPacket,
) -> Result<PathBuf, String> {
    let path = task_packet_path(state_dir, run_id, &packet.task_id);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(packet).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn save_artifact_bundle(
    state_dir: &Path,
    run_id: &str,
    bundle: &ArtifactBundle,
) -> Result<PathBuf, String> {
    let dir = artifact_bundle_dir(state_dir, run_id, &bundle.bundle_id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let meta_path = dir.join("bundle.json");
    let json = serde_json::to_string_pretty(bundle).map_err(|e| e.to_string())?;
    std::fs::write(&meta_path, json).map_err(|e| e.to_string())?;
    Ok(meta_path)
}

pub fn load_artifact_bundle(
    state_dir: &Path,
    run_id: &str,
    bundle_id: &str,
) -> Option<ArtifactBundle> {
    let path = artifact_bundle_dir(state_dir, run_id, bundle_id).join("bundle.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn save_repair_packet(
    state_dir: &Path,
    run_id: &str,
    packet: &RepairPacket,
) -> Result<PathBuf, String> {
    let path = repair_packet_path(state_dir, run_id, &packet.repair_id);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(packet).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Approved artifact registry — JSON file linking delta op_ids to approved bundles.
pub fn save_approved_registry(
    state_dir: &Path,
    run_id: &str,
    entries: &[ApprovedArtifact],
) -> Result<(), String> {
    let path = state_dir
        .join("pipeline")
        .join(run_id)
        .join("approved_artifacts.json");
    let json = serde_json::to_string_pretty(entries).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ApprovedArtifact {
    pub bundle_id: String,
    pub task_id: String,
    pub source_delta_op: String,
    pub artifact_kind: String,
    pub content_digest: String,
    pub path: String,
}

// ── TC-6: ArtifactAuditor — deterministic structural validator ────────────
//
// SCOPE BOUNDARY (mandatory):
// ArtifactAuditor validates structural and contract-level correctness only.
// It does NOT validate:
//   - semantic quality of prompts (system.txt content)
//   - business correctness of configs or workflow logic
//   - normative correctness of Rego logic
//   - whether generated text is "good"
// If an artifact passes the auditor it is structurally valid — not necessarily
// correct for its intended purpose.

pub fn audit_artifact(packet: &BuildTaskPacket, bundle: &ArtifactBundle) -> ArtifactAuditVerdict {
    let mut findings: Vec<AuditFinding> = Vec::new();

    match bundle.artifact_kind.as_str() {
        "runtime_package" => audit_runtime_package(packet, bundle, &mut findings),
        "workflow_definition" => audit_workflow_definition(packet, bundle, &mut findings),
        "opa_bundle" => audit_opa_bundle(packet, bundle, &mut findings),
        "config_bundle" => audit_config_bundle(packet, bundle, &mut findings),
        other => {
            findings.push(AuditFinding {
                code: "UNSUPPORTED_ARTIFACT_KIND".to_string(),
                field: Some("artifact_kind".to_string()),
                message: format!(
                    "artifact_kind '{other}' is not supported by this auditor version"
                ),
                severity: AuditSeverity::Error,
            });
        }
    }

    let blocking_issues: Vec<String> = findings
        .iter()
        .filter(|f| f.severity == AuditSeverity::Error)
        .map(|f| f.code.clone())
        .collect();

    let repair_hints = build_repair_hints(&findings, packet);

    let has_errors = !blocking_issues.is_empty();
    let is_unrecoverable = findings.iter().any(|f| {
        matches!(
            f.code.as_str(),
            "UNSUPPORTED_ARTIFACT_KIND" | "ARTIFACT_TASK_UNDERSPECIFIED"
        )
    });

    let status = if !has_errors {
        AuditStatus::Approved
    } else if is_unrecoverable {
        AuditStatus::Rejected
    } else {
        AuditStatus::Repairable
    };

    let failure_class = if has_errors {
        if findings
            .iter()
            .any(|f| f.code.starts_with("MISSING_FILE") || f.code == "INVALID_LAYOUT")
        {
            Some(FailureClass::ArtifactLayoutInvalid)
        } else {
            Some(FailureClass::ArtifactContractInvalid)
        }
    } else {
        None
    };

    ArtifactAuditVerdict {
        verdict_id: format!("verd-{}", Uuid::new_v4().simple()),
        source_bundle_id: bundle.bundle_id.clone(),
        source_task_id: bundle.source_task_id.clone(),
        status,
        failure_class,
        findings,
        blocking_issues,
        repair_hints,
    }
}

fn audit_runtime_package(
    packet: &BuildTaskPacket,
    bundle: &ArtifactBundle,
    findings: &mut Vec<AuditFinding>,
) {
    let files = bundle.artifact.get("files").and_then(|f| f.as_object());

    // Required files
    for required in &["package.json", "config/default-config.json"] {
        let present = files.map(|f| f.contains_key(*required)).unwrap_or(false);
        if !present {
            findings.push(AuditFinding {
                code: "MISSING_FILE".to_string(),
                field: Some(required.to_string()),
                message: format!("required file '{required}' is absent from the artifact bundle"),
                severity: AuditSeverity::Error,
            });
        }
    }

    // package.json structure
    if let Some(pkg_json_str) = files
        .and_then(|f| f.get("package.json"))
        .and_then(|v| v.as_str())
    {
        match serde_json::from_str::<serde_json::Value>(pkg_json_str) {
            Ok(pkg) => {
                // Required fields in package.json
                for field in &["name", "version", "type", "runtime_base"] {
                    if pkg.get(*field).is_none() {
                        findings.push(AuditFinding {
                            code: "MISSING_PACKAGE_FIELD".to_string(),
                            field: Some(field.to_string()),
                            message: format!("package.json is missing required field '{field}'"),
                            severity: AuditSeverity::Error,
                        });
                    }
                }

                // runtime_base must be in known context
                if let Some(runtime_base) = pkg.get("runtime_base").and_then(|v| v.as_str()) {
                    let known = &packet.known_context.available_runtimes;
                    if !known.is_empty() && !known.iter().any(|r| r == runtime_base) {
                        findings.push(AuditFinding {
                            code: "INVALID_RUNTIME_BASE".to_string(),
                            field: Some("runtime_base".to_string()),
                            message: format!(
                                "runtime_base '{runtime_base}' is not in known available runtimes: {}",
                                known.join(", ")
                            ),
                            severity: AuditSeverity::Error,
                        });
                    }
                }

                // Name convention: must be lowercase with dots or hyphens
                if let Some(name) = pkg.get("name").and_then(|v| v.as_str()) {
                    if name != name.to_lowercase() || name.contains(' ') {
                        findings.push(AuditFinding {
                            code: "INVALID_RUNTIME_NAME".to_string(),
                            field: Some("name".to_string()),
                            message: "runtime name must be lowercase without spaces".to_string(),
                            severity: AuditSeverity::Error,
                        });
                    }
                }
            }
            Err(e) => {
                findings.push(AuditFinding {
                    code: "INVALID_JSON".to_string(),
                    field: Some("package.json".to_string()),
                    message: format!("package.json is not valid JSON: {e}"),
                    severity: AuditSeverity::Error,
                });
            }
        }
    }
}

fn audit_workflow_definition(
    _packet: &BuildTaskPacket,
    bundle: &ArtifactBundle,
    findings: &mut Vec<AuditFinding>,
) {
    let def = &bundle.artifact;

    for field in &["workflow_name", "steps"] {
        if def.get(*field).is_none() {
            findings.push(AuditFinding {
                code: "MISSING_WF_FIELD".to_string(),
                field: Some(field.to_string()),
                message: format!("workflow definition is missing required field '{field}'"),
                severity: AuditSeverity::Error,
            });
        }
    }

    // Check steps is a non-empty array
    if let Some(steps) = def.get("steps").and_then(|v| v.as_array()) {
        if steps.is_empty() {
            findings.push(AuditFinding {
                code: "EMPTY_STEPS".to_string(),
                field: Some("steps".to_string()),
                message: "workflow definition has an empty steps array".to_string(),
                severity: AuditSeverity::Error,
            });
        }
    }
}

fn audit_opa_bundle(
    _packet: &BuildTaskPacket,
    bundle: &ArtifactBundle,
    findings: &mut Vec<AuditFinding>,
) {
    // OPA bundle must have a rego_source field
    let rego = bundle.artifact.get("rego_source").and_then(|v| v.as_str());
    match rego {
        None => {
            findings.push(AuditFinding {
                code: "MISSING_REGO_SOURCE".to_string(),
                field: Some("rego_source".to_string()),
                message: "OPA bundle must contain a 'rego_source' field with valid Rego"
                    .to_string(),
                severity: AuditSeverity::Error,
            });
        }
        Some(src) => {
            // Basic Rego structural check: must have a package declaration
            if !src.contains("package ") {
                findings.push(AuditFinding {
                    code: "MISSING_REGO_PACKAGE".to_string(),
                    field: Some("rego_source".to_string()),
                    message: "Rego source must contain a 'package' declaration".to_string(),
                    severity: AuditSeverity::Error,
                });
            }
        }
    }
}

fn audit_config_bundle(
    packet: &BuildTaskPacket,
    bundle: &ArtifactBundle,
    findings: &mut Vec<AuditFinding>,
) {
    // Config bundle must have all files listed in packet.requirements.required_files
    let required_files: Vec<String> = packet
        .requirements
        .get("required_files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let files = bundle.artifact.get("files").and_then(|f| f.as_object());
    for req in &required_files {
        let present = files.map(|f| f.contains_key(req.as_str())).unwrap_or(false);
        if !present {
            findings.push(AuditFinding {
                code: "MISSING_FILE".to_string(),
                field: Some(req.clone()),
                message: format!("required config file '{req}' is absent"),
                severity: AuditSeverity::Error,
            });
        }
    }
}

fn build_repair_hints(findings: &[AuditFinding], packet: &BuildTaskPacket) -> Vec<RepairHint> {
    findings
        .iter()
        .filter_map(|f| match f.code.as_str() {
            "MISSING_FILE" => Some(RepairHint {
                finding_code: f.code.clone(),
                instruction: format!(
                    "Add the missing file '{}' to the artifact bundle",
                    f.field.as_deref().unwrap_or("?")
                ),
            }),
            "INVALID_RUNTIME_BASE" => Some(RepairHint {
                finding_code: f.code.clone(),
                instruction: format!(
                    "Set runtime_base to one of the available runtimes: {}",
                    packet.known_context.available_runtimes.join(", ")
                ),
            }),
            "MISSING_PACKAGE_FIELD" => Some(RepairHint {
                finding_code: f.code.clone(),
                instruction: format!(
                    "Add the required field '{}' to package.json",
                    f.field.as_deref().unwrap_or("?")
                ),
            }),
            "INVALID_RUNTIME_NAME" => Some(RepairHint {
                finding_code: f.code.clone(),
                instruction: "Use lowercase runtime name with no spaces (dots and hyphens allowed)"
                    .to_string(),
            }),
            "MISSING_REGO_PACKAGE" => Some(RepairHint {
                finding_code: f.code.clone(),
                instruction: "Add 'package <name>' declaration at the top of the Rego source"
                    .to_string(),
            }),
            _ => None,
        })
        .collect()
}

// ── TC-7: Artifact loop controller ────────────────────────────────────────

pub struct ArtifactLoopResult {
    pub failed_task_id: Option<String>,
    pub failure_class: Option<FailureClass>,
    pub error: Option<String>,
    pub tokens_used: u32,
}

/// Failure signature for detecting repeated failures (stop early rule).
#[cfg(test)]
fn failure_signature(verdict: &ArtifactAuditVerdict) -> String {
    let class = verdict
        .failure_class
        .as_ref()
        .map(|c| format!("{c:?}"))
        .unwrap_or_else(|| "none".to_string());
    let codes = verdict.blocking_issues.join(",");
    format!("{class}:{codes}")
}

/// Build a RepairPacket from a failed verdict for the next attempt.
pub fn build_repair_packet(
    verdict: &ArtifactAuditVerdict,
    bundle: &ArtifactBundle,
    attempt: u32,
) -> RepairPacket {
    let required_corrections: Vec<String> = verdict
        .repair_hints
        .iter()
        .map(|h| h.instruction.clone())
        .collect();

    let do_not_repeat: Vec<String> = verdict
        .findings
        .iter()
        .filter(|f| f.severity == AuditSeverity::Error)
        .map(|f| format!("do not repeat: {}", f.message))
        .collect();

    RepairPacket {
        repair_packet_version: "0.1".to_string(),
        repair_id: format!("rep-{}", Uuid::new_v4().simple()),
        source_task_id: verdict.source_task_id.clone(),
        previous_bundle_id: bundle.bundle_id.clone(),
        previous_content_digest: bundle.content_digest.clone(),
        failure_class: verdict
            .failure_class
            .clone()
            .unwrap_or(FailureClass::ArtifactContractInvalid),
        required_corrections,
        do_not_repeat,
        must_preserve: vec![],
        retry_allowed: true,
        attempt,
    }
}

// ── TC-9: Trace event ──────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArtifactLoopTraceEvent {
    pub task_id: String,
    pub artifact_kind: String,
    pub attempt: u32,
    /// generating | auditing | approved | repairable | rejected | blocked
    pub status: String,
    pub verdict: Option<String>,
    pub failure_class: Option<String>,
    pub findings_count: u32,
    pub bundle_id: Option<String>,
    pub timestamp_ms: u64,
}

// ── Unit tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::pipeline_types::*;
    use super::*;
    use serde_json::json;

    fn runtime_packet(available_runtimes: Vec<&str>) -> BuildTaskPacket {
        BuildTaskPacket {
            task_packet_version: "0.1".to_string(),
            task_id: "art-test-001".to_string(),
            artifact_kind: "runtime_package".to_string(),
            source_delta_op: "op-001".to_string(),
            runtime_name: Some("ai.support.demo".to_string()),
            target_kind: "inline_package".to_string(),
            requirements: json!({}),
            constraints: json!({}),
            known_context: BuildTaskKnownContext {
                available_runtimes: available_runtimes.iter().map(|s| s.to_string()).collect(),
                running_nodes: vec![],
            },
            attempt: 0,
            max_attempts: 3,
            cookbook_context: vec![],
        }
    }

    fn valid_runtime_bundle() -> ArtifactBundle {
        ArtifactBundle {
            bundle_version: "0.1".to_string(),
            bundle_id: "bundle-test-001".to_string(),
            source_task_id: "art-test-001".to_string(),
            artifact_kind: "runtime_package".to_string(),
            status: ArtifactBundleStatus::Pending,
            summary: "test runtime package".to_string(),
            artifact: json!({
                "files": {
                    "package.json": r#"{"name":"ai.support.demo","version":"0.1.0","type":"config_only","runtime_base":"AI.common"}"#,
                    "config/default-config.json": r#"{"tenant_id":"tnt:demo"}"#
                }
            }),
            assumptions: vec![],
            verification_hints: vec![],
            content_digest: "sha256:test".to_string(),
            generated_at_ms: 0,
            generator_model: "test".to_string(),
        }
    }

    #[test]
    fn valid_runtime_package_passes_audit() {
        let packet = runtime_packet(vec!["AI.common", "AI.chat"]);
        let bundle = valid_runtime_bundle();
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(
            verdict.status,
            AuditStatus::Approved,
            "valid bundle should pass"
        );
        assert!(verdict.findings.is_empty(), "no findings expected");
    }

    #[test]
    fn missing_package_json_returns_repairable() {
        let packet = runtime_packet(vec!["AI.common"]);
        let mut bundle = valid_runtime_bundle();
        bundle.artifact = json!({ "files": { "config/default-config.json": "{}" } });
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(verdict.status, AuditStatus::Repairable);
        assert!(verdict.blocking_issues.iter().any(|i| i == "MISSING_FILE"));
        assert!(
            !verdict.repair_hints.is_empty(),
            "missing file should have repair hint"
        );
    }

    #[test]
    fn invalid_runtime_base_returns_repairable() {
        let packet = runtime_packet(vec!["AI.common"]);
        let mut bundle = valid_runtime_bundle();
        bundle.artifact = json!({
            "files": {
                "package.json": r#"{"name":"ai.support.demo","version":"0.1.0","type":"config_only","runtime_base":"AI.nonexistent"}"#,
                "config/default-config.json": "{}"
            }
        });
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(verdict.status, AuditStatus::Repairable);
        assert!(verdict
            .blocking_issues
            .iter()
            .any(|i| i == "INVALID_RUNTIME_BASE"));
        let hint = verdict
            .repair_hints
            .iter()
            .find(|h| h.finding_code == "INVALID_RUNTIME_BASE");
        assert!(
            hint.is_some(),
            "should have repair hint for invalid runtime_base"
        );
        assert!(hint.unwrap().instruction.contains("AI.common"));
    }

    #[test]
    fn unsupported_artifact_kind_returns_rejected() {
        let packet = runtime_packet(vec![]);
        let mut bundle = valid_runtime_bundle();
        bundle.artifact_kind = "unknown_kind".to_string();
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(
            verdict.status,
            AuditStatus::Rejected,
            "unknown kind must be rejected"
        );
    }

    #[test]
    fn invalid_package_json_syntax_returns_repairable() {
        let packet = runtime_packet(vec!["AI.common"]);
        let mut bundle = valid_runtime_bundle();
        bundle.artifact = json!({
            "files": {
                "package.json": "{ this is not valid json }",
                "config/default-config.json": "{}"
            }
        });
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(verdict.status, AuditStatus::Repairable);
        assert!(verdict.blocking_issues.iter().any(|i| i == "INVALID_JSON"));
    }

    #[test]
    fn workflow_missing_steps_returns_repairable() {
        let packet = BuildTaskPacket {
            artifact_kind: "workflow_definition".to_string(),
            ..runtime_packet(vec![])
        };
        let mut bundle = valid_runtime_bundle();
        bundle.artifact_kind = "workflow_definition".to_string();
        bundle.artifact = json!({ "workflow_name": "test-wf" }); // missing steps
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(verdict.status, AuditStatus::Repairable);
        assert!(verdict
            .blocking_issues
            .iter()
            .any(|i| i == "MISSING_WF_FIELD"));
    }

    #[test]
    fn opa_bundle_without_package_declaration_fails() {
        let packet = BuildTaskPacket {
            artifact_kind: "opa_bundle".to_string(),
            ..runtime_packet(vec![])
        };
        let mut bundle = valid_runtime_bundle();
        bundle.artifact_kind = "opa_bundle".to_string();
        bundle.artifact = json!({ "rego_source": "allow = true\ndefault allow = false" }); // no package decl
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(verdict.status, AuditStatus::Repairable);
        assert!(verdict
            .blocking_issues
            .iter()
            .any(|i| i == "MISSING_REGO_PACKAGE"));
    }

    #[test]
    fn opa_bundle_with_package_declaration_passes() {
        let packet = BuildTaskPacket {
            artifact_kind: "opa_bundle".to_string(),
            ..runtime_packet(vec![])
        };
        let mut bundle = valid_runtime_bundle();
        bundle.artifact_kind = "opa_bundle".to_string();
        bundle.artifact = json!({ "rego_source": "package fluxbee\nallow = true" });
        let verdict = audit_artifact(&packet, &bundle);
        assert_eq!(verdict.status, AuditStatus::Approved);
    }

    #[test]
    fn repair_packet_contains_correction_instructions() {
        let packet = runtime_packet(vec!["AI.common"]);
        let mut bundle = valid_runtime_bundle();
        bundle.artifact = json!({ "files": { "config/default-config.json": "{}" } }); // missing package.json
        let verdict = audit_artifact(&packet, &bundle);
        let repair = build_repair_packet(&verdict, &bundle, 1);
        assert!(!repair.required_corrections.is_empty());
        assert!(repair
            .required_corrections
            .iter()
            .any(|c| c.contains("package.json")));
        assert_eq!(repair.attempt, 1);
        assert!(repair.retry_allowed);
    }

    #[test]
    fn failure_signature_is_stable() {
        let packet = runtime_packet(vec!["AI.common"]);
        let mut bundle = valid_runtime_bundle();
        bundle.artifact = json!({ "files": {} });
        let v1 = audit_artifact(&packet, &bundle);
        let v2 = audit_artifact(&packet, &bundle);
        // Same inputs must yield same failure signature
        assert_eq!(
            failure_signature(&v1),
            failure_signature(&v2),
            "failure signature must be stable"
        );
    }
}
