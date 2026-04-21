package node

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestCompileWorkflowWritesStagedFiles(t *testing.T) {
	svc := newTestService(t)
	meta, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	})
	if err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	if meta.Version != 1 {
		t.Fatalf("unexpected version %d", meta.Version)
	}
	if meta.GuardCount != 1 || meta.StateCount != 2 || meta.ActionCount != 3 {
		t.Fatalf("unexpected counts %#v", meta)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "definition.json")); err != nil {
		t.Fatalf("missing staged definition: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "metadata.json")); err != nil {
		t.Fatalf("missing staged metadata: %v", err)
	}
}

func TestCompileWorkflowAutoIncrementsVersion(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()}); err != nil {
		t.Fatalf("first CompileWorkflow: %v", err)
	}
	meta, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: validWorkflowDefinition()})
	if err != nil {
		t.Fatalf("second CompileWorkflow: %v", err)
	}
	if meta.Version != 2 {
		t.Fatalf("unexpected version %d", meta.Version)
	}
}

func TestCompileWorkflowRejectsInvalidWorkflowName(t *testing.T) {
	svc := newTestService(t)
	_, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "Invoice", Definition: validWorkflowDefinition()})
	if err == nil || !strings.Contains(err.Error(), "INVALID_WORKFLOW_NAME") {
		t.Fatalf("expected invalid name error, got %v", err)
	}
}

func TestCompileWorkflowRejectsInvalidDefinitionAndDoesNotWriteStaged(t *testing.T) {
	svc := newTestService(t)
	bad := validWorkflowDefinition()
	states := bad["states"].([]any)
	firstState := states[0].(map[string]any)
	transitions := firstState["transitions"].([]any)
	firstTransition := transitions[0].(map[string]any)
	firstTransition["guard"] = "event.payload.complete =="

	_, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: bad})
	if err == nil || !strings.Contains(err.Error(), "COMPILE_ERROR") {
		t.Fatalf("expected COMPILE_ERROR, got %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "staged", "metadata.json")); !os.IsNotExist(err) {
		t.Fatalf("expected no staged metadata, got err=%v", err)
	}
}

func newTestService(t *testing.T) *Service {
	t.Helper()
	cfg := NodeConfig{
		HiveID:             "motherbee",
		NodeName:           "SY.wf-rules@motherbee",
		OrchestratorTarget: "SY.orchestrator@motherbee",
		StateDir:           t.TempDir(),
		DistRuntimeRoot:    filepath.Join(t.TempDir(), "dist", "runtimes"),
	}
	svc := NewService(cfg, nil, nil, func() time.Time {
		return time.Date(2026, 4, 16, 12, 0, 0, 0, time.UTC)
	})
	svc.admin = &fakeAdminClient{
		publishRuntimePackageFunc: func(_ context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
			return installWorkflowPackageViaFakeAdmin(svc, packageFiles)
		},
	}
	return svc
}

func TestApplyWorkflowRotatesAndPublishesPackage(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow: %v", err)
	}
	result, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("ApplyWorkflow: %v", err)
	}
	if result.Current.Version != 1 {
		t.Fatalf("unexpected current version %d", result.Current.Version)
	}
	if _, err := os.Stat(filepath.Join(svc.store.Root(), "invoice", "current", "metadata.json")); err != nil {
		t.Fatalf("missing current metadata: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "1", "package.json")); err != nil {
		t.Fatalf("missing package.json: %v", err)
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "1", "flow", "definition.json")); err != nil {
		t.Fatalf("missing flow/definition.json: %v", err)
	}
	manifestPath := filepath.Join(svc.cfg.DistRuntimeRoot, "manifest.json")
	manifestBytes, err := os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	if !strings.Contains(string(manifestBytes), `"wf.invoice"`) {
		t.Fatalf("manifest missing runtime entry: %s", string(manifestBytes))
	}
}

func TestApplyWorkflowRequiresStaged(t *testing.T) {
	svc := newTestService(t)
	_, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	if err == nil || !strings.Contains(err.Error(), "NOTHING_STAGED") {
		t.Fatalf("expected NOTHING_STAGED, got %v", err)
	}
}

func validWorkflowDefinition() map[string]any {
	return map[string]any{
		"wf_schema_version": "1",
		"workflow_type":     "invoice",
		"description":       "issues invoice",
		"input_schema": map[string]any{
			"type":     "object",
			"required": []any{"customer_id"},
			"properties": map[string]any{
				"customer_id": map[string]any{"type": "string"},
			},
		},
		"initial_state":   "collecting_data",
		"terminal_states": []any{"completed"},
		"states": []any{
			map[string]any{
				"name":        "collecting_data",
				"description": "collects invoice data",
				"entry_actions": []any{
					map[string]any{
						"type":   "send_message",
						"target": "IO.quickbooks@motherbee",
						"meta": map[string]any{
							"msg": "INVOICE_CREATE_REQUEST",
						},
						"payload": map[string]any{
							"customer_id": map[string]any{"$ref": "input.customer_id"},
						},
					},
				},
				"exit_actions": []any{},
				"transitions": []any{
					map[string]any{
						"event_match":  map[string]any{"msg": "DATA_VALIDATION_RESPONSE"},
						"guard":        "event.payload.complete == true",
						"target_state": "completed",
						"actions": []any{
							map[string]any{
								"type":  "set_variable",
								"name":  "validated_at",
								"value": "now()",
							},
							map[string]any{
								"type":      "schedule_timer",
								"timer_key": "invoice_timeout",
								"fire_in":   "5m",
							},
						},
					},
				},
			},
			map[string]any{
				"name":          "completed",
				"description":   "done",
				"entry_actions": []any{},
				"exit_actions":  []any{},
				"transitions":   []any{},
			},
		},
	}
}
