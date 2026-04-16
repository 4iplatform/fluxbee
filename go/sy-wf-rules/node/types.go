package node

import (
	"encoding/json"
	"fmt"
	"regexp"

	wfcel "github.com/4iplatform/json-router/pkg/wfcel"
)

var workflowNamePattern = regexp.MustCompile(`^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)*$`)

type ClockFunc = wfcel.ClockFunc

type CompileRequest struct {
	WorkflowName string `json:"workflow_name"`
	Definition   any    `json:"definition"`
	Version      uint64 `json:"version,omitempty"`
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
}

func (e WfRulesError) Error() string {
	if e.Detail == "" {
		return e.Code
	}
	return fmt.Sprintf("%s: %s", e.Code, e.Detail)
}

func mustMarshalJSON(value any) json.RawMessage {
	raw, _ := json.Marshal(value)
	return raw
}
