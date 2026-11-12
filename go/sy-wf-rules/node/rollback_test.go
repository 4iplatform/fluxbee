package node

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"reflect"
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
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "0.0.1", "package.json")); err != nil {
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

// A deploy whose package SY.admin refuses (another hive holds a WF.<workflow>, FINDINGS A-304) or
// does not take (its runtimes manifest's lock held, RUNTIME_MANIFEST_BUSY) moves no slot: an
// apply's version stays staged, a rollback's backup stays the backup, and current names what the
// WF node runs. The answer is PACKAGE_PUBLISH_FAILED with SY.admin's code and detail, retryable as
// SY.admin says. 0.1.139's integration review: the slots rotated before the publish, so a refused
// apply left current naming a version never published and dropped its backup, and a refused
// rollback rotated them again.
func TestADeployWhosePublishIsRefusedMovesNoSlot(t *testing.T) {
	svc := newTestService(t)
	for i, description := range []string{"issues invoice", "issues invoice v2", "issues invoice v3"} {
		definition := validWorkflowDefinition()
		definition["description"] = description
		if _, err := svc.CompileWorkflow(CompileRequest{WorkflowName: "invoice", Definition: definition}); err != nil {
			t.Fatalf("CompileWorkflow %d: %v", i+1, err)
		}
		if i == 2 {
			break
		}
		if _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); err != nil {
			t.Fatalf("ApplyWorkflow %d: %v", i+1, err)
		}
	}
	slots := func() map[string]uint64 {
		versions := map[string]uint64{}
		for _, slot := range []string{"staged", "current", "backup"} {
			if meta, err := svc.store.ReadMetadata("invoice", slot); err == nil {
				versions[slot] = meta.Version
			}
		}
		return versions
	}
	before := slots()
	if !reflect.DeepEqual(before, map[string]uint64{"staged": 3, "current": 2, "backup": 1}) {
		t.Fatalf("slots before: %v", before)
	}
	admin := svc.admin.(*fakeAdminClient)
	const refusal = "SY.wf-rules@motherbee may not change wf.invoice: wf.invoice is also the package of WF.invoice@worker1, whose config that hive holds, connected or not: only that hive's SY.wf-rules may change it, until its workflow is deleted there"
	const busy = "runtime manifest lock: another writer held SY.admin's own lock of the runtimes manifest for 4s (a publish, a removal or a reset of a runtime ran meanwhile)"
	for _, tc := range []struct {
		name      string
		refused   *adminActionError
		retryable bool
	}{
		{name: "another hive's workflow", refused: &adminActionError{Status: "error", Code: "UNAUTHORIZED", Message: refusal,
			Payload: map[string]any{"status": "error", "error_code": "UNAUTHORIZED", "error_detail": refusal, "payload": nil}}},
		{name: "the manifest's lock held", refused: &adminActionError{Status: "error", Code: "RUNTIME_MANIFEST_BUSY", Message: busy,
			Payload: map[string]any{"status": "error", "error_code": "RUNTIME_MANIFEST_BUSY", "error_detail": busy, "payload": map[string]any{"retryable": true}}},
			retryable: true},
	} {
		admin.publishRuntimePackageFunc = func(context.Context, map[string]string) (*PackagePublishResult, error) {
			return nil, tc.refused
		}
		for _, deploy := range []struct {
			name string
			run  func() error
		}{
			{name: "apply", run: func() error { _, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"}); return err }},
			{name: "rollback", run: func() error { _, err := svc.RollbackWorkflow(RollbackRequest{WorkflowName: "invoice"}); return err }},
		} {
			err := deploy.run()
			var failed WfRulesError
			if !errors.As(err, &failed) || failed.Code != "PACKAGE_PUBLISH_FAILED" || failed.Detail != tc.refused.Error() || failed.Retryable != tc.retryable {
				t.Fatalf("%s, %s: got %#v", tc.name, deploy.name, err)
			}
			if after := slots(); !reflect.DeepEqual(after, before) {
				t.Fatalf("%s, %s moved the slots: %v, were %v", tc.name, deploy.name, after, before)
			}
		}
	}
	// Once SY.admin takes it, the same apply publishes version 3 and moves them.
	admin.publishRuntimePackageFunc = func(_ context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
		return installWorkflowPackageViaFakeAdmin(svc, packageFiles)
	}
	result, err := svc.ApplyWorkflow(ApplyRequest{WorkflowName: "invoice"})
	if err != nil || result.Current.Version != 3 || result.Package.Version != "0.0.3" {
		t.Fatalf("ApplyWorkflow 3: %+v, %v", result, err)
	}
	if after := slots(); !reflect.DeepEqual(after, map[string]uint64{"current": 3, "backup": 2}) {
		t.Fatalf("slots after the apply: %v", after)
	}
}
