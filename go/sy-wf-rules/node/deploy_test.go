package node

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

type fakeOrchestratorClient struct {
	getNodeConfigFunc func(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	runNodeFunc       func(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error)
	startNodeFunc     func(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	restartNodeFunc   func(ctx context.Context, targetNode, nodeName string) (map[string]any, error)
	setNodeConfigFunc func(ctx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error)
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

func (f *fakeOrchestratorClient) SetNodeConfig(ctx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error) {
	f.setConfigCalls++
	if f.setNodeConfigFunc != nil {
		return f.setNodeConfigFunc(ctx, targetNode, nodeName, config, binding, notify)
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
		if config["sy_timer_l2_name"] != "SY.timer@motherbee" {
			t.Fatalf("unexpected sy_timer_l2_name %#v", config["sy_timer_l2_name"])
		}
		if config["gc_retention_days"] != defaultWFGCRetentionDays {
			t.Fatalf("unexpected gc_retention_days %#v", config["gc_retention_days"])
		}
		if config["gc_interval_seconds"] != defaultWFGCIntervalSeconds {
			t.Fatalf("unexpected gc_interval_seconds %#v", config["gc_interval_seconds"])
		}
		if config["tenant_id"] != "tnt:request" {
			t.Fatalf("unexpected tenant_id %#v", config["tenant_id"])
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
		TenantID:     "tnt:request",
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

func TestApplyWorkflowAndDeployDoesNotFallbackToSpawnOnConfigLookupTimeout(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return nil, context.DeadlineExceeded
	}
	fake.runNodeFunc = func(ctx context.Context, targetNode, nodeName, runtimeName, version string, config map[string]any) (map[string]any, error) {
		t.Fatalf("RunNode must not be called after config lookup timeout")
		return nil, nil
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
	if result.WFNode.Action != "none" || result.WFNode.Reason != "orchestrator query failed" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if result.Warning == "" {
		t.Fatalf("expected warning")
	}
	if fake.runCalls != 0 {
		t.Fatalf("expected no run_node calls, got %d", fake.runCalls)
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
	var gotBinding *managedRuntimeBinding
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return map[string]any{
			"status": "ok",
			"config": map[string]any{
				"tenant_id":           "tnt:test",
				"sy_timer_l2_name":    "SY.timer@motherbee",
				"gc_retention_days":   14,
				"gc_interval_seconds": 900,
				"_system": map[string]any{
					"runtime":         "wf.invoice",
					"runtime_version": "1",
					"package_path":    "/var/lib/fluxbee/dist/runtimes/wf.invoice/1",
				},
			},
		}, nil
	}
	fake.setNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error) {
		gotPatch = config
		gotBinding = binding
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
	if _, ok := gotPatch["_system"]; ok {
		t.Fatalf("did not expect _system in config patch: %#v", gotPatch["_system"])
	}
	if gotPatch["tenant_id"] != "tnt:test" || gotPatch["gc_retention_days"] != 14 || gotPatch["gc_interval_seconds"] != 900 {
		t.Fatalf("unexpected preserved config %#v", gotPatch)
	}
	if gotBinding == nil {
		t.Fatalf("expected runtime binding")
	}
	if gotBinding.Runtime != "wf.invoice" || gotBinding.RuntimeVersion != "1" {
		t.Fatalf("unexpected runtime binding %#v", gotBinding)
	}
	if gotBinding.RuntimeBase != workflowRuntimeBase {
		t.Fatalf("unexpected runtime base %#v", gotBinding.RuntimeBase)
	}
	if gotBinding.RequestedRuntimeVersion != "1" || gotBinding.PackagePath == "" {
		t.Fatalf("unexpected runtime binding details %#v", gotBinding)
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

// TestExistingNodeApplyPreservesTimerL2Name verifies that sy_timer_l2_name and
// all operational fields are forwarded to SetNodeConfig exactly as read from
// the existing managed config — no field is silently dropped.
func TestExistingNodeApplyPreservesTimerL2Name(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	var gotPatch map[string]any
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return map[string]any{
			"status": "ok",
			"config": map[string]any{
				"tenant_id":           "tnt:abc123",
				"sy_timer_l2_name":    "SY.timer@motherbee",
				"gc_retention_days":   30,
				"gc_interval_seconds": 1800,
			},
		}, nil
	}
	fake.setNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error) {
		gotPatch = config
		return map[string]any{"status": "ok"}, nil
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if gotPatch["tenant_id"] != "tnt:abc123" {
		t.Fatalf("tenant_id not preserved: %#v", gotPatch["tenant_id"])
	}
	if gotPatch["sy_timer_l2_name"] != "SY.timer@motherbee" {
		t.Fatalf("sy_timer_l2_name not preserved: %#v", gotPatch["sy_timer_l2_name"])
	}
	if gotPatch["gc_retention_days"] != 30 {
		t.Fatalf("gc_retention_days not preserved: %#v", gotPatch["gc_retention_days"])
	}
	if gotPatch["gc_interval_seconds"] != 1800 {
		t.Fatalf("gc_interval_seconds not preserved: %#v", gotPatch["gc_interval_seconds"])
	}
	if _, hasSystem := gotPatch["_system"]; hasSystem {
		t.Fatalf("_system must not appear in config patch (belongs in binding): %#v", gotPatch["_system"])
	}
}

// TestExistingNodeApplyBindsConcretePackagePath verifies that the runtime
// binding sent to SetNodeConfig references the exact versioned package directory
// that was just published, not a wildcard or "current" pointer.
func TestExistingNodeApplyBindsConcretePackagePath(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	var gotBinding *managedRuntimeBinding
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return map[string]any{"status": "ok", "config": map[string]any{}}, nil
	}
	fake.setNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string, config map[string]any, binding *managedRuntimeBinding, notify bool) (map[string]any, error) {
		gotBinding = binding
		return map[string]any{"status": "ok"}, nil
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if gotBinding == nil {
		t.Fatal("expected runtime binding to be set")
	}
	version := fmt.Sprintf("%d", result.Current.Version)
	expectedPath := filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)
	if gotBinding.PackagePath != expectedPath {
		t.Fatalf("package_path = %q, want %q", gotBinding.PackagePath, expectedPath)
	}
	if gotBinding.Runtime != "wf.invoice" {
		t.Fatalf("runtime = %q, want %q", gotBinding.Runtime, "wf.invoice")
	}
	if gotBinding.RuntimeVersion != version {
		t.Fatalf("runtime_version = %q, want %q", gotBinding.RuntimeVersion, version)
	}
	if _, err := os.Stat(gotBinding.PackagePath); err != nil {
		t.Fatalf("bound package_path does not exist on disk: %v", err)
	}
}

// TestRestartFailedCurrentAndPackageStillPresent verifies the partial-success
// invariant: if restart_node fails on both attempts, sy.wf-rules must still
// have rotated current/ to the new definition and published the package to
// dist — the deployment is incomplete but the source of truth is consistent.
func TestRestartFailedCurrentAndPackageStillPresent(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeOrchestratorClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return map[string]any{"status": "ok", "config": map[string]any{}}, nil
	}
	fake.restartNodeFunc = func(ctx context.Context, targetNode, nodeName string) (map[string]any, error) {
		return nil, fmt.Errorf("RESTART_FAILED: node is unresponsive")
	}
	svc.orchestrator = fake

	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if result.WFNode.Action != "restart_failed" {
		t.Fatalf("expected restart_failed, got %q", result.WFNode.Action)
	}

	// current/ must hold the new definition.
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "current", "metadata.json")); err != nil {
		t.Fatalf("current/metadata.json missing after restart_failed: %v", err)
	}

	// Package must be published in dist — a subsequent rollout retry can use it.
	version := fmt.Sprintf("%d", result.Current.Version)
	pkgPath := filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)
	if _, err := os.Stat(filepath.Join(pkgPath, "flow", "definition.json")); err != nil {
		t.Fatalf("dist package flow/definition.json missing after restart_failed: %v", err)
	}
}
