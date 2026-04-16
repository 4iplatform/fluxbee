package node

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestPublishWorkflowPackageWritesDefaultConfigTemplate(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatalf("normalizeDefinitionBytes: %v", err)
	}
	meta := WfRulesMetadata{
		Version:         1,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T12:00:00Z",
	}

	publish, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes)
	if err != nil {
		t.Fatalf("PublishWorkflowPackage: %v", err)
	}

	raw, err := os.ReadFile(filepath.Join(publish.PackagePath, "config", "default-config.json"))
	if err != nil {
		t.Fatalf("ReadFile(default-config.json): %v", err)
	}
	var decoded map[string]any
	if err := json.Unmarshal(raw, &decoded); err != nil {
		t.Fatalf("Unmarshal(default-config.json): %v", err)
	}
	if decoded["sy_timer_l2_name"] != "SY.timer@motherbee" {
		t.Fatalf("unexpected sy_timer_l2_name %#v", decoded["sy_timer_l2_name"])
	}
	if int(decoded["gc_retention_days"].(float64)) != defaultWFGCRetentionDays {
		t.Fatalf("unexpected gc_retention_days %#v", decoded["gc_retention_days"])
	}
	if int(decoded["gc_interval_seconds"].(float64)) != defaultWFGCIntervalSeconds {
		t.Fatalf("unexpected gc_interval_seconds %#v", decoded["gc_interval_seconds"])
	}
}

func TestPurgeWorkflowPackagesKeepsCurrentBackupAndBoundVersion(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatalf("normalizeDefinitionBytes: %v", err)
	}
	for version := uint64(1); version <= 4; version++ {
		meta := WfRulesMetadata{
			Version:         version,
			Hash:            HashDefinition(definitionBytes),
			WorkflowName:    "invoice",
			WorkflowType:    "invoice",
			WFSchemaVersion: "1",
			CompiledAt:      "2026-04-16T12:00:00Z",
		}
		if _, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes); err != nil {
			t.Fatalf("PublishWorkflowPackage(%d): %v", version, err)
		}
	}
	if err := svc.store.WriteSlot("invoice", "current", definitionBytes, WfRulesMetadata{
		Version:         4,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T12:00:00Z",
	}); err != nil {
		t.Fatalf("WriteSlot current: %v", err)
	}
	if err := svc.store.WriteSlot("invoice", "backup", definitionBytes, WfRulesMetadata{
		Version:         3,
		Hash:            HashDefinition(definitionBytes),
		WorkflowName:    "invoice",
		WorkflowType:    "invoice",
		WFSchemaVersion: "1",
		CompiledAt:      "2026-04-16T12:00:00Z",
	}); err != nil {
		t.Fatalf("WriteSlot backup: %v", err)
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

	if err := svc.PurgeWorkflowPackages("invoice", true); err != nil {
		t.Fatalf("PurgeWorkflowPackages: %v", err)
	}

	for _, version := range []string{"1", "3", "4"} {
		if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)); err != nil {
			t.Fatalf("expected version %s kept: %v", version, err)
		}
	}
	if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "2")); !os.IsNotExist(err) {
		t.Fatalf("expected version 2 purged, err=%v", err)
	}
}
