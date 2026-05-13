package node

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	sdk "github.com/4iplatform/json-router/fluxbee-go-sdk"
)

// WF-BUILD-3 — Package-native contract hardening tests.
//
// Covers the 4 cases listed in `docs/onworking COA/wf_v1_tasks.md` §15
// WF-BUILD-3:
//   1. Managed package mode resolves definition from `_system.package_path`.
//   2. CONFIG_SET in managed package mode refuses `workflow_definition_path`
//      mutation with MANAGED_PACKAGE_PATH_LOCKED.
//   3. CONFIG_GET in managed package mode clearly reflects the package-native
//      binding in `effective_config` and `contract`.
//   4. Local-dev / smoke mode with explicit `workflow_definition_path` still
//      works when no managed package binding exists.

func writePackageDefinition(t *testing.T, pkgPath string) string {
	t.Helper()
	flowDir := filepath.Join(pkgPath, "flow")
	if err := os.MkdirAll(flowDir, 0o755); err != nil {
		t.Fatalf("mkdir flow dir: %v", err)
	}
	defPath := filepath.Join(flowDir, "definition.json")
	if err := os.WriteFile(defPath, []byte(`{"id":"wf.test","initial_state":"start","states":{"start":{}}}`), 0o644); err != nil {
		t.Fatalf("write definition.json: %v", err)
	}
	return defPath
}

// Case 1: in managed package mode, LoadConfig derives workflow_definition_path
// from `_system.package_path/flow/definition.json` regardless of (and
// overriding) any value the config carries.
func TestManagedPackageModeResolvesDefinitionFromPackagePath(t *testing.T) {
	dir := t.TempDir()
	pkgPath := filepath.Join(dir, "pkg-wf.test")
	expectedDef := writePackageDefinition(t, pkgPath)
	configPath := filepath.Join(dir, "config.json")
	// Operator (or stale config) accidentally set a different path. Managed
	// mode must override it with the package-native one.
	data := map[string]any{
		"workflow_definition_path": "/some/other/path/wf.json",
		"db_path":                  filepath.Join(dir, "wf.db"),
		"sy_timer_l2_name":         "SY.timer@motherbee",
		"_system": map[string]any{
			"node_name":    "WF.test@motherbee",
			"hive_id":      "motherbee",
			"package_path": pkgPath,
		},
	}
	raw, err := json.Marshal(data)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	if err := os.WriteFile(configPath, raw, 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if !IsManagedPackageMode(cfg) {
		t.Fatalf("expected managed package mode, got cfg=%+v", cfg)
	}
	if cfg.WorkflowDefinitionPath != expectedDef {
		t.Fatalf("expected workflow_definition_path to be package-native %q, got %q", expectedDef, cfg.WorkflowDefinitionPath)
	}
	if got := PackageDefinitionPath(cfg); got != expectedDef {
		t.Fatalf("PackageDefinitionPath(cfg)=%q, expected %q", got, expectedDef)
	}
}

// Case 2: CONFIG_SET that tries to mutate workflow_definition_path in managed
// mode is rejected with MANAGED_PACKAGE_PATH_LOCKED.
func TestApplyWFConfigSetRejectsWorkflowDefinitionPathMutationInManagedMode(t *testing.T) {
	dir := t.TempDir()
	pkgPath := filepath.Join(dir, "pkg-wf.test")
	expectedDef := writePackageDefinition(t, pkgPath)
	configPath := filepath.Join(dir, "config.json")
	current := &Config{
		WorkflowDefinitionPath: expectedDef,
		DBPath:                 filepath.Join(dir, "wf.db"),
		SYTimerL2Name:          "SY.timer@motherbee",
		System: &ManagedSystemConfig{
			NodeName:    "WF.test@motherbee",
			HiveID:      "motherbee",
			PackagePath: pkgPath,
		},
	}
	req := &sdk.NodeConfigSetPayload{
		ApplyMode:     sdk.NodeConfigApplyModeReplace,
		ConfigVersion: 2,
		Config: map[string]any{
			"workflow_definition_path": "/etc/fluxbee/operator-tried-to-change.json",
			"db_path":                  current.DBPath,
			"sy_timer_l2_name":         current.SYTimerL2Name,
		},
	}
	out, nextCfg, err := applyWFConfigSet(configPath, "WF.test@motherbee", current, req)
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if nextCfg != nil {
		t.Fatalf("expected no config mutation, got %+v", nextCfg)
	}
	if ok, _ := out["ok"].(bool); ok {
		t.Fatalf("expected ok=false on rejection, got %+v", out)
	}
	errObj, _ := out["error"].(map[string]any)
	if errObj == nil {
		t.Fatalf("expected error payload, got %+v", out)
	}
	if errObj["code"] != "MANAGED_PACKAGE_PATH_LOCKED" {
		t.Fatalf("expected error.code=MANAGED_PACKAGE_PATH_LOCKED, got %v", errObj["code"])
	}
}

// Case 2b: idempotent CONFIG_SET that passes the same package-resolved path
// is NOT rejected — the operator may roundtrip GET→SET and shouldn't get a
// spurious lock error when nothing meaningful changed.
func TestApplyWFConfigSetAcceptsMatchingPackagePathInManagedMode(t *testing.T) {
	dir := t.TempDir()
	pkgPath := filepath.Join(dir, "pkg-wf.test")
	expectedDef := writePackageDefinition(t, pkgPath)
	configPath := filepath.Join(dir, "config.json")
	current := &Config{
		WorkflowDefinitionPath: expectedDef,
		DBPath:                 filepath.Join(dir, "wf.db"),
		SYTimerL2Name:          "SY.timer@motherbee",
		System: &ManagedSystemConfig{
			NodeName:    "WF.test@motherbee",
			HiveID:      "motherbee",
			PackagePath: pkgPath,
		},
	}
	req := &sdk.NodeConfigSetPayload{
		ApplyMode:     sdk.NodeConfigApplyModeReplace,
		ConfigVersion: 3,
		Config: map[string]any{
			"workflow_definition_path": expectedDef,
			"db_path":                  current.DBPath,
			"sy_timer_l2_name":         current.SYTimerL2Name,
		},
	}
	out, nextCfg, err := applyWFConfigSet(configPath, "WF.test@motherbee", current, req)
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if ok, _ := out["ok"].(bool); !ok {
		t.Fatalf("expected ok=true for idempotent set, got %+v", out)
	}
	if nextCfg == nil {
		t.Fatalf("expected next config to be returned")
	}
	if nextCfg.WorkflowDefinitionPath != expectedDef {
		t.Fatalf("expected next workflow_definition_path=%q, got %q", expectedDef, nextCfg.WorkflowDefinitionPath)
	}
}

// Case 3: CONFIG_GET in managed mode exposes the package-native binding
// clearly in `effective_config` and `contract`.
func TestBuildWFConfigGetPayloadReflectsPackageNativeBindingInManagedMode(t *testing.T) {
	dir := t.TempDir()
	pkgPath := filepath.Join(dir, "pkg-wf.test")
	expectedDef := writePackageDefinition(t, pkgPath)
	cfg := &Config{
		WorkflowDefinitionPath: expectedDef,
		DBPath:                 filepath.Join(dir, "wf.db"),
		SYTimerL2Name:          "SY.timer@motherbee",
		GCRetentionDays:        7,
		GCIntervalSeconds:      3600,
		System: &ManagedSystemConfig{
			NodeName:      "WF.test@motherbee",
			HiveID:        "motherbee",
			PackagePath:   pkgPath,
			ConfigVersion: 5,
		},
	}
	out := buildWFConfigGetPayload("WF.test@motherbee", cfg)

	effective, _ := out["effective_config"].(map[string]any)
	if effective == nil {
		t.Fatalf("expected effective_config map, got %+v", out)
	}
	if effective["managed_package_mode"] != true {
		t.Fatalf("expected managed_package_mode=true, got %v", effective["managed_package_mode"])
	}
	if effective["workflow_definition_source"] != "managed_package" {
		t.Fatalf("expected workflow_definition_source=managed_package, got %v", effective["workflow_definition_source"])
	}
	if effective["package_path"] != pkgPath {
		t.Fatalf("expected package_path=%q, got %v", pkgPath, effective["package_path"])
	}
	if effective["package_definition_path"] != expectedDef {
		t.Fatalf("expected package_definition_path=%q, got %v", expectedDef, effective["package_definition_path"])
	}
	if effective["workflow_definition_path"] != expectedDef {
		t.Fatalf("expected workflow_definition_path=%q, got %v", expectedDef, effective["workflow_definition_path"])
	}

	contract, _ := out["contract"].(map[string]any)
	if contract == nil {
		t.Fatalf("expected contract map, got %+v", out)
	}
	if contract["package_native_binding"] != true {
		t.Fatalf("expected contract.package_native_binding=true, got %v", contract["package_native_binding"])
	}
	if contract["workflow_definition_path_locked"] != true {
		t.Fatalf("expected contract.workflow_definition_path_locked=true, got %v", contract["workflow_definition_path_locked"])
	}
	notes, _ := contract["notes"].([]string)
	if len(notes) == 0 {
		t.Fatalf("expected contract.notes to describe managed-mode binding, got %v", contract["notes"])
	}
	joined := strings.Join(notes, " | ")
	if !strings.Contains(joined, "MANAGED_PACKAGE_PATH_LOCKED") {
		t.Fatalf("expected notes to mention MANAGED_PACKAGE_PATH_LOCKED, got %q", joined)
	}
}

// Case 4: local-dev / smoke mode with explicit workflow_definition_path and no
// managed package binding still works. CONFIG_SET can mutate the path.
func TestLocalDevModeWorkflowDefinitionPathRemainsMutable(t *testing.T) {
	dir := t.TempDir()
	configPath := filepath.Join(dir, "config.json")
	defOne := filepath.Join(dir, "wf.one.json")
	defTwo := filepath.Join(dir, "wf.two.json")
	for _, p := range []string{defOne, defTwo} {
		if err := os.WriteFile(p, []byte(`{"id":"wf.test","initial_state":"start","states":{"start":{}}}`), 0o644); err != nil {
			t.Fatalf("write definition: %v", err)
		}
	}
	// Boot a config with no _system block (local-dev smoke).
	data := map[string]any{
		"workflow_definition_path": defOne,
		"db_path":                  filepath.Join(dir, "wf.db"),
		"sy_timer_l2_name":         "SY.timer@motherbee",
	}
	raw, err := json.Marshal(data)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	if err := os.WriteFile(configPath, raw, 0o644); err != nil {
		t.Fatalf("write config: %v", err)
	}
	cfg, err := LoadConfig(configPath)
	if err != nil {
		t.Fatalf("LoadConfig: %v", err)
	}
	if IsManagedPackageMode(cfg) {
		t.Fatalf("expected local-dev mode (no managed binding), got cfg=%+v", cfg)
	}
	if cfg.WorkflowDefinitionPath != defOne {
		t.Fatalf("expected initial workflow_definition_path=%q, got %q", defOne, cfg.WorkflowDefinitionPath)
	}

	// CONFIG_SET to change the path must succeed in local-dev mode.
	req := &sdk.NodeConfigSetPayload{
		ApplyMode:     sdk.NodeConfigApplyModeReplace,
		ConfigVersion: 1,
		Config: map[string]any{
			"workflow_definition_path": defTwo,
			"db_path":                  cfg.DBPath,
			"sy_timer_l2_name":         cfg.SYTimerL2Name,
		},
	}
	out, nextCfg, err := applyWFConfigSet(configPath, "WF.test@motherbee", cfg, req)
	if err != nil {
		t.Fatalf("unexpected err: %v", err)
	}
	if ok, _ := out["ok"].(bool); !ok {
		t.Fatalf("expected ok=true in local-dev mutation, got %+v", out)
	}
	if nextCfg == nil || nextCfg.WorkflowDefinitionPath != defTwo {
		t.Fatalf("expected mutated workflow_definition_path=%q, got %+v", defTwo, nextCfg)
	}

	// And CONFIG_GET should report source=config (not managed_package).
	view := buildWFConfigGetPayload("WF.test@motherbee", nextCfg)
	effective, _ := view["effective_config"].(map[string]any)
	if effective["managed_package_mode"] != false {
		t.Fatalf("expected managed_package_mode=false in local-dev, got %v", effective["managed_package_mode"])
	}
	if effective["workflow_definition_source"] != "config" {
		t.Fatalf("expected workflow_definition_source=config in local-dev, got %v", effective["workflow_definition_source"])
	}
	if effective["package_path"] != "" {
		t.Fatalf("expected empty package_path in local-dev, got %v", effective["package_path"])
	}
	contract, _ := view["contract"].(map[string]any)
	if contract["package_native_binding"] != false {
		t.Fatalf("expected contract.package_native_binding=false in local-dev, got %v", contract["package_native_binding"])
	}
	if _, locked := contract["workflow_definition_path_locked"]; locked {
		t.Fatalf("expected no workflow_definition_path_locked flag in local-dev, got %v", contract["workflow_definition_path_locked"])
	}
}
