package node

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestDeleteWorkflowRejectsUnknownInstancesWhenWFDoesNotRespond(t *testing.T) {
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
					"_system": map[string]any{"runtime_version": "1"},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 0, context.DeadlineExceeded
		},
	}

	_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: false})
	if err == nil || !strings.Contains(err.Error(), "INSTANCES_UNKNOWN") {
		t.Fatalf("expected INSTANCES_UNKNOWN, got %v", err)
	}
}

func TestDeleteWorkflowRejectsActiveInstances(t *testing.T) {
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
					"_system": map[string]any{"runtime_version": "1"},
				},
			}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 2, nil
		},
	}

	_, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: false})
	if err == nil || !strings.Contains(err.Error(), "INSTANCES_ACTIVE") {
		t.Fatalf("expected INSTANCES_ACTIVE, got %v", err)
	}
}

func TestDeleteWorkflowForceRemovesStateAndPackages(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	killed := false
	svc.orchestrator = &fakeOrchestratorClient{
		getNodeConfigFunc: func(_ context.Context, targetNode, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{"runtime_version": "1"},
				},
			}, nil
		},
		killNodeFunc: func(_ context.Context, targetNode, nodeName string, force, purgeInstance bool) (map[string]any, error) {
			killed = true
			if !force || !purgeInstance {
				t.Fatalf("expected force=true purge_instance=true")
			}
			return map[string]any{"status": "ok"}, nil
		},
	}
	svc.wfNodes = &fakeWFNodeClient{
		countRunningInstancesFunc: func(nodeName string) (int, error) {
			return 5, nil
		},
	}

	result, err := svc.DeleteWorkflow(DeleteWorkflowRequest{WorkflowName: "invoice", Force: true})
	if err != nil {
		t.Fatalf("DeleteWorkflow: %v", err)
	}
	if !result.Deleted || !killed {
		t.Fatalf("unexpected result %#v killed=%v", result, killed)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice")); !os.IsNotExist(err) {
		t.Fatalf("expected local state removed, err=%v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice")); !os.IsNotExist(err) {
		t.Fatalf("expected runtime dir removed, err=%v", err)
	}
}
