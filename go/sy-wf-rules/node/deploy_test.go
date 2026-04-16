package node

import (
	"context"
	"fmt"
	"testing"
)

type fakeOrchestratorClient struct {
	getNodeConfigFunc func(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	runNodeFunc       func(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error)
	startNodeFunc     func(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	restartNodeFunc   func(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	setNodeConfigFunc func(ctx context.Context, targetNode, nodeName string, config map[string]any, notify bool) (map[string]any, error)
	killNodeFunc      func(ctx context.Context, targetNode, nodeName string, force, purgeInstance bool) (map[string]any, error)
	runCalls          int
	restartCalls      int
	setConfigCalls    int
}

func (f *fakeOrchestratorClient) GetNodeConfig(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
	if f.getNodeConfigFunc != nil {
		return f.getNodeConfigFunc(ctx, targetNode, nodeName)
	}
	return nil, &orchestratorActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
}

func (f *fakeOrchestratorClient) RunNode(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error) {
	f.runCalls++
	if f.runNodeFunc != nil {
		return f.runNodeFunc(ctx, targetNode, nodeName, runtimeName, version, config)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeOrchestratorClient) StartNode(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
	if f.startNodeFunc != nil {
		return f.startNodeFunc(ctx, targetNode, nodeName)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeOrchestratorClient) RestartNode(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
	f.restartCalls++
	if f.restartNodeFunc != nil {
		return f.restartNodeFunc(ctx, targetNode, nodeName)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeOrchestratorClient) SetNodeConfig(ctx context.Context, targetNode, nodeName string, config map[string]any, notify bool) (map[string]any, error) {
	f.setConfigCalls++
	if f.setNodeConfigFunc != nil {
		return f.setNodeConfigFunc(ctx, targetNode, nodeName, config, notify)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeOrchestratorClient) KillNode(ctx context.Context, targetNode, nodeName string, force, purgeInstance bool) (map[string]any, error) {
	if f.killNodeFunc != nil {
		return f.killNodeFunc(ctx, targetNode, nodeName, force, purgeInstance)
	}
	return map[string]any{"status": "ok"}, nil
}

func TestApplyWorkflowAndDeployAutoSpawnFirstDeploy(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return nil, &orchestratorActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	fake.runNodeFunc = func(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error) {
		if targetNode != "SY.orchestrator@motherbee" {
			t.Fatalf("unexpected target %q", targetNode)
		}
		if nodeName != "WF.invoice@motherbee" {
			t.Fatalf("unexpected nodeName %q", nodeName)
		}
		if runtimeName != "wf.invoice" || version != "1" {
			t.Fatalf("unexpected runtime bind %q %q", runtimeName, version)
		}
		return map[string]any{"status": "ok"}, nil
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{
		WorkflowName: "invoice",
		AutoSpawn:    true,
	})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if result.WFNode.Action != "restarted" || result.WFNode.Status != "ok" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if fake.runCalls != 1 {
		t.Fatalf("expected one run_node call, got %d", fake.runCalls)
	}
}

func TestApplyWorkflowAndDeployPublishesOnlyWhenAutoSpawnDisabled(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return nil, &orchestratorActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{
		WorkflowName: "invoice",
		AutoSpawn:    false,
	})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if result.WFNode.Action != "none" || result.WFNode.Reason != "auto_spawn disabled" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if fake.runCalls != 0 {
		t.Fatalf("expected no run_node calls, got %d", fake.runCalls)
	}
}

func TestApplyWorkflowAndDeployExistingNodeLeavesDeploymentDeferred(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	var gotPatch map[string]any
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
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
	}
	fake.setNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string, config map[string]any, notify bool) (map[string]any, error) {
		gotPatch = config
		if notify {
			t.Fatalf("expected notify=false")
		}
		return map[string]any{"status": "ok"}, nil
	}
	fake.restartNodeFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		if targetNode != "SY.orchestrator@motherbee" {
			t.Fatalf("unexpected target %q", targetNode)
		}
		if nodeName != "WF.invoice@motherbee" {
			t.Fatalf("unexpected nodeName %q", nodeName)
		}
		return map[string]any{"status": "ok"}, nil
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{
		WorkflowName: "invoice",
		AutoSpawn:    true,
	})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if result.WFNode.Action != "restarted" || result.WFNode.Status != "ok" {
		t.Fatalf("unexpected action %#v", result.WFNode)
	}
	if result.Warning != "" {
		t.Fatalf("unexpected warning %q", result.Warning)
	}
	if fake.runCalls != 0 {
		t.Fatalf("expected no run_node call, got %d", fake.runCalls)
	}
	if fake.setConfigCalls != 1 || fake.restartCalls != 1 {
		t.Fatalf("expected one set_config and one restart, got set=%d restart=%d", fake.setConfigCalls, fake.restartCalls)
	}
	system, _ := gotPatch["_system"].(map[string]any)
	if system["runtime"] != "wf.invoice" || system["runtime_version"] != "1" {
		t.Fatalf("unexpected _system patch %#v", system)
	}
}

func TestApplyWorkflowAndDeploySurfacesSpawnFailureAsPartial(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return nil, &orchestratorActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	fake.runNodeFunc = func(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error) {
		return nil, fmt.Errorf("SPAWN_FAILED: boom")
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{
		WorkflowName: "invoice",
		AutoSpawn:    true,
	})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if result.WFNode.Action != "restart_failed" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if result.Warning == "" {
		t.Fatalf("expected warning")
	}
}

func TestApplyWorkflowAndDeployExistingNodeRestartFailureIsPartial(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return map[string]any{
			"status": "ok",
			"config": map[string]any{
				"_system": map[string]any{
					"runtime":         "wf.invoice",
					"runtime_version": "1",
				},
			},
		}, nil
	}
	fake.restartNodeFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return nil, fmt.Errorf("RESTART_FAILED: boom")
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{
		WorkflowName: "invoice",
		AutoSpawn:    true,
	})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if result.WFNode.Action != "restart_failed" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if result.Warning == "" {
		t.Fatalf("expected warning")
	}
	if fake.restartCalls != 2 {
		t.Fatalf("expected one retry, got %d restart calls", fake.restartCalls)
	}
}
