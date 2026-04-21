package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"testing"
)

func TestPublishWorkflowPackageWritesDefaultConfigTemplate(t *testing.T) {
	svc := newTestService(t)
	fakeAdmin := &fakeAdminClient{
		publishRuntimePackageFunc: func(_ context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
			rawConfig, ok := packageFiles["config/default-config.json"]
			if !ok {
				t.Fatalf("missing default config in package files")
			}
			var decoded map[string]any
			if err := json.Unmarshal([]byte(rawConfig), &decoded); err != nil {
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
			rawPackage, ok := packageFiles["package.json"]
			if !ok {
				t.Fatalf("missing package.json in package files")
			}
			var packageMeta map[string]any
			if err := json.Unmarshal([]byte(rawPackage), &packageMeta); err != nil {
				t.Fatalf("Unmarshal(package.json): %v", err)
			}
			return &PackagePublishResult{
				RuntimeName: fmt.Sprintf("%v", packageMeta["name"]),
				Version:     fmt.Sprintf("%v", packageMeta["version"]),
				PackagePath: filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "1"),
			}, nil
		},
	}
	svc.admin = fakeAdmin
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
	if fakeAdmin.publishCalls != 1 {
		t.Fatalf("expected one admin publish call, got %d", fakeAdmin.publishCalls)
	}
	if publish.RuntimeName != "wf.invoice" || publish.Version != "1" {
		t.Fatalf("unexpected publish result %#v", publish)
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

type fakeAdminClient struct {
	publishRuntimePackageFunc func(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error)
	publishCalls              int
	lastPackageFiles          map[string]string
}

func (f *fakeAdminClient) PublishRuntimePackage(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error) {
	f.publishCalls++
	f.lastPackageFiles = packageFiles
	if f.publishRuntimePackageFunc != nil {
		return f.publishRuntimePackageFunc(ctx, packageFiles)
	}
	return nil, fmt.Errorf("publishRuntimePackageFunc not configured")
}

func installWorkflowPackageViaFakeAdmin(svc *Service, packageFiles map[string]string) (*PackagePublishResult, error) {
	rawPackage, ok := packageFiles["package.json"]
	if !ok {
		return nil, fmt.Errorf("missing package.json")
	}
	var packageMeta struct {
		Name    string `json:"name"`
		Version string `json:"version"`
	}
	if err := json.Unmarshal([]byte(rawPackage), &packageMeta); err != nil {
		return nil, err
	}
	if packageMeta.Name == "" || packageMeta.Version == "" {
		return nil, fmt.Errorf("package metadata missing name/version")
	}
	packageDir := filepath.Join(svc.cfg.DistRuntimeRoot, packageMeta.Name, packageMeta.Version)
	for relPath, contents := range packageFiles {
		targetPath := filepath.Join(packageDir, filepath.FromSlash(relPath))
		if err := os.MkdirAll(filepath.Dir(targetPath), 0o755); err != nil {
			return nil, err
		}
		if err := os.WriteFile(targetPath, []byte(contents), 0o644); err != nil {
			return nil, err
		}
	}
	if err := svc.updateRuntimeManifest(packageMeta.Name, packageMeta.Version); err != nil {
		return nil, err
	}
	return &PackagePublishResult{
		RuntimeName: packageMeta.Name,
		Version:     packageMeta.Version,
		PackagePath: packageDir,
	}, nil
}
