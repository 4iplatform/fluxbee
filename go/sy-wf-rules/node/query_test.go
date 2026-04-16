package node

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestGetWorkflowReturnsCurrentDefinition(t *testing.T) {
	svc := newTestService(t)
	meta, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	})
	if err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice", Version: meta.Version}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	workflow, err := svc.GetWorkflow(GetWorkflowRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("GetWorkflow: %v", err)
	}
	if workflow.Metadata.Version != 1 {
		t.Fatalf("unexpected version %d", workflow.Metadata.Version)
	}
	var decoded map[string]any
	if err := json.Unmarshal(workflow.Definition, &decoded); err != nil {
		t.Fatalf("definition should be valid JSON: %v", err)
	}
	if decoded["workflow_type"] != "invoice" {
		t.Fatalf("unexpected workflow_type %#v", decoded["workflow_type"])
	}
}

func TestGetWorkflowRequiresCurrentDefinition(t *testing.T) {
	svc := newTestService(t)
	_, err := svc.GetWorkflow(GetWorkflowRequest{WorkflowName: "invoice"})
	if err == nil || !strings.Contains(err.Error(), "WORKFLOW_NOT_FOUND") {
		t.Fatalf("expected WORKFLOW_NOT_FOUND, got %v", err)
	}
}
