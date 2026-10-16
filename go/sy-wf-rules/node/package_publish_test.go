package node

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
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
				PackagePath: filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", "0.0.1"),
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
	if publish.RuntimeName != "wf.invoice" || publish.Version != "0.0.1" {
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
	svc.nodes = &fakeNodeClient{
		getNodeConfigFunc: func(_ context.Context, nodeName string) (map[string]any, error) {
			return map[string]any{
				"status": "ok",
				"config": map[string]any{
					"_system": map[string]any{
						"runtime_version": "0.0.1",
					},
				},
			}, nil
		},
	}

	if err := svc.PurgeWorkflowPackages("invoice", true); err != nil {
		t.Fatalf("PurgeWorkflowPackages: %v", err)
	}

	// Current (4), backup (3) and the version the node is bound to (1) stay; 2 goes, through
	// SY.admin, which writes the motherbee's dist and manifest (A-68): wf-rules does not, even on
	// the motherbee.
	admin := svc.admin.(*fakeAdminClient)
	if strings.Join(admin.removedVersions, ",") != "wf.invoice@0.0.2" {
		t.Fatalf("want only 0.0.2 removed through the admin, got %v", admin.removedVersions)
	}
	for _, version := range []string{"0.0.1", "0.0.2", "0.0.3", "0.0.4"} {
		if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)); err != nil {
			t.Fatalf("wf-rules leaves the dist tree to the admin, %s gone: %v", version, err)
		}
	}
}

// On a worker the dist tree is a receive-only mirror of the motherbee's: purging must ask SY.admin
// to remove the obsolete versions there and leave the local copy alone (lab 2026-09-30: a local
// delete on worker1 was never restored, and the next deploy of that version found no files).
func TestPurgeWorkflowPackagesOnAMirrorHiveGoesThroughTheAdmin(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatalf("normalizeDefinitionBytes: %v", err)
	}
	for version := uint64(1); version <= 3; version++ {
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
	manifestPath := filepath.Join(svc.cfg.DistRuntimeRoot, "manifest.json")
	manifestBefore, err := os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	// The motherbee refuses to remove the current version while others remain: make 0.0.1 the
	// current one and expect it last. And 0.0.2 is already gone there (a directory the manifest
	// no longer lists): that counts as removed.
	manifest, err := loadRuntimeManifest(manifestPath)
	if err != nil {
		t.Fatalf("load manifest: %v", err)
	}
	entry := manifest.Runtimes["wf.invoice"]
	entry.Current = "0.0.1"
	manifest.Runtimes["wf.invoice"] = entry
	if err := writeRuntimeManifest(manifestPath, manifest); err != nil {
		t.Fatalf("write manifest: %v", err)
	}
	manifestBefore, err = os.ReadFile(manifestPath)
	if err != nil {
		t.Fatalf("read manifest: %v", err)
	}
	svc.cfg.HiveID = "worker1"
	admin := svc.admin.(*fakeAdminClient)
	admin.removeErr = map[string]error{
		"0.0.2": &adminActionError{Status: "error", Code: "RUNTIME_VERSION_NOT_FOUND", Message: "gone"},
	}

	if err := svc.PurgeWorkflowPackages("invoice", false); err != nil {
		t.Fatalf("PurgeWorkflowPackages: %v", err)
	}

	want := []string{"wf.invoice@0.0.2", "wf.invoice@0.0.3", "wf.invoice@0.0.1"}
	if strings.Join(admin.removedVersions, ",") != strings.Join(want, ",") {
		t.Fatalf("want removals %v through the admin, got %v", want, admin.removedVersions)
	}
	for _, version := range []string{"0.0.1", "0.0.2", "0.0.3"} {
		if _, err := os.Stat(filepath.Join(svc.cfg.DistRuntimeRoot, "wf.invoice", version)); err != nil {
			t.Fatalf("the local mirror must be left alone, %s gone: %v", version, err)
		}
	}
	if manifestAfter, _ := os.ReadFile(manifestPath); string(manifestAfter) != string(manifestBefore) {
		t.Fatal("the local manifest must be left alone on a mirror hive")
	}
}

type fakeAdminClient struct {
	publishRuntimePackageFunc func(ctx context.Context, packageFiles map[string]string) (*PackagePublishResult, error)
	publishCalls              int
	lastPackageFiles          map[string]string
	removedVersions           []string
	removeErr                 map[string]error
	// currentOnPrimary is refused with RUNTIME_CURRENT_CONFLICT while other versions remain, as
	// the motherbee's admin does.
	currentOnPrimary  string
	versionsOnPrimary int
}

func (f *fakeAdminClient) RemoveRuntimeVersion(_ context.Context, runtimeName, version string) error {
	f.removedVersions = append(f.removedVersions, runtimeName+"@"+version)
	if version == f.currentOnPrimary && f.versionsOnPrimary > 1 {
		return &adminActionError{Status: "error", Code: "RUNTIME_CURRENT_CONFLICT", Message: "cannot delete current version"}
	}
	if err := f.removeErr[version]; err != nil {
		return err
	}
	if f.versionsOnPrimary > 0 {
		f.versionsOnPrimary--
	}
	return nil
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

// A worker's copy of the manifest can lag behind the motherbee's: after a rollback the motherbee's
// current version is the older one. A version the admin refuses as current is removed once the
// others are gone (0.1.70 gate on 8.x, A-68).
func TestPurgeRemovesTheCurrentVersionLastEvenWithALaggingManifest(t *testing.T) {
	svc := newTestService(t)
	definitionBytes, err := normalizeDefinitionBytes(validWorkflowDefinition())
	if err != nil {
		t.Fatal(err)
	}
	for version := uint64(1); version <= 2; version++ {
		meta := WfRulesMetadata{Version: version, Hash: HashDefinition(definitionBytes), WorkflowName: "invoice", WorkflowType: "invoice", WFSchemaVersion: "1", CompiledAt: "2026-04-16T12:00:00Z"}
		if _, err := svc.PublishWorkflowPackage("invoice", meta, definitionBytes); err != nil {
			t.Fatal(err)
		}
	}
	// This hive's copy says 0.0.2 is current; on the motherbee it is 0.0.1.
	svc.cfg.HiveID = "worker1"
	admin := svc.admin.(*fakeAdminClient)
	admin.currentOnPrimary = "0.0.1"
	admin.versionsOnPrimary = 2

	if err := svc.PurgeWorkflowPackages("invoice", false); err != nil {
		t.Fatalf("PurgeWorkflowPackages: %v", err)
	}
	want := "wf.invoice@0.0.1,wf.invoice@0.0.2,wf.invoice@0.0.1"
	if got := strings.Join(admin.removedVersions, ","); got != want {
		t.Fatalf("removals = %s, want %s", got, want)
	}
}
