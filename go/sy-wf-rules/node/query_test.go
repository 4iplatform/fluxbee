package node

import (
	"context"
	"encoding/json"
	"errors"
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

type fakeWFNodeClient struct {
	countRunningInstancesFunc func(nodeName string) (int, error)
}

func (f *fakeWFNodeClient) CountRunningInstances(_ context.Context, nodeName string) (int, error) {
	if f.countRunningInstancesFunc != nil {
		return f.countRunningInstancesFunc(nodeName)
	}
	return 0, nil
}

func TestGetWorkflowStatusReturnsNodeState(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	svc.orchestrator = &fakeOrchestratorClient{
		getNodeConfigFunc: func(_ context.Context, targetNode, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{
						"runtime":         "wf.invoice",
						"runtime_version": "1",
						"package_path":    "/var/lib/fluxbee/dist/runtimes/wf.invoice/1",
					},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			if nodeName != "WF.invoice@motherbee" {
				t.Fatalf("unexpected node %q", nodeName)
			}
			return 12, nil
		},
	}

	status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("GetWorkflowStatus: %v", err)
	}
	if status.CurrentVersion == nil || *status.CurrentVersion != 1 {
		t.Fatalf("unexpected current version %#v", status.CurrentVersion)
	}
	if status.WFNode.ActiveInstances == nil || *status.WFNode.ActiveInstances != 12 {
		t.Fatalf("unexpected active instances %#v", status.WFNode.ActiveInstances)
	}
	if !status.WFNode.Running || status.WFNode.RuntimeVersion != "1" {
		t.Fatalf("unexpected wf node snapshot %#v", status.WFNode)
	}
}

func TestGetWorkflowStatusTreatsWFTimeoutAsNonFatal(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	svc.orchestrator = &fakeOrchestratorClient{
		getNodeConfigFunc: func(_ context.Context, targetNode, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{
						"runtime_version": "1",
					},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 0, errors.New("timeout")
		},
	}

	status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("GetWorkflowStatus: %v", err)
	}
	if !status.WFNode.Timeout || status.WFNode.Running || status.WFNode.ActiveInstances != nil {
		t.Fatalf("unexpected wf node snapshot %#v", status.WFNode)
	}
}

func TestGetWorkflowStatusTreatsOrchestratorTimeoutAsNonFatal(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	svc.orchestrator = &fakeOrchestratorClient{
		getNodeConfigFunc: func(_ context.Context, targetNode, nodeName string) (map[string]any, error) {
			return nil, context.DeadlineExceeded
		},
	}

	status, err := svc.GetWorkflowStatus(GetStatusRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("GetWorkflowStatus: %v", err)
	}
	if status.CurrentVersion == nil || *status.CurrentVersion != 1 {
		t.Fatalf("unexpected current version %#v", status.CurrentVersion)
	}
	if !status.WFNode.Timeout || status.WFNode.Running || status.WFNode.ConfigExists {
		t.Fatalf("unexpected wf node snapshot %#v", status.WFNode)
	}
}

func TestListWorkflowStatusesReturnsStructuredItems(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	svc.orchestrator = &fakeOrchestratorClient{
		getNodeConfigFunc: func(_ context.Context, targetNode, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{
						"runtime_version": "1",
					},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 3, nil
		},
	}

	items, err := svc.ListWorkflowStatuses()
	if err != nil {
		t.Fatalf("ListWorkflowStatuses: %v", err)
	}
	if len(items) != 1 {
		t.Fatalf("unexpected item count %d", len(items))
	}
	if items[0].WorkflowName != "invoice" || !items[0].WFNodeRunning {
		t.Fatalf("unexpected item %#v", items[0])
	}
	if items[0].ActiveInstances == nil || *items[0].ActiveInstances != 3 {
		t.Fatalf("unexpected active instances %#v", items[0].ActiveInstances)
	}
}
