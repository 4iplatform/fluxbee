package node

import (
	"fmt"
	"regexp"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

// workflowNamePattern is the runtime naming policy (runtime_name_regex, src/runtime_package.rs),
// since a workflow's runtime is wf.<workflow_name>: lowercase letters and digits, in parts joined by
// dots, the first starting with a letter. It allowed hyphens before 0.1.98, which every check and
// compile passed and the package's publish refused (PACKAGE_PUBLISH_FAILED), after the slots had
// rotated (WF Case 5's gate).
var workflowNamePattern = regexp.MustCompile(`^[a-z][a-z0-9]*(\.[a-z0-9]+)*$`)

// invalidWorkflowName refuses a workflow name, saying the rule.
func invalidWorkflowName(name string) WfRulesError {
	return WfRulesError{Code: "INVALID_WORKFLOW_NAME", Detail: fmt.Sprintf("invalid workflow_name %q: lowercase letters and digits, in parts joined by dots, the first starting with a letter, no hyphens (its runtime is wf.<workflow_name>)", name)}
}

type ClockFunc = wfcel.ClockFunc

type CompileRequest struct {
	WorkflowName string `json:"workflow_name"`
	Definition   any    `json:"definition"`
	Version      uint64 `json:"version,omitempty"`
	AutoSpawn    bool   `json:"auto_spawn,omitempty"`
	TenantID     string `json:"tenant_id,omitempty"`
	// LaunchedBy is who asked for the deploy, stamped by SY.admin (operator, archi).
	LaunchedBy string `json:"launched_by,omitempty"`
}

type GetWorkflowRequest struct {
	WorkflowName string `json:"workflow_name"`
}

type GetStatusRequest struct {
	WorkflowName string `json:"workflow_name"`
}

type DeleteWorkflowRequest struct {
	WorkflowName string `json:"workflow_name"`
	Force        bool   `json:"force,omitempty"`
}

type WfRulesMetadata struct {
	Version         uint64 `json:"version"`
	Hash            string `json:"hash"`
	WorkflowName    string `json:"workflow_name"`
	WorkflowType    string `json:"workflow_type"`
	WFSchemaVersion string `json:"wf_schema_version"`
	CompiledAt      string `json:"compiled_at"`
	GuardCount      int    `json:"guard_count"`
	StateCount      int    `json:"state_count"`
	ActionCount     int    `json:"action_count"`
	ErrorDetail     string `json:"error_detail"`
}

type WfRulesError struct {
	Code   string
	Detail string
	// Retryable says that asking again, nothing changed first, may pass: the answer carries
	// retryable true (a change refused BUSY, FINDINGS A-307).
	Retryable bool
}

func (e WfRulesError) Error() string {
	if e.Detail == "" {
		return e.Code
	}
	return fmt.Sprintf("%s: %s", e.Code, e.Detail)
}
