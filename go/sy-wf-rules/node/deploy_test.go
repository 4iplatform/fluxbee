package node

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

type fakeNodeClient struct {
	getNodeConfigFunc func(ctx context.Context, nodeName string) (map[string]any, error)
	runNodeFunc       func(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error)
	restartNodeFunc   func(ctx context.Context, nodeName string) (map[string]any, error)
	setNodeConfigFunc func(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error)
	killNodeFunc      func(ctx context.Context, nodeName string, force, purgeInstance bool) (map[string]any, error)
	getNodeStatusFunc func(ctx context.Context, nodeName string) (map[string]any, error)
	runCalls          int
	restartCalls      int
	setConfigCalls    int
	launchedBy        string
}

func (f *fakeNodeClient) GetNodeConfig(ctx context.Context, nodeName string) (map[string]any, error) {
	if f.getNodeConfigFunc != nil {
		return f.getNodeConfigFunc(ctx, nodeName)
	}
	return nil, &nodeActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
}

func (f *fakeNodeClient) RunNode(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error) {
	f.runCalls++
	f.launchedBy = launchedBy
	if f.runNodeFunc != nil {
		return f.runNodeFunc(ctx, nodeName, runtimeName, version, config, launchedBy)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeNodeClient) RestartNode(ctx context.Context, nodeName string) (map[string]any, error) {
	f.restartCalls++
	if f.restartNodeFunc != nil {
		return f.restartNodeFunc(ctx, nodeName)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeNodeClient) SetNodeConfig(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error) {
	f.setConfigCalls++
	if f.setNodeConfigFunc != nil {
		return f.setNodeConfigFunc(ctx, nodeName, config, binding)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeNodeClient) KillNode(ctx context.Context, nodeName string, force, purgeInstance bool) (map[string]any, error) {
	if f.killNodeFunc != nil {
		return f.killNodeFunc(ctx, nodeName, force, purgeInstance)
	}
	return map[string]any{"status": "ok"}, nil
}

func (f *fakeNodeClient) GetNodeStatus(ctx context.Context, nodeName string) (map[string]any, error) {
	if f.getNodeStatusFunc != nil {
		return f.getNodeStatusFunc(ctx, nodeName)
	}
	return map[string]any{"lifecycle_state": "RUNNING", "health_state": "HEALTHY"}, nil
}

func TestApplyWorkflowAndDeployAutoSpawnFirstDeploy(t *testing.T) {
	svc := newTestService(t)
	fakeAdmin, _ := svc.admin.(*fakeAdminClient)
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, &nodeActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	fake.runNodeFunc = func(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error) {
		if nodeName != "WF.invoice@motherbee" {
			t.Fatalf("unexpected nodeName %q", nodeName)
		}
		if runtimeName != "wf.invoice" || version != "0.0.1" {
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
	svc.nodes = fake

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
	if fakeAdmin == nil || fakeAdmin.publishCalls != 1 {
		t.Fatalf("expected one canonical admin publish call, got %#v", fakeAdmin)
	}
	if _, ok := fakeAdmin.lastPackageFiles["flow/definition.json"]; !ok {
		t.Fatalf("expected flow/definition.json in published package")
	}
	if _, ok := fakeAdmin.lastPackageFiles["config/default-config.json"]; !ok {
		t.Fatalf("expected config/default-config.json in published package")
	}
}

func TestApplyWorkflowAndDeployDoesNotFallbackToSpawnOnConfigLookupTimeout(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, context.DeadlineExceeded
	}
	fake.runNodeFunc = func(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error) {
		t.Fatalf("RunNode must not be called after config lookup timeout")
		return nil, nil
	}
	svc.nodes = fake

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
	if result.WFNode.Action != "none" || result.WFNode.Reason != "node query failed" {
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
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, &nodeActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	svc.nodes = fake

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

func TestApplyWorkflowAndDeployRequiresTenantForFirstDeploy(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, &nodeActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	fake.runNodeFunc = func(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error) {
		t.Fatalf("RunNode must not be called without tenant_id on first deploy")
		return nil, nil
	}
	svc.nodes = fake

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
	if result.WFNode.Action != "none" || result.WFNode.Reason != "tenant_id required for first deploy" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if result.Warning == "" {
		t.Fatalf("expected warning")
	}
	if fake.runCalls != 0 {
		t.Fatalf("expected no run_node calls, got %d", fake.runCalls)
	}
}

func TestApplyWorkflowAndDeployExistingNodeLeavesDeploymentDeferred(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	var gotPatch map[string]any
	var gotBinding *managedRuntimeBinding
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return map[string]any{
			"status": "ok",
			"config": map[string]any{
				"tenant_id":           "tnt:test",
				"sy_timer_l2_name":    "SY.timer@motherbee",
				"gc_retention_days":   14,
				"gc_interval_seconds": 900,
				"_system": map[string]any{
					"runtime":         "wf.invoice",
					"runtime_version": "0.0.1",
					"package_path":    "/var/lib/fluxbee/dist/runtimes/wf.invoice/0.0.1",
				},
			},
		}, nil
	}
	fake.setNodeConfigFunc = func(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error) {
		gotPatch = config
		gotBinding = binding
		return map[string]any{"status": "ok"}, nil
	}
	fake.restartNodeFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		if nodeName != "WF.invoice@motherbee" {
			t.Fatalf("unexpected nodeName %q", nodeName)
		}
		return map[string]any{"status": "ok"}, nil
	}
	svc.nodes = fake

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
	// The rebind changes only the binding: set_node_config merges, so the node's own config
	// (tenant, timer, GC) stays as it is without being copied back (A-68).
	if gotPatch == nil || len(gotPatch) != 0 {
		t.Fatalf("the rebind patch must be empty, got %#v", gotPatch)
	}
	if gotBinding == nil {
		t.Fatalf("expected runtime binding")
	}
	if gotBinding.Runtime != "wf.invoice" || gotBinding.RuntimeVersion != "0.0.1" {
		t.Fatalf("unexpected runtime binding %#v", gotBinding)
	}
	if gotBinding.RuntimeBase != workflowRuntimeBase {
		t.Fatalf("unexpected runtime base %#v", gotBinding.RuntimeBase)
	}
	if gotBinding.RequestedRuntimeVersion != "0.0.1" || gotBinding.PackagePath == "" {
		t.Fatalf("unexpected runtime binding details %#v", gotBinding)
	}
}

func TestApplyWorkflowAndDeploySurfacesSpawnFailureAsPartial(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, &nodeActionError{Status: "error", Code: "NODE_CONFIG_NOT_FOUND", Message: "missing"}
	}
	fake.runNodeFunc = func(ctx context.Context, nodeName, runtimeName, version string, config map[string]any, launchedBy string) (map[string]any, error) {
		return nil, fmt.Errorf("SPAWN_FAILED: boom")
	}
	svc.nodes = fake

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
	if result.WFNode.Action != "restart_failed" {
		t.Fatalf("unexpected wf_node %#v", result.WFNode)
	}
	if result.Warning == "" {
		t.Fatalf("expected warning")
	}
}

func TestApplyWorkflowAndDeployExistingNodeRestartFailureIsPartial(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return map[string]any{
			"status": "ok",
			"config": map[string]any{
				"_system": map[string]any{
					"runtime":         "wf.invoice",
					"runtime_version": "0.0.1",
				},
			},
		}, nil
	}
	fake.restartNodeFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, fmt.Errorf("RESTART_FAILED: boom")
	}
	svc.nodes = fake

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
// A redeploy does not copy the node's config back into the rebind: an older WF node wrote keys at
// the top of its config.json (config_version, its package's definition path), and copying them on
// every rebind would carry them forever. set_node_config merges, so the config stays as it is.
func TestExistingNodeApplySendsOnlyTheBinding(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	var gotPatch map[string]any
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return map[string]any{
			"status": "ok",
			"config": map[string]any{
				"tenant_id":                "tnt:abc123",
				"sy_timer_l2_name":         "SY.timer@motherbee",
				"gc_retention_days":        30,
				"config_version":           4,
				"workflow_definition_path": "/var/lib/fluxbee/dist/runtimes/wf.invoice/0.0.1/flow/definition.json",
			},
		}, nil
	}
	fake.setNodeConfigFunc = func(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error) {
		gotPatch = config
		return map[string]any{"status": "ok"}, nil
	}
	svc.nodes = fake

	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if _, err := svc.ApplyWorkflowAndDeploy(ApplyRequest{WorkflowName: "invoice"}); err != nil {
		t.Fatalf("ApplyWorkflowAndDeploy: %v", err)
	}
	if gotPatch == nil || len(gotPatch) != 0 {
		t.Fatalf("the rebind must carry no config, got %#v", gotPatch)
	}
}

// TestExistingNodeApplyBindsConcretePackagePath verifies that the runtime
// binding sent to SetNodeConfig references the exact versioned package directory
// that was just published, not a wildcard or "current" pointer.
func TestExistingNodeApplyBindsConcretePackagePath(t *testing.T) {
	svc := newTestService(t)
	fake := &fakeNodeClient{}
	var gotBinding *managedRuntimeBinding
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return map[string]any{"status": "ok", "config": map[string]any{}}, nil
	}
	fake.setNodeConfigFunc = func(ctx context.Context, nodeName string, config map[string]any, binding *managedRuntimeBinding) (map[string]any, error) {
		gotBinding = binding
		return map[string]any{"status": "ok"}, nil
	}
	svc.nodes = fake

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
	version := workflowRuntimeVersion(result.Current.Version)
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
	fake := &fakeNodeClient{}
	fake.getNodeConfigFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return map[string]any{"status": "ok", "config": map[string]any{}}, nil
	}
	fake.restartNodeFunc = func(ctx context.Context, nodeName string) (map[string]any, error) {
		return nil, fmt.Errorf("RESTART_FAILED: node is unresponsive")
	}
	svc.nodes = fake

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
	version := workflowRuntimeVersion(result.Current.Version)
	pkgPath := filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)
	if _, err := os.Stat(filepath.Join(pkgPath, "flow", "definition.json")); err != nil {
		t.Fatalf("dist package flow/definition.json missing after restart_failed: %v", err)
	}
}
