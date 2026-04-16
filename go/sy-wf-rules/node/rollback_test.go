package node

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRollbackWorkflowRestoresBackupAndPublishesPackage(t *testing.T) {
	svc := newTestService(t)
	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   validWorkflowDefinition(),
	}); err != nil {
		t.Fatalf("CompileWorkflow v1: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice", Version: 1}); err != nil {
		t.Fatalf("ApplyWorkflow v1: %v", err)
	}
	version2 := validWorkflowDefinition()
	version2["description"] = "issues invoice v2"
	if _, err := svc.CompileWorkflow(CompileRequest{
		WorkflowName: "invoice",
		Definition:   version2,
	}); err != nil {
		t.Fatalf("CompileWorkflow v2: %v", err)
	}
	if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice", Version: 2}); err != nil {
		t.Fatalf("ApplyWorkflow v2: %v", err)
	}

	result, err := svc.RollbackWorkflow(RollbackRequest{WorkflowName: "invoice"})
	if err != nil {
		t.Fatalf("RollbackWorkflow: %v", err)
	}
	if result.Current.Version != 1 {
		t.Fatalf("expected rollback to version 1, got %d", result.Current.Version)
	}
	currentMeta, err := svc.store.ReadCurrentMetadata("invoice")
	if err != nil {
		t.Fatalf("ReadCurrentMetadata: %v", err)
	}
	if currentMeta.Version != 1 {
		t.Fatalf("unexpected current version %d", currentMeta.Version)
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "1", "package.json")); err != nil {
		t.Fatalf("missing restored package: %v", err)
	}
}

func TestRollbackWorkflowRequiresBackup(t *testing.T) {
	svc := newTestService(t)
	_, err := svc.RollbackWorkflow(RollbackRequest{WorkflowName: "invoice"})
	if err == nil || !strings.Contains(err.Error(), "NO_BACKUP") {
		t.Fatalf("expected NO_BACKUP, got %v", err)
	}
}
